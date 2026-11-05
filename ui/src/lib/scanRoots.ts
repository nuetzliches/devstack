// The folder to scan for new repositories, suggested from the stacks already
// registered: the folders those repositories sit in. Worked out from the
// dashboard's own list each time; nothing is kept.

export type ScanRoot = { path: string; count: number };

const SEP = /[\\/]/;

/** A Windows-style path: a drive letter, or backslashes. Its folders compare without regard to case. */
const isWindows = (p: string) => /^[A-Za-z]:/.test(p) || p.includes('\\');

/**
 * The folder an absolute path sits in, or null for a path that has none to
 * offer: a relative or empty one, a root itself, a share's root (`\\host\share`).
 * The separators of the input are kept.
 */
export function parentFolder(path: string): string | null {
  const p = path.trim();
  if (!/^[A-Za-z]:[\\/]/.test(p) && !/^[\\/]/.test(p)) return null;
  const bare = p.replace(/[\\/]+$/, '');
  if (!bare || /^[A-Za-z]:$/.test(bare)) return null;
  const at = Math.max(bare.lastIndexOf('\\'), bare.lastIndexOf('/'));
  const parent = bare.slice(0, at);
  if (!parent) return bare[at]; // `/shop`: the filesystem root
  if (/^[A-Za-z]:$/.test(parent)) return parent + bare[at]; // `C:\shop`: the drive's root
  if (/^[\\/]{2}[^\\/]+$/.test(parent)) return null; // `\\host\share`: above the share is no folder
  return SEP.test(parent) ? parent : null;
}

/**
 * The folders the given repositories sit in, the one holding the most first
 * (then by path), at most `max`. The spelling of the first repository of a
 * folder is the one shown.
 */
export function suggestRoots(repos: readonly string[], max = 3): ScanRoot[] {
  const roots = new Map<string, ScanRoot>();
  for (const repo of repos) {
    const parent = parentFolder(repo);
    if (parent === null) continue;
    const key = isWindows(parent) ? parent.replace(/\//g, '\\').toLowerCase() : parent;
    const known = roots.get(key);
    if (known) known.count++;
    else roots.set(key, { path: parent, count: 1 });
  }
  return [...roots.values()].sort((a, b) => b.count - a.count || a.path.localeCompare(b.path)).slice(0, max);
}
