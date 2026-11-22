"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";

import { useConfirm, useToast } from "@/components/feedback";
import { useSubscribeEntry } from "@/components/subscribe-entry";
import {
  fetchItemSimilar,
  getLibrary,
  getLibraryItemDetail,
  refreshItemMetadata,
  type LibraryItemDetail,
  type LibraryItemFile,
  type MediaLibrary,
} from "@/lib/api/libraries";
import { clearPlaybackHistory } from "@/lib/api/playback";
import { useBackNavigation } from "@/lib/back-navigation";
import { useBackdrop } from "@/lib/backdrop";
import { getDiscoveryReturnPath } from "@/lib/discovery-return-path";
import { refreshItemConfirm, rereadItemNfoConfirm } from "@/lib/library-confirm";
import { isPlayableFile } from "@/lib/library-file-pages";
import { fileResolutionLabel, sortHdrFormats } from "@/lib/library-item-format";
import type { MediaItem } from "@/lib/media-types";
import { usePermissions } from "@/lib/permissions";
import { waitForSimilar } from "@/lib/similar-read";
import { useIsMobile } from "@/lib/use-media-query";
import { usePageTitle } from "@/lib/use-page-title";

import { imageUrl } from "./item-image-url";
import type { SelectedEpisodeContext } from "./season-episodes-section";
import { useItemChaptersAndProbe } from "./use-item-chapters-and-probe";
import { useItemPlaybackMarks } from "./use-item-playback-marks";

