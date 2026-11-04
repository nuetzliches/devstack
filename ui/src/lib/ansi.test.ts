import { describe, expect, it } from 'vitest';
import { parseAnsi, shownLine, stripAnsi } from './ansi';

const E = '\u001b';

describe('stripAnsi', () => {
  it('leaves plain text alone', () => {
    expect(stripAnsi('plain')).toBe('plain');
  });

  it('drops colour, cursor and OSC sequences', () => {
    expect(stripAnsi(`${E}[31merror${E}[0m done`)).toBe('error done');
    expect(stripAnsi(`${E}[2K${E}[1Gline`)).toBe('line');
    expect(stripAnsi(`${E}]0;title\u0007text`)).toBe('text');
    expect(stripAnsi(`${E}]8;;http://x${E}\\link`)).toBe('link');
  });
});

describe('parseAnsi', () => {
  it('is null without escapes', () => {
    expect(parseAnsi('no colour')).toBeNull();
  });

  it('styles segments and resets', () => {
    expect(parseAnsi(`a${E}[1;31mb${E}[0mc`)).toEqual([
      { text: 'a', cls: '', style: '' },
      { text: 'b', cls: 'ansi-bold', style: 'color:var(--ansi-1)' },
      { text: 'c', cls: '', style: '' },
    ]);
  });

  it('knows bright, 256 and true colours', () => {
    expect(parseAnsi(`${E}[92mx`)?.[0].style).toBe('color:var(--ansi-10)');
    expect(parseAnsi(`${E}[38;5;196mx`)?.[0].style).toBe('color:rgb(255,0,0)');
    expect(parseAnsi(`${E}[38;5;232mx`)?.[0].style).toBe('color:rgb(8,8,8)');
    expect(parseAnsi(`${E}[48;2;1;2;3mx`)?.[0].style).toBe('background:rgb(1,2,3)');
  });

  it('ignores sequences that are no colour', () => {
    expect(parseAnsi(`${E}[2Kx`)).toEqual([{ text: 'x', cls: '', style: '' }]);
  });
});

describe('shownLine', () => {
  it('keeps what is left after a progress bar redrew the line', () => {
    const cargo = `${E}[1m    Building${E}[0m [==>   ] 310/381: tauri\r          \r${E}[1m   Compiling${E}[0m vcpkg v0.2.15`;
    expect(shownLine(cargo)).toBe(`${E}[1m   Compiling${E}[0m vcpkg v0.2.15`);
  });

  it('treats a trailing carriage return as no redraw', () => {
    expect(shownLine('line\r')).toBe('line');
    expect(shownLine('plain')).toBe('plain');
  });
});
