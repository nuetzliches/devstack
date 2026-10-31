import { defineStore } from 'pinia';
import { computed, ref, shallowRef, triggerRef, watch } from 'vue';
import { api, paths, sharedRef, v1, type GroupAction, type RestartMode, type ServiceAction, type StreamState, type Subscription } from '@/api';
import type { Branches, JournalEntry, LaneView, Point, Pruned, Run, ServiceView, SkillsState, StackEvent, StackView, UpgradeState, UsageReport, McpState } from '@/api/types';
import { laneLabel, MAIN, restartLanesText, restartPlan, samePath } from '@/lib/lanes';
import type { LaneAction } from '@/lib/laneActions';
import { act } from '@/lib/actions';
import { forTree, sameEntry } from '@/lib/journal';
import { persist } from '@/lib/persist';
import { statusBucket } from '@/lib/status';
import { useHub } from './hub';
import { usePool } from './pool';
import { useUi, type ConfirmOption } from './ui';

const MAX_EVENTS = 500;
const HISTORY_WINDOW_MS = 10 * 60_000;

/** An entry of the events timeline (usage ticks are left out). */
export type TimelineEvent = Exclude<StackEvent, { type: 'usage' } | { type: 'lanes' } | { type: 'notices' } | { type: 'journal' }>;

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
  /** What the journal recorded for the tree shown (ADR-0067), newest first: fetched on each snapshot, then live. */
  const journal = shallowRef<JournalEntry[]>([]);
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
  /** Someone else's lane: this machine has no API of it. */
  const foreign = computed(() => !!shared.value);
  /** Someone else's lane, or one whose worktree is gone: nothing here changes it. */
  const readOnly = computed(() => foreign.value || !!ending.value);
  const entry = computed(() => {
    const own = hub.entry(id.value);
    const l = sharedLane.value;
    if (own || !l || !id.value) return own;
    return { id: id.value, name: `${l.repo} · ${l.lane}`, file: '', repo: l.repo, running: true, daemon: null, last_seen: null };
  });
  const running = computed(() => !!entry.value?.running);

  /**
   * The worktrees of a stack whose daemon does not run (ADR-0078), as lanes
   * not started: read from the hub, again when its version of them changes.
   */
  const offlineLanes = ref<LaneView[]>([]);
  /** Which stack `offlineLanes` belongs to: it lags behind `id` while a switch loads. */
  const offlineLanesOf = ref<string | null>(null);
  async function loadOfflineLanes() {
    const sid = id.value;
    if (!sid || running.value || foreign.value) {
      offlineLanes.value = [];
      offlineLanesOf.value = null;
      return;
    }
    try {
      const r = await api().request<LaneView[]>('GET', paths.worktrees(sid));
      if (sid === id.value && r.ok && Array.isArray(r.body)) {
        offlineLanes.value = r.body;
        offlineLanesOf.value = sid;
      }
    } catch {
      /* keep what is known */
    }
  }
  watch(() => [id.value, running.value, id.value ? hub.worktrees[id.value] : undefined] as const, () => void loadOfflineLanes(), { immediate: true });
  /** The lane shown while its stack does not run, `main` too: its start page stands in. */
  const offlineLane = computed(() => {
    if (running.value || foreign.value) return null;
    return lane.value ? (offlineLanes.value.find((l) => l.name === lane.value) ?? null) : mainLane.value;
  });
  /** Start a worktree's lane of a stack that does not run: its daemon comes up, the main tree's services stay stopped. */
  async function startOfflineLane(l: LaneView, on: string | null, profile?: string) {
    const sid = id.value;
    if (!sid) return;
    busy.value['lane-start'] = l.name;
    const r = await act({
      label: on ? `Start ${l.name} on ${on === 'pool' ? 'the pool' : on}` : `Start ${l.name}`,
      path: paths.laneStart(sid),
      body: { path: l.path, ...(on ? { on } : {}), ...(profile ? { profile } : {}) },
      success: on ? `${l.name} starts on ${on === 'pool' ? 'the pool' : on}; its stack's daemon comes up first` : `${l.name} starts here; its stack's daemon comes up first`,
    });
    if (!r.ok) delete busy.value['lane-start'];
  }
  // The daemon is up: its own lane list and stream take over.
  watch(running, (now) => {
    if (now) delete busy.value['lane-start'];
  });
  /** The view shown is not live: hub away, stack down, or not connected yet. */
  const stale = computed(() => !live.value);
  const services = computed(() => view.value?.services ?? []);
  const pending = computed(() => services.value.filter((s) => s.candidate));
  /** The colour of what is not live yet, from the worst of it: a failed
   *  candidate, one held for a promote, else one on its way. */
  const pendingTone = computed<'danger' | 'warn' | 'accent'>(() => {
    const states = pending.value.map((s) => s.candidate!.state);
    if (states.includes('failed')) return 'danger';
    if (states.includes('held')) return 'warn';
    return 'accent';
  });

  // ------------------------------------------------------------ selection

  /** The lane shown (plan §3.5); `null`: the main working tree. */
  const lane = ref<string | null>(null);
  /** The shown lane as it was last, once the daemon no longer lists it. */
  const goneLane = ref<LaneView | null>(null);
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
    if (!sid || !running.value || foreign.value) return;
    try {
      const res = await api().request<LaneView[]>('GET', daemonPath(sid, v1.lanes));
      if (sid === id.value && res.ok && Array.isArray(res.body)) setLanes(res.body);
    } catch {
      // keep the list; the next connect loads it again
    }
    void loadMain();
  }

  /** At most every few seconds: a stream that keeps failing asks once a round. */
  let laneCheckedAt = 0;
  function checkLaneGone() {
    if (Date.now() - laneCheckedAt < 5000) return;
    laneCheckedAt = Date.now();
    void loadLanes();
  }

  /**
   * The lanes now. The one shown whose worktree is gone stays shown, as it
   * was last, until the user leaves it: being removed (`removing`), then
   * removed (`gone`, the dashboard's own mark). Its stream ends with it.
   */
  function setLanes(next: LaneView[]) {
    const before = lane.value ? (lanes.value.find((l) => l.name === lane.value) ?? null) : null;
    const wasActive = lane.value ? laneActive(lane.value) : null;
    lanes.value = next;
    lanesOf.value = id.value;
    if (!lane.value) return;
    const now = next.find((l) => l.name === lane.value) ?? null;
    if (!now) {
      // Never seen (a lane remembered from before a reload): the main tree.
      if (!before && !goneLane.value) return selectLane(null);
      goneLane.value = { ...(before ?? goneLane.value!), removing: false, gone: true };
      disconnect();
      return;
    }
    // Its stream lasts while it stops; when it ends, the next check finds the lane gone.
    if (now.removing) return;
    // Made again (restored): this one is shown now.
    goneLane.value = null;
    // The shown lane started, or stopped: its own stream, or the main tree's.
    if (id.value && wasActive !== laneActive(lane.value)) reset(id.value);
  }
  /**
   * The main working tree as the lane `main` (ADR-0080, ADR-0081), so the
   * header, the start card and the branch picker treat it as any lane: at
   * the repository, on offset 0, holding the main ports unless a lane does,
   * started unless nothing of it runs. `null` for someone else's lane.
   */
  const mainLane = computed<LaneView | null>(() => {
    if (!id.value || foreign.value) return null;
    const mv = mainView.value;
    const services = mv?.services ?? [];
    const containers = (mv?.infra ?? []).flatMap((g) => g.containers).filter((c) => c.managed !== false);
    return {
      name: MAIN,
      path: mv?.repo ?? entry.value?.repo ?? '',
      branch: mv?.branch ?? null,
      number: 0,
      active: mainActive.value,
      port_offset: 0,
      owns_main: !(lanesOf.value === id.value && lanes.value.some((l) => l.owns_main)),
      running: services.filter((x) => statusBucket(x.status) !== 'idle').length,
      services: services.length,
      containers_running: containers.filter((c) => c.state === 'running').length,
      containers: containers.length,
      ...(mv?.profile ? { profile: mv.profile } : {}),
    };
  });
  /**
   * The lane shown, also once it is gone (see `setLanes`); the main working
   * tree as `main`; of a stack that does not run, its worktree as the hub
   * lists it. `null` for someone else's lane, or one not known (yet).
   */
  const shownLane = computed<LaneView | null>(() => {
    if (!lane.value) return mainLane.value;
    const l = lanes.value.find((x) => x.name === lane.value) ?? (goneLane.value?.name === lane.value ? goneLane.value : null);
    if (l) return l;
    return running.value ? null : (offlineLanes.value.find((x) => x.name === lane.value) ?? null);
  });
  /** The shown lane's worktree is gone: it is being removed, or was. */
  const ending = computed(() => (shownLane.value?.removing || shownLane.value?.gone ? shownLane.value : null));

  /** The lanes that run: they keep the daemon when the main tree stops (ADR-0024). */
  const runningLanes = computed(() => (lanesOf.value === id.value ? lanes.value.filter((l) => l.active !== false).map((l) => l.name) : []));
  /** The main tree is shown and nothing of it runs, while its daemon stays for the lanes. */
  const mainIdle = computed(
    () => running.value && lane.value === null && live.value && services.value.length > 0 && services.value.every((s) => statusBucket(s.status) === 'idle'),
  );

  /**
   * The main working tree's view as last seen: its own stream's while it is
   * shown (or the stack page is), else read with the lanes. The stack page
   * and the lane `main` take its branch, state and singletons from it.
   */
  const mainSeen = shallowRef<StackView | null>(null);
  const mainView = computed<StackView | null>(() => (lane.value === null && live.value && view.value ? view.value : mainSeen.value));
  watch([view, live, lane], ([v, l, ln]) => {
    if (v && l && ln === null) mainSeen.value = v;
  });
  async function loadMain() {
    const sid = id.value;
    if (!sid || !running.value || foreign.value || lane.value === null) return;
    try {
      const res = await api().request<StackView>('GET', paths.api(sid, v1.stack));
      if (sid === id.value && res.ok && res.body && typeof res.body === 'object' && Array.isArray(res.body.services)) mainSeen.value = res.body;
    } catch {
      /* keep what is known */
    }
  }
  /**
   * Whether the main working tree runs something: its services, or with
   * none its containers. Not known yet: it does while the daemon runs.
   */
  const mainActive = computed(() => {
    if (!running.value) return false;
    const mv = mainView.value;
    if (!mv) return true;
    if (mv.services.length) return mv.services.some((x) => statusBucket(x.status) !== 'idle');
    const containers = mv.infra.flatMap((g) => g.containers).filter((c) => c.managed !== false);
    return !containers.length || containers.some((c) => c.state === 'running');
  });

  /** A lane that is started; one not started has no API of its own. */
  function laneActive(name: string) {
    return lanes.value.find((l) => l.name === name)?.active !== false;
  }

  /**
   * Show lane `next` (`null`: the main working tree). `remember`: it is the
   * lane an older link without one opens (the stack page follows the main
   * tree's stream without making it the lane shown last).
   */
  function selectLane(next: string | null, remember = true) {
    if (remember && id.value) persist.set(`lane.${id.value}`, next);
    if (next === lane.value) return;
    goneLane.value = null;
    lane.value = next;
    reset(id.value!);
  }

  /** Show stack `next`, on `lane` when named (`null`: the main tree), else on the lane shown last. */
  function select(next: string, opts: { lane?: string | null; remember?: boolean } = {}) {
    if (next === id.value) {
      if (opts.lane !== undefined) selectLane(opts.lane, opts.remember ?? true);
      return;
    }
    id.value = next;
    mainSeen.value = persist.stack(next)?.view ?? null;
    // Another stack's lanes are not this one's, even while it is offline and
    // loads none.
    lanes.value = [];
    lanesOf.value = null;
    goneLane.value = null;
    branches.value = null;
    skills.value = null;
    upgrade.value = null;
    mcp.value = null;
    void loadSkills();
    void loadUpgrade();
    void loadMcp();
    persist.saveLastStack(next);
    lane.value = opts.lane !== undefined ? opts.lane : (persist.get<string | null>(`lane.${next}`) ?? null);
    if (opts.lane !== undefined && (opts.remember ?? true)) persist.set(`lane.${next}`, opts.lane);
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
    journal.value = [];
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
        label: `Lane ${name} serves the main ports`,
        path: daemonPath(id.value, v1.laneMain(name)),
        success: `Lane ${name} serves the main ports`,
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

  /**
   * Restart a started lane where it runs: `warm` its running services from
   * the artifacts there are, `rebuild` built again, `cold` stopped, its data
   * thrown away and started again at the same place (asks first).
   */
  async function restartLane(name: string, mode: RestartMode) {
    if (refuseReadOnly() || !id.value) return;
    const what = { warm: 'Restart', rebuild: 'Rebuild', cold: 'Cold restart' }[mode];
    busy.value['lane-restart'] = name;
    try {
      const r = await act<LaneView[]>({
        label: `${what} lane ${name}`,
        path: daemonPath(id.value, v1.laneRestart(name)),
        body: { mode },
        ...(mode === 'cold'
          ? {
              confirmText:
                'It stops, its containers and volumes go (here and on the nodes it ran on), and it starts again where it ran: built again, its databases seeded again. The worktree stays.',
              confirmLabel: 'Cold restart',
              danger: true,
            }
          : {}),
        success: mode === 'cold' ? `Lane ${name} starts again, fresh` : `${what} lane ${name}: done`,
        conflictHint: mode === 'cold' ? undefined : 'Services that failed keep their running instance.',
      });
      if (r.ok && Array.isArray(r.body)) setLanes(r.body);
    } finally {
      delete busy.value['lane-restart'];
    }
  }

  /**
   * The main working tree's actions as a lane's (ADR-0081), on what its
   * daemon offers: Start its `up`, Stop its `down` (its services and
   * containers; the daemon and the other lanes stay), Restart and Rebuild
   * each of its running services. The rest is disabled in its menu.
   */
  function mainAction(action: LaneAction) {
    switch (action) {
      case 'start':
        return startStack();
      case 'stop':
        return stopMain();
      case 'restart':
        return restartMain('restart');
      case 'rebuild':
        return restartMain('rebuild');
      default:
        return undefined;
    }
  }

  /** Stop the main working tree only: its services and containers; the daemon and every lane keep running. */
  async function stopMain() {
    if (refuseReadOnly() || !id.value) return;
    busy.value['lane-stop'] = MAIN;
    try {
      await act({ label: 'Stop main', path: paths.api(id.value, v1.downTree), success: 'main stopped' });
    } finally {
      delete busy.value['lane-stop'];
    }
  }

  /**
   * Restart (or rebuild) the main working tree's running services where they
   * run, as a lane's warm restart does: the daemon has no restart of the
   * whole main tree, so each one's own, at the same time. Its containers stay.
   */
  async function restartMain(how: 'restart' | 'rebuild') {
    if (refuseReadOnly() || !id.value) return;
    const sid = id.value;
    const what = how === 'restart' ? 'Restart' : 'Rebuild';
    busy.value['lane-restart'] = MAIN;
    try {
      let list = mainView.value?.services ?? [];
      try {
        const res = await api().request<StackView>('GET', paths.api(sid, v1.stack));
        if (res.ok && res.body && typeof res.body === 'object' && Array.isArray(res.body.services)) list = res.body.services;
      } catch {
        /* the last view seen */
      }
      const names = list.filter((x) => statusBucket(x.status) !== 'idle' && (how === 'restart' || x.build_id)).map((x) => x.name);
      if (!names.length) {
        ui.toast('info', `${what} main`, 'Nothing of it runs.');
        return;
      }
      const results = await Promise.all(
        names.map((n) => act({ label: `${what} ${n}`, path: paths.api(sid, v1.serviceAction(n, how)), success: false, conflictHint: 'The running instance stays as it is.' })),
      );
      if (results.every((r) => r.ok)) ui.toast('success', `${what} main: done`);
    } finally {
      delete busy.value['lane-restart'];
    }
  }

  /** One of a lane's actions ([laneActions]), from the header or the branch picker. */
  function laneAction(l: LaneView, action: LaneAction) {
    if (l.name === MAIN) return mainAction(action);
    switch (action) {
      case 'restart':
        return restartLane(l.name, 'warm');
      case 'rebuild':
        return restartLane(l.name, 'rebuild');
      case 'cold':
        return restartLane(l.name, 'cold');
      case 'start':
        return running.value ? startLane(l.name) : startOfflineLane(l, null);
      case 'stop':
        return stopLane(l.name);
      case 'reset':
        return resetLane(l.name);
      case 'remove':
        return removeWorktree(l.path, laneLabel(l));
    }
  }

  /** Throw a stopped lane's data away, here and on its nodes (ADR-0038): its next start is fresh. */
  // ------------------------------------------------------------ agent skills

  /** The agent skills in the stack's repository, against this devstack (`GET /hub/stacks/{id}/skills`). */
  const skills = shallowRef<SkillsState | null>(null);

  async function loadSkills() {
    const sid = id.value;
    if (!sid || foreign.value) return;
    try {
      const res = await api().request<SkillsState>('GET', paths.skills(sid));
      const b = res.body;
      if (sid === id.value && res.ok && b && typeof b === 'object' && Array.isArray(b.outdated)) skills.value = b;
    } catch {
      // nothing shown; the next visit asks again
    }
  }



  /** How far the stack file is behind this devstack (`GET /hub/stacks/{id}/upgrade`): `[stack] reviewed`, not the skills. */
  const upgrade = shallowRef<UpgradeState | null>(null);

  async function loadUpgrade() {
    const sid = id.value;
    if (!sid || foreign.value) return;
    try {
      const res = await api().request<UpgradeState>('GET', paths.upgrade(sid));
      const b = res.body;
      if (sid === id.value && res.ok && b && typeof b === 'object' && typeof b.behind === 'boolean') upgrade.value = b;
    } catch {
      // nothing shown; the next visit asks again
    }
  }

  /** Whether an agent configuration registers `devstack mcp` for the repository (`GET /hub/stacks/{id}/mcp`). */
  const mcp = shallowRef<McpState | null>(null);

  async function loadMcp() {
    const sid = id.value;
    if (!sid || foreign.value) return;
    try {
      const res = await api().request<McpState>('GET', paths.mcp(sid));
      const b = res.body;
      if (sid === id.value && res.ok && b && typeof b === 'object' && typeof b.registered === 'boolean') mcp.value = b;
    } catch {
      // nothing shown; the next visit asks again
    }
  }

  // ------------------------------------------------------------ branches

  /** The repository's branches (ADR-0047), loaded when the picker opens. */
  const branches = shallowRef<Branches | null>(null);

  async function loadBranches() {
    const sid = id.value;
    if (!sid || foreign.value) return;
    try {
      const res = await api().request<Branches>('GET', paths.branches(sid));
      const b = res.body;
      if (sid === id.value && res.ok && b && typeof b === 'object' && Array.isArray(b.branches)) branches.value = b;
    } catch {
      // keep the list; opening the picker again asks again
    }
  }

  /** `git fetch --all --prune` (ADR-0047 point 7): only when asked. */
  async function fetchBranches() {
    if (refuseReadOnly() || !id.value) return;
    busy.value.branches = 'fetch';
    try {
      const r = await act({ label: 'Fetch', path: paths.fetch(id.value), success: false });
      if (r.ok) await loadBranches();
    } finally {
      delete busy.value.branches;
    }
  }

  /**
   * A lane on `branch`, or on a new branch `create` made from it (ADR-0047):
   * the hub makes the worktree, the daemon finds it within a second, and the
   * lane is shown, not started.
   */
  async function newLane(branch: string, create?: string) {
    if (refuseReadOnly() || !id.value) return;
    const sid = id.value;
    busy.value.branches = 'add';
    try {
      const r = await act<{ path: string }>({
        label: create ? `New branch ${create}` : `New lane on ${branch}`,
        path: paths.worktrees(sid),
        body: create ? { branch, new: create } : { branch },
        success: (b) => `Worktree made: ${b?.path}`,
      });
      if (!r.ok || !r.body?.path) return;
      const at = r.body.path;
      await loadBranches();
      // A stack that does not run: its new worktree from the hub, shown with its start page.
      if (!running.value) {
        await loadOfflineLanes();
        const l = offlineLanes.value.find((x) => samePath(x.path, at));
        if (l && sid === id.value) selectLane(l.name);
        return;
      }
      for (let i = 0; i < 20 && sid === id.value && running.value; i++) {
        await loadLanes();
        const l = lanes.value.find((x) => samePath(x.path, at));
        if (l) {
          selectLane(l.name);
          return;
        }
        await new Promise((res) => setTimeout(res, 250));
      }
    } finally {
      delete busy.value.branches;
    }
  }

  /** `git worktree remove` of a stopped lane's worktree; git refuses one with changes. The branch stays. */
  async function removeWorktree(path: string, label: string) {
    if (refuseReadOnly() || !id.value) return;
    const r = await act({
      label: `Remove lane ${label}`,
      path: paths.worktreeRemove(id.value),
      body: { path },
      confirmText: `git removes the worktree ${path}. It refuses one with changes (commit or stash them first); the branch stays.`,
      confirmLabel: 'Remove',
      danger: true,
      success: `Lane ${label} removed`,
    });
    if (!r.ok) return;
    const shown = lanes.value.find((l) => l.name === lane.value);
    if (shown && samePath(shown.path, path)) selectLane(null);
    await loadBranches();
    await loadLanes();
  }



  async function resetLane(name: string) {
    if (!id.value) return;
    const r = await act<Pruned[]>({
      label: `Discard the data of lane ${name}`,
      path: daemonPath(id.value, v1.laneReset(name)),
      confirmText: 'Its containers and volumes go, here and on the nodes it ran on: its databases are seeded again at its next start. The worktree and its builds stay.',
      confirmLabel: 'Discard data',
      danger: true,
      success: false,
    });
    if (!r.ok) return;
    // Removed everywhere, or where it was not: those places keep its data.
    const failed = (r.body ?? []).filter((p) => !p.removed);
    if (!failed.length) ui.toast('success', `Lane ${name} starts fresh next time`);
    else ui.toast('error', `Lane ${name}: not all of its data went`, failed.map((p) => `${p.place}: ${p.error ?? 'not removed'}`).join('; '));
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
        void fetchJournal();
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
      case 'watcher':
        if (view.value) view.value.watcher_error = ev.error;
        save();
        break;
      case 'notices':
        if (view.value) view.value.notices = ev.notices;
        save();
        return; // shown as banners; what happened once is a `notice`
      case 'notice':
        ui.toast(ev.level === 'error' ? 'error' : 'warning', ev.text);
        break;
      case 'usage':
        addUsage(ev);
        return; // not part of the timeline
      case 'lanes':
        setLanes(ev.lanes);
        return; // the lane bar and the stack switcher show it
      case 'journal':
        addJournal([ev.entry]);
        return; // its own list, merged into the timeline by the page
    }
    viewAt.value = Date.now();
    const list = events.value;
    list.unshift(ev);
    if (list.length > MAX_EVENTS) list.length = MAX_EVENTS;
    triggerRef(events);
  }

  /**
   * The journal since the newest entry known, after each snapshot: on
   * connecting, and after the stream fell behind (ADR-0067). An older
   * daemon has none; the page then shows the events only.
   */
  async function fetchJournal() {
    const stackId = id.value;
    if (!stackId) return;
    const since = Math.max(0, (journal.value[0]?.at ?? 0) - 1);
    const lane = apiLane.value;
    // A lane here reads the stack's journal; one on a node, or shared, its own daemon's.
    const ownDaemon = foreign.value || !!lanes.value.find((l) => l.name === lane)?.node;
    try {
      const res = await api().request<JournalEntry[]>('GET', daemonPath(stackId, v1.journal(since)));
      if (!res.ok || !Array.isArray(res.body) || id.value !== stackId || apiLane.value !== lane) return;
      addJournal(res.body.filter((e) => forTree(e, lane, ownDaemon)));
    } catch {
      /* the stream will tell us */
    }
  }

  /** Entries into `journal`, newest first, each once (the fetch and the stream overlap). */
  function addJournal(entries: JournalEntry[]) {
    const fresh = entries.filter((e) => !journal.value.some((k) => sameEntry(k, e)));
    if (!fresh.length) return;
    const list = [...fresh, ...journal.value].sort((a, b) => b.at - a.at);
    if (list.length > MAX_EVENTS) list.length = MAX_EVENTS;
    journal.value = list;
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
    if (ending.value) {
      useUi().toast('info', 'Worktree removed', `The worktree of ${ending.value.branch ?? ending.value.name} is gone, and its lane with it.`);
      return true;
    }
    const l = sharedLane.value;
    useUi().toast('info', 'Read only', `This lane is ${l ? `${l.owner_name}'s` : "someone else's"}: you view it, its owner runs it.`);
    return true;
  }

  /**
   * A click on a link of the lane shown. A shared lane's links name its ports
   * on the node, where nothing on this machine listens: the click opens a
   * forward here to the service on that port instead (ADR-0035), the link's
   * path kept. A link of one's own stack is followed as it is.
   */
  function followLink(e: MouseEvent, url: string) {
    const r = shared.value;
    // The context menu's "open in new tab" is not caught: it gets the link as it is.
    if (!r || e.button > 1) return;
    e.preventDefault();
    let u: URL;
    try {
      u = new URL(url);
    } catch {
      return;
    }
    const port = Number(u.port) || (u.protocol === 'https:' ? 443 : 80);
    const l = sharedLane.value;
    const svc = l?.services.find((s) => (s.here || s.port) === port);
    if (!l || !svc) {
      useUi().toast('error', `Not opening ${u.host}`, `${l ? `${l.owner_name} shares` : 'This lane shares'} nothing on port ${port}.`);
      return;
    }
    const path = u.pathname === '/' && !u.search && !u.hash ? '' : u.pathname + u.search + u.hash;
    void usePool().openShared(r.pool, r.lease, svc.name, path);
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
        conflictHint: action === 'restart' || action === 'rebuild' ? 'Members that failed keep their running instance.' : undefined,
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

  /**
   * Start the stack (`devstack up`); with `shift`, moved by a port shift on
   * this machine (`--shift`, ADR-0082), which sticks to it until `none`.
   */
  async function startStack(shift?: 'auto' | 'none' | number) {
    if (refuseReadOnly()) return;
    if (!id.value) return;
    const stackId = id.value;
    if (running.value) {
      // The daemon runs (for its lanes, or the main tree was stopped): the main tree starts in it.
      busy.value['lane-start'] = MAIN;
      try {
        await act({ label: 'Start main', path: paths.api(stackId, v1.up), success: 'main is starting…' });
      } finally {
        delete busy.value['lane-start'];
      }
      return;
    }
    hub.starting[stackId] = true;
    const res = await act({
      label: `Start ${entry.value?.name ?? 'stack'}`,
      path: paths.up(stackId),
      body: shift === undefined ? undefined : { shift },
      success: shift === undefined ? 'Starting the stack…' : 'Starting the stack with a port shift…',
    });
    if (!res.ok) delete hub.starting[stackId];
  }

  /** What a restart does to the started lanes (ADR-0052). */
  const restartLanes = computed(() => restartPlan(lanesOf.value === id.value ? lanes.value : [], entry.value?.daemon?.carries_lanes));

  /**
   * Down, wait until the daemon is gone, up: picks up the installed devstack
   * and a changed stack file. The daemon leaves its lanes to the next one
   * (ADR-0052): those on nodes keep running, those here restart with it.
   */
  async function restartStack() {
    if (refuseReadOnly()) return;
    if (!id.value) return;
    const stackId = id.value;
    const name = entry.value?.name ?? 'stack';
    const lanesText = restartLanesText(restartLanes.value);
    const ok = await ui.confirm({
      title: `Restart ${name}`,
      text: `All services stop and start again, with the installed devstack and the current stack file. Containers keep running unless the file changes them.${lanesText ? ` ${lanesText}` : ''}`,
      confirmLabel: 'Restart stack',
      danger: !restartLanes.value.carried && !!lanesText,
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
      const down = await act({ label: `Stop ${name}`, path: paths.down(stackId, false, true), success: false });
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

  /** Stop the whole stack (ADR-0081): every lane, the main working tree, and the daemon. */
  async function stopStack() {
    if (refuseReadOnly()) return;
    if (!id.value) return;
    const name = entry.value?.name ?? 'stack';
    const stopping = runningLanes.value;
    // Stop takes the containers too (ADR-0068), unless asked to keep them.
    const containers = (mainView.value ?? view.value)?.infra.some((g) => g.containers.length) ?? true;
    const option = containers ? keepContainers() : undefined;
    const ok = await ui.confirm({
      title: `Stop ${name}`,
      text: stopping.length
        ? `All services and containers of this stack stop: the main working tree's, and ${laneList(stopping)}. Its daemon exits; their volumes stay.`
        : 'All services and containers of this stack stop and its daemon exits. Their volumes stay.',
      confirmLabel: 'Stop stack',
      danger: true,
      option,
    });
    if (!ok) return;
    busy.value['stack'] = 'stop';
    try {
      await act({ label: `Stop ${name}`, path: paths.down(id.value, !option?.checked, false, true), success: 'Stopping the stack…' });
    } finally {
      delete busy.value['stack'];
    }
  }

  /** The choice to leave a stack's containers running when it stops (`devstack down --keep-infra`). */
  function keepContainers(): ConfirmOption {
    return { label: 'Keep its containers running', hint: 'The next start is faster; `devstack down` removes them later.', checked: false };
  }

  function laneList(names: string[]) {
    return names.length === 1 ? `those of lane ${names[0]}` : `those of lanes ${names.join(', ')}`;
  }

  return {
    offlineLanes,
    offlineLanesOf,
    offlineLane,
    startOfflineLane,
    keepContainers,
    id,
    view,
    viewAt,
    stream,
    live,
    stale,
    entry,
    running,
    readOnly,
    foreign,
    shownLane,
    mainLane,
    mainView,
    mainActive,
    loadMain,
    ending,
    sharedLane,
    skills,
    loadSkills,
    upgrade,
    loadUpgrade,
    mcp,
    loadMcp,
    branches,
    loadBranches,
    fetchBranches,
    newLane,
    removeWorktree,
    restartLane,
    laneAction,
    followLink,
    runningLanes,
    mainIdle,
    events,
    journal,
    usage,
    history,
    runs,
    busy,
    services,
    pending,
    pendingTone,
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
    restartStack, restartLanes, shareLane };
});
