"use client";

import { Link } from "react-router-dom";

import { ArtworkImage } from "@/components/artwork-image";
import { CastRow, type CastRowPerson } from "@/components/cast-row";
import { CollectionsAddButton } from "@/components/collections-add-button";
import { ArrowLeftIcon } from "@/components/icons";
import { MediaRow } from "@/components/media-row";
import { MediaTrackRows } from "@/components/media-track-rows";
import { invalidateLibraryDetailSnapshot } from "@/lib/library-detail-snapshot";
import { playLinkFileId } from "@/lib/player/active-target";
import { playHref, rememberPlayerReturnPath } from "@/lib/player/play-links";

import { BrandStrip } from "./media-detail/brand-strip";
import { ExpandablePlot, SourceLink } from "./media-detail/detail-metadata-widgets";
import { MediaDetailFallback, MediaDetailScaffold } from "./media-detail/media-detail-scaffold";
import { FileSection } from "./library-item/file-section";
import { ItemActionsMenu } from "./library-item/item-actions-menu";
import { ItemAdminModals } from "./library-item/item-admin-modals";
import { ItemChaptersSection } from "./library-item/item-chapters-section";
import { imageUrl } from "./library-item/item-image-url";
import { PlayAction } from "./library-item/play-action";
import {
  SeasonEpisodesSection,
  type EpisodeSectionItem,
  type SelectedEpisodeContext,
} from "./library-item/season-episodes-section";
import { useLibraryItemController } from "./library-item/use-library-item-controller";

export {
  PlayAction,
  ExpandablePlot,
  SourceLink,
  SeasonEpisodesSection,
  type SelectedEpisodeContext,
  type EpisodeSectionItem,
};

/**
 * 媒体库条目详情页（/library/[id]/item/[mediaItemId]）。
 */
