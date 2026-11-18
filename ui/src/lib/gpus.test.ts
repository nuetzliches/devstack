import { describe, expect, it } from 'vitest';
import { summarise } from './gpus';

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
});
