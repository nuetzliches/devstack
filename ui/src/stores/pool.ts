// The pool this machine belongs to (ADR-0031, ADR-0032), as the hub reads it
// from the coordinator with this machine's certificate. Read on a timer: the
// coordinator has no stream yet, and its answer is small.
import { defineStore } from 'pinia';
import { computed, ref } from 'vue';
import { api, paths, Unreachable } from '@/api';
import type { NodePolicy, PoolOverview, PoolRole, ShareSettings, ShareStatus } from '@/api/types';
import { act } from '@/lib/actions';

export const usePool = defineStore('pool', () => {
  const overview = ref<PoolOverview | null>(null);
  /** Why the last read failed (the coordinator down, the hub unreachable). */
  const error = ref<string | null>(null);
  const loading = ref(false);
  /** What this machine shares; `null` until read (or from an older hub). */
  const share = ref<ShareStatus | null>(null);
  let timer: ReturnType<typeof setInterval> | null = null;
  let users = 0;

  const isAdmin = computed(() => overview.value?.me?.role === 'admin');
  const pending = computed(() => (overview.value?.members ?? []).filter((m) => m.state === 'pending'));

  async function loadShare() {
    try {
      const res = await api().request<ShareStatus>('GET', paths.poolShare);
      if (res.ok && res.body && typeof res.body === 'object') share.value = res.body;
    } catch {
      /* keep what is known */
    }
  }

  async function load() {
    loading.value = true;
    void loadShare();
    try {
      const res = await api().request<PoolOverview>('GET', paths.pool);
      if (res.ok && res.body && typeof res.body === 'object') {
        overview.value = res.body;
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
      path: paths.poolApprove(id),
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
      path: paths.poolRemove(id),
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
      path: paths.poolRole(id),
      body: { role },
      success: `${name} is ${role === 'admin' ? 'an admin' : 'a member'} now`,
    });
    if (r.ok) await load();
  }

  async function setPaused(id: string, name: string, paused: boolean) {
    const r = await act({
      label: paused ? `Pause ${name}` : `Resume ${name}`,
      path: paths.poolPause(id, paused),
      success: paused ? `${name} takes no new lanes; running ones stay` : `${name} takes lanes again`,
    });
    if (r.ok) await load();
  }

  async function endLease(id: string, what: string) {
    const r = await act({
      label: `End ${what}`,
      path: paths.poolEnd(id),
      confirmText: `End ${what}? Its node stops the lane within about 15 s; its owner sees it ended and can start it again.`,
      confirmLabel: 'End it',
      success: `${what} ends`,
    });
    if (r.ok) setTimeout(() => void load(), 1500);
  }

  async function setTags(id: string, name: string, tags: string[]) {
    const r = await act({
      label: `Tags of ${name}`,
      path: paths.poolTags(id),
      body: { tags },
      success: tags.length ? `${name}: ${tags.join(', ')}` : `${name} has no tags`,
    });
    if (r.ok) await load();
  }

  /** `null` lifts the node's policy: every member may run any lane there. */
  async function setPolicy(id: string, name: string, policy: NodePolicy | null) {
    const r = await act({
      label: `What runs on ${name}`,
      path: paths.poolPolicy(id),
      body: policy as unknown as Record<string, unknown>,
      success: policy ? `${name}: policy saved` : `${name}: every member may run any lane there`,
    });
    if (r.ok) await load();
  }

  async function join(coordinator: string, fingerprint: string, name: string) {
    const r = await act({
      label: 'Join the pool',
      path: paths.poolJoin,
      body: { coordinator, fingerprint, name },
      success: 'Asked to join: an admin of the pool approves it',
    });
    if (r.ok) await load();
    return r.ok;
  }

  /** Change the switch or the caps; the hub reports at once. */
  async function setShare(next: ShareSettings) {
    const r = await act<ShareStatus>({
      label: next.enabled ? 'Share this machine' : 'Stop sharing this machine',
      method: 'PUT',
      path: paths.poolShare,
      body: next as unknown as Record<string, unknown>,
      success: false,
    });
    if (r.ok && r.body) share.value = r.body;
    setTimeout(() => void load(), 1500);
  }

  return { overview, error, loading, share, isAdmin, pending, load, loadShare, watch, approve, remove, setRole, setShare, setPaused, endLease, join, setTags, setPolicy };
});
