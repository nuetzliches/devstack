/**
 * `fn` at most once per `ms`: a call within that time after the last run
 * runs once when the time is up, however many came meanwhile. For reading
 * something again on events that come faster than it is worth reading
 * (a pool's nodes report every few seconds). `cancel` drops a run still due.
 */
export function throttle(fn: () => void, ms: number) {
  let last = 0;
  let due: ReturnType<typeof setTimeout> | null = null;
  const run = () => {
    due = null;
    last = Date.now();
    fn();
  };
  const call = () => {
    if (due) return;
    const wait = last + ms - Date.now();
    if (wait <= 0) run();
    else due = setTimeout(run, wait);
  };
  call.cancel = () => {
    if (due) clearTimeout(due);
    due = null;
  };
  return call;
}
