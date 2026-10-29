import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { throttle } from './throttle';

describe('throttle', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it('runs at once, then once more after the time for all calls meanwhile', () => {
    const fn = vi.fn();
    const t = throttle(fn, 1000);
    t();
    expect(fn).toHaveBeenCalledTimes(1);
    t();
    t();
    t();
    expect(fn).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(1000);
    expect(fn).toHaveBeenCalledTimes(2);
    vi.advanceTimersByTime(5000);
    expect(fn).toHaveBeenCalledTimes(2);
    t();
    expect(fn).toHaveBeenCalledTimes(3);
  });

  it('drops a run still due when cancelled', () => {
    const fn = vi.fn();
    const t = throttle(fn, 1000);
    t();
    t();
    t.cancel();
    vi.advanceTimersByTime(2000);
    expect(fn).toHaveBeenCalledTimes(1);
  });
});
