import { describe, expect, it } from 'vitest';
import type { Opener } from '@/api/types';
import { arrange } from './openers';

const o = (id: string): Opener => ({ id, label: id, kind: 'editor' });
const ids = (l: Opener[]) => l.map((x) => x.id);

describe('the path menu programs', () => {
  it('keep the hub order until arranged', () => {
    expect(ids(arrange([o('files'), o('wt'), o('vscode')], { order: [], hidden: [] }))).toEqual(['files', 'wt', 'vscode']);
  });

  it('follow the chosen order, the ones found since after them', () => {
    const found = [o('files'), o('wt'), o('vscode'), o('orca')];
    expect(ids(arrange(found, { order: ['vscode', 'files', 'gone'], hidden: [] }))).toEqual(['vscode', 'files', 'wt', 'orca']);
  });
});
