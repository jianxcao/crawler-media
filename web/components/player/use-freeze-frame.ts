"use client";

import { useCallback, useEffect, useRef, useState, type MutableRefObject } from "react";

import { createArtworkImage } from "@/lib/artwork-loading";
import {
  canReleaseFreeze,
  captureFrame,
  drawTile,
  FREEZE_FRAME_MAX_MS,
  whenTileReady,
} from "@/lib/player/freeze-frame";
import { awaitsUserDecision, type PlayerPhase } from "@/lib/player/machine";
import { type TrickplayIndex, tileAt } from "@/lib/player/trickplay";

export function useFreezeFrame({
  video,
  phase,
  unitKey,
  trickplayRef,
  pendingCommitRef,
}: {
  video: HTMLVideoElement | null;
  phase: PlayerPhase;
  unitKey: string;
  trickplayRef: MutableRefObject<TrickplayIndex | null>;
  pendingCommitRef: MutableRefObject<number | null>;
}) {
  const freezeCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const [frozen, setFrozen] = useState(false);
  const freezeTokenRef = useRef(0);
  const sheetCacheRef = useRef(
    new Map<string, { image: HTMLImageElement; abort: AbortController }>(),
  );

  const freezeFrame = useCallback(
    (targetFileMs?: number) => {
      const canvas = freezeCanvasRef.current;
      if (!video || !canvas) return;
      const token = freezeTokenRef.current + 1;
      freezeTokenRef.current = token;
      if (captureFrame(video, canvas)) setFrozen(true);

      const tile = targetFileMs === undefined ? null : tileAt(trickplayRef.current, targetFileMs);
      if (!tile) return;
      const cache = sheetCacheRef.current;
      let entry = cache.get(tile.url);
      if (!entry) {
        while (cache.size >= 4) {
          const oldest = cache.keys().next().value;
          if (oldest === undefined) break;
          cache.get(oldest)?.abort.abort();
          cache.delete(oldest);
        }
        const abort = new AbortController();
        const request = createArtworkImage(abort.signal, 0);
        entry = { image: request.image, abort };
        request.load(tile.url);
        cache.set(tile.url, entry);
      }
      const image = entry.image;
      const paint = () => {
        if (freezeTokenRef.current !== token) return;
        if (drawTile(canvas, image, tile)) setFrozen(true);
      };
      whenTileReady(image, paint);
    },
    [video, trickplayRef],
  );

  useEffect(() => {
    if (!frozen || !video) return;
    const release = () => {
      if (pendingCommitRef.current !== null) return;
      if (canReleaseFreeze({ seeking: video.seeking, readyState: video.readyState })) {
        freezeTokenRef.current += 1;
        setFrozen(false);
      }
    };
    const events = ["seeked", "playing", "canplay", "timeupdate"] as const;
    for (const event of events) video.addEventListener(event, release);
    const timer = window.setTimeout(() => setFrozen(false), FREEZE_FRAME_MAX_MS);
    return () => {
      for (const event of events) video.removeEventListener(event, release);
      window.clearTimeout(timer);
    };
  }, [frozen, pendingCommitRef, video]);

  useEffect(() => {
    if (awaitsUserDecision(phase)) setFrozen(false);
  }, [phase]);

  useEffect(() => {
    setFrozen(false);
    freezeTokenRef.current += 1;
    const cache = sheetCacheRef.current;
    for (const entry of cache.values()) entry.abort.abort();
    cache.clear();
    return () => {
      for (const entry of cache.values()) entry.abort.abort();
      cache.clear();
    };
  }, [unitKey]);

  return { freezeCanvasRef, frozen, freezeFrame };
}
