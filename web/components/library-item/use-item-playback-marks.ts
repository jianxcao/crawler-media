"use client";

import { useCallback, useEffect, useMemo, useState, type MutableRefObject } from "react";

import {
  fetchPlaybackMarks,
  fetchResumeState,
  setPlaybackMarks,
  type PlaybackUnit,
  type PlaybackWatchState,
} from "@/lib/api/playback";

export function useItemPlaybackMarks({
  pageKey,
  currentPageKey,
  hasDetail,
  mediaItemId,
  detailMediaItemId,
  detailKind,
  selectedSeason,
  selectedEpisodeNumber,
  explicitTrackFileId,
  toast,
}: {
  pageKey: string;
  currentPageKey: MutableRefObject<string>;
  hasDetail: boolean;
  mediaItemId: string;
  detailMediaItemId?: string;
  detailKind?: string;
  selectedSeason?: number;
  selectedEpisodeNumber?: number;
  explicitTrackFileId?: string;
  toast: { error: (message: string) => void };
}) {
  const [watched, setWatched] = useState<PlaybackWatchState | null>(null);
  const [favorite, setFavorite] = useState<boolean | null>(null);
  const [marking, setMarking] = useState(false);
  const [episodesVersion, setEpisodesVersion] = useState(0);

  const isTv = detailKind === "tv";
  const playUnit = useMemo<PlaybackUnit | null>(() => {
    if (!hasDetail || !detailMediaItemId) return null;
    if (!isTv) {
      return {
        media_item_id: detailMediaItemId,
        file_id: explicitTrackFileId,
      };
    }
    if (selectedSeason == null || selectedEpisodeNumber == null) return null;
    return {
      media_item_id: detailMediaItemId,
      season_number: selectedSeason,
      episode_number: selectedEpisodeNumber,
      file_id: explicitTrackFileId,
    };
  }, [hasDetail, detailMediaItemId, isTv, selectedSeason, selectedEpisodeNumber, explicitTrackFileId]);

  useEffect(() => {
    if (!hasDetail || !detailMediaItemId || (isTv && (selectedSeason == null || selectedEpisodeNumber == null))) {
      setWatched(null);
      return;
    }
    const unit: PlaybackUnit = isTv
      ? {
          media_item_id: detailMediaItemId,
          season_number: selectedSeason,
          episode_number: selectedEpisodeNumber,
          file_id: explicitTrackFileId,
        }
      : {
          media_item_id: detailMediaItemId,
          file_id: explicitTrackFileId,
        };
    let cancelled = false;
    setWatched(null);
    fetchResumeState(unit)
      .then((state) => {
        if (!cancelled) setWatched(state);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [
    hasDetail,
    detailMediaItemId,
    isTv,
    selectedSeason,
    selectedEpisodeNumber,
    explicitTrackFileId,
    episodesVersion,
  ]);

  useEffect(() => {
    let cancelled = false;
    setFavorite(null);
    fetchPlaybackMarks({ media_item_id: mediaItemId })
      .then((marks) => {
        if (!cancelled) setFavorite(marks.is_favorite);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [mediaItemId]);

  const toggleFavorite = useCallback(async () => {
    if (marking) return;
    const key = pageKey;
    const next = !(favorite ?? false);
    setMarking(true);
    setFavorite(next);
    try {
      const marks = await setPlaybackMarks(
        { media_item_id: mediaItemId },
        { favorite: next },
      );
      if (currentPageKey.current !== key) return;
      setFavorite(marks.is_favorite);
    } catch (e) {
      if (currentPageKey.current !== key) return;
      setFavorite(!next);
      toast.error(e instanceof Error ? e.message : "收藏失败，请稍后重试");
    } finally {
      if (currentPageKey.current === key) setMarking(false);
    }
  }, [currentPageKey, favorite, marking, mediaItemId, pageKey, toast]);

  const togglePlayed = useCallback(async () => {
    if (marking || !playUnit) return;
    const key = pageKey;
    const next = !(watched?.played ?? false);
    setMarking(true);
    try {
      await setPlaybackMarks(playUnit, { played: next });
      if (currentPageKey.current !== key) return;
      setEpisodesVersion((v) => v + 1);
    } catch (e) {
      if (currentPageKey.current !== key) return;
      toast.error(e instanceof Error ? e.message : "标记失败，请稍后重试");
    } finally {
      if (currentPageKey.current === key) setMarking(false);
    }
  }, [currentPageKey, marking, pageKey, playUnit, toast, watched]);

  const resetMarking = useCallback(() => {
    setMarking(false);
  }, []);

  return {
    watched,
    favorite,
    marking,
    episodesVersion,
    toggleFavorite,
    togglePlayed,
    resetMarking,
  };
}
