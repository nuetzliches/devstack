import { describe, expect, it } from 'vitest';
import { line, words } from './commandLine';

describe('a command line', () => {
  it('splits at spaces and keeps quoted words together', () => {
    expect(words('npx -y @example/mcp-server')).toEqual(['npx', '-y', '@example/mcp-server']);
    expect(words('  "C:/Program Files/srv/srv.exe"   --port 0 ')).toEqual(['C:/Program Files/srv/srv.exe', '--port', '0']);
    expect(words('run ""')).toEqual(['run', '']);
  });

  it('comes back as it went', () => {
    for (const cmd of [['npx', '-y', 'x'], ['C:/Program Files/a.exe', '--flag'], ['run', '']]) expect(words(line(cmd))).toEqual(cmd);
  });
});
