// CHANGELOG.md, read for the "What's new" view (stores/whatsNew.ts): the
// Keep a Changelog format `docs/releasing.md` asks for. Per release a heading
// `## [X.Y.Z] - YYYY-MM-DD`, a summary (paragraphs), then groups (`### Added`,
// `### Fixed`, ...) of bullets. Pure functions; the file itself comes in
// through `changelogText.ts`.

export type ChangelogGroup = { heading: string; items: string[] };
export type ChangelogSection = {
  version: string;
  date: string | null;
  /** The paragraphs before the first group: the release in a few sentences. */
  summary: string[];
  groups: ChangelogGroup[];
};

const RELEASE = /^##\s+\[(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)\](?:\s+-\s+(\S+))?\s*$/;
const GROUP = /^###\s+(.+?)\s*$/;
/** The link references at the end of the file: `[0.40.0]: https://...`. */
const REFERENCE = /^\[[^\]]+\]:\s/;

/** Sections of a changelog, newest first as the file has them; `[Unreleased]` is left out. */
export function parseChangelog(text: string): ChangelogSection[] {
  const sections: ChangelogSection[] = [];
  let section: ChangelogSection | null = null;
  let group: ChangelogGroup | null = null;
  /** A summary paragraph being read, lines joined. */
  let paragraph: string[] = [];
  let item: string | null = null;

  const endParagraph = () => {
    if (section && paragraph.length) section.summary.push(paragraph.join(' '));
    paragraph = [];
  };
  const endItem = () => {
    if (group && item !== null) group.items.push(item);
    item = null;
  };

  for (const raw of text.split(/\r?\n/)) {
    const release = RELEASE.exec(raw);
    if (release || raw.startsWith('## ')) {
      endParagraph();
      endItem();
      group = null;
      section = release ? { version: release[1]!, date: release[2] ?? null, summary: [], groups: [] } : null;
      if (section) sections.push(section);
      continue;
    }
    if (!section || REFERENCE.test(raw)) continue;
    const heading = GROUP.exec(raw);
    if (heading) {
      endParagraph();
      endItem();
      group = { heading: heading[1]!, items: [] };
      section.groups.push(group);
      continue;
    }
    if (!group) {
      if (raw.trim()) paragraph.push(raw.trim());
      else endParagraph();
      continue;
    }
    if (raw.startsWith('- ')) {
      endItem();
      item = raw.slice(2).trim();
    } else if (item !== null && raw.trim()) {
      // A bullet's next lines, indented.
      item += ` ${raw.trim()}`;
    }
  }
  endParagraph();
  endItem();
  return sections;
}

function core(v: string): [number, number, number] | null {
  const m = /^(\d+)\.(\d+)\.(\d+)/.exec(v);
  return m ? [Number(m[1]), Number(m[2]), Number(m[3])] : null;
}

/** `a` is a later version than `b` (`x.y.z`, a pre-release suffix is ignored); anything unparsable is not later. */
export function laterVersion(a: string, b: string): boolean {
  const x = core(a);
  const y = core(b);
  if (!x || !y) return false;
  for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return x[i]! > y[i]!;
  return false;
}

/** The sections after `from` up to and including `to`, in the order given (newest first). */
export function sectionsBetween(sections: ChangelogSection[], from: string, to: string): ChangelogSection[] {
  return sections.filter((s) => laterVersion(s.version, from) && !laterVersion(s.version, to));
}

/** The newest `n` sections up to and including `to`. */
export function newestUpTo(sections: ChangelogSection[], to: string, n: number): ChangelogSection[] {
  return sections.filter((s) => !laterVersion(s.version, to)).slice(0, n);
}

/** A piece of a changelog line: plain text, `code`, with `strong` for **bold**. */
export type Inline = { text: string; code: boolean; strong: boolean };

const INLINE = /\*\*(.+?)\*\*|`([^`]+)`|\[([^\]]+)\]\([^)]*\)/;

/**
 * A line of the changelog as pieces to render (no HTML is made from the file):
 * **bold** and `code` are kept, a link shows its text only (its target is a
 * path in the repository, no address in the dashboard).
 */
export function inline(text: string, strong = false): Inline[] {
  const out: Inline[] = [];
  let rest = text;
  for (;;) {
    const m = INLINE.exec(rest);
    if (!m) break;
    if (m.index > 0) out.push({ text: rest.slice(0, m.index), code: false, strong });
    if (m[1] !== undefined) out.push(...inline(m[1], true));
    else if (m[2] !== undefined) out.push({ text: m[2], code: true, strong });
    else out.push({ text: m[3]!, code: false, strong });
    rest = rest.slice(m.index + m[0].length);
  }
  if (rest) out.push({ text: rest, code: false, strong });
  return out;
}
