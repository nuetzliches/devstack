// The pools this machine belongs to (ADR-0031, ADR-0032, ADR-0034), as the hub
// reads them from their coordinators with this machine's certificates. Read
// when shown, and again when the hub says a pool changed (`pools` on
// `/hub/events`, ADR-0073): never on a timer, but while this machine waits
// for its approval (it has no certificate to follow a stream with yet). The
// pool area shows one pool at a time (`current`); lanes start and move to the
// nodes of all of them.
import { defineStore } from 'pinia';
import { computed, ref } from 'vue';
import { api, paths, Unreachable } from '@/api';
import type { NodePolicy, PoolEntry, PoolFeed, PoolMember, PoolNode, PoolOverview, PoolRole, SharedLane, ShareStatus } from '@/api/types';
import { act } from '@/lib/actions';
import { persist } from '@/lib/persist';
import { webUrl } from '@/lib/format';
import { useUi } from './ui';

export const DEFAULT_POOL = 'default';

/** A node a lane can start on or move to, with what `--on` names it by. */
export type LaneTarget = { pool: string; node: PoolNode; on: string; label: string };

/** A machine of a pool: its membership and, once it reports, its node side. */
export type PoolMachine = { id: string; name: string; member?: PoolMember; node?: PoolNode };

/** Whether this machine is connected to a shared lane: its services forwarded here (ADR-0053). */
export function connectedTo(l: SharedLane): boolean {
  return l.services.some((s) => !!s.forwarded);
}

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
  /** The pools as the hub follows them (ADR-0073); empty from a hub that does not. */
  const feeds = ref<Record<string, PoolFeed>>({});
  /** The versions each pool had when it was read last. */
  let readAt: Record<string, string> = {};
  /** While this machine waits for its approval: read again until it has it. */
  let pendingTimer: ReturnType<typeof setInterval> | null = null;
  let users = 0;
  /** A read under way, and whether another is due once it is done. */
  let reading: Promise<void> | null = null;
  let again = false;
  /** How the shown pool is followed; `null` from a hub that does not follow pools. */
  const live = computed<PoolFeed | null>(() => feeds.value[current.value] ?? null);

  const isAdmin = computed(() => overview.value?.me?.role === 'admin');
  const pending = computed(() => (overview.value?.members ?? []).filter((m) => m.state === 'pending'));
  /** The shown pool's machines: each approved member with its node side, if it reports one. */
  const machines = computed<PoolMachine[]>(() => {
    const o = overview.value;
    const members = (o?.members ?? []).filter((m) => m.state === 'approved');
    const nodes = o?.nodes ?? [];
    const ids = [...new Set([...members.map((m) => m.id), ...nodes.map((n) => n.id)])];
    return ids.map((id) => {
      const member = members.find((m) => m.id === id);
      const node = nodes.find((n) => n.id === id);
      return { id, name: member?.name ?? node?.name ?? id, member, node };
    });
  });
  /** A member's or node's name in the shown pool, else its id. */
  const nameOf = (id: string) => (overview.value?.members ?? []).find((m) => m.id === id)?.name ?? overview.value?.nodes?.find((n) => n.id === id)?.name ?? id;

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

  /** What others share with this machine in pool `p`, and which of it is connected. */
  async function loadShared(p: string) {
    try {
      const r = await api().request<SharedLane[]>('GET', at(paths.poolShared, p));
      if (r.ok && Array.isArray(r.body)) sharedBy.value = { ...sharedBy.value, [p]: r.body };
    } catch {
      // The hub away: the list stays as it was.
    }
  }

  /** Read everything shown; one read at a time, one more after it when asked meanwhile. */
  async function load() {
    if (reading) {
      again = true;
      return reading;
    }
    reading = (async () => {
      do {
        again = false;
        await readAll();
      } while (again);
    })();
    try {
      await reading;
    } finally {
      reading = null;
    }
  }

  /** The version key of what a pool's read shows: every part but the MCP upstreams (their own page). */
  function stamp(f: PoolFeed | undefined) {
    if (!f) return '';
    const v = f.versions;
    return [f.live, v.members, v.nodes, v.leases, v.telemetry].join('|');
  }

  /** The hub's pools changed (ADR-0073): read again what changed, if anything is shown. */
  function onFeed(next: Record<string, PoolFeed>) {
    const before = feeds.value;
    feeds.value = next;
    if (users <= 0) return;
    const changed = Object.keys({ ...before, ...next }).some((p) => stamp(next[p]) !== (readAt[p] ?? ''));
    if (changed) void load();
    const t = next[current.value]?.versions.telemetry;
    if (t && t !== before[current.value]?.versions.telemetry) void loadTelemetry();
  }

  async function readAll() {
    readAt = Object.fromEntries(Object.entries(feeds.value).map(([p, f]) => [p, stamp(f)]));
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
        if (p.joined) void loadShared(p.name);
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
      waitForApproval();
    }
  }

  /** Read again every 10 s while the shown pool has not approved this machine yet, and stop then. */
  function waitForApproval() {
    const waiting = users > 0 && overview.value?.me?.state === 'pending';
    if (waiting && !pendingTimer) pendingTimer = setInterval(() => void load(), 10_000);
    if (!waiting && pendingTimer) {
      clearInterval(pendingTimer);
      pendingTimer = null;
    }
  }

  /** Read once now, and again on each change the hub tells, while someone shows the pool; `stop` when gone. */
  function watch() {
    users++;
    if (users === 1) {
      readAt = {};
      void load();
    }
    return () => {
      users = Math.max(0, users - 1);
      if (users === 0) waitForApproval();
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

  /** Let a member's lanes cause provisioning (ADR-0051 point 3), or take that back. */
  async function setProvision(id: string, name: string, on: boolean) {
    const r = await act({
      label: on ? `Let ${name} cause provisioning` : `No provisioning for ${name}`,
      path: at(paths.poolProvision(id)),
      body: { on },
      success: on ? `${name}'s lanes may have the pool make a node` : `${name}'s lanes make no node any more`,
    });
    if (r.ok) await load();
  }

  /** Confirm a provisioned node (ADR-0051 point 6): it admits lanes from now on. */
  async function confirmNode(id: string, name: string, machine: string, address: string, key?: string) {
    const r = await act({
      label: `Confirm ${name}`,
      path: at(paths.poolConfirm(id)),
      confirmText: `Confirm ${name} as the machine the provisioner made? Machine ${machine}, address ${address}${key ? `, key ${key}` : ''}: check them against the provider's console. It admits lanes from now on.`,
      confirmLabel: 'Confirm it',
      danger: false,
      success: `${name} admits lanes`,
    });
    if (r.ok) await load();
  }

  /** Give a provisioned node back by hand (ADR-0051 point 9). */
  async function giveBack(id: string, name: string) {
    const lanesOn = (overview.value?.leases ?? []).filter((l) => l.node === id).length;
    const r = await act({
      label: `Give ${name} back`,
      path: at(paths.poolGiveBack(id)),
      confirmText: `Give ${name} back to its provider? The machine is deleted with all it holds${lanesOn ? `, and the ${lanesOn} lane(s) running on it end` : ''}.`,
      confirmLabel: 'Give it back',
      success: `${name} is given back`,
    });
    if (r.ok) setTimeout(() => void load(), 1500);
  }

  /** Whether this member's lanes may run on provisioned nodes (ADR-0051 point 10). */
  async function setProvisioned(on: boolean) {
    const r = await act({
      label: on ? 'Lanes on provisioned nodes' : 'No lanes on provisioned nodes',
      method: 'PUT',
      path: at(paths.poolProvisioned),
      body: { on },
      success: on ? "This machine's lanes may run on provisioned nodes" : "This machine's lanes stay off provisioned nodes",
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

  /** Connect to a shared lane: each of its services forwarded here, on its owner's port if free (ADR-0053). */
  async function connectShared(pool: string, lease: string) {
    const r = await act<{ services: { name: string; port: number; url: string }[]; warning?: string }>({
      label: 'Connect',
      path: at(paths.poolSharedConnect(lease), pool),
      success: (b) => `Connected: ${(b?.services ?? []).map((s) => `${s.name} on localhost:${s.port}`).join(', ') || 'it shares no web page'}`,
    });
    if (r.ok && r.body?.warning) useUi().toast('warning', 'Another devstack', r.body.warning, 12_000);
    await loadShared(pool);
  }

  /** Disconnect from a shared lane: its forwards close, its ports here are free again (ADR-0053). */
  async function disconnectShared(pool: string, lease: string) {
    await act({ label: 'Disconnect', path: at(paths.poolSharedDisconnect(lease), pool), success: 'Disconnected: its ports here are free again' });
    await loadShared(pool);
  }

  /** Open a service of a shared lane: its lane connected if it is not (ADR-0053), then the page; `path` (`/ui?x#y`) after its address. */
  async function openShared(pool: string, lease: string, service: string, path = '') {
    const r = await act<{ url: string; port: number; warning?: string }>({
      label: `Open ${service}`,
      path: at(paths.poolSharedOpen(lease), pool),
      body: { service },
      success: false,
    });
    void loadShared(pool);
    if (!r.ok || !r.body?.url) return;
    // Its node or owner runs another devstack: a failure there may be why.
    if (r.body.warning) useUi().toast('warning', `${service}: another devstack`, r.body.warning, 12_000);
    // The scheme comes from the lane's owner: open web pages only.
    const url = webUrl(r.body.url.replace(/\/$/, '') + path);
    if (url) window.open(url, '_blank', 'noopener');
    else useUi().toast('error', `Not opening ${service}`, `The lane shares it as ${r.body.url}; only http and https addresses are opened.`);
  }

  /** Joined any pool at all. */
  const joinedAny = computed(() => pools.value.some((p) => p.joined));

  return {
    overview,
    overviews,
    feeds,
    live,
    onFeed,
    pools,
    current,
    targets,
    placeable,
    defaultOn,
    setDefaultOn,
    shared,
    openShared,
    connectShared,
    disconnectShared,
    telemetry,
    setTelemetry,
    joinedAny,
    error,
    loading,
    share,
    isAdmin,
    pending,
    machines,
    nameOf,
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
    setProvision,
    confirmNode,
    giveBack,
    setProvisioned,
  };
});
