import { describe, expect, it } from 'vitest';
import { inline, laterVersion, newestUpTo, parseChangelog, sectionsBetween } from './changelog';
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

describe('the repository CHANGELOG.md', () => {
  it('has releases that follow the format docs/releasing.md asks for', () => {
    expect(changelog.length).toBeGreaterThan(5);
    for (const s of changelog) {
      expect(s.date, `${s.version} has a date`).toMatch(/^\d{4}-\d{2}-\d{2}$/);
      // 0.10.1 to 0.14.0 were written before the summary paragraph was asked for.
      if (laterVersion(s.version, '0.14.1')) expect(s.summary.length, `${s.version} has a summary`).toBeGreaterThan(0);
    }
  });
});
