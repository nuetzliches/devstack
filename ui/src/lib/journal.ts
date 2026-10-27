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
 * each with what a journal row says of it. A route not listed shows as
 * `METHOD path`: a new route is visible before it is named here.
 */
const routes: { method: string; re: RegExp; label: (m: string[]) => string }[] = [
  { method: 'POST', re: /^\/v1\/up$/, label: () => 'up' },
  { method: 'POST', re: /^\/v1\/down$/, label: () => 'down' },
  { method: 'POST', re: /^\/v1\/shutdown$/, label: () => 'stop the daemon' },
  { method: 'POST', re: /^\/v1\/services\/([^/]+)\/([^/]+)$/, label: (m) => `${verb(m[2])} ${m[1]}` },
  { method: 'POST', re: /^\/v1\/groups\/([^/]+)\/([^/]+)$/, label: (m) => `${verb(m[2])} group ${m[1]}` },
  { method: 'POST', re: /^\/v1\/infra\/check$/, label: () => 'check the containers' },
  { method: 'POST', re: /^\/v1\/infra\/([^/]+)\/([^/]+)\/([^/]+)$/, label: (m) => `${m[3]} container ${m[1]}.${m[2]}` },
  { method: 'DELETE', re: /^\/v1\/store\/([^/]+)\/([^/]+)$/, label: (m) => `remove ${m[1]}'s store entry ${m[2].slice(0, 12)}` },
  { method: 'POST', re: /^\/v1\/commands\/([^/]+)$/, label: (m) => `run ${m[1]}` },
  { method: 'POST', re: /^\/v1\/runs\/(\d+)\/cancel$/, label: (m) => `cancel run #${m[1]}` },
  { method: 'POST', re: /^\/v1\/lanes$/, label: () => 'start a lane' },
  { method: 'POST', re: /^\/v1\/lanes\/prune$/, label: () => 'remove the stopped lanes' },
  { method: 'POST', re: /^\/v1\/lanes\/([^/]+)\/(stop|reset|move|share)$/, label: (m) => `${m[2]} lane ${m[1]}` },
  { method: 'POST', re: /^\/v1\/lanes\/([^/]+)\/main$/, label: (m) => (m[1] === 'main' ? 'main tree takes the main ports back' : `lane ${m[1]} takes the main ports`) },
  { method: 'AUTO', re: /^\/v1\/lanes\/([^/]+)\/main$/, label: (m) => (m[1] === 'main' ? 'main ports back to the main tree' : `main ports to lane ${m[1]}`) },
  { method: 'POST', re: /^\/v1\/main-slot$/, label: () => 'set the main slot' },
  { method: 'POST', re: /^\/v1\/telemetry\/take$/, label: () => 'take the telemetry lines' },
];

/** What a journal row says the change was. */
export function journalLabel(e: JournalEntry): string {
  const path = treePath(e.path);
  for (const r of routes) {
    if (r.method !== e.method) continue;
    const m = r.re.exec(path);
    if (m) return r.label(m.map(dec));
  }
  return `${e.method} ${e.path}`;
}

/** The service a journal entry acted on, if any. */
export function journalService(e: JournalEntry): string | null {
  const m = /^\/v1\/services\/([^/]+)\//.exec(treePath(e.path));
  return m ? dec(m[1]) : null;
}

/**
 * Whether a tree's view shows an entry of `GET /v1/journal`, as its stream
 * would (ADR-0067): the main tree (`lane` null) every entry, a lane here
 * those of its lane. A lane on a node, or one shared with this machine,
 * reads the journal of its own daemon there, which runs as a main tree: all of it.
 */
export function forTree(e: JournalEntry, lane: string | null, ownDaemon: boolean): boolean {
  return !lane || ownDaemon || e.lane === lane;
}

/** The same change, from the fetch and from the stream. */
export function sameEntry(a: JournalEntry, b: JournalEntry): boolean {
  return a.at === b.at && a.method === b.method && a.path === b.path && a.via === b.via && a.status === b.status;
}
