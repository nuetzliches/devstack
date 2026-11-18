// A machine's GPUs as one line of its card (ADR-0036, ADR-0112): how many
// are free for a lane, how many it offers the pool (free or held by a lane),
// and what each is, for the cards that show a machine's load.
import { bytes } from '@/lib/format';

/** A GPU as a node's or the hub's report carries it. */
export type Gpu = { name: string; memory: number; memory_used: number; load: number; offered: boolean; lane?: string };

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
          `${g.name}, ${bytes(g.memory)} (${bytes(g.memory_used)} used, ${g.load.toFixed(0)} %): ${g.lane ? `held by ${g.lane}` : g.offered ? 'free' : 'not offered'}`,
      )
      .join('\n'),
  };
}
