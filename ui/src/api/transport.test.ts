import { describe, expect, it } from 'vitest';
import { paths } from './transport';

describe('paths.prompt', () => {
  it('names the prompt and its target, never text', () => {
    expect(paths.prompt('upgrade', { stack: 'a1' })).toBe('/hub/prompt?kind=upgrade&stack=a1');
    expect(paths.prompt('fix-file', { path: 'C:\\src\\my shop' })).toBe('/hub/prompt?kind=fix-file&path=C%3A%5Csrc%5Cmy%20shop');
  });
});
