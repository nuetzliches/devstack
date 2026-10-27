// A command line as the user types it, and back: the program and its
// arguments of an MCP upstream that is a command (ADR-0069).

/** Words apart by spaces; double quotes keep a word with spaces together. */
export function words(line: string): string[] {
  const out: string[] = [];
  let cur = '';
  let quoted = false;
  let started = false;
  for (const ch of line) {
    if (ch === '"') {
      quoted = !quoted;
      started = true;
    } else if (!quoted && (ch === ' ' || ch === '\t')) {
      if (started) out.push(cur);
      cur = '';
      started = false;
    } else {
      cur += ch;
      started = true;
    }
  }
  if (started) out.push(cur);
  return out;
}

/** The words as one line again, quoted where [`words`] needs it. */
export function line(command: string[]): string {
  return command.map((w) => (w === '' || w.includes(' ') ? `"${w}"` : w)).join(' ');
}
