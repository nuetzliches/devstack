// A machine's GPUs as one line of its card (ADR-0036, ADR-0112): how many
// are free for a lane, how many it offers the pool (free or held by a lane),
// and what each is, for the cards that show a machine's load.
import { bytes } from '@/lib/format';

/** A GPU as a node's or the hub's report carries it. */
export type Gpu = { name: string; memory: number; memory_used: number; load: number; offered: boolean; lane?: string; busy?: boolean };

export type GpuSummary = {
  /** All the machine has. */
  count: number;
  /** Offered and held by no lane: a lane can get it now. */
  free: number;
  /** Offered to the pool: free, or held by a lane. */
  offered: number;
  /** Memory of all of them, and what is used of it (any process, not only lanes). */
  memory: number;
  used: number;
  /** Memory of the ones offered. */
  offeredMemory: number;
  /** One line per GPU, for a title. */
  title: string;
};

export function summarise(gpus: Gpu[] | undefined): GpuSummary | null {
  if (!gpus?.length) return null;
  const sum = (xs: Gpu[], f: (g: Gpu) => number) => xs.reduce((a, g) => a + f(g), 0);
  const offered = gpus.filter((g) => g.offered || g.lane);
  return {
    count: gpus.length,
    free: gpus.filter((g) => g.offered && !g.lane).length,
    offered: offered.length,
    memory: sum(gpus, (g) => g.memory),
    used: sum(gpus, (g) => g.memory_used),
    offeredMemory: sum(offered, (g) => g.memory),
    title: gpus
      .map(
        (g) =>
          `${g.name}, ${bytes(g.memory)} (${bytes(g.memory_used)} used, ${g.load.toFixed(0)} %): ${gpuState(g)}`,
      )
      .join('\n'),
  };
}

/** What a GPU is to the pool, in a word or three. */
export function gpuState(g: Gpu): string {
  if (g.lane) return `held by ${g.lane}`;
  if (g.offered) return 'free';
  return g.busy ? 'in use on the machine' : 'not offered';
}

/**
 * The GPU cap as typed in the share switch's form: empty is no cap, a whole
 * number from 0 up a cap (0: none for the pool); anything else is `invalid`,
 * which the form says and does not save (it was taken for "no cap" before).
 */
export function parseCap(v: string): { cap?: number; invalid: boolean } {
  if (!v.trim()) return { invalid: false };
  const n = Number(v.trim());
  return Number.isInteger(n) && n >= 0 ? { cap: n, invalid: false } : { invalid: true };
}

/** A shared machine's GPUs in its row: `1 GPU free`, `1 GPU held`, `2 GPUs (1 free)`; none offered: `GPU in use` or `GPU not offered`. */
export function sharedText(gpus: Gpu[] | undefined): string | null {
  const s = summarise(gpus);
  if (!s) return null;
  if (!s.offered) return gpus!.some((g) => g.busy) ? 'GPU in use' : 'GPU not offered';
  const n = `${s.offered} GPU${s.offered === 1 ? '' : 's'}`;
  if (s.offered === 1) return `${n} ${s.free ? 'free' : 'held'}`;
  return `${n} (${s.free} free)`;
}
