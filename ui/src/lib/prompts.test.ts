import { describe, expect, it } from 'vitest';
import { upgradePrompt } from './prompts';

describe('upgradePrompt', () => {
  const o = { repo: 'C:\src\shop', file: 'C:\src\shop\devstack.toml', from: '0.17.0', current: '0.18.0' };

  it('names the versions, and the changelog and reference of the one installed', () => {
    const p = upgradePrompt(o);
    expect(p).toContain('C:\src\shop up to devstack 0.18.0');
    expect(p).toContain('last set up with devstack 0.17.0');
    expect(p).toContain('the sections after 0.17.0, up to 0.18.0, in https://github.com/nuetzliches/devstack/blob/v0.18.0/CHANGELOG.md');
    expect(p).toContain('https://github.com/nuetzliches/devstack/blob/v0.18.0/docs/stack-file.md');
    expect(p).toContain('`devstack skills install`');
  });

  it('does without a version it does not know', () => {
    const p = upgradePrompt({ ...o, from: null });
    expect(p).toContain('last set up with an earlier devstack');
    expect(p).toContain('the newest sections of');
  });
});
