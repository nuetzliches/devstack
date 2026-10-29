/** The indices of `ats` (ascending times) in runs without a gap longer than `gap`: what a chart draws apart, not joined. */
export function runs(ats: number[], gap: number): number[][] {
  const out: number[][] = [];
  ats.forEach((at, i) => {
    if (i === 0 || at - ats[i - 1] > gap) out.push([]);
    out[out.length - 1].push(i);
  });
  return out;
}
