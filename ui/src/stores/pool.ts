// The pools this machine belongs to (ADR-0031, ADR-0032, ADR-0034), as the hub
// reads them from their coordinators with this machine's certificates. Read
// on a timer: a coordinator has no stream yet, and its answer is small. The
// pool area shows one pool at a time (`current`); lanes start and move to the
// nodes of all of them.
import { defineStore } from 'pinia';
import { computed, ref } from 'vue';
import { api, paths, Unreachable } from '@/api';
import type { NodePolicy, PoolEntry, PoolNode, PoolOverview, PoolRole, SharedLane, ShareStatus } from '@/api/types';
import { act } from '@/lib/actions';
import { persist } from '@/lib/persist';
import { webUrl } from '@/lib/format';
import { useUi } from './ui';

export const DEFAULT_POOL = 'default';

/** A node a lane can start on or move to, with what `--on` names it by. */
export type LaneTarget = { pool: string; node: PoolNode; on: string; label: string };

export const usePool = defineStore('pool', () => {
  /** The pools this machine belongs to; the default one first. */
  const pools = ref<PoolEntry[]>([{ name: DEFAULT_POOL, joined: false }]);
  /** The pool the pool area shows. */
  const current = ref<string>(persist.get<string>('pool.current') ?? DEFAULT_POOL);
  const overviews = ref<Record<string, PoolOverview>>({});
  /** Whether this machine sends the shown pool telemetry, and whether it collects (ADR-0037). */
  const telemetry = ref<{ consent: boolean; collects: boolean; what: string[] } | null>(null);
  /** Lanes others share with this machine, per pool (ADR-0035). */
  const sharedBy = ref<Record<string, SharedLane[]>>({});
  const overview = computed<PoolOverview | null>(() => overviews.value[current.value] ?? null);
  /** Why the last read failed (the coordinator down, the hub unreachable). */
  const error = ref<string | null>(null);
  const loading = ref(false);
  /** What this machine shares; `null` until read (or from an older hub). */
  const share = ref<ShareStatus | null>(null);
  let timer: ReturnType<typeof setInterval> | null = null;
  let users = 0;

  const isAdmin = computed(() => overview.value?.me?.role === 'admin');
  const pending = computed(() => (overview.value?.members ?? []).filter((m) => m.state === 'pending'));

  /** `path` for the pool `p` (the current one by default). */
  const at = (path: string, p = current.value) => (p === DEFAULT_POOL ? path : `${path}${path.includes('?') ? '&' : '?'}pool=${encodeURIComponent(p)}`);

  function select(p: string) {
    current.value = p;
    persist.set('pool.current', p);
    share.value = null;
    telemetry.value = null;
    void load();
  }

  async function loadShare() {
    try {
      const res = await api().request<ShareStatus>('GET', at(paths.poolShare));
      if (res.ok && res.body && typeof res.body === 'object') share.value = res.body;
    } catch {
      /* keep what is known */
    }
  }

  async function loadTelemetry() {
    try {
      const r = await api().request<{ consent: boolean; collects: boolean; what: string[] }>('GET', at(paths.poolTelemetry));
      if (r.ok && r.body && typeof r.body === 'object') telemetry.value = r.body;
    } catch {
      /* keep what is known */
    }
  }

  /** Where this machine's lanes start without `--on` (ADR-0025): `null` here. */
  const defaultOn = ref<string | null>(null);
  async function loadDefaultOn() {
    try {
      const r = await api().request<{ on: string | null }>('GET', paths.poolDefaultOn);
      if (r.ok && r.body && typeof r.body === 'object') defaultOn.value = r.body.on ?? null;
    } catch {
      /* keep what is known */
    }
  }
  /** Set it: consent to send a worktree there without being asked; `null`: here. */
  async function setDefaultOn(on: string | null) {
    const r = await act<{ on: string | null }>({
      label: 'Where lanes start',
      method: 'PUT',
      path: paths.poolDefaultOn,
      body: { on },
      success: on ? `Lanes start on ${on === 'pool' ? 'the pool' : on} unless told otherwise` : 'Lanes start on this machine unless told otherwise',
    });
    if (r.ok) await loadDefaultOn();
  }

  /** Agree to send the shown pool telemetry, or take it back. */
  async function setTelemetry(enabled: boolean) {
    const r = await act({
      label: enabled ? 'Send telemetry' : 'Stop sending telemetry',
      method: 'PUT',
      path: at(paths.poolTelemetry),
      body: { enabled },
      success: enabled ? 'This machine sends the pool telemetry' : 'This machine sends the pool no telemetry',
    });
    if (r.ok) await loadTelemetry();
  }

  async function load() {
    loading.value = true;
    void loadShare();
    void loadTelemetry();
    void loadDefaultOn();
    try {
      const list = await api().request<PoolEntry[]>('GET', paths.pools);
      if (list.ok && Array.isArray(list.body) && list.body.length) pools.value = list.body;
      if (!pools.value.some((p) => p.name === current.value)) current.value = DEFAULT_POOL;
      // What others share, in every pool.
      for (const p of pools.value) {
        if (!p.joined) continue;
        void api()
          .request<SharedLane[]>('GET', at(paths.poolShared, p.name))
          .then((r) => {
            if (r.ok && Array.isArray(r.body)) sharedBy.value = { ...sharedBy.value, [p.name]: r.body };
          })
          .catch(() => undefined);
      }
      // The other pools' nodes, for starting and moving lanes there.
      for (const p of pools.value) {
        if (p.name === current.value || !p.joined) continue;
        void api()
          .request<PoolOverview>('GET', at(paths.pool, p.name))
          .then((r) => {
            if (r.ok && r.body && typeof r.body === 'object') overviews.value = { ...overviews.value, [p.name]: r.body };
          })
          .catch(() => undefined);
      }
      const shown = current.value;
      const res = await api().request<PoolOverview>('GET', at(paths.pool, shown));
      if (res.ok && res.body && typeof res.body === 'object') {
        overviews.value = { ...overviews.value, [shown]: res.body };
        error.value = null;
      } else {
        const b = res.body as { error?: string } | string | null;
        error.value = (typeof b === 'object' && b?.error) || (typeof b === 'string' && b) || `the hub answered ${res.status}`;
      }
    } catch (e) {
      error.value = e instanceof Unreachable ? 'the hub is not reachable' : String(e);
    } finally {
      loading.value = false;
    }
  }

  /** Keep reading every `ms` while someone shows the pool; `stop` when gone. */
  function watch(ms: number) {
    users++;
    if (!timer) {
      void load();
      timer = setInterval(() => void load(), ms);
    }
    return () => {
      users--;
      if (users <= 0 && timer) {
        clearInterval(timer);
        timer = null;
        users = 0;
      }
    };
  }

  async function approve(id: string, name: string, role: PoolRole = 'member') {
    const r = await act({
      label: `Approve ${name}`,
      path: at(paths.poolApprove(id)),
      body: { role },
      success: `${name} is a ${role} of the pool; it picks up its certificate by itself`,
    });
    if (r.ok) await load();
  }

  async function remove(id: string, name: string, pendingRequest: boolean) {
    const node = overview.value?.nodes?.find((n) => n.id === id);
    const lanesOn = (overview.value?.leases ?? []).filter((l) => l.node === id).length;
    const asNode =
      node && (node.kind === 'server' || node.kind === 'node')
        ? ` It is a ${node.kind === 'server' ? 'server' : 'node'} of the pool: it stops taking lanes${lanesOn ? `, and the ${lanesOn} lane(s) running on it lose their lease` : ''}.`
        : '';
    const r = await act({
      label: pendingRequest ? `Reject ${name}` : `Remove ${name}`,
      path: at(paths.poolRemove(id)),
      confirmText: pendingRequest
        ? `Reject the request of ${name}? It can only ask again with a new key.`
        : `Remove ${name} from the pool? Its certificate is refused from now on; it can only join again with a new key.${asNode}`,
      confirmLabel: pendingRequest ? 'Reject' : 'Remove',
      success: pendingRequest ? `${name}'s request rejected` : `${name} removed from the pool`,
    });
    if (r.ok) await load();
  }

  async function setRole(id: string, name: string, role: PoolRole) {
    const r = await act({
      label: role === 'admin' ? `Make ${name} an admin` : `Make ${name} a member`,
      path: at(paths.poolRole(id)),
      body: { role },
      success: `${name} is ${role === 'admin' ? 'an admin' : 'a member'} now`,
    });
    if (r.ok) await load();
  }

  async function setPaused(id: string, name: string, paused: boolean) {
    const r = await act({
      label: paused ? `Pause ${name}` : `Resume ${name}`,
      path: at(paths.poolPause(id, paused)),
      success: paused ? `${name} takes no new lanes; running ones stay` : `${name} takes lanes again`,
    });
    if (r.ok) await load();
  }

  /** Have a server update its devstack (ADR-0039): it waits until its lanes have ended. */
  async function updateNode(id: string, name: string) {
    const r = await act({
      label: `Update devstack on ${name}`,
      path: at(paths.poolUpdate(id)),
      body: { drain: true },
      confirmText: `${name} takes no new lanes, waits until the ones it runs have ended, installs the latest devstack release (checked against its checksums) and starts its agent again. Its own policy may refuse it.`,
      confirmLabel: 'Update it',
      danger: false,
      success: `${name} updates at its next report`,
    });
    if (r.ok) setTimeout(() => void load(), 1500);
  }

  /** Withdraw an update a server waits for its lanes for (ADR-0039): it takes lanes again. */
  async function cancelUpdate(id: string, name: string) {
    const r = await act({
      label: `Cancel the update of ${name}`,
      path: at(paths.poolUpdate(id)),
      method: 'DELETE',
      success: `${name} takes lanes again after its next report`,
    });
    if (r.ok) setTimeout(() => void load(), 1500);
  }

  /** Have a node remove the stopped lanes it keeps (ADR-0038). */
  async function pruneNode(id: string, name: string) {
    const r = await act({
      label: `Remove the kept lanes on ${name}`,
      path: at(paths.poolPrune(id)),
      confirmText: `The stopped lanes ${name} keeps for their next start go: their mirrors, builds and data. Their next start there is a fresh one. Running lanes stay.`,
      confirmLabel: 'Remove them',
      success: `${name} removes its kept lanes at its next report`,
    });
    if (r.ok) setTimeout(() => void load(), 1500);
  }

  async function endLease(id: string, what: string) {
    const r = await act({
      label: `End ${what}`,
      path: at(paths.poolEnd(id)),
      confirmText: `End ${what}? Its node stops the lane within about 15 s; its owner sees it ended and can start it again.`,
      confirmLabel: 'End it',
      success: `${what} ends`,
    });
    if (r.ok) setTimeout(() => void load(), 1500);
  }

  async function setTags(id: string, name: string, tags: string[]) {
    const r = await act({
      label: `Tags of ${name}`,
      path: at(paths.poolTags(id)),
      body: { tags },
      success: tags.length ? `${name}: ${tags.join(', ')}` : `${name} has no tags`,
    });
    if (r.ok) await load();
  }

  /**
   * Let a member run the pool's lanes (a node), or take that back: the
   * coordinator places no new lanes there then (as for a paused node).
   */
  async function setNode(id: string, name: string, node: boolean) {
    const r = await act({
      label: node ? `Let ${name} run lanes` : `No more lanes on ${name}`,
      path: at(paths.poolNode(id)),
      body: { node },
      success: node ? `${name} may run the pool's lanes` : `${name} takes no new lanes of the pool`,
    });
    if (r.ok) await load();
  }

  /** `null` lifts the node's policy: every member may run any lane there. */
  async function setPolicy(id: string, name: string, policy: NodePolicy | null) {
    const r = await act({
      label: `What runs on ${name}`,
      path: at(paths.poolPolicy(id)),
      body: policy as unknown as Record<string, unknown>,
      success: policy ? `${name}: policy saved` : `${name}: every member may run any lane there`,
    });
    if (r.ok) await load();
  }

  /** Ask to join a pool, under the name `as` here (ADR-0034); it becomes the one shown. */
  async function join(coordinator: string, fingerprint: string, name: string, as = current.value) {
    const r = await act({
      label: as === DEFAULT_POOL ? 'Join the pool' : `Join the pool ${as}`,
      path: at(paths.poolJoin, as),
      body: { coordinator, fingerprint, name },
      success: 'Asked to join: an admin of the pool approves it',
    });
    if (r.ok) select(as);
    return r.ok;
  }

  /** The nodes of every pool that can take a lane now, but this machine. */
  const targets = computed<LaneTarget[]>(() =>
    pools.value.flatMap((p) => {
      const o = overviews.value[p.name];
      return (o?.nodes ?? [])
        .filter((n) => n.online && n.report?.api && n.report.sharing && !n.paused && n.id !== o?.me?.id)
        .map((n) => ({
          pool: p.name,
          node: n,
          on: p.name === DEFAULT_POOL ? n.name : `${p.name}:${n.name}`,
          label: p.name === DEFAULT_POOL ? n.name : `${p.name}:${n.name}`,
        }));
    }),
  );
  /** Pools whose coordinator can place a lane now, with what `--on` names them by. */
  const placeable = computed(() =>
    pools.value
      .filter((p) => targets.value.some((t) => t.pool === p.name))
      .map((p) => ({ pool: p.name, on: p.name === DEFAULT_POOL ? 'pool' : p.name })),
  );
  /** Every lane shared with this machine, with its pool. */
  const shared = computed(() => pools.value.flatMap((p) => (sharedBy.value[p.name] ?? []).map((l) => ({ ...l, pool: p.name }))));

  /** Open a service of a shared lane: a forward here, then the page. */
  async function openShared(pool: string, lease: string, service: string) {
    const r = await act<{ url: string; port: number; warning?: string }>({
      label: `Open ${service}`,
      path: at(paths.poolSharedOpen(lease), pool),
      body: { service },
      success: false,
    });
    if (!r.ok || !r.body?.url) return;
    // Its node or owner runs another devstack: a failure there may be why.
    if (r.body.warning) useUi().toast('warning', `${service}: another devstack`, r.body.warning, 12_000);
    // The scheme comes from the lane's owner: open web pages only.
    const url = webUrl(r.body.url);
    if (url) window.open(url, '_blank', 'noopener');
    else useUi().toast('error', `Not opening ${service}`, `The lane shares it as ${r.body.url}; only http and https addresses are opened.`);
  }

  /** Joined any pool at all. */
  const joinedAny = computed(() => pools.value.some((p) => p.joined));

  return {
    overview,
    overviews,
    pools,
    current,
    targets,
    placeable,
    defaultOn,
    setDefaultOn,
    shared,
    openShared,
    telemetry,
    setTelemetry,
    joinedAny,
    error,
    loading,
    share,
    isAdmin,
    pending,
    select,
    load,
    loadShare,
    watch,
    approve,
    remove,
    setRole,
    setPaused,
    updateNode,
    cancelUpdate,
    pruneNode,
    endLease,
    join,
    setTags,
    setNode,
    setPolicy,
  };
});
