import { beforeEach, describe, expect, it } from 'vitest';
import { forgetFavourite, forgetOpened, noteOpened, openedTimes, orderStacks, savedOrder, saveOrder, useFavourites } from './stackOrder';

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
});
