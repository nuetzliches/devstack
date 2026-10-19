import type { Candidate, CandidateState, ContainerView, KnownCandidateState, KnownServiceKind, ServiceView, Status } from '@/api/types';
import type { IconName } from '@/components/icons/icons';

export type Tone = 'ok' | 'warn' | 'danger' | 'info' | 'accent' | 'neutral';

export type StatusLook = { label: string; tone: Tone; busy: boolean; detail: string | null };

export function statusLook(s: Status): StatusLook {
  switch (s.state) {
    case 'healthy':
      return { label: 'Healthy', tone: 'ok', busy: false, detail: null };
    case 'running':
      return { label: 'Running', tone: 'ok', busy: false, detail: null };
    case 'starting':
      return { label: 'Starting', tone: 'accent', busy: true, detail: null };
    case 'building':
      return { label: 'Building', tone: 'accent', busy: true, detail: null };
    case 'stopped':
      return { label: 'Stopped', tone: 'neutral', busy: false, detail: null };
    case 'unhealthy':
      return { label: 'Unhealthy', tone: 'warn', busy: false, detail: s.reason };
    case 'failed':
      return { label: 'Failed', tone: 'danger', busy: false, detail: s.reason };
    case 'completed':
      return { label: s.exit_code === 0 ? 'Completed' : `Completed (${s.exit_code})`, tone: s.exit_code === 0 ? 'neutral' : 'danger', busy: false, detail: null };
    case 'exited':
      return { label: `Exited (${s.exit_code})`, tone: s.exit_code === 0 ? 'neutral' : 'danger', busy: false, detail: null };
  }
  // A state from a newer daemon: shown by its name, never a crash.
  const state = String((s as { state?: unknown }).state ?? 'unknown');
  return { label: cap(state), tone: 'neutral', busy: false, detail: null };
}

/** Healthy/starting/failed buckets for the summary. */
export function statusBucket(s: Status): 'healthy' | 'starting' | 'failed' | 'idle' {
  switch (s.state) {
    case 'healthy':
    case 'running':
      return 'healthy';
    case 'starting':
    case 'building':
      return 'starting';
    case 'failed':
    case 'unhealthy':
      return 'failed';
    case 'exited':
      return s.exit_code === 0 ? 'idle' : 'failed';
    default:
      return 'idle';
  }
}

export type CandidateLook = { label: string; tone: Tone; busy: boolean; icon: IconName; explain: string };

const candidateLooks: Record<KnownCandidateState, CandidateLook> = {
  changed: { label: 'Changed', tone: 'info', busy: true, icon: 'zap', explain: 'A change was detected; the build starts after the debounce.' },
  building: { label: 'Building', tone: 'accent', busy: true, icon: 'wrench', explain: 'The new build is compiling. The running instance keeps serving.' },
  superseded: { label: 'Superseded', tone: 'neutral', busy: true, icon: 'history', explain: 'A newer change arrived; this build was cancelled and a new one follows.' },
  starting: { label: 'Starting', tone: 'accent', busy: true, icon: 'play', explain: 'The build succeeded; the candidate starts on a free port and is health-checked.' },
  switching: { label: 'Switching', tone: 'accent', busy: true, icon: 'restart', explain: 'The candidate is healthy; traffic is moving over to it.' },
  held: { label: 'Held', tone: 'warn', busy: false, icon: 'hourglass', explain: 'Built and health-checked, but it waits for an explicit promote.' },
  failed: { label: 'Failed', tone: 'danger', busy: false, icon: 'xCircle', explain: 'The build or health check failed. The running instance stays.' },
};

function knownCandidate(s: CandidateState): s is KnownCandidateState {
  return Object.hasOwn(candidateLooks, s);
}

/** A state from a newer daemon: shown by its name, never a crash. */
function unknownCandidateLook(s: string): CandidateLook {
  return { label: cap(s || 'unknown'), tone: 'neutral', busy: false, icon: 'history', explain: `The daemon reports "${s}", which this dashboard does not know yet.` };
}

