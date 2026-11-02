import { beforeEach, describe, expect, it } from 'vitest';
import { forgetOpened, noteOpened, openedTimes, orderStacks, savedOrder, saveOrder } from './stackOrder';

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
});
