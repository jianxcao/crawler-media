import { startSingleFlightPoll, type PollClock } from "./single-flight-poll.ts";

export type FrameState = "queued" | "running" | "completed" | "partial" | "failed" | "cancelled" | "superseded";
export interface FrameTask {
  task_id: string;
  generation: string;
  attempt: number;
  state: FrameState;
  total: number;
  ready: number;
  failed: number;
  targets: { index: number; ready: boolean; error: string | null }[];
}
export type FrameIdentity = Pick<FrameTask, "task_id" | "generation">;
export interface FrameStatus { task: FrameTask | null; empty: boolean; superseded?: boolean }
export function applyFrameStatus(current: FrameTask, incoming: FrameTask): FrameTask {
  return current.task_id !== incoming.task_id || current.generation !== incoming.generation || incoming.attempt < current.attempt ? current : incoming;
}
export function shouldPollFrameTask(task: FrameTask | null): boolean {
  return task?.state === "queued" || task?.state === "running";
}

export interface ChapterFrameOperation {
  signal: AbortSignal;
  current(): boolean;
  discover(): Promise<void>;
}
interface SessionOptions<Chapter> {
  loadStatus(expected: FrameIdentity | undefined, signal: AbortSignal): Promise<FrameStatus>;
  loadChapters(signal: AbortSignal): Promise<Chapter[]>;
  onTask(task: FrameTask | null): void;
  onChapters(chapters: Chapter[]): void;
  onError(error: string | null): void;
  onBusy(busy: boolean): void;
  clock?: PollClock;
}

/** One selected file owns one session; every operation invalidates all old continuations. */
export function createChapterFrameSession<Chapter>(options: SessionOptions<Chapter>) {
  let epoch = 0;
  let stopped = false;
  let controller: AbortController | null = null;
  let stopPoll: (() => void) | undefined;
  let task: FrameTask | null = null;
  const invalidate = () => { epoch++; controller?.abort(); stopPoll?.(); stopPoll = undefined; };
  const begin = () => {
    invalidate();
    const token = epoch;
    controller = new AbortController();
    const signal = controller.signal;
    const current = () => !stopped && token === epoch && !signal.aborted;
    let reloadPending = false;
    const reload = async () => {
      if (!current()) return;
      reloadPending = true;
      try {
        const chapters = await options.loadChapters(signal);
        if (!current()) return;
        options.onChapters(chapters);
        options.onError(null);
        reloadPending = false;
      } catch {
        if (current()) options.onError("章节暂不可用");
      }
    };
    const accept = async (response: FrameStatus, explicit: boolean) => {
      if (!current()) return;
      if (explicit || response.superseded || !task) task = response.task;
      else if (response.task) task = applyFrameStatus(task, response.task);
      options.onTask(task);
      options.onError(null);
      if (response.superseded || !shouldPollFrameTask(task)) await reload();
    };
    const follow = () => {
      if (!current() || (!shouldPollFrameTask(task) && !reloadPending)) return;
      stopPoll = startSingleFlightPoll(async (roundSignal) => {
        if (!current() || roundSignal.aborted) return false;
        if (reloadPending && !shouldPollFrameTask(task)) {
          await reload();
          return current() && !roundSignal.aborted && (reloadPending || shouldPollFrameTask(task));
        }
        if (!task) return false;
        const expected = { task_id: task.task_id, generation: task.generation };
        try {
          const response = await options.loadStatus(expected, signal);
          if (!current() || roundSignal.aborted) return false;
          await accept(response, false);
          if (!current() || roundSignal.aborted) return false;
          if (reloadPending && shouldPollFrameTask(task) && !response.superseded) {
            await reload();
            if (!current() || roundSignal.aborted) return false;
          }
        } catch {
          if (!current() || roundSignal.aborted) return false;
          options.onError("状态暂不可用");
        }
        return shouldPollFrameTask(task) || reloadPending;
      }, 1000, options.clock);
    };
    const discover = async () => {
      await reload();
      if (!current()) return;
      try {
        const response = await options.loadStatus(undefined, signal);
        if (!current()) return;
        await accept(response, true);
        if (current()) follow();
      } catch {
        if (!current()) return;
        options.onError("状态暂不可用");
        // Discovery also recovers without requiring a write or inventing a task.
        stopPoll = startSingleFlightPoll(async () => {
          if (!current()) return false;
          try {
            const response = await options.loadStatus(undefined, signal);
            if (!current()) return false;
            await accept(response, true);
            if (!current()) return false;
            follow();
            return false;
          } catch { return current(); }
        }, 1000, options.clock);
      }
    };
    return { signal, current, discover, ...{ accept, follow } };
  };
  const run = async (work: (operation: ChapterFrameOperation) => Promise<void>) => {
    if (stopped) return;
    const operation = begin();
    options.onBusy(true);
    options.onError(null);
    try { await work(operation); }
    catch (error) {
      if (operation.current()) {
        options.onError(error instanceof Error ? error.message : "章节暂不可用");
        operation.follow();
      }
    }
    finally { if (operation.current()) options.onBusy(false); }
  };
  return {
    run,
    discover: () => run((operation) => operation.discover()),
    generate: (load: (signal: AbortSignal) => Promise<FrameStatus>) => run(async (operation) => {
      const response = await load(operation.signal);
      if (!operation.current()) return;
      const internal = operation as ReturnType<typeof begin>;
      await internal.accept(response, true);
      if (operation.current()) internal.follow();
    }),
    version: () => epoch,
    active: (version: number) => !stopped && epoch === version,
    stop: () => { stopped = true; invalidate(); },
  };
}
