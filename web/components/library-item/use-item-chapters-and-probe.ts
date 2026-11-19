"use client";

import { useEffect, useMemo, useRef, useState, type MutableRefObject } from "react";

import {
  fetchChapterFrameStatus,
  fetchItemChapters,
  fetchItemProbeStatus,
  generateItemChapters,
  probeItem,
  refreshItemChapters,
  type ChapterTarget,
  type ItemChapter,
  type LibraryItemDetail,
  type LibraryItemFile,
} from "@/lib/api/libraries";
import { createChapterFrameSession, type FrameTask } from "@/lib/chapter-frame-task";
import { createLibraryItemProbe } from "@/lib/library-item-probe";
import { startSingleFlightPoll } from "@/lib/single-flight-poll";

import type { SelectedEpisodeContext } from "./season-episodes-section";

export function useItemChaptersAndProbe({
  libraryId,
  mediaItemId,
  pageKey,
  currentPageKey,
  detail,
  selectedSeriesEpisode,
  selectedTrackFile,
  canManageLibraries,
  fetchDetail,
  toast,
  onError,
}: {
  libraryId: string;
  mediaItemId: string;
  pageKey: string;
  currentPageKey: MutableRefObject<string>;
  detail: LibraryItemDetail | null;
  selectedSeriesEpisode: SelectedEpisodeContext | null;
  selectedTrackFile: LibraryItemFile | null;
  canManageLibraries: boolean;
  fetchDetail: (signal?: AbortSignal) => Promise<LibraryItemDetail | null>;
  toast: { info: (message: string) => void; success: (message: string) => void };
  onError: (message: string | null) => void;
}) {
  const [chapterRows, setChapters] = useState<ItemChapter[]>([]);
  const [chapterPending, setChaptersBusy] = useState(false);
  const [knownChapterError, setChapterError] = useState<string | null>(null);
  const [knownFrameTask, setFrameTask] = useState<FrameTask | null>(null);
  const chapterSession = useRef<ReturnType<typeof createChapterFrameSession<ItemChapter>> | null>(
    null,
  );
  const itemProbe = useRef<ReturnType<typeof createLibraryItemProbe> | null>(null);
  const [chapterDiscovery, setChapterDiscovery] = useState(0);
  const [presentedChapterKey, setPresentedChapterKey] = useState("");
  const [probing, setProbing] = useState(false);

  const chapterFileId = selectedTrackFile?.file_id;
  const chapterIsTv = detail?.kind === "tv";
  const chapterSeason = selectedSeriesEpisode?.seasonNumber;
  const chapterEpisode = selectedSeriesEpisode?.episode.episode_number;
  const chapterTarget = useMemo<ChapterTarget | null>(() => {
    if (!chapterFileId || (chapterIsTv && (chapterSeason == null || chapterEpisode == null))) {
      return null;
    }
    return {
      file_id: chapterFileId,
      ...(chapterIsTv ? { season: chapterSeason, episode: chapterEpisode } : {}),
    };
  }, [chapterFileId, chapterIsTv, chapterSeason, chapterEpisode]);
  const chapterKey = JSON.stringify([libraryId, mediaItemId, chapterTarget]);
  const currentChapterSelection = chapterKey === presentedChapterKey;
  const chapters = currentChapterSelection ? chapterRows : [];
  const frameTask = currentChapterSelection ? knownFrameTask : null;
  const chapterError = currentChapterSelection ? knownChapterError : null;
  const chaptersBusy = currentChapterSelection && chapterPending;

  useEffect(() => {
    const operation = createLibraryItemProbe({
      request: (signal) => probeItem(libraryId, mediaItemId, signal),
      reload: fetchDetail,
      chapterSession: () => chapterSession.current,
      pageCurrent: () => currentPageKey.current === pageKey,
      onBusy: setProbing,
      onError,
      onResult: (result) => {
        if (result.already_running) toast.info("该条目已有媒体探测任务正在运行");
        else toast.success(`已提交 ${result.queued} 个文件的媒体信息与声纹探测`);
      },
    });
    itemProbe.current = operation;
    setProbing(false);
    return () => {
      operation.stop();
      if (itemProbe.current === operation) itemProbe.current = null;
    };
  }, [currentPageKey, fetchDetail, libraryId, mediaItemId, onError, pageKey, toast]);

  const probeQueued = !!detail?.files.some((file) => file.probe_queued);
  useEffect(() => {
    if (!probeQueued) return;
    return startSingleFlightPoll(async (signal) => {
      const latest = await fetchDetail(signal);
      return latest == null || latest.files.some((file) => file.probe_queued);
    }, 5000);
  }, [probeQueued, fetchDetail]);

  useEffect(() => {
    setPresentedChapterKey(chapterKey);
    setChapters([]);
    setFrameTask(null);
    setChapterError(null);
    setChaptersBusy(false);
    if (!chapterTarget) return;
    const session = createChapterFrameSession<ItemChapter>({
      loadChapters: (signal) => fetchItemChapters(libraryId, mediaItemId, chapterTarget, signal),
      loadStatus: (expected, signal) =>
        fetchChapterFrameStatus(libraryId, mediaItemId, chapterTarget, expected, signal),
      onChapters: setChapters,
      onTask: setFrameTask,
      onError: setChapterError,
      onBusy: setChaptersBusy,
    });
    chapterSession.current = session;
    void session.discover();
    return () => {
      session.stop();
      if (chapterSession.current === session) chapterSession.current = null;
    };
  }, [libraryId, mediaItemId, chapterTarget, chapterKey]);

  const probeSeasonsKey =
    detail?.kind === "tv" ? [...new Set(detail.seasons)].sort((a, b) => a - b).join(",") : "";
  useEffect(() => {
    const session = chapterSession.current;
    if (!canManageLibraries || !session || !chapterTarget || !probeSeasonsKey) return;
    const controller = new AbortController();
    const observedActive = new Set<number>();
    const read = async () => {
      let active = false;
      for (const season of probeSeasonsKey.split(",").map(Number)) {
        const version = session.version();
        try {
          const status = await fetchItemProbeStatus(libraryId, mediaItemId, season, controller.signal);
          if (controller.signal.aborted) return false;
          if (!session.active(version)) {
            active = true;
            continue;
          }
          if (status.active) {
            observedActive.add(season);
            active = true;
            continue;
          }
          const wasActive = observedActive.delete(season);
          if (chapterTarget.season === season && (wasActive || status.job)) await session.discover();
          if (controller.signal.aborted) return false;
        } catch {
          if (!controller.signal.aborted) active = true;
        }
      }
      return active;
    };
    let stop: (() => void) | undefined;
    void read().then((active) => {
      if (!controller.signal.aborted && active) stop = startSingleFlightPoll(read, 5000);
    });
    return () => {
      controller.abort();
      stop?.();
    };
  }, [canManageLibraries, libraryId, mediaItemId, probeSeasonsKey, chapterTarget, chapterDiscovery]);

  const wasProbeQueued = useRef(false);
  useEffect(() => {
    if (wasProbeQueued.current && !probeQueued) void chapterSession.current?.discover();
    wasProbeQueued.current = probeQueued;
  }, [probeQueued]);

  const regenerateChapters = async () => {
    if (!chapterTarget || !detail) return;
    await chapterSession.current?.generate((signal) =>
      generateItemChapters(
        libraryId,
        detail.media_item_id,
        chapterTarget,
        frameTask?.state === "partial" || frameTask?.state === "failed",
        signal,
      ),
    );
  };

  const forceRefreshChaptersAndMarkers = async (capturePolicy: "reuse_valid" | "recapture") => {
    if (!chapterTarget || !detail) return;
    await chapterSession.current?.run(async (operation) => {
      const refreshed = await refreshItemChapters(
        libraryId,
        detail.media_item_id,
        chapterTarget,
        capturePolicy,
        operation.signal,
      );
      if (!operation.current()) return;
      if (refreshed.fingerprint_refresh_error) throw new Error(refreshed.fingerprint_refresh_error);
      if (refreshed.fingerprint_refresh_queued || refreshed.fingerprint_refresh_already_running) {
        toast.info("已提交片头片尾任务，当前结果会在完成后更新");
        setChapterDiscovery((value) => value + 1);
      }
      await operation.discover();
    });
  };

  const handleProbeMedia = () => itemProbe.current?.run();

  return {
    chapterTarget,
    chapters,
    frameTask,
    chapterError,
    chaptersBusy,
    probing,
    regenerateChapters,
    forceRefreshChaptersAndMarkers,
    handleProbeMedia,
  };
}
