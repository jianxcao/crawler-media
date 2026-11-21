import type { PlayerPhase } from "./machine.ts";

export interface PlaybackPageSnapshot {
  unitKey: string;
  playSessionId: string | null;
  renewedFromPlaySessionId?: string | null;
  phase: PlayerPhase;
  positionMs: number;
  wantsPlay: boolean;
  announced: boolean;
  endedByAdmin: boolean;
}

export interface PlaybackPageHandlers {
  snapshot(): PlaybackPageSnapshot;
  suspend(snapshot: PlaybackPageSnapshot): void;
  restore(snapshot: PlaybackPageSnapshot): void;
  stop(snapshot: PlaybackPageSnapshot): void;
}

const RESUMABLE = new Set<PlayerPhase>([
  "playing", "buffering", "ready", "seeking", "deciding", "degrading", "session-starting",
]);

/** Pair one persisted hide with one show, while the same page still owns the grant. */
export function bindPlaybackPageLifecycle(
  target: EventTarget,
  handlers: PlaybackPageHandlers,
): () => void {
  let saved: PlaybackPageSnapshot | null = null;
  const hide = (event: Event) => {
    const snapshot = handlers.snapshot();
    if ((event as PageTransitionEvent).persisted) {
      saved = { ...snapshot };
      handlers.suspend(saved);
    } else {
      saved = null;
      handlers.stop(snapshot);
    }
  };
  const show = (event: Event) => {
    if (!(event as PageTransitionEvent).persisted || !saved) return;
    const snapshot = saved;
    saved = null;
    const current = handlers.snapshot();
    const sameIdentity = current.playSessionId === snapshot.playSessionId
      || (snapshot.playSessionId !== null
        && current.renewedFromPlaySessionId === snapshot.playSessionId);
    if (current.unitKey !== snapshot.unitKey || !sameIdentity
      || current.endedByAdmin || snapshot.endedByAdmin
      || !RESUMABLE.has(current.phase) || !RESUMABLE.has(snapshot.phase)) return;
    handlers.restore({ ...snapshot, playSessionId: current.playSessionId });
  };
  target.addEventListener("pagehide", hide);
  target.addEventListener("pageshow", show);
  return () => {
    saved = null;
    target.removeEventListener("pagehide", hide);
    target.removeEventListener("pageshow", show);
  };
}
