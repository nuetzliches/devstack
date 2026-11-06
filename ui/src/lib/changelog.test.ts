import { describe, expect, it } from 'vitest';
import { inline, laterVersion, newestUpTo, parseChangelog, sectionsBetween, withoutAdr } from './changelog';
import { changelog } from './changelogText';

const FIXTURE = [
  '# Changelog',
  '',
  'Intro text.',
  '',
  '## [Unreleased]',
  '',
  '### Added',
  '',
  '- Not released yet.',
  '',
  '## [0.40.0] - 2026-11-05',
  '',
  'First paragraph',
  'on two lines.',
  '',
  'Second paragraph.',
  '',
  '### Added',
  '',
  '- **A bullet** with `code`',
  '  on a second line.',
  '- Another bullet.',
  '',
  '### Fixed',
  '',
  '- A fix.',
  '',
  '## [0.39.0] - 2026-11-04',
  '',
  'Only a summary.',
  '',
  '## [0.38.1-rc.1] - 2026-11-03',
  '',
  'A pre-release.',
  '',
  '[Unreleased]: https://example.invalid/compare/v0.40.0...HEAD',
  '[0.40.0]: https://example.invalid/compare/v0.39.0...v0.40.0',
  '',
].join('\r\n');

describe('parseChangelog', () => {
  const sections = parseChangelog(FIXTURE);

  it('lists the releases, not [Unreleased], newest first as the file has them', () => {
    expect(sections.map((s) => s.version)).toEqual(['0.40.0', '0.39.0', '0.38.1-rc.1']);
    expect(sections[0]!.date).toBe('2026-11-05');
  });

  it('reads the summary paragraphs and joins their lines', () => {
    expect(sections[0]!.summary).toEqual(['First paragraph on two lines.', 'Second paragraph.']);
    expect(sections[1]!.summary).toEqual(['Only a summary.']);
    expect(sections[1]!.groups).toEqual([]);
  });

  it('reads the groups and their bullets, a bullet over several lines', () => {
    expect(sections[0]!.groups).toEqual([
      { heading: 'Added', items: ['**A bullet** with `code` on a second line.', 'Another bullet.'] },
      { heading: 'Fixed', items: ['A fix.'] },
    ]);
  });

  it('does not take the link references at the end for a bullet or a summary', () => {
    expect(sections[2]!.summary).toEqual(['A pre-release.']);
  });
});

describe('versions', () => {
  it('compares numerically and ignores a pre-release suffix', () => {
    expect(laterVersion('0.10.0', '0.9.9')).toBe(true);
    expect(laterVersion('0.40.0-rc.1', '0.39.0')).toBe(true);
    expect(laterVersion('0.40.0', '0.40.0')).toBe(false);
    expect(laterVersion('0.39.0', '0.40.0')).toBe(false);
    expect(laterVersion('garbage', '0.1.0')).toBe(false);
  });

  const sections = parseChangelog(FIXTURE);
  it('takes the sections after the version seen, up to the installed one', () => {
    expect(sectionsBetween(sections, '0.38.0', '0.40.0').map((s) => s.version)).toEqual(['0.40.0', '0.39.0', '0.38.1-rc.1']);
    expect(sectionsBetween(sections, '0.39.0', '0.40.0').map((s) => s.version)).toEqual(['0.40.0']);
    expect(sectionsBetween(sections, '0.39.0', '0.39.5')).toEqual([]);
    expect(sectionsBetween(sections, '0.40.0', '0.40.0')).toEqual([]);
  });

  it('takes the newest sections up to the installed version', () => {
    expect(newestUpTo(sections, '0.40.0', 2).map((s) => s.version)).toEqual(['0.40.0', '0.39.0']);
    expect(newestUpTo(sections, '0.39.0', 5).map((s) => s.version)).toEqual(['0.39.0', '0.38.1-rc.1']);
  });
});

describe('inline', () => {
  it('keeps bold and code, shows a link by its text', () => {
    expect(inline('a **b `c`** d [e](../x.md) `f`')).toEqual([
      { text: 'a ', code: false, strong: false },
      { text: 'b ', code: false, strong: true },
      { text: 'c', code: true, strong: true },
      { text: ' d ', code: false, strong: false },
      { text: 'e', code: false, strong: false },
      { text: ' ', code: false, strong: false },
      { text: 'f', code: true, strong: false },
    ]);
  });

  it('returns plain text as one piece', () => {
    expect(inline('nothing special')).toEqual([{ text: 'nothing special', code: false, strong: false }]);
  });
});

describe('withoutAdr (ADR-0006 addendum)', () => {
  it('drops a bracket of ADR references, and the ADR parts of a mixed one', () => {
    expect(withoutAdr('**Hooks** (ADR-0094). Text.')).toBe('**Hooks**. Text.');
    expect(withoutAdr('a lane (ADR-0024 point 7 amended), b (ADR-0062, ADR-0063 addendum 2026-10-27)')).toBe('a lane, b');
    expect(withoutAdr('**Skills** (ADR-0094, #453): more')).toBe('**Skills** (#453): more');
    expect(withoutAdr('x ([#128](https://example.invalid/128), ADR-0025).')).toBe('x ([#128](https://example.invalid/128)).');
    expect(withoutAdr('y ([ADR-0038](docs/adr/0038-x.md)) and z (ADR-0031 point 8; [pool.md](docs/pool.md#v)).')).toBe('y and z ([pool.md](docs/pool.md#v)).');
  });

  it('drops a sentence about an ADR, and leaves other brackets alone', () => {
    expect(withoutAdr("Not code-signed. ADR-0017 is amended for ADR-0041's profiles. `doctor` says so.")).toBe('Not code-signed. `doctor` says so.');
    expect(withoutAdr('ADR-0023 holds; `AGENTS.md` says so. ADR-0091 names ADR-0092.')).toBe('');
    expect(withoutAdr('kept (`a`, `b`, ...) as is')).toBe('kept (`a`, `b`, ...) as is');
    expect(withoutAdr('**A stop** (ADR-0095, amending ADR-0068): no longer offered.')).toBe('**A stop**: no longer offered.');
  });

  it('leaves out a bullet that was only about ADRs', () => {
    const text = ['## [1.0.0] - 2026-11-05', '', 'Summary (ADR-0001).', '', '### Changed', '', '- ADR-0023 holds.', '- A change (ADR-0002).', ''];
    const [s] = parseChangelog(text.join('\n'));
    expect(s!.summary).toEqual(['Summary.']);
    expect(s!.groups[0]!.items).toEqual(['A change.']);
  });
});

describe('the repository CHANGELOG.md', () => {
  it('shows no ADR in the dashboard', () => {
    const lines = changelog.flatMap((s) => [...s.summary, ...s.groups.flatMap((g) => g.items)]);
    expect(lines.filter((l) => /ADR-\d{4}/.test(l))).toEqual([]);
  });

  it('has releases that follow the format docs/releasing.md asks for', () => {
    expect(changelog.length).toBeGreaterThan(5);
    for (const s of changelog) {
      expect(s.date, `${s.version} has a date`).toMatch(/^\d{4}-\d{2}-\d{2}$/);
      // 0.10.1 to 0.14.0 were written before the summary paragraph was asked for.
      if (laterVersion(s.version, '0.14.1')) expect(s.summary.length, `${s.version} has a summary`).toBeGreaterThan(0);
    }
  });
});
