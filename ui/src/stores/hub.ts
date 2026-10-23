import { defineStore } from 'pinia';
import { computed, ref } from 'vue';
import { api, paths, Unreachable, type StreamState, type Subscription } from '@/api';
import type { DockerEngine, Guest, HubEvent, HubSession, Opener, StackEntry, UpdateState } from '@/api/types';
import { act } from '@/lib/actions';
import { checkForAppUpdate } from '@/lib/app-update';
import { persist } from '@/lib/persist';
import { useUi } from '@/stores/ui';

export type SessionStatus = 'loading' | 'ok' | 'unpaired' | 'unreachable';

/** localStorage: when the last update outcome this browser showed ended. */
const UPDATE_SEEN = 'updateSeen';
/** An update outcome older than this is not news to a page that did not start it. */
const UPDATE_FRESH_MS = 10 * 60 * 1000;

/** The hub: session, the list of stacks, and whether we can reach it at all. */
export const useHub = defineStore('hub', () => {
  const cached = persist.hub();
  const status = ref<SessionStatus>('loading');
  const version = ref<string | null>(cached?.version ?? null);
  /** Why the "Start hub" link cannot work on the hub's machine (macOS: not yet); kept for the time the hub is down. */
  const linkUnavailable = ref<string | null>(cached?.linkUnavailable ?? null);
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
  /** Why the hub's last check for a newer release failed (no network, GitHub's rate limit); null when it did not. */
  const releaseError = ref<string | null>(null);
  /** `devstack update` started from the dashboard runs. */
  const updating = ref(false);
  /** The last "Check for updates" of this page: when, and what it found or why not. */
  const lastCheck = ref<{ at: number; newer: string | null; error: string | null } | null>(null);
  const checkingUpdate = ref(false);
  /** Lanes of other members running (or about to) on this machine. */
  const guests = ref<Guest[]>([]);
  /** What this machine opens a stack's folder with (ADR-0046); null until asked. */
  const openers = ref<Opener[] | null>(null);
  let sub: Subscription | null = null;
  let hadBreak = false;
  let checking: Promise<void> | null = null;

  const reachable = computed(() => status.value === 'ok' && stream.value === 'open');

  /**
   * The outcome of an update, once: from the hub that ran it (it failed or
   * there was nothing to install), or from the hub it started (it installed
   * a version). Every tab and every reload sees the same outcome again; only
   * one that ended in the last few minutes, and that this browser has not
   * shown yet, makes a toast.
   */
  function onUpdate(u: UpdateState) {
    const was = updating.value;
    updating.value = u.running;
    if (u.running) return;
    const at = u.finished ?? null;
    if (at === null) {
      // An older hub: no time, so only an update this page saw start.
      if (was && u.error) useUi().toast('error', 'The update failed', u.error, 15000);
      return;
    }
    if (persist.get<number>(UPDATE_SEEN) === at) return;
    if (!was && Date.now() - at > UPDATE_FRESH_MS) return;
    persist.set(UPDATE_SEEN, at);
    if (u.error) useUi().toast('error', 'The update failed', u.error, 15000);
    else if (u.installed) useUi().toast('success', `devstack ${u.installed} is installed`, 'Running stacks switch at their next restart.');
  }

  function save() {
    persist.saveHub({
      stacks: stacks.value,
      version: version.value,
      at: lastContact.value ?? Date.now(),
      linkUnavailable: linkUnavailable.value,
    });
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
          linkUnavailable.value = res.body.link_unavailable ?? null;
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
          releaseError.value = ev.error ?? null;
          return;
        }
        if (ev.type === 'update') {
          onUpdate(ev.update);
          return;
        }
        if (ev.type === 'guests') {
          guests.value = ev.guests;
          return;
        }
        if (ev.type === 'notice') {
          useUi().toast(ev.level === 'error' ? 'error' : 'warning', ev.text);
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
        // Running, or its start failed (the hub says why): no longer "Starting…".
        for (const s of ev.stacks) if ((s.running || s.start_error) && starting.value[s.id]) delete starting.value[s.id];
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

  /**
   * Install the newer release (`devstack update` on the hub's machine). On
   * success the hub restarts on the new binary and this page reconnects and
   * reloads with the new dashboard; running stacks switch at their next
   * restart (their banner offers it).
   */
  async function installUpdate() {
    if (!newer.value || updating.value) return;
    const res = await act({
      label: `Update to devstack ${newer.value}`,
      path: paths.hubUpdate,
      danger: false,
      confirmLabel: 'Update',
      confirmText: `Installs devstack ${newer.value} and restarts the hub; the dashboard reconnects on its own. Running stacks keep running and switch at their next restart.`,
      success: `Installing devstack ${newer.value}…`,
    });
    if (res.ok) updating.value = true;
  }

  /** Ask the hub to look for a newer release now (`POST /hub/update/check`). */
  async function checkUpdate() {
    if (checkingUpdate.value) return;
    checkingUpdate.value = true;
    const at = () => Date.now();
    try {
      const res = await api().request<{ newer?: string | null; error?: string }>('POST', paths.hubUpdateCheck);
      const body = typeof res.body === 'object' && res.body ? res.body : {};
      if (res.ok) {
        newer.value = body.newer ?? null;
        lastCheck.value = { at: at(), newer: newer.value, error: null };
      } else {
        const why = res.status === 404 ? 'This hub checks on its own only (it is older than this dashboard).' : (body.error ?? `The hub answered ${res.status}.`);
        lastCheck.value = { at: at(), newer: null, error: why };
      }
    } catch {
      lastCheck.value = { at: at(), newer: null, error: 'The hub is not reachable.' };
    } finally {
      checkingUpdate.value = false;
    }
  }

  /** Ask the hub once what it can open folders with; an older hub knows nothing. */
  async function loadOpeners() {
    if (openers.value) return;
    try {
      const res = await api().request<Opener[]>('GET', paths.openers);
      openers.value = res.ok && Array.isArray(res.body) ? res.body : [];
    } catch {
      openers.value = null;
    }
  }

  /** Open one of a stack's trees with a program the hub found. */
  async function openWith(stack: string, path: string, o: Opener) {
    await act({ label: `Open in ${o.label}`, path: paths.open, body: { stack, path, with: o.id }, success: false });
  }

  return {
    status,
    version,
    linkUnavailable,
    stacks,
    lastContact,
    stream,
    fresh,
    starting,
    docker,
    dockerStarting,
    newer,
    releaseError,
    guests,
    updating,
    lastCheck,
    checkingUpdate,
    openers,
    reachable,
    init,
    checkSession,
    startDocker,
    installUpdate,
    checkUpdate,
    loadOpeners,
    openWith,
    entry,
    save,
  };
});