export function useLibraryItemController({
  libraryId,
  mediaItemId,
  returnTo,
  fromRecent,
}: {
  libraryId: string;
  mediaItemId: string;
  returnTo?: string;
  fromRecent: boolean;
}) {
  // detail 与 library 必须是前两次 useState（library-item-detail-probe.test.mjs 依赖该顺序）。
  const [detail, setDetail] = useState<LibraryItemDetail | null>(null);
  const [library, setLibrary] = useState<MediaLibrary | null>(null);
  const [failed, setFailed] = useState(false);
  const [reidentifyOpen, setReidentifyOpen] = useState(false);
  const [reidentifyDirty, setReidentifyDirty] = useState(false);
  const [refreshError, setRefreshError] = useState<string | null>(null);
  const [kicking, setKicking] = useState(false);
  const [artworkOpen, setArtworkOpen] = useState(false);
  const [deleteOpen, setDeleteOpen] = useState(false);
  const [deleteFileTarget, setDeleteFileTarget] = useState<LibraryItemFile | null>(null);
  const [selectedSeriesEpisode, setSelectedSeriesEpisode] =
    useState<SelectedEpisodeContext | null>(null);
  const [selectedTrackFileId, setSelectedTrackFileId] = useState<string | null>(null);
  const [similar, setSimilar] = useState<MediaItem[]>([]);
  const [similarPending, setSimilarPending] = useState(false);
  const [similarError, setSimilarError] = useState<string | null>(null);

  const { canManageLibraries } = usePermissions();
  const { canSubscribe, subscriptionOf, open: openSubscribe } = useSubscribeEntry();
  const navigate = useNavigate();
  const discoveryReturnPath = getDiscoveryReturnPath(returnTo);
  const confirm = useConfirm();
  const toast = useToast();
  const pageKey = `${libraryId}/${mediaItemId}`;
  const currentPageKey = useRef(pageKey);
  currentPageKey.current = pageKey;

  const trackFiles =
    detail?.kind !== "tv" ? (detail?.files ?? []) : (selectedSeriesEpisode?.files ?? []);
  const availableTrackFiles = trackFiles.filter(isPlayableFile);
  const explicitTrackFileId = availableTrackFiles.some(
    (file) => file.file_id === selectedTrackFileId,
  )
    ? (selectedTrackFileId ?? undefined)
    : undefined;
  const selectedTrackFile =
    availableTrackFiles.find((file) => file.file_id === selectedTrackFileId) ??
    availableTrackFiles[0] ??
    null;

  const chapterSeason = selectedSeriesEpisode?.seasonNumber;
  const chapterEpisode = selectedSeriesEpisode?.episode.episode_number;
  const handleEpisodeChange = useCallback(
    (selection: SelectedEpisodeContext | null) => {
      if (
        selection?.seasonNumber !== chapterSeason ||
        selection?.episode.episode_number !== chapterEpisode
      ) {
        setSelectedTrackFileId(null);
      }
      setSelectedSeriesEpisode(selection);
    },
    [chapterSeason, chapterEpisode],
  );

  const playback = useItemPlaybackMarks({
    pageKey,
    currentPageKey,
    hasDetail: detail !== null,
    mediaItemId,
    detailMediaItemId: detail?.media_item_id,
    detailKind: detail?.kind,
    selectedSeason: selectedSeriesEpisode?.seasonNumber,
    selectedEpisodeNumber: selectedSeriesEpisode?.episode.episode_number,
    explicitTrackFileId,
    toast,
  });
  const { resetMarking, bumpEpisodesVersion } = playback;

  const reloadSeq = useRef(0);
  const fetchDetail = useCallback(
    async (signal?: AbortSignal): Promise<LibraryItemDetail | null> => {
      const seq = ++reloadSeq.current;
      setFailed(false);
      try {
        const [data, lib] = await Promise.all([
          getLibraryItemDetail(libraryId, mediaItemId, { signal }),
          getLibrary(libraryId).catch(() => null),
        ]);
        if (signal?.aborted || seq !== reloadSeq.current || currentPageKey.current !== pageKey) {
          return null;
        }
        setDetail(data);
        setLibrary(lib);
        return data;
      } catch (error) {
        if (!signal?.aborted && seq === reloadSeq.current && currentPageKey.current === pageKey) {
          setFailed(true);
        }
        throw error;
      }
    },
    [libraryId, mediaItemId, pageKey],
  );
  const reload = useCallback(() => {
    fetchDetail().catch(() => undefined);
  }, [fetchDetail]);

  const chaptersProbe = useItemChaptersAndProbe({
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
    onError: setRefreshError,
  });

  useEffect(() => {
    setDetail(null);
    setRefreshError(null);
    setSelectedSeriesEpisode(null);
    setSelectedTrackFileId(null);
    resetMarking();
    setDeleteFileTarget(null);
    const controller = new AbortController();
    fetchDetail(controller.signal).catch(() => undefined);
    return () => controller.abort();
  }, [fetchDetail, resetMarking]);

  const { setOverrideBackdrop } = useBackdrop();
  const isMobile = useIsMobile();

  const immersiveUrl = imageUrl(detail?.backdrop_url ?? detail?.poster_url ?? null);
  useEffect(() => {
    if (!immersiveUrl) return;
    setOverrideBackdrop(immersiveUrl);
    return () => setOverrideBackdrop(null);
  }, [immersiveUrl, setOverrideBackdrop]);

  usePageTitle(detail?.title);

  const navFallback = discoveryReturnPath
    ? { label: "发现详情", href: discoveryReturnPath }
    : fromRecent
      ? { label: "媒体库", href: "/library" }
      : { label: library?.name || "库存", href: `/library/${libraryId}` };
  const back = useBackNavigation(navFallback.href);

  const mediaTmdbId = detail?.tmdb_id;
  const mediaItemIdKey = detail?.media_item_id;
  useEffect(() => {
    setSimilar([]);
    setSimilarPending(false);
    setSimilarError(null);
    if (!mediaTmdbId || !mediaItemIdKey) return;
    const controller = new AbortController();
    void waitForSimilar(
      (signal) => fetchItemSimilar(libraryId, mediaItemIdKey, signal),
      { signal: controller.signal, onPending: () => setSimilarPending(true) },
    )
      .then((hits) => {
        if (controller.signal.aborted) return;
        setSimilarPending(false);
        setSimilar(
          hits.map(
            (hit) =>
              ({
                id: hit.tmdb_id ?? "",
                title: hit.title,
                type: hit.kind === "tv" ? "tv" : "movie",
                year: hit.year ?? undefined,
                posterUrl: hit.poster_url ? imageUrl(hit.poster_url) : null,
                href: hit.tmdb_id
                  ? `/media/${hit.kind === "tv" ? "tv" : "movie"}/${hit.tmdb_id}`
                  : undefined,
              }) as unknown as MediaItem,
          ),
        );
      })
      .catch((error: unknown) => {
        if (controller.signal.aborted) return;
        setSimilarPending(false);
        setSimilarError(error instanceof Error ? error.message : "相似推荐暂时不可用");
      });
    return () => controller.abort();
  }, [libraryId, mediaTmdbId, mediaItemIdKey]);

  const runMetadataRefresh = async () => {
    const ask = detail?.kind === "video" ? rereadItemNfoConfirm : refreshItemConfirm;
    if (!(await confirm(ask(detail?.title ?? "此条目")))) return;
    setKicking(true);
    try {
      await refreshItemMetadata(libraryId, mediaItemId);
      reload();
      setTimeout(reload, 1500);
    } catch (err) {
      setRefreshError(err instanceof Error ? err.message : "元数据刷新失败，请稍后重试");
    } finally {
      setKicking(false);
    }
  };

  const handleClearHistory = () => {
    if (!detail) return;
    const expectedKey = currentPageKey.current;
    void confirm({
      title: `清除《${detail.title}》的观看记录？`,
      description: "续播进度、已看标记和播放次数都会清除，无法恢复。只影响你自己的记录。",
      confirmLabel: "清除",
      tone: "danger",
    }).then((ok) => {
      if (!ok) return;
      clearPlaybackHistory("item", { mediaItemId: detail.media_item_id })
        .then((cleared) => {
          if (currentPageKey.current !== expectedKey) return;
          bumpEpisodesVersion();
          toast.success(`已清除 ${cleared.deleted_states} 条观看记录`);
        })
        .catch((e) => toast.error((e as Error).message));
    });
  };

  const handleUpgrade =
    detail && canSubscribe && detail.tmdb_id != null && detail.kind !== "video"
      ? () => {
          const existing = subscriptionOf({
            id: String(detail.tmdb_id),
            type: detail.kind === "tv" ? "tv" : "movie",
          });
          if (existing) {
            navigate(`/subscriptions/${existing.id}?upgrade-run=1`);
            return;
          }
          void openSubscribe(
            {
              id: String(detail.tmdb_id),
              title: detail.title,
              rating: 0,
              posterUrl: "",
              type: detail.kind === "tv" ? "tv" : "movie",
              year: detail.year ?? undefined,
            },
            { upgradeIntent: true },
          );
        }
      : undefined;

  const isMovie = detail ? detail.kind !== "tv" : false;
  const resolutionSources = isMovie
    ? (detail?.files ?? [])
    : selectedTrackFile
      ? [selectedTrackFile]
      : [];
  const itemResolutions = [
    ...new Set(
      resolutionSources
        .map((file) => fileResolutionLabel(file))
        .filter((resolution): resolution is string => Boolean(resolution)),
    ),
  ];
  const itemHdrFormats = sortHdrFormats(
    resolutionSources.map((file) => file.hdr).filter((hdr): hdr is string => Boolean(hdr)),
  );
  const itemFacts = detail
    ? [
        detail.year ? String(detail.year) : null,
        detail.rating ? `评分 ${detail.rating}` : null,
        detail.content_rating ? `分级 ${detail.content_rating}` : null,
        detail.runtime_minutes ? `${detail.runtime_minutes} 分钟` : null,
      ].filter((fact): fact is string => Boolean(fact))
    : [];

  return {
    detail,
    library,
    failed,
    canManageLibraries,
    navigate,
    toast,
    isMobile,
    navFallback,
    back,
    isMovie,
    scrapedLibrary: library != null && (library.kind === "movie" || library.kind === "tv"),
    scrapingNow: kicking,
    refreshError,
    immersiveUrl,
    trackFiles,
    availableTrackFiles,
    explicitTrackFileId,
    selectedTrackFile,
    setSelectedTrackFileId,
    selectedSeriesEpisode,
    handleEpisodeChange,
    itemResolutions,
    itemHdrFormats,
    itemFacts,
    itemPlot:
      detail?.overview ?? (isMovie ? undefined : (selectedSeriesEpisode?.episode.overview ?? undefined)),
    similar,
    similarPending,
    similarError,
    reidentifyOpen,
    setReidentifyOpen,
    reidentifyDirty,
    setReidentifyDirty,
    artworkOpen,
    setArtworkOpen,
    deleteOpen,
    setDeleteOpen,
    deleteFileTarget,
    setDeleteFileTarget,
    reload,
    runMetadataRefresh,
    handleClearHistory,
    handleUpgrade,
    ...playback,
    ...chaptersProbe,
  };
}
