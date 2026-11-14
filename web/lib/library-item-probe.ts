interface ProbeResult { queued: number; already_running: boolean }
interface ChapterSession {
  version(): number;
  active(version: number): boolean;
  discover(): Promise<void>;
}
interface ProbeOptions {
  request(signal: AbortSignal): Promise<ProbeResult>;
  reload(signal: AbortSignal): Promise<unknown>;
  chapterSession(): ChapterSession | null;
  pageCurrent(): boolean;
  onBusy(busy: boolean): void;
  onResult(result: ProbeResult): void;
  onError(error: string | null): void;
}

/** Item-wide probing belongs to the page, independently of selected chapter files. */
export function createLibraryItemProbe(options: ProbeOptions) {
  let epoch = 0;
  let stopped = false;
  let controller: AbortController | null = null;
  return {
    run: async () => {
      if (stopped || !options.pageCurrent()) return;
      const token = ++epoch;
      controller?.abort();
      controller = new AbortController();
      const signal = controller.signal;
      const current = () => !stopped && token === epoch && !signal.aborted && options.pageCurrent();
      const session = options.chapterSession();
      const version = session?.version();
      options.onBusy(true);
      options.onError(null);
      try {
        const result = await options.request(signal);
        if (!current()) return;
        options.onResult(result);
        await options.reload(signal);
        if (!current()) return;
        if (session && session === options.chapterSession() && version !== undefined && session.active(version)) {
          await session.discover();
          if (!current()) return;
        }
      } catch (error) {
        if (current()) options.onError(error instanceof Error ? error.message : "重新探测媒体信息失败");
      } finally {
        if (current()) options.onBusy(false);
      }
    },
    stop: () => { stopped = true; epoch++; controller?.abort(); },
  };
}
