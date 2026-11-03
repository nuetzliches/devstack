import type { Client, JournalEntry } from '@/api/types';

/** How a client is named in a sentence. */
export const clientLabel: Record<Client, string> = {
  cli: 'CLI',
  mcp: 'MCP server (an agent)',
  dashboard: 'dashboard',
  tray: 'tray',
  auto: 'devstack',
  unknown: 'unknown client',
};

/** Through which client a change came, in a sentence: "via the dashboard", "by devstack". */
export function viaText(via: Client): string {
  if (via === 'auto') return 'by devstack';
  if (via === 'unknown') return 'by an unknown client';
  return `via the ${clientLabel[via] ?? via}`;
}

/** A journal entry's path in the API of the tree it was for: a lane's `/v1/lanes/<lane>/v1/...` as `/v1/...`. */
export function treePath(path: string): string {
  return path.replace(/^\/v1\/lanes\/[^/]+(?=\/v1\/)/, '');
}

const dec = (s: string) => {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
};
const verb = (action: string) => action.replace(/-/g, ' ');

/**
 * The mutating routes of the daemon's API (`crates/devstack-daemon/src/api.rs`),
 * each with what a journal row says of it, and, for a route that acts on
 * one thing (`journalSubject`), what it says beside a column naming that
 * thing (`action`). A route not listed shows as `METHOD path`: a new route
 * is visible before it is named here.
 */
const routes: { method: string; re: RegExp; label: (m: string[]) => string; action?: (m: string[]) => string }[] = [
  { method: 'POST', re: /^\/v1\/up$/, label: () => 'up' },
  { method: 'POST', re: /^\/v1\/down$/, label: () => 'down' },
  { method: 'POST', re: /^\/v1\/shutdown$/, label: () => 'stop the daemon' },
  { method: 'POST', re: /^\/v1\/services\/([^/]+)\/([^/]+)$/, label: (m) => `${verb(m[2])} ${m[1]}`, action: (m) => verb(m[2]) },
  { method: 'POST', re: /^\/v1\/groups\/([^/]+)\/([^/]+)$/, label: (m) => `${verb(m[2])} group ${m[1]}`, action: (m) => `${verb(m[2])} group` },
  { method: 'POST', re: /^\/v1\/infra\/check$/, label: () => 'check the containers' },
  { method: 'POST', re: /^\/v1\/infra\/([^/]+)\/([^/]+)\/([^/]+)$/, label: (m) => `${m[3]} container ${m[1]}.${m[2]}`, action: (m) => `${m[3]} container ${m[2]}` },
  { method: 'DELETE', re: /^\/v1\/store\/([^/]+)\/([^/]+)$/, label: (m) => `remove ${m[1]}'s store entry ${m[2].slice(0, 12)}`, action: (m) => `remove store entry ${m[2].slice(0, 12)}` },
  { method: 'POST', re: /^\/v1\/commands\/([^/]+)$/, label: (m) => `run ${m[1]}`, action: () => 'run' },
  { method: 'POST', re: /^\/v1\/runs\/(\d+)\/cancel$/, label: (m) => `cancel run #${m[1]}` },
  { method: 'POST', re: /^\/v1\/lanes$/, label: () => 'start a lane' },
  { method: 'POST', re: /^\/v1\/lanes\/prune$/, label: () => 'remove the stopped lanes' },
  { method: 'POST', re: /^\/v1\/lanes\/([^/]+)\/(stop|reset|restart|move|share)$/, label: (m) => `${m[2]} lane ${m[1]}` },
  { method: 'POST', re: /^\/v1\/lanes\/([^/]+)\/main$/, label: (m) => (m[1] === 'main' ? 'main tree takes the main ports back' : `lane ${m[1]} takes the main ports`) },
  { method: 'AUTO', re: /^\/v1\/lanes\/([^/]+)\/main$/, label: (m) => (m[1] === 'main' ? 'main ports back to the main tree' : `main ports to lane ${m[1]}`) },
  { method: 'POST', re: /^\/v1\/main-slot$/, label: () => 'set the main slot' },
  { method: 'POST', re: /^\/v1\/telemetry\/take$/, label: () => 'take the telemetry lines' },
];

/** What a journal row says the change was; `bySubject`: beside a column naming what it acted on. */
export function journalLabel(e: JournalEntry, bySubject = false): string {
  const path = treePath(e.path);
  for (const r of routes) {
    if (r.method !== e.method) continue;
    const m = r.re.exec(path);
    if (m) return ((bySubject && r.action) || r.label)(m.map(dec));
  }
  return `${e.method} ${e.path}`;
}

/**
 * What a journal entry acted on, if one thing: a service (its store entry
 * too), a Compose group (a container of it too) or a command. The same
 * names the events carry, so one filter takes both.
 */
export function journalSubject(e: JournalEntry): string | null {
  const m = /^\/v1\/(?:services|store|groups|infra|commands)\/([^/]+)\//.exec(treePath(e.path) + '/');
  return m && m[1] !== 'check' ? dec(m[1]) : null;
}

/**
 * The stack's own changes rather than a tree's: a lane started (by an
 * older daemon, which did not name it), the stopped lanes removed, the
 * daemon stopped, the main slot set. They have no lane, as the main tree's
 * own have none; the path tells them apart.
 */
const stackRoutes = [/^\/v1\/lanes$/, /^\/v1\/lanes\/prune$/, /^\/v1\/shutdown$/, /^\/v1\/main-slot$/, /^\/v1\/telemetry\/take$/];
export function stackWide(e: JournalEntry): boolean {
  return !e.lane && stackRoutes.some((re) => re.test(e.path));
}

/**
 * Whether a lane's Activity shows an entry (ADR-0067): a lane those of its
 * lane, the main tree (`lane` null) those without one or of the lane
 * `main`, not the stack's (the stack's page shows those). A lane on a node,
 * or one shared with this machine, reads the journal of its own daemon
 * there, which is all its own.
 */
export function forTree(e: JournalEntry, lane: string | null, ownDaemon: boolean): boolean {
  if (ownDaemon) return true;
  if (lane && lane !== 'main') return e.lane === lane;
  return (!e.lane || e.lane === 'main') && !stackWide(e);
}

/** The same change, from the fetch and from the stream. */
export function sameEntry(a: JournalEntry, b: JournalEntry): boolean {
  return a.at === b.at && a.method === b.method && a.path === b.path && a.via === b.via && a.status === b.status;
}
