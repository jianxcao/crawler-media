"use client";

import { CastRow } from "@/components/cast-row";
import { DetailBackdropSlideshow } from "@/components/detail-backdrop-slideshow";
import { ArrowLeftIcon } from "@/components/icons";
import { MediaRow } from "@/components/media-row";
import { SubscribeDialog } from "@/components/subscribe-dialog";
import type { MediaSource, MediaType } from "@/lib/media-types";

import { ExpandablePlot, SourceLink } from "./media-detail/detail-metadata-widgets";
import { MediaDetailHero } from "./media-detail/media-detail-hero";
import { MediaDetailFallback, MediaDetailScaffold } from "./media-detail/media-detail-scaffold";
import { PhotoWall } from "./media-detail/photo-wall";
import { SeasonsSection } from "./media-detail/seasons-section";
import { TrailerRow } from "./media-detail/trailer-row";
import { useMediaDetail } from "./media-detail/use-media-detail";

/**
 * 发现影片详情页（/media/[type]/[id]、/media/douban/[id]）：
 * 与 LibraryItemDetailView 共享 MediaDetailScaffold / DetailPageNav 骨架。
 */
export function MediaDetailView({
  type,
  id,
  source = "tmdb",
}: {
  type?: MediaType;
  id: string;
  source?: MediaSource;
}) {
  const c = useMediaDetail({ type, id, source });
  const { item, detail, info } = c;

  if (!item) {
    return (
      <MediaDetailFallback
        failed={c.loadFailed}
        navFallback={c.navFallback}
        onBack={c.back}
        loadingLabel="正在加载详情…"
        failedTitle="未能加载该影片详情"
        failedDescription="资源可能已下线，或网络暂时不可达。请返回后重试。"
        failedAction={
          <button
            type="button"
            onClick={c.back}
            className="btn-glass px-4 py-2 text-ui font-medium text-[var(--text)]"
          >
            <ArrowLeftIcon className="size-4" />
            返回
          </button>
        }
      />
    );
  }

  return (
    <MediaDetailScaffold
      title={item.title}
      navFallback={c.navFallback}
      onBack={c.back}
      mobileHeroSrc={c.mobileHeroSrc}
      netflixBackdrop={
        detail && detail.backdrops.length > 1 ? (
          <DetailBackdropSlideshow
            images={detail.backdrops}
            initialUrl={detail.backdropOriginalUrl ?? detail.backdrops[0]?.fullUrl}
          />
        ) : null
      }
      lead={
        <>
          <MediaDetailHero
            item={item}
            info={info}
            isMovie={c.isMovie}
            libraryLinks={c.libraryLinks}
            libraryReturnTo={c.libraryReturnTo}
            sub={c.sub}
            showSubscribeButton={c.showSubscribeButton}
            showSearchButton={c.showSearchButton}
            onOpenSubscribe={c.openSubscribe}
          />

          <SubscribeDialog
            target={c.subscribeTarget}
            onClose={() => c.setSubscribeTarget(null)}
            onChanged={c.refreshSubscriptions}
          />

          {item.overview && (
            <div className="mt-4 px-12 max-md:px-4">
              <ExpandablePlot text={item.overview} selectable />
            </div>
          )}
        </>
      }
    >
      {!c.isMovie && detail?.seasons && detail.seasons.length > 0 && (
        <div className="mt-9 px-12 max-md:mt-6 max-md:px-4">
          <SeasonsSection key={item.id} mediaId={item.id} seasons={detail.seasons} />
        </div>
      )}

      {c.people.length > 0 && (
        <div className="mt-9 px-12 max-md:mt-6 max-md:px-4">
          <CastRow cast={c.people} personHrefPrefix="/discover/people" />
        </div>
      )}

      {detail && detail.videos.length > 0 && (
        <div className="mt-9 px-12 max-md:mt-6 max-md:px-4">
          <TrailerRow title={item.title} videos={detail.videos} />
        </div>
      )}

      {detail && (detail.backdrops.length > 0 || detail.posters.length > 0) && (
        <div className="mt-9 px-12 max-md:mt-6 max-md:px-4">
          <PhotoWall
            title={item.title}
            backdrops={detail.backdrops}
            posters={detail.posters}
          />
        </div>
      )}

      {c.collection && c.collection.items.length > 1 && (
        <div className="mt-9">
          <MediaRow
            row={{
              id: `collection-${c.collection.id}`,
              title: c.collection.name,
              items: c.collection.items,
            }}
            insetClassName="px-12 max-md:px-4"
          />
        </div>
      )}

      {c.related.length > 0 && (
        <div className="mt-9">
          <MediaRow
            row={{ id: `related-${item.id}`, title: "相似推荐", items: c.related }}
            insetClassName="px-12 max-md:px-4"
          />
        </div>
      )}

      {(source === "tmdb" || info?.sourceUrl) && (
        <nav
          aria-label="外部词条"
          className="mx-12 mt-9 flex flex-wrap items-center gap-x-4 gap-y-2 border-t border-white/[0.04] pt-4 text-caption max-md:mx-4"
        >
          <span className="text-[var(--text-faint)]">相关链接</span>
          {source === "tmdb" && (
            <SourceLink
              href={`https://www.themoviedb.org/${item.type}/${item.id}`}
              label="TMDB"
            />
          )}
          {info?.sourceUrl && (
            <SourceLink href={c.doubanAppHref ?? info.sourceUrl} label="豆瓣" />
          )}
        </nav>
      )}
    </MediaDetailScaffold>
  );
}
