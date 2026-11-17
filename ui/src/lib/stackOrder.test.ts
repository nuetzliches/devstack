import { beforeEach, describe, expect, it } from 'vitest';
import {
  filterStacks,
  forgetFavourite,
  forgetHidden,
  forgetOpened,
  matchesQuery,
  noteOpened,
  openedTimes,
  orderStacks,
  savedOrder,
  saveOrder,
  useFavourites,
  useHidden,
  useStackOrdering,
} from './stackOrder';

const s = (id: string, name: string) => ({ id, name });
const all = [s('c', 'web'), s('a', 'api'), s('b', 'worker'), s('d', 'db')];

beforeEach(() => localStorage.clear());

describe('the order of the stacks', () => {
  it('is by name unless chosen otherwise', () => {
    expect(savedOrder()).toBe('name');
    expect(orderStacks(all, 'name', { b: 9 }).map((x) => x.name)).toEqual(['api', 'db', 'web', 'worker']);
    saveOrder('opened');
    expect(savedOrder()).toBe('opened');
  });

  it('by last opened puts the most recent first, the never opened after it by name', () => {
    expect(orderStacks(all, 'opened', { b: 20, c: 30 }).map((x) => x.name)).toEqual(['web', 'worker', 'api', 'db']);
  });

  it('keeps when each stack was opened, and forgets a removed one', () => {
    noteOpened('a', 5);
    noteOpened('b', 7);
    expect(openedTimes()).toEqual({ a: 5, b: 7 });
    forgetOpened('a');
    expect(openedTimes()).toEqual({ b: 7 });
  });

  it('puts the favourites first, each part in the chosen order (ADR-0086)', () => {
    expect(orderStacks(all, 'name', {}, ['c', 'b']).map((x) => x.name)).toEqual(['web', 'worker', 'api', 'db']);
    expect(orderStacks(all, 'opened', { b: 20, c: 10, d: 5 }, ['c', 'b']).map((x) => x.name)).toEqual(['worker', 'web', 'db', 'api']);
  });

  it('keeps the favourites in this browser, and forgets a removed one', () => {
    const favs = useFavourites();
    favs.toggle('a');
    favs.toggle('b');
    expect(favs.list()).toEqual(['a', 'b']);
    expect(JSON.parse(localStorage.getItem('devstack.sidebar.favourites') ?? '[]')).toEqual(['a', 'b']);
    favs.toggle('a');
    expect(favs.has('a')).toBe(false);
    forgetFavourite('b');
    expect(favs.list()).toEqual([]);
    expect(JSON.parse(localStorage.getItem('devstack.sidebar.favourites') ?? 'null')).toEqual([]);
  });

  it('by last edited puts the newest commit first, a stack without one after it by name', () => {
    saveOrder('edited');
    expect(savedOrder()).toBe('edited');
    expect(orderStacks(all, 'edited', {}, [], { b: 20, c: 30 }).map((x) => x.name)).toEqual(['web', 'worker', 'api', 'db']);
    // Opening changes nothing about it; a favourite still lists first.
    expect(orderStacks(all, 'edited', { d: 99 }, ['d'], { b: 20, c: 30 }).map((x) => x.name)).toEqual(['db', 'web', 'worker', 'api']);
  });

  it('keeps the hidden stacks in this browser, and forgets a removed one', () => {
    const hidden = useHidden();
    hidden.hide('a');
    hidden.hide('b');
    hidden.hide('a');
    expect(hidden.list()).toEqual(['a', 'b']);
    expect(JSON.parse(localStorage.getItem('devstack.sidebar.hidden') ?? '[]')).toEqual(['a', 'b']);
    hidden.show('a');
    expect(hidden.has('a')).toBe(false);
    forgetHidden('b');
    expect(hidden.list()).toEqual([]);
    expect(JSON.parse(localStorage.getItem('devstack.sidebar.hidden') ?? 'null')).toEqual([]);
  });
});

describe('the order of two pages', () => {
  const list = [
    { id: 'a', name: 'alpha', last_commit: 100 },
    { id: 'b', name: 'beta', last_commit: 200 },
  ];

  it('is one choice: either page chooses, both follow, and the browser keeps it', () => {
    const sidebar = useStackOrdering(() => list);
    const start = useStackOrdering(() => list);
    expect(sidebar.sort(list).map((s) => s.id)).toEqual(['a', 'b']);
    start.choose('edited');
    expect(sidebar.order.value).toBe('edited');
    expect(sidebar.sort(list).map((s) => s.id)).toEqual(['b', 'a']);
    expect(start.sort(list).map((s) => s.id)).toEqual(['b', 'a']);
    expect(savedOrder()).toBe('edited');
    sidebar.choose('name');
    expect(start.sort(list).map((s) => s.id)).toEqual(['a', 'b']);
  });

  it('takes the snapshot of a commit time anew with every choice', async () => {
    const stacks = [{ id: 'a', name: 'alpha', last_commit: 100 }, { id: 'b', name: 'beta', last_commit: 200 }];
    const page = useStackOrdering(() => stacks);
    page.choose('edited');
    await Promise.resolve();
    expect(page.sort(stacks).map((s) => s.id)).toEqual(['b', 'a']);
    // A commit to `a` moves nothing until the order is chosen again.
    stacks[0].last_commit = 300;
    expect(page.sort(stacks).map((s) => s.id)).toEqual(['b', 'a']);
    page.choose('edited');
    await Promise.resolve();
    expect(page.sort(stacks).map((s) => s.id)).toEqual(['a', 'b']);
  });
});

describe('the filter of the stacks', () => {
  const f = (id: string, name: string, repo: string) => ({ id, name, repo });
  const list = [f('a', 'shop', '/src/shop'), f('b', 'Billing', '/work/ledger'), f('c', 'docs', '/src/docs')];
  const labels = (x: { id: string }) => (x.id === 'a' ? ['feat-cart', 'fix-tax'] : x.id === 'c' ? ['feat-search'] : []);

  it('keeps every stack for an empty query', () => {
    expect(filterStacks(list, '  ', labels).stacks).toHaveLength(3);
  });

  it('matches name or repository folder, whatever the case', () => {
    expect(filterStacks(list, 'BILL', labels).stacks.map((x) => x.id)).toEqual(['b']);
    expect(filterStacks(list, 'ledger', labels).stacks.map((x) => x.id)).toEqual(['b']);
  });

  it('matches a worktree, and says the stack matched only through it', () => {
    const r = filterStacks(list, 'feat', labels);
    expect(r.stacks.map((x) => x.id)).toEqual(['a', 'c']);
    expect([...r.byWorktree]).toEqual(['a', 'c']);
    // A stack that matches by itself is not "by worktree".
    expect([...filterStacks(list, 'docs', labels).byWorktree]).toEqual([]);
  });

  it('needs every word, in the name or in a worktree', () => {
    expect(filterStacks(list, 'shop cart', labels).stacks.map((x) => x.id)).toEqual(['a']);
    expect(filterStacks(list, 'shop search', labels).stacks).toEqual([]);
  });

  it('matches the stack itself by every word of name and folder, never by a worktree', () => {
    expect(matchesQuery(list[1], '')).toBe(true);
    expect(matchesQuery(list[1], 'BILLING ledger')).toBe(true);
    expect(matchesQuery(list[1], 'billing shop')).toBe(false);
    // `feat` is a worktree label of "shop" only: the stack itself does not match.
    expect(matchesQuery(list[0], 'feat')).toBe(false);
  });
});
