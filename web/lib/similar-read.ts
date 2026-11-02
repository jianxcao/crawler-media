import type { SimilarHit, SimilarRead } from "./api/libraries";

interface SimilarWaitOptions {
  signal?: AbortSignal;
  onPending?: () => void;
  pause?: (ms: number, signal?: AbortSignal) => Promise<void>;
}

function pause(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    signal?.throwIfAborted();
    const abort = () => {
      clearTimeout(timer);
      reject(signal?.reason ?? new DOMException("Aborted", "AbortError"));
    };
    const timer = setTimeout(() => {
      signal?.removeEventListener("abort", abort);
      resolve();
    }, ms);
    signal?.addEventListener("abort", abort, { once: true });
  });
}

/** Wait only for explicit pending; source failures remain errors for the caller. */
export async function waitForSimilar(
  read: (signal?: AbortSignal) => Promise<SimilarRead>,
  options: SimilarWaitOptions = {},
): Promise<SimilarHit[]> {
  for (;;) {
    options.signal?.throwIfAborted();
    const result = await read(options.signal);
    options.signal?.throwIfAborted();
    if (result.state === "ready") return result.items;
    options.onPending?.();
    await (options.pause ?? pause)(Math.min(5000, Math.max(100, result.retry_after_ms)), options.signal);
  }
}
