// Terminal colour codes in service output (`ESC [ 33 m`): rendered as styled
// segments, every other escape sequence dropped. Search, filters and level
// detection work on the visible text only.

export type AnsiSeg = { text: string; cls: string; style: string };

const ESC = '\u001b';
// CSI (parameters, intermediates, final byte), OSC (up to BEL or ESC \), any other ESC x
// eslint-disable-next-line no-control-regex
const SEQ = /\u001b\[([0-?]*)[ -/]*([@-~])|\u001b\][^\u0007\u001b]*(?:\u0007|\u001b\\)|\u001b./g;

/** The visible text, without escape sequences. */
export function stripAnsi(text: string): string {
  return text.includes(ESC) ? text.replace(SEQ, '') : text;
}

type State = { fg: string | null; bg: string | null; bold: boolean; dim: boolean; italic: boolean; underline: boolean };

function xterm256(n: number): string {
  if (n < 16) return `var(--ansi-${n})`;
  if (n >= 232) {
    const v = 8 + (n - 232) * 10;
    return `rgb(${v},${v},${v})`;
  }
  const i = n - 16;
  const c = (x: number) => (x === 0 ? 0 : 55 + x * 40);
  return `rgb(${c(Math.floor(i / 36))},${c(Math.floor(i / 6) % 6)},${c(i % 6)})`;
}

function apply(s: State, params: string) {
  const p = params === '' ? [0] : params.split(';').map((x) => Number(x) || 0);
  for (let i = 0; i < p.length; i++) {
    const n = p[i];
    if (n === 0) Object.assign(s, { fg: null, bg: null, bold: false, dim: false, italic: false, underline: false });
    else if (n === 1) s.bold = true;
    else if (n === 2) s.dim = true;
    else if (n === 3) s.italic = true;
    else if (n === 4) s.underline = true;
    else if (n === 22) s.bold = s.dim = false;
    else if (n === 23) s.italic = false;
    else if (n === 24) s.underline = false;
    else if (n >= 30 && n <= 37) s.fg = `var(--ansi-${n - 30})`;
    else if (n >= 90 && n <= 97) s.fg = `var(--ansi-${n - 90 + 8})`;
    else if (n === 39) s.fg = null;
    else if (n >= 40 && n <= 47) s.bg = `var(--ansi-${n - 40})`;
    else if (n >= 100 && n <= 107) s.bg = `var(--ansi-${n - 100 + 8})`;
    else if (n === 49) s.bg = null;
    else if ((n === 38 || n === 48) && p[i + 1] === 5) {
      const c = xterm256(p[i + 2] ?? 0);
      if (n === 38) s.fg = c;
      else s.bg = c;
      i += 2;
    } else if ((n === 38 || n === 48) && p[i + 1] === 2) {
      const c = `rgb(${p[i + 2] ?? 0},${p[i + 3] ?? 0},${p[i + 4] ?? 0})`;
      if (n === 38) s.fg = c;
      else s.bg = c;
      i += 4;
    }
  }
}

/** Styled segments, or `null` when the text has no escape sequence at all. */
export function parseAnsi(text: string): AnsiSeg[] | null {
  if (!text.includes(ESC)) return null;
  const out: AnsiSeg[] = [];
  const s: State = { fg: null, bg: null, bold: false, dim: false, italic: false, underline: false };
  let last = 0;
  const push = (t: string) => {
    if (!t) return;
    const cls = [s.bold && 'ansi-bold', s.dim && 'ansi-dim', s.italic && 'ansi-italic', s.underline && 'ansi-underline'].filter(Boolean).join(' ');
    const style = [s.fg && `color:${s.fg}`, s.bg && `background:${s.bg}`].filter(Boolean).join(';');
    out.push({ text: t, cls, style });
  };
  SEQ.lastIndex = 0;
  for (let m = SEQ.exec(text); m; m = SEQ.exec(text)) {
    push(text.slice(last, m.index));
    last = m.index + m[0].length;
    if (m[2] === 'm') apply(s, m[1] ?? '');
  }
  push(text.slice(last));
  return out;
}
