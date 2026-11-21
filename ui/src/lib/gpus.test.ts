import { describe, expect, it } from 'vitest';
import { parseCap, sharedText, summarise } from './gpus';

describe('the GPU cap as typed', () => {
  it('is no cap when empty, a cap from 0 up, and invalid otherwise', () => {
    expect(parseCap('')).toEqual({ invalid: false });
    expect(parseCap(' 0 ')).toEqual({ cap: 0, invalid: false });
    expect(parseCap('2')).toEqual({ cap: 2, invalid: false });
    // Taken for "no cap" before (review before 0.52.0).
    for (const v of ['zwei', '1.5', '-1']) expect(parseCap(v)).toEqual({ invalid: true });
  });
});

const GIB = 2 ** 30;
const gpu = (offered: boolean, lane?: string) => ({ name: 'NVIDIA L4', memory: 24 * GIB, memory_used: 2 * GIB, load: 10, offered, lane });

describe("a machine's GPUs as one line", () => {
  it('is nothing without a GPU', () => {
    expect(summarise(undefined)).toBeNull();
    expect(summarise([])).toBeNull();
  });

  it('counts a GPU a lane holds as offered, not free', () => {
    const s = summarise([gpu(true), gpu(false, 'shop/train'), gpu(false)])!;
    expect(s).toMatchObject({ count: 3, free: 1, offered: 2, memory: 72 * GIB, used: 6 * GIB, offeredMemory: 48 * GIB });
    expect(s.title.split('\n')).toEqual([
      'NVIDIA L4, 24.0 GB (2.0 GB used, 10 %): free',
      'NVIDIA L4, 24.0 GB (2.0 GB used, 10 %): held by shop/train',
      'NVIDIA L4, 24.0 GB (2.0 GB used, 10 %): not offered',
    ]);
  });

  it('says a GPU the owner uses is in use on the machine', () => {
    expect(summarise([{ ...gpu(false), busy: true }])!.title).toBe('NVIDIA L4, 24.0 GB (2.0 GB used, 10 %): in use on the machine');
  });
});

describe("a shared machine's GPUs in its row", () => {
  it('counts what it offers, and says why it offers none', () => {
    expect(sharedText(undefined)).toBeNull();
    expect(sharedText([gpu(true)])).toBe('1 GPU free');
    expect(sharedText([gpu(false, 'shop/train')])).toBe('1 GPU held');
    expect(sharedText([gpu(true), gpu(false, 'shop/train'), gpu(false)])).toBe('2 GPUs (1 free)');
    expect(sharedText([gpu(false)])).toBe('GPU not offered');
    expect(sharedText([{ ...gpu(false), busy: true }])).toBe('GPU in use');
  });
});
