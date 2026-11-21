"use client";

import { useEffect, useState } from "react";

import type { SubscribeTarget } from "@/components/subscribe-dialog";
import { useSubscribeEntry } from "@/components/subscribe-entry";
import {
  fetchDiscoveredTitleDetails,
  titleRef,
  type MediaDetailData,
} from "@/lib/api/discover";
import { useBackNavigation } from "@/lib/back-navigation";
import { useBackdrop } from "@/lib/backdrop";
import { createArtworkImage } from "@/lib/artwork-loading";
import { buildDiscoveryReturnPath } from "@/lib/discovery-return-path";
import { useDoubanAppHref } from "@/lib/douban-app-link";
import { upgradedTmdbOriginalUrl } from "@/lib/image-proxy";
import { getMediaSeed } from "@/lib/media-detail";
import {
  buildDetailPeopleList,
  resolveImmersiveBackdropUrl,
  resolveMediaDetailOwnership,
  resolveMobileHeroSrc,
  type HdArtworkState,
} from "@/lib/media-detail-helpers";
import type { MediaSource, MediaType } from "@/lib/media-types";
import { usePermissions } from "@/lib/permissions";
import { usePageTitle } from "@/lib/use-page-title";

export function useMediaDetail({
  type,
  id,
  source,
}: {
  type?: MediaType;
  id: string;
  source: MediaSource;
}) {
  const { canSearch } = usePermissions();
  const [detail, setDetail] = useState<MediaDetailData | null>(null);
  const [loadFailed, setLoadFailed] = useState(false);

  const {
    canSubscribe,
    subscriptionOf,
    refresh: refreshSubscriptions,
  } = useSubscribeEntry();
  const sub = subscriptionOf({ id, source, type: type ?? "movie" });
  const [subscribeTarget, setSubscribeTarget] = useState<SubscribeTarget | null>(null);

  const listItem = getMediaSeed(source, id);
  const fallbackType = listItem?.type ?? type ?? "movie";
  const navFallback = {
    label: fallbackType === "tv" ? "发现剧集" : "发现电影",
    href: `/discover/${fallbackType}`,
  };
  const back = useBackNavigation(navFallback.href);
  const reference = listItem?.titleRef ?? titleRef(source, type ?? "movie", id);

  useEffect(() => {
    setDetail(null);
    setLoadFailed(false);
    const controller = new AbortController();
    fetchDiscoveredTitleDetails(reference, {
      signal: controller.signal,
    })
      .then((data) => {
        if (!controller.signal.aborted) setDetail(data);
      })
      .catch(() => {
        if (!controller.signal.aborted) setLoadFailed(true);
      });
    return () => controller.abort();
  }, [reference]);

  const item = detail?.item ?? listItem;
  usePageTitle(item?.title);

  const { setOverrideBackdrop } = useBackdrop();
  const fallbackBackdrop = item?.backdropUrl || item?.posterUrl || "";
  const hdUrl =
    source === "douban"
      ? undefined
      : (detail?.backdropOriginalUrl ??
        (fallbackBackdrop ? upgradedTmdbOriginalUrl(fallbackBackdrop) : undefined));
  const [hdState, setHdState] = useState<HdArtworkState>("pending");

  useEffect(() => {
    if (!hdUrl) {
      setHdState("failed");
      return;
    }
    setHdState("pending");
    let cancelled = false;
    const preload = new AbortController();
    const { image: img, load: loadImage } = createArtworkImage(preload.signal);
    const settle = () => {
      img
        .decode()
        .catch(() => {})
        .then(() => {
          if (!cancelled) setHdState("ok");
        });
    };
    img.onload = settle;
    img.onerror = () => {
      if (!cancelled) setHdState("failed");
    };
    loadImage(hdUrl);
    return () => {
      cancelled = true;
      preload.abort();
    };
  }, [hdUrl]);

  const immersiveUrl = resolveImmersiveBackdropUrl({
    source,
    hdState,
    hdUrl,
    fallbackBackdrop,
  });
  useEffect(() => {
    if (!immersiveUrl) return;
    setOverrideBackdrop(immersiveUrl);
    return () => setOverrideBackdrop(null);
  }, [immersiveUrl, setOverrideBackdrop]);

  const mobileHeroSrc = resolveMobileHeroSrc({ hdState, hdUrl, fallbackBackdrop });
  const doubanAppHref = useDoubanAppHref(source === "douban" ? id : null);

  const info = detail?.info;
  const collection = detail?.collection;
  const related = detail?.related ?? [];
  const libraryLinks = detail?.libraryLinks ?? [];
  const libraryReturnTo = item ? buildDiscoveryReturnPath(source, type ?? item.type, id) : "";
  const isMovie = (item?.type ?? fallbackType) === "movie";
  const ownership = resolveMediaDetailOwnership({
    isMovie,
    libraryLinksCount: libraryLinks.length,
    hasSubscription: Boolean(sub),
    canSubscribe,
    canSearch,
  });
  const people = info
    ? buildDetailPeopleList({
        isMovie,
        directorCredits: info.directorCredits,
        directors: info.directors,
        cast: info.cast,
      })
    : [];

  const openSubscribe = () => {
    if (!item) return;
    setSubscribeTarget({
      titleRef: reference,
      kind: item.type,
      title: item.title,
      year: item.year || undefined,
    });
  };

  return {
    detail,
    item,
    info,
    collection,
    related,
    libraryLinks,
    libraryReturnTo,
    isMovie,
    sub,
    subscribeTarget,
    setSubscribeTarget,
    openSubscribe,
    refreshSubscriptions,
    showSubscribeButton: ownership.showSubscribeButton,
    showSearchButton: ownership.showSearchButton,
    people,
    loadFailed,
    navFallback,
    back,
    mobileHeroSrc,
    doubanAppHref,
  };
}
