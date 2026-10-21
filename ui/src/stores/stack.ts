import { defineStore } from 'pinia';
import { computed, ref, shallowRef, triggerRef, watch } from 'vue';
import { api, paths, sharedRef, v1, type GroupAction, type ServiceAction, type StreamState, type Subscription } from '@/api';
import type { LaneView, Point, Run, ServiceView, StackEvent, StackView, UsageReport } from '@/api/types';
import { act } from '@/lib/actions';
import { persist } from '@/lib/persist';
import { statusBucket } from '@/lib/status';
import { useHub } from './hub';
import { usePool } from './pool';
import { useUi, type ConfirmOption } from './ui';

const MAX_EVENTS = 500;
const HISTORY_WINDOW_MS = 10 * 60_000;

/** An entry of the events timeline (usage ticks are left out). */
export type TimelineEvent = Exclude<StackEvent, { type: 'usage' } | { type: 'lanes' }>;

/** Keys of usage series: svc:<name>, ctr:<group>/<service>, builds, grp:<name>, lane:<name>. */
export type SeriesKey = string;

/** The selected stack: its last state, live events, usage history, command runs. */
export const useStack = defineStore('stack', () => {
  const hub = useHub();
  const ui = useUi();

  const id = ref<string | null>(null);
  const view = ref<StackView | null>(null);
  /** When `view` was last known to be current (live: now; offline: when we last saw it). */
  const viewAt = ref<number | null>(null);
  const stream = ref<StreamState>('connecting');
  /** A snapshot arrived on the current stream connection. */
  const live = ref(false);
  const events = shallowRef<TimelineEvent[]>([]);
  const usage = shallowRef<UsageReport | null>(null);
  const history = shallowRef<Map<SeriesKey, Point[]>>(new Map());
  const runs = ref<Record<string, Run[]>>({});
  const busy = ref<Record<string, string>>({});
  /** Increments with each `instance` swap or new build; lets views react without deep watching. */
  const changeTick = ref(0);

  let sub: Subscription | null = null;
  let refetchTimer: ReturnType<typeof setTimeout> | null = null;
  let saveTimer: ReturnType<typeof setTimeout> | null = null;
  let usageFetched = false;

  /** A lane someone shares with this machine (ADR-0040): shown as one's own, read only. */
  const shared = computed(() => sharedRef(id.value));
  const sharedLane = computed(() => {
    const r = shared.value;
    return r ? (usePool().shared.find((l) => l.pool === r.pool && l.lease === r.lease) ?? null) : null;
  });
  /** Someone else's lane: nothing here changes it. */
  const readOnly = computed(() => !!shared.value);
  const entry = computed(() => {
    const own = hub.entry(id.value);
    const l = sharedLane.value;
    if (own || !l || !id.value) return own;
    return { id: id.value, name: `${l.repo} · ${l.lane}`, file: '', repo: l.repo, running: true, daemon: null, last_seen: null };
  });
  const running = computed(() => !!entry.value?.running);
  /** The view shown is not live: hub away, stack down, or not connected yet. */
  const stale = computed(() => !live.value);
  const services = computed(() => view.value?.services ?? []);
  const pending = computed(() => services.value.filter((s) => s.candidate));

  // ------------------------------------------------------------ selection

  /** The lane shown (plan §3.5); `null`: the main working tree. */
  const lane = ref<string | null>(null);
  const lanes = ref<LaneView[]>([]);
  /**
   * The lane whose API the daemon paths of the shown stack go to: the shown
   * lane once it is started, else `null` (the main tree, whose stream also
   * carries a lane's start). Pass it to `paths.api` for this stack.
   */
  const apiLane = ref<string | null>(null);
  /** `paths.api` for this store, in the lane shown. */
  const daemonPath = (stack: string, path: string) => paths.api(stack, path, apiLane.value);

  /** Which stack `lanes` belongs to (it lags behind `id` while a switch loads). */
  const lanesOf = ref<string | null>(null);
  /** The last view of each lane seen in this page session, so switching back shows it at once. */
  const laneViews = new Map<string, { view: StackView; at: number }>();

  async function loadLanes() {
    const sid = id.value;
    if (!sid || !running.value || readOnly.value) return;
    try {
      const res = await api().request<LaneView[]>('GET', daemonPath(sid, v1.lanes));
      if (sid === id.value && res.ok && Array.isArray(res.body)) setLanes(res.body);
    } catch {
      // keep the list; the next connect loads it again
    }
  }

  /** At most every few seconds: a stream that keeps failing asks once a round. */
  let laneCheckedAt = 0;
  function checkLaneGone() {
    if (Date.now() - laneCheckedAt < 5000) return;
    laneCheckedAt = Date.now();
    void loadLanes();
  }

  /** The lanes now; the one shown is gone (its worktree was removed): back to the main tree. */
  function setLanes(next: LaneView[]) {
    const wasActive = lane.value ? laneActive(lane.value) : null;
    lanes.value = next;
    lanesOf.value = id.value;
    if (lane.value && !next.some((l) => l.name === lane.value)) {
      const gone = lane.value;
      selectLane(null);
      useUi().toast('info', `Lane ${gone} is gone`, 'Its worktree was removed; devstack removed the lane with it. Showing the main working tree.');
      return;
    }
    // The shown lane started, or stopped: its own stream, or the main tree's.
    if (lane.value && id.value && wasActive !== laneActive(lane.value)) reset(id.value);
  }

  /** The lanes that run: they keep the daemon when the main tree stops (ADR-0024). */
  const runningLanes = computed(() => (lanesOf.value === id.value ? lanes.value.filter((l) => l.active !== false).map((l) => l.name) : []));
  /** The main tree is shown and nothing of it runs, while its daemon stays for the lanes. */
  const mainIdle = computed(
    () => running.value && lane.value === null && live.value && services.value.length > 0 && services.value.every((s) => statusBucket(s.status) === 'idle'),
  );

  /** A lane that is started; one not started has no API of its own. */
  function laneActive(name: string) {
    return lanes.value.find((l) => l.name === name)?.active !== false;
  }

  function selectLane(next: string | null) {
    if (next === lane.value) return;
    lane.value = next;
    if (id.value) persist.set(`lane.${id.value}`, next);
    reset(id.value!);
  }

  function select(next: string) {
    if (next === id.value) return;
    id.value = next;
    // Another stack's lanes are not this one's, even while it is offline and
    // loads none.
    lanes.value = [];
    lanesOf.value = null;
    persist.saveLastStack(next);
    lane.value = persist.get<string | null>(`lane.${next}`) ?? null;
    reset(next);
  }

  function reset(next: string) {
    disconnect();
    // A lane not started is shown with its start, from the main tree's
    // stream (which carries the lanes, its progress included).
    apiLane.value = lane.value && laneActive(lane.value) ? lane.value : null;
    const cached = lane.value ? (laneViews.get(`${next}#${lane.value}`) ?? null) : persist.stack(next);
    view.value = cached?.view ?? null;
    viewAt.value = cached?.at ?? null;
    events.value = [];
    usage.value = null;
    history.value = new Map();
    runs.value = {};
    busy.value = {};
    usageFetched = false;
    if (running.value) connect();
    void loadLanes();
  }

  async function laneMain(name: string) {
    if (!id.value) return;
    busy.value['lane-main'] = 'promote';
    try {
      await act({
        label: name === 'main' ? 'Main ports back to the main tree' : `Lane ${name} serves the main ports`,
        path: daemonPath(id.value, v1.laneMain(name)),
        success: name === 'main' ? 'The main working tree serves the main ports again' : `Lane ${name} serves the main ports`,
      });
      await loadLanes();
    } finally {
      delete busy.value['lane-main'];
    }
  }

  /**
   * Wait until the start of lane `name` on a node is over, as the `lanes`
   * events tell it: `ok` once it runs there, else its error (none when the
   * lane went away, or another stack is shown now).
   */
  function laneStarted(name: string): Promise<{ ok: boolean; error?: string }> {
    const sid = id.value;
    return new Promise((resolve) => {
      let over = false;
      let stop: (() => void) | null = null;
      const check = () => {
        if (over) return;
        const l = id.value === sid ? lanes.value.find((x) => x.name === name) : undefined;
        let result: { ok: boolean; error?: string } | null = null;
        if (!l) result = { ok: false };
        else if (l.moving?.error) result = { ok: false, error: l.moving.error };
        else if (!l.moving && l.active !== false) result = { ok: true };
        else if (!l.moving) result = { ok: false, error: 'its start on the node ended, and it does not run' };
        if (!result) return;
        over = true;
        stop?.();
        resolve(result);
      };
      stop = watch([lanes, id], check);
      check();
    });
  }

  /** Start a lane not started yet (what `devstack up` in its worktree does), then show it. */
  /** `on`: a node of the pool, or `pool` (the coordinator chooses); absent: this machine. */
  async function startLane(name: string, on?: string, profile?: string) {
    const l = lanes.value.find((x) => x.name === name);
    if (!id.value || !l) return;
    busy.value[`lane-start`] = name;
    try {
      const res = await act({
        label: on ? `Start lane ${name} on ${on === 'pool' ? 'the pool' : on}` : `Start lane ${name}`,
        path: daemonPath(id.value, v1.lanes),
        body: { ...(on ? { path: l.path, on } : { path: l.path, infra: true }), ...(profile ? { profile } : {}) },
        success: false,
      });
      if (!res.ok) return;
      // On a node the daemon answers `202` once the lane is leased; the sync
      // and the start there go on, shown as the lane's `moving`.
      if (res.status === 202) {
        const sid = id.value;
        await loadLanes();
        const started = await laneStarted(name);
        if (!started.ok) {
          if (started.error) useUi().toast('error', `Start lane ${name} on ${on === 'pool' ? 'the pool' : on} failed`, started.error);
          return;
        }
        if (id.value !== sid) return;
      }
      if (!id.value) return;
      const up = await act({ label: `Start lane ${name}`, path: daemonPath(id.value, `${v1.lane(name)}/v1/up`), success: `Lane ${name} is starting` });
      await loadLanes();
      // Show it only once the daemon lists it as started: never a lane the
      // list still calls "not started".
      if (up.ok && lanes.value.some((x) => x.name === name && x.active !== false)) selectLane(name);
    } finally {
      delete busy.value['lane-start'];
    }
  }

  /** Stop a lane: its services stop, it is no longer started; the worktree stays (git's). */
  /** Move a started lane to a node of the pool or to `local` (ADR-0025 point 10). */
  /** Share a lane on a node with others in its pool, or with nobody (`[]`), ADR-0035. */
  async function shareLane(name: string, withWhom: string[]) {
    if (!id.value) return;
    const r = await act<LaneView[]>({
      label: withWhom.length ? `Share ${name}` : `Stop sharing ${name}`,
      path: daemonPath(id.value, `/v1/lanes/${encodeURIComponent(name)}/share`),
      body: { with: withWhom },
      success: withWhom.length ? `${name} is shared with ${withWhom.includes('*') ? 'everyone in the pool' : withWhom.join(', ')}` : `${name} is shared with nobody`,
    });
    if (r.ok && Array.isArray(r.body)) setLanes(r.body);
  }

  async function moveLane(name: string, to: string) {
    if (!id.value) return;
    busy.value[`lane-move`] = to;
    try {
      const res = await act({
        label: `Move lane ${name} to ${to === 'local' ? 'this machine' : to}`,
        path: daemonPath(id.value, v1.laneMove(name)),
        body: { to },
        success: `Lane ${name} runs on ${to === 'local' ? 'this machine' : to} now`,
      });
      if (res.ok) await loadLanes();
    } finally {
      delete busy.value['lane-move'];
    }
  }

  async function stopLane(name: string) {
    if (!id.value) return;
    const res = await act({
      label: `Stop lane ${name}`,
      path: daemonPath(id.value, v1.laneStop(name)),
      success: `Lane ${name} stopped`,
    });
    // The lane stays shown, with its start: the eyes are on the button that
    // stopped it, and "Stop stack" there would be the main tree's.
    if (res.ok) await loadLanes();
  }

  /** Throw a stopped lane's data away, here and on its nodes (ADR-0038): its next start is fresh. */
  async function resetLane(name: string) {
    if (!id.value) return;
    await act({
      label: `Discard the data of lane ${name}`,
      path: daemonPath(id.value, v1.laneReset(name)),
      confirmText: 'Its containers and volumes go, here and on the nodes it ran on: its databases are seeded again at its next start. The worktree and its builds stay.',
      confirmLabel: 'Discard data',
      danger: true,
      success: `Lane ${name} starts fresh next time`,
    });
  }

  // Follow the hub: connect when the stack starts, drop the stream when it stops.
  // A lane chosen before the hub said the stack runs (a link, the first load
  // of a page) streams from its own API, which does not carry the lanes:
  // they are read now, as `reset` would have.
  watch(running, (r) => {
    if (r) {
      if (!sub) {
        connect();
        if (lanesOf.value !== id.value) void loadLanes();
      }
      return;
    }
    if (sub) {
      disconnect();
      if (viewAt.value === null && view.value) viewAt.value = Date.now();
    }
    // A start under way, or one that failed, lived in the daemon that is gone.
    if (lanes.value.some((l) => l.moving)) lanes.value = lanes.value.map((l) => ({ ...l, moving: undefined }));
    // Its services went with it (ADR-0009), after its API: it never said so.
    if (view.value?.services.some(ran)) {
      view.value = { ...view.value, services: view.value.services.map((s) => (ran(s) ? { ...s, status: { state: 'stopped' }, pid: null, instance_port: null } : s)) };
      if (id.value && !lane.value) persist.saveStack(id.value, { view: view.value, at: viewAt.value ?? Date.now() });
    }
  });
  /** A service that runs, or is on its way to. */
  const ran = (s: ServiceView) => !['idle', 'failed'].includes(statusBucket(s.status));

  function connect() {
    if (!id.value) return;
    const stackId = id.value;
    sub?.close();
    live.value = false;
    sub = api().subscribe<StackEvent>(daemonPath(stackId, v1.events), {
      onMessage: (ev) => {
        if (id.value === stackId) handle(ev);
      },
      onState: (s) => {
        stream.value = s;
        if (s !== 'open') {
          if (live.value) viewAt.value = Date.now();
          live.value = false;
        }
        // A lane's own stream does not carry the lanes: when it breaks, the
        // lane may be gone (its worktree removed); the main tree says.
        if (s === 'error' && apiLane.value && id.value === stackId) checkLaneGone();
      },
    });
  }

  function disconnect() {
    sub?.close();
    sub = null;
    if (live.value) viewAt.value = Date.now();
    live.value = false;
    stream.value = 'connecting';
    if (refetchTimer) clearTimeout(refetchTimer);
  }

  // ------------------------------------------------------------ events

  function svc(name: string): ServiceView | undefined {
    return view.value?.services.find((s) => s.name === name);
  }

  function handle(ev: StackEvent) {
    switch (ev.type) {
      case 'snapshot':
        view.value = ev.stack;
        live.value = true;
        viewAt.value = Date.now();
        save();
        if (!usageFetched) void fetchUsage();
        break;
      case 'service': {
        const s = svc(ev.service);
        if (s) {
          const buildChanged = s.build_id !== ev.build_id;
          s.status = ev.status;
          s.build_id = ev.build_id;
          s.since = ev.at;
          // pid, ports and proxy are not part of the event: read them once.
          if (buildChanged) {
            changeTick.value++;
            scheduleRefetch();
          } else if (ev.status.state === 'stopped' || ev.status.state === 'healthy') scheduleRefetch();
        } else scheduleRefetch();
        save();
        break;
      }
      case 'candidate': {
        const s = svc(ev.service);
        if (s) {
          const before = s.candidate;
          s.candidate = ev.candidate;
          if (s.proxy) s.proxy = { ...s.proxy, pending: ev.candidate?.build_id ?? null };
          if (ev.candidate === null && before) scheduleRefetch();
          if (ev.candidate?.state === 'failed' && before?.state !== 'failed') ui.toast('error', `${ev.service}: build failed`, 'The running instance stays. See the service for the compiler output.');
          if (ev.candidate?.state === 'held' && before?.state !== 'held') ui.toast('warning', `${ev.service}: candidate held`, 'It changes schema inputs and waits for promote.', 7000);
        } else scheduleRefetch();
        save();
        break;
      }
      case 'devserver_rebuild': {
        const s = svc(ev.service);
        if (s) {
          const before = s.rebuild;
          s.rebuild = ev.rebuild;
          if (ev.rebuild?.state === 'failed' && before?.state !== 'failed') ui.toast('error', `${ev.service}: rebuild failed`, ev.rebuild.errors?.[0] ?? 'See the service for the output.');
        } else scheduleRefetch();
        save();
        break;
      }
      case 'infra':
        scheduleRefetch();
        break;
      case 'command':
        onCommandEvent(ev);
        break;
      case 'config':
        if (view.value) view.value.config_changed = ev.changed;
        save();
        break;
      case 'usage':
        addUsage(ev);
        return; // not part of the timeline
      case 'lanes':
        setLanes(ev.lanes);
        return; // the lane bar and the stack switcher show it
    }
    viewAt.value = Date.now();
    const list = events.value;
    list.unshift(ev);
    if (list.length > MAX_EVENTS) list.length = MAX_EVENTS;
    triggerRef(events);
  }

  /** One fetch of /v1/stack in reaction to events, debounced (fields that events do not carry). */
  function scheduleRefetch() {
    if (refetchTimer) clearTimeout(refetchTimer);
    refetchTimer = setTimeout(() => void refetch(), 300);
  }

  async function refetch() {
    const stackId = id.value;
    if (!stackId || !live.value) return;
    try {
      const res = await api().request<StackView>('GET', daemonPath(stackId, v1.stack));
      if (res.ok && res.body && typeof res.body === 'object' && id.value === stackId) {
        applyView(res.body);
      }
    } catch {
      /* the stream will tell us */
    }
  }

  function applyView(v: StackView) {
    view.value = v;
    viewAt.value = Date.now();
    save();
  }

  function save() {
    if (saveTimer) return;
    saveTimer = setTimeout(() => {
      saveTimer = null;
      if (id.value && view.value && !lane.value) persist.saveStack(id.value, { view: view.value, at: Date.now() });
      if (id.value && view.value && lane.value) laneViews.set(`${id.value}#${lane.value}`, { view: view.value, at: Date.now() });
    }, 500);
  }

  // ------------------------------------------------------------ usage

  async function fetchUsage() {
    const stackId = id.value;
    if (!stackId) return;
    usageFetched = true;
    try {
      const res = await api().request<UsageReport>('GET', daemonPath(stackId, v1.usage));
      if (!res.ok || !res.body || typeof res.body !== 'object' || id.value !== stackId) return;
      const r = res.body;
      const map = new Map<SeriesKey, Point[]>();
      const put = (key: string, h?: Point[]) => {
        if (h?.length) map.set(key, [...h]);
      };
      for (const s of r.services) put(`svc:${s.name}`, s.history);
      for (const c of r.containers) put(`ctr:${c.group}/${c.service}`, c.history);
      put('builds', r.builds.history);
      for (const g of r.groups) put(`grp:${g.name}`, g.history);
      for (const l of r.lanes) put(`lane:${l.name}`, l.history);
      // keep live points that arrived while the request ran
      for (const [k, pts] of history.value) {
        const base = map.get(k) ?? [];
        const lastAt = base.length ? base[base.length - 1].at : 0;
        map.set(k, [...base, ...pts.filter((p) => p.at > lastAt)]);
      }
      history.value = map;
      if (!usage.value || usage.value.at < r.at) usage.value = r;
    } catch {
      usageFetched = false;
    }
  }

  function addUsage(r: UsageReport) {
    usage.value = r;
    const map = history.value;
    const cutoff = r.at - HISTORY_WINDOW_MS;
    const add = (key: string, cpu: number, memory: number) => {
      // new arrays, so computed values derived from a series see the change
      const pts = (map.get(key) ?? []).filter((p) => p.at >= cutoff);
      pts.push({ at: r.at, cpu, memory });
      map.set(key, pts);
    };
    for (const s of r.services) add(`svc:${s.name}`, s.cpu, s.memory);
    for (const c of r.containers) add(`ctr:${c.group}/${c.service}`, c.cpu, c.memory);
    add('builds', r.builds.cpu, r.builds.memory);
    for (const g of r.groups) add(`grp:${g.name}`, g.cpu, g.memory);
    for (const l of r.lanes) add(`lane:${l.name}`, l.cpu, l.memory);
    triggerRef(history);
  }

  // ------------------------------------------------------------ commands

  async function loadRuns(command: string) {
    const stackId = id.value;
    if (!stackId) return;
    try {
      const res = await api().request<Run[]>('GET', daemonPath(stackId, v1.commandRuns(command)));
      if (res.ok && Array.isArray(res.body) && id.value === stackId) {
        // merge: events may already have added newer runs
        const known = new Map((runs.value[command] ?? []).map((r) => [r.id, r]));
        for (const r of res.body) if (!known.has(r.id) || known.get(r.id)!.status !== 'running') known.set(r.id, r);
        runs.value[command] = [...known.values()].sort((a, b) => b.id - a.id);
      }
    } catch {
      /* ignore: shown as empty */
    }
  }

  function onCommandEvent(ev: Extract<StackEvent, { type: 'command' }>) {
    const list = runs.value[ev.command] ?? [];
    const run = list.find((r) => r.id === ev.run);
    if (run) {
      run.status = ev.status;
      if (ev.status !== 'running') run.finished = ev.at;
    } else {
      list.unshift({ id: ev.run, command: ev.command, started: ev.at, finished: ev.status === 'running' ? null : ev.at, status: ev.status, error: null, log: null });
    }
    runs.value[ev.command] = [...list].sort((a, b) => b.id - a.id);
    if (ev.status === 'failed') ui.toast('error', `${ev.command} #${ev.run} failed`);
    else if (ev.status === 'succeeded') ui.toast('success', `${ev.command} #${ev.run} succeeded`);
  }

  /** An action on someone else's lane: said, and not sent (the node refuses it anyway). */
  function refuseReadOnly(): boolean {
    if (!readOnly.value) return false;
    const l = sharedLane.value;
    useUi().toast('info', 'Read only', `This lane is ${l ? `${l.owner_name}'s` : "someone else's"}: you view it, its owner runs it.`);
    return true;
  }

  async function runCommand(name: string): Promise<number | null> {
    if (refuseReadOnly()) return null;
    if (!id.value) return null;
    const cmd = view.value?.commands.find((c) => c.name === name);
    const res = await act<{ run: number }>({
      label: `Run ${name}`,
      path: daemonPath(id.value, v1.command(name)),
      body: { confirm: false },
      danger: true,
      confirmLabel: `Run ${name}`,
      confirmText: cmd?.confirm ?? undefined,
      success: (b) => (b ? `${name}: run #${b.run} started` : `${name} started`),
      conflictHint: cmd?.requires.length ? `Requires: ${cmd.requires.join(', ')}` : undefined,
    });
    return res.ok && res.body ? res.body.run : null;
  }

  // ------------------------------------------------------------ actions

  const actionLabels: Record<ServiceAction, string> = { start: 'Start', stop: 'Stop', restart: 'Restart', rebuild: 'Rebuild', promote: 'Promote', rollback: 'Roll back', 'hot-reload-on': 'Hot reload on for', 'hot-reload-off': 'Hot reload off for' };

  async function serviceAction(name: string, action: ServiceAction) {
    if (refuseReadOnly()) return;
    if (!id.value) return;
    const key = `svc:${name}`;
    if (busy.value[key]) return;
    busy.value[key] = action;
    try {
      const res = await act<StackView>({
        label: `${actionLabels[action]} ${name}`,
        path: daemonPath(id.value, v1.serviceAction(name, action)),
        success: action === 'promote' ? `${name}: candidate promoted` : action === 'rollback' ? `${name}: rolled back` : `${actionLabels[action]} ${name}: done`,
        conflictHint: action === 'restart' || action === 'promote' ? 'The running instance stays as it is.' : undefined,
      });
      if (res.ok && res.body && 'services' in res.body) applyView(res.body);
    } finally {
      delete busy.value[key];
    }
  }

  /** start / stop / restart every member of a group (plan §6). */
  async function groupAction(group: string, label: string, action: GroupAction) {
    if (refuseReadOnly()) return;
    if (!id.value) return;
    const key = `grp:${group}`;
    if (busy.value[key]) return;
    busy.value[key] = action;
    try {
      const res = await act<StackView>({
        label: `${actionLabels[action]} ${label}`,
        path: daemonPath(id.value, v1.groupAction(group, action)),
        success: `${actionLabels[action]} ${label}: done`,
        conflictHint: action === 'restart' ? 'Members that failed keep their running instance.' : undefined,
      });
      if (res.ok && res.body && 'services' in res.body) applyView(res.body);
    } finally {
      delete busy.value[key];
    }
  }

  /** Start, stop or restart one container of the stack shown (a lane: its own). */
  async function containerAction(group: string, service: string, action: 'start' | 'stop' | 'restart') {
    if (refuseReadOnly()) return;
    if (!id.value) return;
    const key = `ctr:${group}/${service}`;
    busy.value[key] = action;
    const label = { start: 'Start', stop: 'Stop', restart: 'Restart' }[action];
    try {
      await act({ label: `${label} ${service}`, path: daemonPath(id.value, v1.infraAction(group, service, action)), success: `${service}: ${action} done` });
    } finally {
      delete busy.value[key];
    }
  }

  function restartContainer(group: string, service: string) {
    return containerAction(group, service, 'restart');
  }

  async function startStack() {
    if (refuseReadOnly()) return;
    if (!id.value) return;
    const stackId = id.value;
    if (running.value) {
      // The daemon stayed for its lanes: the main tree starts in it.
      await act({ label: `Start ${entry.value?.name ?? 'stack'}`, path: paths.api(stackId, v1.up), success: 'Starting the main working tree…' });
      return;
    }
    hub.starting[stackId] = true;
    const res = await act({ label: `Start ${entry.value?.name ?? 'stack'}`, path: paths.up(stackId), success: 'Starting the stack…' });
    if (!res.ok) delete hub.starting[stackId];
  }

  /** Down, wait until the daemon is gone, up: picks up a changed stack file. */
  async function restartStack() {
    if (refuseReadOnly()) return;
    if (!id.value) return;
    const stackId = id.value;
    const name = entry.value?.name ?? 'stack';
    const kept = runningLanes.value;
    const ok = await ui.confirm({
      title: `Restart ${name}`,
      text: kept.length
        ? `The main working tree's services stop and start again. ${laneList(kept)} keep running, and with them the daemon: the installed devstack and a changed stack file take effect once no lane runs.`
        : 'All services stop and start again, with the installed devstack and the current stack file. Containers keep running unless the file changes them.',
      confirmLabel: 'Restart stack',
      danger: false,
    });
    if (!ok) return;
    busy.value['stack'] = 'restart';
    try {
      // A stack file this devstack cannot load (it needs a newer one) would
      // stop the stack for good: say so and leave it running (#11).
      const repo = entry.value?.repo;
      if (repo) {
        const res = await api().request<{ valid?: { Err?: string } | null }>('GET', paths.inspect(repo));
        const why = res.ok && res.body && typeof res.body === 'object' ? res.body.valid?.Err : null;
        if (why) {
          ui.toast('error', `${name} would not start again`, `${why} It keeps running.`, 15000);
          return;
        }
      }
      if (kept.length) {
        // The daemon stays for its lanes: down and up in it.
        const down = await act({ label: `Stop ${name}`, path: paths.api(stackId, v1.down), success: false });
        if (!down.ok) return;
        await act({ label: `Start ${name}`, path: paths.api(stackId, v1.up), success: 'Starting the main working tree again…' });
        return;
      }
      const down = await act({ label: `Stop ${name}`, path: paths.down(stackId, false), success: false });
      if (!down.ok) return;
      // The hub's stack stream says when the daemon is gone; 60 s at most.
      const gone = await new Promise<boolean>((resolve) => {
        const timer = setTimeout(() => {
          stop();
          resolve(false);
        }, 60_000);
        const stop = watch(
          () => hub.entry(stackId)?.running,
          (r) => {
            if (r === false) {
              clearTimeout(timer);
              stop();
              resolve(true);
            }
          },
          { immediate: true },
        );
      });
      if (!gone) {
        ui.toast('error', `${name} did not stop within 60 s`, 'Start it again once it is down.');
        return;
      }
      hub.starting[stackId] = true;
      const up = await act({ label: `Start ${name}`, path: paths.up(stackId), success: 'Starting the stack again…' });
      if (!up.ok) delete hub.starting[stackId];
    } finally {
      delete busy.value['stack'];
    }
  }

  async function stopStack() {
    if (refuseReadOnly()) return;
    if (!id.value) return;
    const name = entry.value?.name ?? 'stack';
    const kept = runningLanes.value;
    // Lanes that run keep the containers they share (ADR-0030): nothing to choose.
    const containers = !kept.length && (view.value?.infra.some((g) => g.containers.length) ?? true);
    // Containers only, nothing else to stop: then they are what "stop" means.
    const option = containers ? stopContainers(view.value !== null && view.value.services.length === 0) : undefined;
    const ok = await ui.confirm({
      title: `Stop ${name}`,
      text: kept.length
        ? `The main working tree's services stop. ${laneList(kept)} keep running, and with them the daemon. Infrastructure containers keep running.`
        : 'All services of this stack stop and its daemon exits.',
      confirmLabel: 'Stop stack',
      danger: true,
      option,
    });
    if (!ok) return;
    busy.value['stack'] = 'stop';
    try {
      await act({ label: `Stop ${name}`, path: paths.down(id.value, !!option?.checked), success: 'Stopping the stack…' });
    } finally {
      delete busy.value['stack'];
    }
  }

  /** The choice to take a stack's containers down with it (`devstack down --infra`). */
  function stopContainers(checked: boolean): ConfirmOption {
    return { label: 'Stop its containers too', hint: 'Docker Compose takes them down; their volumes stay.', checked };
  }

  function laneList(names: string[]) {
    return names.length === 1 ? `Lane ${names[0]} and its ports` : `Lanes ${names.join(', ')} and their ports`;
  }

  return {
    stopContainers,
    id,
    view,
    viewAt,
    stream,
    live,
    stale,
    entry,
    running,
    readOnly,
    sharedLane,
    runningLanes,
    mainIdle,
    events,
    usage,
    history,
    runs,
    busy,
    services,
    pending,
    changeTick,
    select,
    svc,
    loadRuns,
    runCommand,
    lane,
    lanes,
    lanesOf,
    apiLane,
    loadLanes,
    selectLane,
    laneMain,
    startLane,
    stopLane,
    resetLane,
    moveLane,
    serviceAction,
    groupAction,
    restartContainer,
    containerAction,
    startStack,
    stopStack,
    restartStack, shareLane };
});
