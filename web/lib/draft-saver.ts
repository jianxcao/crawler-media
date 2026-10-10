/** Debounced drafts share one ordered save queue, including explicit resets. */
export function createDraftSaver<T>(options: {
  save: (value: T) => Promise<unknown>;
  delayMs: number;
  schedule?: (callback: () => void, delayMs: number) => ReturnType<typeof setTimeout>;
  cancel?: (timer: ReturnType<typeof setTimeout>) => void;
  onError?: (error: unknown) => void;
}) {
  const schedule = options.schedule ?? setTimeout;
  const cancel = options.cancel ?? clearTimeout;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let pending: { value: T } | null = null;
  let queue = Promise.resolve();

  function cancelPending() {
    if (timer !== null) cancel(timer);
    timer = null;
    pending = null;
  }

  function enqueue(value: T): Promise<void> {
    const saved = queue.then(async () => { await options.save(value); });
    // A failed request must not prevent the next edit or reset from being saved.
    queue = saved.catch(() => undefined);
    return saved;
  }

  function flush(): Promise<void> {
    const draft = pending;
    cancelPending();
    return draft ? enqueue(draft.value) : queue;
  }

  return {
    schedule(value: T) {
      cancelPending();
      pending = { value };
      timer = schedule(() => {
        void flush().catch((error: unknown) => options.onError?.(error));
      }, options.delayMs);
    },
    flush,
    reset(value: T) {
      cancelPending();
      return enqueue(value);
    },
  };
}
