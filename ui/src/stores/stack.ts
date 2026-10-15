import { defineStore } from 'pinia';
import { computed, ref, shallowRef, triggerRef, watch } from 'vue';
import { api, paths, v1, type ServiceAction, type StreamState, type Subscription } from '@/api';
import type { Point, Run, ServiceView, StackEvent, StackView, UsageReport } from '@/api/types';
import { act } from '@/lib/actions';
import { persist } from '@/lib/persist';
import { useHub } from './hub';
import { useUi } from './ui';

const MAX_EVENTS = 500;
const HISTORY_WINDOW_MS = 10 * 60_000;

/** An entry of the events timeline (usage ticks are left out). */
export type TimelineEvent = Exclude<StackEvent, { type: 'usage' }>;

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

  const entry = computed(() => hub.entry(id.value));
  const running = computed(() => !!entry.value?.running);
  /** The view shown is not live: hub away, stack down, or not connected yet. */
  const stale = computed(() => !live.value);
  const services = computed(() => view.value?.services ?? []);
  const pending = computed(() => services.value.filter((s) => s.candidate));

  // ------------------------------------------------------------ selection

  function select(next: string) {
    if (next === id.value) return;
    disconnect();
    id.value = next;
    persist.saveLastStack(next);
    const cached = persist.stack(next);
    view.value = cached?.view ?? null;
    viewAt.value = cached?.at ?? null;
    events.value = [];
    usage.value = null;
    history.value = new Map();
    runs.value = {};
    busy.value = {};
    usageFetched = false;
    if (running.value) connect();
  }

  // Follow the hub: connect when the stack starts, drop the stream when it stops.
  watch(running, (r) => {
    if (r && !sub) connect();
    if (!r && sub) {
      disconnect();
      if (viewAt.value === null && view.value) viewAt.value = Date.now();
    }
  });

  function connect() {
    if (!id.value) return;
    const stackId = id.value;
    sub?.close();
    live.value = false;
    sub = api().subscribe<StackEvent>(paths.api(stackId, v1.events), {
      onMessage: (ev) => {
        if (id.value === stackId) handle(ev);
      },
      onState: (s) => {
        stream.value = s;
        if (s !== 'open') {
          if (live.value) viewAt.value = Date.now();
          live.value = false;
        }
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
      const res = await api().request<StackView>('GET', paths.api(stackId, v1.stack));
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
      if (id.value && view.value) persist.saveStack(id.value, { view: view.value, at: Date.now() });
    }, 500);
  }

  // ------------------------------------------------------------ usage

  async function fetchUsage() {
    const stackId = id.value;
    if (!stackId) return;
    usageFetched = true;
    try {
      const res = await api().request<UsageReport>('GET', paths.api(stackId, v1.usage));
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
      const res = await api().request<Run[]>('GET', paths.api(stackId, v1.commandRuns(command)));
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

  async function runCommand(name: string): Promise<number | null> {
    if (!id.value) return null;
    const cmd = view.value?.commands.find((c) => c.name === name);
    const res = await act<{ run: number }>({
      label: `Run ${name}`,
      path: paths.api(id.value, v1.command(name)),
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

  const actionLabels: Record<ServiceAction, string> = { start: 'Start', stop: 'Stop', restart: 'Restart', promote: 'Promote', rollback: 'Roll back' };

  async function serviceAction(name: string, action: ServiceAction) {
    if (!id.value) return;
    const key = `svc:${name}`;
    if (busy.value[key]) return;
    busy.value[key] = action;
    try {
      const res = await act<StackView>({
        label: `${actionLabels[action]} ${name}`,
        path: paths.api(id.value, v1.serviceAction(name, action)),
        success: action === 'promote' ? `${name}: candidate promoted` : action === 'rollback' ? `${name}: rolled back` : `${actionLabels[action]} ${name}: done`,
        conflictHint: action === 'restart' || action === 'promote' ? 'The running instance stays as it is.' : undefined,
      });
      if (res.ok && res.body && 'services' in res.body) applyView(res.body);
    } finally {
      delete busy.value[key];
    }
  }

  async function restartContainer(group: string, service: string) {
    if (!id.value) return;
    const key = `ctr:${group}/${service}`;
    busy.value[key] = 'restart';
    try {
      await act({ label: `Restart ${service}`, path: paths.api(id.value, v1.infraRestart(group, service)), success: `${service}: restart requested` });
    } finally {
      delete busy.value[key];
    }
  }

  async function startStack() {
    if (!id.value) return;
    const stackId = id.value;
    hub.starting[stackId] = true;
    const res = await act({ label: `Start ${entry.value?.name ?? 'stack'}`, path: paths.up(stackId), success: 'Starting the stack…' });
    if (!res.ok) delete hub.starting[stackId];
  }

  /** Down, wait until the daemon is gone, up: picks up a changed stack file. */
  async function restartStack() {
    if (!id.value) return;
    const stackId = id.value;
    const name = entry.value?.name ?? 'stack';
    const ok = await ui.confirm({
      title: `Restart ${name}`,
      text: 'All services stop and start again with the changed stack file. Containers keep running unless the file changes them.',
      confirmLabel: 'Restart stack',
      danger: false,
    });
    if (!ok) return;
    busy.value['stack'] = 'restart';
    try {
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
      const up = await act({ label: `Start ${name}`, path: paths.up(stackId), success: 'Starting the stack with the new stack file…' });
      if (!up.ok) delete hub.starting[stackId];
    } finally {
      delete busy.value['stack'];
    }
  }

  async function stopStack() {
    if (!id.value) return;
    const name = entry.value?.name ?? 'stack';
    const ok = await ui.confirm({
      title: `Stop ${name}`,
      text: 'All services of this stack stop and its daemon exits. Infrastructure containers keep running.',
      confirmLabel: 'Stop stack',
      danger: true,
    });
    if (!ok) return;
    busy.value['stack'] = 'stop';
    try {
      await act({ label: `Stop ${name}`, path: paths.down(id.value, false), success: 'Stopping the stack…' });
    } finally {
      delete busy.value['stack'];
    }
  }

  return {
    id,
    view,
    viewAt,
    stream,
    live,
    stale,
    entry,
    running,
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
    serviceAction,
    restartContainer,
    startStack,
    stopStack,
    restartStack,
  };
});