export function LibraryItemDetailView({
  libraryId,
  mediaItemId,
  returnTo,
  fromRecent = false,
  initialSeason,
  initialEpisode,
}: {
  libraryId: string;
  mediaItemId: string;
  returnTo?: string;
  fromRecent?: boolean;
  initialSeason?: number;
  initialEpisode?: number;
}) {
  const c = useLibraryItemController({ libraryId, mediaItemId, returnTo, fromRecent });
  const { detail } = c;

  if (c.failed || !detail) {
    return (
      <MediaDetailFallback
        outerAmbientFallback
        failed={c.failed}
        navFallback={c.navFallback}
        onBack={c.back}
        loadingLabel="正在读取本地刮削信息…"
        failedTitle="未能加载该条目"
        failedDescription="条目可能已被删除或重新识别为其他作品，请返回后查看。"
        failedAction={
          <Link
            to={c.navFallback.href}
            replace
            className="btn-glass flex items-center gap-2 px-4 py-2 text-ui font-medium text-[var(--text)]"
          >
            <ArrowLeftIcon className="size-4" />
            返回{c.navFallback.label}
          </Link>
        }
      />
    );
  }

  const itemActions = (
    <ItemActionsMenu
      canManage={c.canManageLibraries}
      onClearHistory={c.handleClearHistory}
      identifiable={c.scrapedLibrary}
      scraped={c.scrapedLibrary && detail.tmdb_id != null}
      readsNfo={detail.kind === "video"}
      scraping={c.scrapingNow}
      searchHref={`/search?q=${encodeURIComponent(detail.title)}`}
      onReidentify={() => c.setReidentifyOpen(true)}
      onRefreshMetadata={c.runMetadataRefresh}
      onRefreshChapters={
        detail.files.length > 0
          ? () => void c.forceRefreshChaptersAndMarkers("reuse_valid")
          : undefined
      }
      onRecaptureAudio={
        detail.files.length > 0 ? () => void c.forceRefreshChaptersAndMarkers("recapture") : undefined
      }
      onChangeArtwork={() => c.setArtworkOpen(true)}
      onDelete={() => c.setDeleteOpen(true)}
      onUpgrade={c.handleUpgrade}
    />
  );

  const files = c.isMovie ? detail.files : (c.selectedSeriesEpisode?.files ?? []);

  return (
    <MediaDetailScaffold
      title={detail.title}
      navFallback={c.navFallback}
      onBack={c.back}
      actions={itemActions}
      mobileHeroSrc={c.immersiveUrl}
      footerModals={
        c.canManageLibraries ? (
          <ItemAdminModals
            libraryId={libraryId}
            mediaItemId={mediaItemId}
            detail={detail}
            deleteOpen={c.deleteOpen}
            onCloseDelete={() => c.setDeleteOpen(false)}
            onItemDeleted={() => {
              invalidateLibraryDetailSnapshot(libraryId);
              c.navigate(`/library/${libraryId}`, { replace: true });
            }}
            deleteFileTarget={c.deleteFileTarget}
            onCloseDeleteFile={() => c.setDeleteFileTarget(null)}
            reidentifyOpen={c.reidentifyOpen}
            onCloseReidentify={() => {
              c.setReidentifyOpen(false);
              if (c.reidentifyDirty) {
                c.setReidentifyDirty(false);
                c.reload();
              }
            }}
            onMarkReidentifyDirty={() => c.setReidentifyDirty(true)}
            artworkOpen={c.artworkOpen}
            onCloseArtwork={() => c.setArtworkOpen(false)}
            onReload={c.reload}
            onFileDeletedToast={(name) => c.toast.success(`已删除文件「${name}」`)}
          />
        ) : null
      }
    >
      <div className="relative z-10 px-12 pt-6 max-md:px-4 max-md:pt-3">
        {!c.isMobile && c.immersiveUrl && (
          <div className="mb-6 flex items-start gap-6">
            <ArtworkImage
              src={c.immersiveUrl}
              alt=""
              decoding="async"
              className="aspect-[2/3] w-44 shrink-0 rounded-xl object-cover shadow-[0_18px_45px_rgba(0,0,0,0.55)] ring-1 ring-white/15"
            />
          </div>
        )}
        <div className="min-w-0 max-w-5xl pb-1">
          <h1 className="text-on-image break-words text-[42px] font-bold leading-[1.1] tracking-[-0.02em] text-white max-md:text-[28px]">
            {detail.title}
          </h1>
          {!c.isMovie && c.selectedSeriesEpisode && (
            <p className="text-on-image mt-2 text-body text-white/65 max-md:mt-1.5 max-md:text-ui">
              {`第 ${c.selectedSeriesEpisode.seasonNumber} 季 第 ${c.selectedSeriesEpisode.episode.episode_number} 集${
                c.selectedSeriesEpisode.episode.name
                  ? ` - ${c.selectedSeriesEpisode.episode.name}`
                  : ""
              }`}
            </p>
          )}
          {(c.itemFacts.length > 0 ||
            c.itemResolutions.length > 0 ||
            c.itemHdrFormats.length > 0) && (
            <div className="tnum mt-3.5 flex flex-wrap items-center gap-x-3 gap-y-2 text-ui text-white/80 max-md:mt-2 max-md:gap-x-2 max-md:text-sub">
              {c.itemFacts.map((fact, index) => (
                <span key={fact} className="flex items-center gap-3 max-md:gap-2">
                  {index > 0 && <span aria-hidden="true">·</span>}
                  <span>{fact}</span>
                </span>
              ))}
              {c.itemResolutions.length > 0 && (
                <span className="flex items-center gap-3 max-md:gap-2">
                  {c.itemFacts.length > 0 && <span aria-hidden="true">·</span>}
                  <span>{c.itemResolutions.join(" / ")}</span>
                </span>
              )}
              {c.itemHdrFormats.length > 0 && (
                <span className="flex items-center gap-3 max-md:gap-2">
                  {(c.itemFacts.length > 0 || c.itemResolutions.length > 0) && (
                    <span aria-hidden="true">·</span>
                  )}
                  <span>{c.itemHdrFormats.join(" / ")}</span>
                </span>
              )}
            </div>
          )}
          <BrandStrip brands={detail.brands} />
          <MediaTrackRows
            files={c.trackFiles}
            selectedFileId={c.selectedTrackFile?.file_id ?? null}
            onSelectedFileIdChange={c.setSelectedTrackFileId}
            onChanged={c.reload}
          />

          {c.availableTrackFiles.length > 0 && (c.isMovie || c.selectedSeriesEpisode) && (
            <PlayAction
              watched={c.watched}
              favorite={c.favorite}
              favoriteLabel={c.isMovie ? "这部电影" : "这部剧"}
              marking={c.marking}
              onToggleFavorite={c.toggleFavorite}
              onTogglePlayed={c.togglePlayed}
              extraAction={<CollectionsAddButton mediaItemId={detail.media_item_id} />}
              onPlay={() => {
                rememberPlayerReturnPath(window.location.pathname + window.location.search);
                c.navigate(
                  playHref(detail.media_item_id, {
                    season:
                      !c.isMovie && c.selectedSeriesEpisode
                        ? c.selectedSeriesEpisode.seasonNumber
                        : undefined,
                    episode:
                      !c.isMovie && c.selectedSeriesEpisode
                        ? c.selectedSeriesEpisode.episode.episode_number
                        : undefined,
                    fileId: playLinkFileId(c.explicitTrackFileId),
                  }),
                );
              }}
            />
          )}

          {c.refreshError && (
            <div className="mt-4 max-w-2xl rounded-xl border border-white/[0.1] bg-[rgba(14,16,22,0.6)] px-4 py-3 text-sub leading-6 text-[#ff9f9f] backdrop-blur-md">
              {c.refreshError}
            </div>
          )}
        </div>
      </div>

      {c.itemPlot && (
        <div className="mt-4 px-12 max-md:px-4">
          <ExpandablePlot text={c.itemPlot} />
        </div>
      )}

      {!c.isMovie && ((detail.season_summaries?.length ?? 0) > 0 || detail.seasons.length > 0) && (
        <div className="mt-6 px-12 max-md:px-4">
          <SeasonEpisodesSection
            libraryId={libraryId}
            detail={detail}
            initialSeason={initialSeason}
            initialEpisode={initialEpisode}
            onEpisodeChange={c.handleEpisodeChange}
            refreshKey={c.episodesVersion}
          />
        </div>
      )}

      {detail.files.length > 0 && (
        <ItemChaptersSection
          isTv={detail.kind === "tv"}
          canManageLibraries={c.canManageLibraries}
          chapterTarget={c.chapterTarget}
          chapters={c.chapters}
          frameTask={c.frameTask}
          chapterError={c.chapterError}
          chaptersBusy={c.chaptersBusy}
          probing={c.probing}
          resumeMs={c.watched?.position_ms ?? null}
          onRefreshChapters={() => void c.forceRefreshChaptersAndMarkers("reuse_valid")}
          onRegenerateChapters={() => void c.regenerateChapters()}
          onProbeMedia={() => void c.handleProbeMedia()}
          onPlayChapter={(chapter) => {
            rememberPlayerReturnPath(window.location.pathname + window.location.search);
            c.navigate(
              playHref(detail.media_item_id, {
                season:
                  !c.isMovie && c.selectedSeriesEpisode
                    ? c.selectedSeriesEpisode.seasonNumber
                    : undefined,
                episode:
                  !c.isMovie && c.selectedSeriesEpisode
                    ? c.selectedSeriesEpisode.episode.episode_number
                    : undefined,
                fileId: c.selectedTrackFile?.file_id,
                tSeconds: Math.floor(chapter.start_ms / 1000),
              }),
            );
          }}
        />
      )}

      {detail.cast.length > 0 && (
        <div className="mt-6 px-12 max-md:px-4">
          <CastRow
            cast={detail.cast.map<CastRowPerson>((member) => ({
              name: member.name,
              role: member.role,
              avatarUrl: member.avatar_url ? imageUrl(member.avatar_url) : null,
              tmdbPersonId: member.tmdb_person_id,
            }))}
            personHrefPrefix="/discover/people"
          />
        </div>
      )}

      {files.length > 0 && (
        <div className="mt-6 px-12 max-md:px-4">
          <FileSection
            files={files}
            title="文件"
            onDeleteFile={
              c.canManageLibraries
                ? (file) => {
                    if (detail.files.length <= 1) c.setDeleteOpen(true);
                    else c.setDeleteFileTarget(file);
                  }
                : undefined
            }
          />
        </div>
      )}

      {c.similarPending && (
        <p role="status" className="mt-8 px-12 text-sm text-[var(--text-muted)] max-md:px-4">
          正在加载相似推荐…
        </p>
      )}
      {c.similarError && (
        <p role="alert" className="mt-8 px-12 text-sm text-[var(--text-muted)] max-md:px-4">
          相似推荐暂时不可用：{c.similarError}
        </p>
      )}
      {c.similar.length > 0 && (
        <div className="mt-8">
          <MediaRow
            row={{ id: "similar", title: "相似推荐", items: c.similar }}
            insetClassName="px-12 max-md:px-4"
          />
        </div>
      )}

      {detail.tmdb_id != null && (
        <div className="mt-6 px-12 max-md:px-4">
          <nav
            aria-label="外部词条"
            className="flex flex-wrap items-center gap-x-4 gap-y-2 border-t border-white/[0.04] pt-4 text-caption"
          >
            <span className="text-[var(--text-faint)]">相关链接</span>
            <SourceLink
              href={`https://www.themoviedb.org/${detail.kind}/${detail.tmdb_id}`}
              label="TMDB"
            />
          </nav>
        </div>
      )}
    </MediaDetailScaffold>
  );
}
