import { describe, expect, it } from 'vitest';
import { parentFolder, suggestRoots } from './scanRoots';

describe('the folder a repository sits in', () => {
  it('drops the last segment, keeping the separators', () => {
    expect(parentFolder('D:\\Sources\\shop')).toBe('D:\\Sources');
    expect(parentFolder('D:/Sources/shop')).toBe('D:/Sources');
    expect(parentFolder('/home/me/src/shop')).toBe('/home/me/src');
    expect(parentFolder('\\\\host\\share\\team\\shop')).toBe('\\\\host\\share\\team');
  });

  it('ignores a trailing separator and surrounding space', () => {
    expect(parentFolder(' D:\\Sources\\shop\\ ')).toBe('D:\\Sources');
    expect(parentFolder('/home/me/src/shop//')).toBe('/home/me/src');
  });

  it('knows a drive root and the filesystem root as folders', () => {
    expect(parentFolder('C:\\shop')).toBe('C:\\');
    expect(parentFolder('/shop')).toBe('/');
  });

  it('has none for a relative path, a root itself or the root of a share', () => {
    for (const p of ['', '  ', 'shop', '..\\shop', 'src/shop', 'C:', 'C:\\', '/', '\\\\host\\share']) {
      expect(parentFolder(p), p).toBeNull();
    }
  });
});

describe('the suggested folders to scan', () => {
  it('is the one folder the repositories sit in, with how many', () => {
    expect(suggestRoots(['D:\\Sources\\shop', 'D:\\Sources\\crm', 'D:\\Sources\\billing'])).toEqual([{ path: 'D:\\Sources', count: 3 }]);
  });

  it('puts the folder with the most repositories first, then by path', () => {
    const repos = ['D:\\Work\\a', 'D:\\Sources\\b', 'D:\\Sources\\c', 'D:\\Work\\d', 'D:\\Archive\\e'];
    expect(suggestRoots(repos).map((r) => [r.path, r.count])).toEqual([
      ['D:\\Sources', 2],
      ['D:\\Work', 2],
      ['D:\\Archive', 1],
    ]);
  });

  it('counts a Windows folder once whatever its case or separators', () => {
    const roots = suggestRoots(['D:\\Sources\\shop', 'd:\\sources\\crm', 'D:/Sources/billing']);
    expect(roots).toEqual([{ path: 'D:\\Sources', count: 3 }]);
  });

  it('tells POSIX folders apart by case', () => {
    expect(suggestRoots(['/src/a', '/Src/b']).map((r) => r.path).sort()).toEqual(['/Src', '/src']);
  });

  it('offers at most three, or as many as asked', () => {
    const repos = ['/a/1', '/b/1', '/c/1', '/d/1'];
    expect(suggestRoots(repos)).toHaveLength(3);
    expect(suggestRoots(repos, 1)).toHaveLength(1);
    expect(suggestRoots(repos, 10)).toHaveLength(4);
  });

  it('skips what has no folder to offer, and has nothing for no stacks', () => {
    expect(suggestRoots(['shop', '', 'C:\\'])).toEqual([]);
    expect(suggestRoots([])).toEqual([]);
  });
});
