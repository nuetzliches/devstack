import { describe, expect, it } from 'vitest';

// Text a user sees names no ADR: the decision log is for developers, so a
// reference stays in a code comment beside the string (ADR-0006, addendum
// 2026-11-05). This reads every source file, drops its comments and fails on
// an ADR number left in a template or a string.
const sources = import.meta.glob<string>(['../**/*.vue', '../**/*.ts', '!../**/*.test.ts'], {
  query: '?raw',
  import: 'default',
  eager: true,
});

function withoutComments(text: string): string {
  return text
    .replace(/<!--[\s\S]*?-->/g, '')
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .replace(/(^|\s)\/\/.*$/gm, '$1');
}

describe('user-visible text', () => {
  it('reads the sources', () => {
    expect(Object.keys(sources).length).toBeGreaterThan(50);
  });

  it('names no ADR outside comments', () => {
    const found = Object.entries(sources).flatMap(([path, text]) =>
      withoutComments(text)
        .split('\n')
        .filter((line) => /ADR-\d{4}/.test(line))
        .map((line) => `${path}: ${line.trim()}`),
    );
    expect(found).toEqual([]);
  });
});
