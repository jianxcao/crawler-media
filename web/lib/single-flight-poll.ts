/**
 * Single-flight polling (review L5).
 *
 * The next round is scheduled only after the previous one settles, so a slow
 * request never overlaps the next tick. `stop()` aborts the in-flight round's
 * signal and cancels the pending timer; a round that settles after `stop()` is
 * ignored. A round resolving `false` ends polling; a rejected round retries on
 * the next interval.
 */
export interface PollClock {
  setTimeout(fn: () => void, ms: number): unknown;
  clearTimeout(id: unknown): void;
}

const browserClock: PollClock = {
  setTimeout: (fn, ms) => globalThis.setTimeout(fn, ms),
  clearTimeout: (id) => globalThis.clearTimeout(id as ReturnType<typeof setTimeout>),
};

export function startSingleFlightPoll(
  round: (signal: AbortSignal) => Promise<boolean>,
  intervalMs: number,
  clock: PollClock = browserClock,
): () => void {
  let stopped = false;
  let timer: unknown = null;
  let controller: AbortController | null = null;

  const schedule = () => {
    if (stopped) return;
    timer = clock.setTimeout(() => {
      timer = null;
      void run();
    }, intervalMs);
  };

  const run = async () => {
    if (stopped) return;
    const current = new AbortController();
    controller = current;
    let again = true;
    try {
      again = await round(current.signal);
    } catch {
      again = true;
    }
    if (controller === current) controller = null;
    if (!stopped && again) schedule();
  };

  schedule();
  return () => {
    stopped = true;
    if (timer != null) clock.clearTimeout(timer);
    timer = null;
    controller?.abort();
    controller = null;
  };
}
