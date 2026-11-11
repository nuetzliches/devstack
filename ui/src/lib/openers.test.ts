import { describe, expect, it } from 'vitest';
import type { Opener } from '@/api/types';
import { arrange, favorites, normalize } from './openers';

const o = (id: string): Opener => ({ id, label: id, kind: 'editor' });
const ids = (l: Opener[]) => l.map((x) => x.id);
const none = { order: [], hidden: [], favorites: [] };

describe('the path menu programs', () => {
  it('keep the hub order until arranged', () => {
    expect(ids(arrange([o('files'), o('wt'), o('vscode')], none))).toEqual(['files', 'wt', 'vscode']);
  });

  it('follow the chosen order, the ones found since after them', () => {
    const found = [o('files'), o('wt'), o('vscode'), o('orca')];
    expect(ids(arrange(found, { ...none, order: ['vscode', 'files', 'gone'] }))).toEqual(['vscode', 'files', 'wt', 'orca']);
  });

  it('give their favourites in the menu order, without hidden or missing ones', () => {
    const found = [o('files'), o('wt'), o('vscode'), o('zed')];
    const p = { order: ['zed', 'wt'], hidden: ['vscode'], favorites: ['wt', 'vscode', 'gone', 'zed'] };
    expect(ids(favorites(found, p))).toEqual(['zed', 'wt']);
  });

  it('read a stored value from before favourites', () => {
    expect(normalize({ order: ['wt'], hidden: ['zed'] })).toEqual({ order: ['wt'], hidden: ['zed'], favorites: [] });
    expect(normalize(null)).toEqual(none);
  });
});
