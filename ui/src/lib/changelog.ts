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

/** An ADR named in a link, `[ADR-0038](docs/adr/...)`: its text alone. */
const ADR_LINK = /\[(ADR-\d{4}[^\]]*)\]\([^)]*\)/g;
/** A bracket, links in it included: `([#128](https://...), ADR-0025)`. */
const BRACKET = /(\s?)\(((?:[^()]|\[[^\]]*\]\([^)]*\))*)\)/g;
/** A part of a bracket about an ADR: `ADR-0051`, `ADR-0024 point 7 amended`, `ADR-0046's list itself is unchanged`, `amending ADR-0068`. */
const ADR_PART = /\bADR-\d{4}\b/;
/** A sentence about an ADR: `ADR-0017 is amended for ADR-0041's profiles.` */
const ADR_SENTENCE = /(?<=^|[.!?]\s+)ADR-\d{4}.*?\.(?=\s|$)\s*/g;

/**
 * A changelog line without its ADR references: the decision log is for the
 * people who build devstack, the dashboard shows no ADR (ADR-0006, addendum
 * 2026-11-05). A bracket's parts about an ADR go, `(ADR-0094, #453)` keeps
 * `(#453)`, and so do the sentences about one; an empty line is dropped by
 * the caller.
 */
export function withoutAdr(text: string): string {
  return text
    .replace(ADR_LINK, '$1')
    .replace(BRACKET, (whole, space: string, inner: string) => {
      const parts = inner.split(/\s*[,;]\s*/);
      const kept = parts.filter((p) => !ADR_PART.test(p.trim()));
      if (kept.length === parts.length) return whole;
      return kept.length ? `${space}(${kept.join(', ')})` : '';
    })
    .replace(ADR_SENTENCE, '')
    .trim();
}

/** Sections of a changelog, newest first as the file has them; `[Unreleased]` is left out. */
export function parseChangelog(text: string): ChangelogSection[] {
  const sections: ChangelogSection[] = [];
  let section: ChangelogSection | null = null;
  let group: ChangelogGroup | null = null;
  /** A summary paragraph being read, lines joined. */
  let paragraph: string[] = [];
  let item: string | null = null;

  const endParagraph = () => {
    const text = withoutAdr(paragraph.join(' '));
    if (section && text) section.summary.push(text);
    paragraph = [];
  };
  const endItem = () => {
    const text = item === null ? '' : withoutAdr(item);
    if (group && text) group.items.push(text);
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
