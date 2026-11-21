"use client";

import { Link } from "react-router-dom";

import {
  BellIcon,
  CheckIcon,
  FolderIcon,
  SearchIcon,
  StarIcon,
} from "@/components/icons";
import type { MediaDetailInfo } from "@/lib/api/discover";
import type { Subscription } from "@/lib/api/subscriptions";
import type { MediaItem, MediaLibraryLink } from "@/lib/media-types";
import {
  subscriptionProgressNote,
  subscriptionStatus,
  subscriptionStatusMeta,
} from "@/lib/subscription-ui";

/** 在线影片详情头部信息与核心操作区（标题、评分/年代/规格、在库链接、订阅/搜种）。 */
export function MediaDetailHero({
  item,
  info,
  isMovie,
  libraryLinks,
  libraryReturnTo,
  sub,
  showSubscribeButton,
  showSearchButton,
  onOpenSubscribe,
}: {
  item: MediaItem;
  info?: MediaDetailInfo;
  isMovie: boolean;
  libraryLinks: MediaLibraryLink[];
  libraryReturnTo: string;
  sub: Subscription | null | undefined;
  showSubscribeButton: boolean;
  showSearchButton: boolean;
  onOpenSubscribe: () => void;
}) {
  return (
    <div className="relative z-10 px-12 pt-6 max-md:px-4 max-md:pt-3">
      <div className="min-w-0 max-w-5xl pb-1">
        <h1 className="text-on-image break-words text-[42px] font-bold leading-[1.1] tracking-[-0.02em] text-white max-md:text-[28px]">
          {item.title}
        </h1>

        <div className="tnum mt-3.5 flex flex-wrap items-center gap-x-3 gap-y-2 text-ui text-white/80 max-md:mt-2 max-md:gap-x-2 max-md:text-sub">
          {item.rating > 0 && (
            <span className="flex items-center gap-1.5">
              <StarIcon className="size-4 text-[var(--warn)]" />
              <span className="text-title-sm font-bold text-white">{item.rating.toFixed(1)}</span>
            </span>
          )}
          {!!item.year && (
            <span className="flex items-center gap-3 max-md:gap-2">
              {item.rating > 0 && <span aria-hidden="true">·</span>}
              <span>{item.year}</span>
            </span>
          )}
          {item.extent && (
            <span className="flex items-center gap-3 max-md:gap-2">
              {(item.rating > 0 || !!item.year) && <span aria-hidden="true">·</span>}
              <span>{item.extent}</span>
            </span>
          )}
          {item.badges.length > 0 && (
            <span className="flex items-center gap-3 max-md:gap-2">
              {(item.rating > 0 || !!item.year || !!item.extent) && (
                <span aria-hidden="true">·</span>
              )}
              <span>{item.badges.join(" / ")}</span>
            </span>
          )}
        </div>

        {(info?.country || info?.language || item.genres.length > 0) && (
          <p className="text-on-image mt-3 flex flex-wrap items-center gap-x-3 gap-y-1.5 text-ui leading-6 text-white/72 max-md:mt-2 max-md:text-sub">
            {info && (info.country || info.language) && (
              <span className="whitespace-nowrap text-white/65">
                {[info.country, info.language].filter(Boolean).join(" · ")}
              </span>
            )}
            {item.genres.length > 0 && (
              <span className="flex min-w-0 items-start gap-3">
                {info && (info.country || info.language) && (
                  <span aria-hidden="true" className="shrink-0 text-white/25">
                    ｜
                  </span>
                )}
                <span>{item.genres.join(" · ")}</span>
              </span>
            )}
          </p>
        )}

        {info?.released && (
          <p className="tnum text-on-image mt-1.5 text-sub leading-6 text-white/55">
            {isMovie ? "上映日期" : "首播日期"} · {info.released}
          </p>
        )}

        {libraryLinks.length > 0 && (
          <div className="mt-4 max-w-full max-md:mt-3">
            <div className="inline-flex max-w-full flex-wrap items-center gap-x-3 gap-y-2 rounded-lg border border-emerald-300/20 bg-emerald-400/[0.08] px-3.5 py-2.5 text-sub text-emerald-100/90 max-md:px-3">
              <span className="flex shrink-0 items-center gap-1.5 font-medium text-emerald-200">
                <FolderIcon className="size-4" />
                在库
              </span>
              {libraryLinks.map((libraryLink) => (
                <Link
                  key={`${libraryLink.libraryId}:${libraryLink.mediaItemId}`}
                  to={`/library/${libraryLink.libraryId}/item/${libraryLink.mediaItemId}?returnTo=${encodeURIComponent(libraryReturnTo)}`}
                  className="min-w-0 max-w-full break-words font-medium text-emerald-100 underline decoration-emerald-200/45 underline-offset-4 transition-colors hover:text-white hover:decoration-emerald-100"
                >
                  {libraryLink.libraryName}
                </Link>
              ))}
            </div>
          </div>
        )}

        {(showSubscribeButton || showSearchButton || sub) && (
          <div className="mt-5 flex flex-wrap items-center gap-3 max-md:mt-3.5 max-md:gap-2">
            {showSubscribeButton &&
              (sub ? (
                <button
                  type="button"
                  onClick={onOpenSubscribe}
                  className="btn-glass flex h-10 items-center gap-2 bg-white/10 px-5 text-ui font-medium backdrop-blur-md transition hover:bg-white/15"
                >
                  <CheckIcon
                    className="size-4"
                    style={{ color: subscriptionStatusMeta[subscriptionStatus(sub)].color }}
                  />
                  已订阅 · {subscriptionStatusMeta[subscriptionStatus(sub)].label}
                </button>
              ) : (
                <button
                  type="button"
                  onClick={onOpenSubscribe}
                  className="btn-accent flex h-10 items-center gap-2 rounded-full px-5 text-ui font-semibold"
                >
                  <BellIcon className="size-4" />
                  订阅追踪
                </button>
              ))}
            {showSearchButton && item.title && (
              <Link
                to={`/search?q=${encodeURIComponent(item.title)}`}
                className="btn-glass flex h-10 items-center gap-2 bg-white/10 px-5 text-ui font-medium backdrop-blur-md transition hover:bg-white/15"
              >
                <SearchIcon className="size-4" />
                搜索资源
              </Link>
            )}
            {sub && (
              <span className="text-on-image flex items-center gap-1.5 text-sub text-[var(--text-muted)]">
                <span
                  className="size-1.5 rounded-full"
                  style={{ backgroundColor: subscriptionStatusMeta[subscriptionStatus(sub)].color }}
                />
                {subscriptionProgressNote(sub)}
              </span>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