export function candidateLook(c: Candidate): CandidateLook {
  return knownCandidate(c.state) ? candidateLooks[c.state] : unknownCandidateLook(c.state);
}

/** One line: "Not yet active: building" etc. */
export function candidateSummary(c: Candidate): string {
  switch (c.state) {
    case 'changed':
      return 'change detected';
    case 'building':
      return 'building';
    case 'superseded':
      return 'superseded by a newer change';
    case 'starting':
      return 'starting candidate';
    case 'switching':
      return 'switching over';
    case 'held':
      return 'held, waiting for promote';
    case 'failed':
      return 'build failed';
    default:
      return c.state || 'unknown';
  }
}

export function containerLook(c: ContainerView): StatusLook {
  const state = c.state.toLowerCase();
  const health = c.health.toLowerCase();
  if (state === 'running') {
    if (health === 'unhealthy') return { label: 'Unhealthy', tone: 'warn', busy: false, detail: null };
    if (health === 'starting') return { label: 'Starting', tone: 'accent', busy: true, detail: null };
    if (health === 'healthy') return { label: 'Healthy', tone: 'ok', busy: false, detail: null };
    return { label: 'Running', tone: 'ok', busy: false, detail: null };
  }
  if (state === 'restarting' || state === 'created') return { label: cap(state), tone: 'accent', busy: true, detail: null };
  // 0, 130, 137, 143: stopped (by devstack, Docker or a signal), not failed.
  if (state === 'exited' && [0, 130, 137, 143].includes(c.exit_code ?? -1)) return { label: 'Stopped', tone: 'neutral', busy: false, detail: null };
  if (state === 'exited' || state === 'dead') return { label: cap(state), tone: 'danger', busy: false, detail: c.exit_code ? `exit code ${c.exit_code}` : null };
  if (state === 'paused') return { label: 'Paused', tone: 'warn', busy: false, detail: null };
  return { label: cap(state || 'unknown'), tone: 'neutral', busy: false, detail: null };
}

/** Why the daemon refuses `start` and `restart` of a container, or null when it does not. */
export function containerRefuses(c: ContainerView | null): string | null {
  // Older daemons omit `managed`: every container they show is the group's.
  if (!c || c.managed !== false) return null;
  return `${c.name} belongs to the Compose project, but the group does not start it (not in its \`services\` in the stack file): devstack can stop it, not start or restart it.`;
}

export function containerBucket(c: ContainerView): 'healthy' | 'starting' | 'failed' | 'idle' {
  const t = containerLook(c).tone;
  return t === 'ok' ? 'healthy' : t === 'accent' ? 'starting' : t === 'danger' || t === 'warn' ? 'failed' : 'idle';
}

function cap(s: string): string {
  return s.charAt(0).toUpperCase() + s.slice(1);
}

const kindLabels: Record<KnownServiceKind, string> = { service: 'service', job: 'job', devserver: 'dev server', tool: 'tool', static: 'static site' };
const kindIcons: Record<KnownServiceKind, IconName> = { service: 'server', job: 'play', devserver: 'globe', tool: 'wrench', static: 'file' };

function knownKind(k: string): k is KnownServiceKind {
  return Object.hasOwn(kindLabels, k);
}

/** A kind from a newer daemon is shown by its name. */
export function kindLabel(s: ServiceView): string {
  return knownKind(s.kind) ? kindLabels[s.kind] : s.kind;
}

export function kindIcon(s: ServiceView): IconName {
  return knownKind(s.kind) ? kindIcons[s.kind] : 'server';
}

/** "compose.<group>.<service>" -> [group, service]. Group names do not contain dots. */
export function parseComposeMember(m: string): [string, string] | null {
  if (!m.startsWith('compose.')) return null;
  const rest = m.slice('compose.'.length);
  const dot = rest.indexOf('.');
  if (dot < 0) return null;
  return [rest.slice(0, dot), rest.slice(dot + 1)];
}
