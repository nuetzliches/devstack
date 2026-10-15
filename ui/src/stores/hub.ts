import { defineStore } from 'pinia';
import { computed, ref } from 'vue';
import { api, paths, Unreachable, type StreamState, type Subscription } from '@/api';
import type { DockerEngine, HubEvent, HubSession, StackEntry } from '@/api/types';
import { act } from '@/lib/actions';
import { checkForAppUpdate } from '@/lib/app-update';
import { persist } from '@/lib/persist';

export type SessionStatus = 'loading' | 'ok' | 'unpaired' | 'unreachable';

/** The hub: session, the list of stacks, and whether we can reach it at all. */
export const useHub = defineStore('hub', () => {
  const cached = persist.hub();
  const status = ref<SessionStatus>('loading');
  const version = ref<string | null>(cached?.version ?? null);
  const stacks = ref<StackEntry[]>(cached?.stacks ?? []);
  const lastContact = ref<number | null>(cached?.at ?? null);
  const stream = ref<StreamState>('connecting');
  /** True once the hub stream delivered a list in this page session. */
  const fresh = ref(false);
  const starting = ref<Record<string, boolean>>({});
  /** The Docker engine; null until the hub said (older hubs never do). */
  const docker = ref<DockerEngine | null>(null);
  const dockerStarting = ref(false);
  /** A newer devstack release, if the hub found one. */
  const newer = ref<string | null>(null);
  let sub: Subscription | null = null;
  let hadBreak = false;
  let checking: Promise<void> | null = null;

  const reachable = computed(() => status.value === 'ok' && stream.value === 'open');

  function save() {
    persist.saveHub({ stacks: stacks.value, version: version.value, at: lastContact.value ?? Date.now() });
  }

  async function checkSession(): Promise<void> {
    if (checking) return checking;
    checking = (async () => {
      try {
        const res = await api().request<HubSession>('GET', paths.session);
        if (res.status === 401) status.value = 'unpaired';
        else if (res.ok && res.body && typeof res.body === 'object') {
          status.value = 'ok';
          version.value = res.body.version;
          lastContact.value = Date.now();
        } else status.value = 'unreachable';
      } catch (e) {
        status.value = e instanceof Unreachable ? 'unreachable' : 'unreachable';
      } finally {
        checking = null;
      }
    })();
    return checking;
  }

  async function init() {
    await checkSession();
    sub?.close();
    sub = api().subscribe<HubEvent>(paths.hubEvents, {
      onMessage(ev) {
        if (ev.type === 'release') {
          newer.value = ev.newer;
          return;
        }
        if (ev.type === 'docker') {
          docker.value = ev.docker;
          if (ev.docker.state === 'running') dockerStarting.value = false;
          return;
        }
        if (ev.type !== 'stacks') return;
        stacks.value = ev.stacks;
        lastContact.value = Date.now();
        fresh.value = true;
        if (status.value !== 'ok') void checkSession();
        for (const s of ev.stacks) if (s.running && starting.value[s.id]) delete starting.value[s.id];
        save();
      },
      onState(s) {
        // Back after a break (error, then connecting, then open): the hub may
        // have been updated meanwhile.
        if (s === 'error') hadBreak = true;
        if (s === 'open' && hadBreak) {
          hadBreak = false;
          checkForAppUpdate();
        }
        stream.value = s;
        // The stream cannot tell us why it failed: ask once (not on a timer).
        if (s === 'error') void checkSession();
      },
    });
  }

  async function startDocker() {
    dockerStarting.value = true;
    const res = await act<{ started: string | null }>({
      label: 'Start Docker',
      path: paths.dockerStart,
      success: (b) => (b?.started ? `Starting ${b.started}…` : 'Docker is running'),
    });
    if (!res.ok) dockerStarting.value = false;
  }

  function entry(id: string | null | undefined): StackEntry | undefined {
    return id ? stacks.value.find((s) => s.id === id) : undefined;
  }

  return { status, version, stacks, lastContact, stream, fresh, starting, docker, dockerStarting, newer, reachable, init, checkSession, startDocker, entry, save };
});
