// The pool this machine belongs to (ADR-0031, ADR-0032), as the hub reads it
// from the coordinator with this machine's certificate. Read on a timer: the
// coordinator has no stream yet, and its answer is small.
import { defineStore } from 'pinia';
import { computed, ref } from 'vue';
import { api, paths, Unreachable } from '@/api';
import type { PoolOverview, PoolRole } from '@/api/types';
import { act } from '@/lib/actions';

export const usePool = defineStore('pool', () => {
  const overview = ref<PoolOverview | null>(null);
  /** Why the last read failed (the coordinator down, the hub unreachable). */
  const error = ref<string | null>(null);
  const loading = ref(false);
  let timer: ReturnType<typeof setInterval> | null = null;
  let users = 0;

  const isAdmin = computed(() => overview.value?.me?.role === 'admin');
  const pending = computed(() => (overview.value?.members ?? []).filter((m) => m.state === 'pending'));

  async function load() {
    loading.value = true;
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
    const r = await act({
      label: pendingRequest ? `Reject ${name}` : `Remove ${name}`,
      path: paths.poolRemove(id),
      confirmText: pendingRequest
        ? `Reject the request of ${name}? It can only ask again with a new key.`
        : `Remove ${name} from the pool? Its certificate is refused from now on; it can only join again with a new key.`,
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

  return { overview, error, loading, isAdmin, pending, load, watch, approve, remove, setRole };
});
