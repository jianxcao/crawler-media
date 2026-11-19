"use client";

import { Link, useNavigate } from "react-router-dom";

import { BrandLoader } from "@/components/brand-loader";
import { useConfirm } from "@/components/feedback";
import { LibraryCoverModal } from "@/components/library-cover-modal";
import { FilterEmptyState, LibraryFilterBar, WallSortControl } from "@/components/library-filter-bar";
import { LibraryFormDialog } from "@/components/library-form-dialog";
import { LIBRARY_KIND_META } from "@/components/library-kind-meta";
import { LibraryMediaInfoDialog } from "@/components/library-media-info-dialog";
import { LibraryRemovalNotice } from "@/components/library-removal-notice";
import { PageNav } from "@/components/page-nav";
import { PosterWall } from "@/components/poster-wall";
import {
  GALLERY_LOAD_MARGIN,
  VideoGalleryLightbox,
  VideoGalleryWall,
} from "@/components/video-gallery";
import { WallLoadMore, WallRecallPill } from "@/components/wall-chrome";
import {
  deleteMissingRows,
  startLibraryMetadataRefresh,
  startLibraryScan,
  verifyLibraryFiles,
} from "@/lib/api/libraries";
import { refreshLibraryConfirm, scanLibraryConfirm } from "@/lib/library-confirm";
import { sortOptions } from "@/lib/library-detail-helpers";
import { useLibraryRemoval } from "@/lib/use-library-removal";
import { SORT_DIRECTIONS } from "@/lib/wall-sort";

import { IssueDrawer } from "./library-detail/issue-drawer";
import { LibraryActionsMenu } from "./library-detail/library-actions-menu";
import { LibraryDetailHeader } from "./library-detail/library-detail-header";
import { useLibraryWallController } from "./library-detail/use-library-wall-controller";
import { WallIndexBar } from "./library-detail/wall-index-bar";

export function LibraryDetailView({ libraryId }: { libraryId: string }) {
  const confirm = useConfirm();
  const navigate = useNavigate();
  const c = useLibraryWallController(libraryId);
  const { libraries, toast } = c;

  const library = libraries?.find((l) => l.id === libraryId) ?? null;
  const removal = useLibraryRemoval(libraries, (removed) => {
    toast.success(`已删除媒体库「${removed.name}」`);
    navigate("/library");
  }, libraryId);
  const removeLibrary = async () => {
    if (!library || removal.states[libraryId]?.pending) return;
    const ok = await confirm({
      title: `删除媒体库「${library.name}」？`,
      description:
        "将停止该库相关任务，清除扫描记录与探测缓存。磁盘文件、NFO 和图片不会删除。若作品在其他媒体库中没有文件，也未被合集或订阅保留，其观看进度、已看状态和收藏将一并清除，重新扫描不会恢复这些记录。",
      confirmLabel: "删除媒体库",
      tone: "danger",
    });
    if (ok) removal.remove(library);
  };

  const navFallback = { label: "媒体库", href: "/library" };

  if (c.failed && libraries === null) {
    return (
      <div className="flex-1">
        <PageNav title="媒体库" fallback={navFallback} />
        <CenteredNote>
          <p className="text-ui text-[var(--text-muted)]">媒体库加载失败</p>
          <button
            type="button"
            onClick={c.reload}
            className="btn-glass h-8 px-4 text-sub font-medium"
          >
            重试
          </button>
        </CenteredNote>
      </div>
    );
  }

  if (libraries === null) {
    return (
      <div className="flex-1">
        <PageNav title="媒体库" fallback={navFallback} />
        <CenteredNote>
          <BrandLoader className="size-6" />
          <p className="text-sub text-[var(--text-muted)]">正在加载媒体库…</p>
        </CenteredNote>
      </div>
    );
  }

  if (library === null) {
    return (
      <div className="flex-1">
        <PageNav title="媒体库" fallback={navFallback} />
        <CenteredNote>
          <p className="text-ui text-[var(--text-muted)]">这个媒体库不存在（可能已被删除）</p>
          <Link to="/library" replace className="btn-glass h-8 px-4 text-sub font-medium">
            返回媒体库
          </Link>
        </CenteredNote>
      </div>
    );
  }

  const meta = LIBRARY_KIND_META[library.kind];
  const galleryAvailable = true;
  const hasMenuItems = c.canManageLibraries || c.gallery || galleryAvailable;
  const removalActive = Boolean(
    removal.states[libraryId]?.pending ||
      removal.states[libraryId]?.operation ||
      library.removal,
  );

  const handleToggleScan = () => {
    c.setNotice(null);
    void confirm(scanLibraryConfirm(library.name)).then((ok) => {
      if (!ok) return;
      void startLibraryScan(library.id)
        .then(() => c.reload())
        .catch((e) => c.setNotice((e as Error).message));
    });
  };

  const handleToggleMetaRefresh = () => {
    c.setNotice(null);
    void confirm(refreshLibraryConfirm(library.name)).then((ok) => {
      if (!ok) return;
      startLibraryMetadataRefresh(libraryId)
        .then(() => c.reload())
        .catch((e) => c.setNotice((e as Error).message));
    });
  };

  const handleClearMissingFiles = () => {
    void confirm({
      title: `清理 ${c.missingFileCount} 条「文件缺失」记录？`,
      description: "磁盘上已不存在的文件，其台账记录会被删除；文件还在的条目不受影响。",
      tone: "danger",
      confirmLabel: "清理缺失记录",
    }).then((ok) => {
      if (!ok) return;
      void verifyLibraryFiles(library.id)
        .then(() => deleteMissingRows(library.id))
        .then(() => c.reload())
        .catch((e) => toast.error(e instanceof Error ? e.message : "清理缺失记录失败"));
    });
  };

  const actionsMenu =
    hasMenuItems && !removalActive ? (
      <LibraryActionsMenu
        library={library}
        canManage={c.canManageLibraries}
        toast={toast}
        reload={c.reload}
        onToggleScan={handleToggleScan}
        pendingCount={c.pendingCount}
        onOpenPending={() => c.setIssueTab(c.pendingTab)}
        onToggleMetaRefresh={handleToggleMetaRefresh}
        onMediaInfo={() => c.setMediaInfoOpen(true)}
        onEdit={() => c.setEditing(library)}
        onDelete={() => void removeLibrary()}
        onCoverModal={() => c.setCoverModalOpen(true)}
        density={c.gallery ? c.wallDensity : undefined}
        onDensityChange={c.gallery ? c.setWallDensity : undefined}
        galleryMode={c.gallery}
        onGalleryModeChange={(next: boolean) => {
          c.setLightboxIndex(null);
          c.setGalleryMode(next);
        }}
        grouped={c.gallery ? c.galleryGrouped : undefined}
        onGroupedChange={c.gallery ? c.setGalleryGrouped : undefined}
      />
    ) : undefined;

  const sortControl = (
    <WallSortControl
      value={c.wallSortPref}
      options={sortOptions(c.timeline ? "按时间" : "按标题", c.timeline)}
      disabled={c.probing}
      onChange={c.setWallSortPref}
      direction={{
        ascending: c.sortAscending,
        label: SORT_DIRECTIONS[c.effectiveSort][c.sortAscending ? "asc" : "desc"],
        onToggle: c.toggleWallSortReversed,
      }}
    />
  );

  return (
    <div ref={c.scrollRef} className="scroll-thin scroll-safe flex-1 overflow-y-auto pb-10">
      <PageNav title={library.name} fallback={navFallback} actions={actionsMenu} />

      <LibraryDetailHeader
        library={library}
        kindLabel={meta.label}
        canManageLibraries={c.canManageLibraries}
        removalActive={removalActive}
        removalNotice={
          <LibraryRemovalNotice
            library={library}
            state={removal.states[libraryId]}
            onRetry={(operation) =>
              operation ? removal.resume(library, operation) : removal.remove(library)
            }
          />
        }
        missing={c.missing}
        missingFileCount={c.missingFileCount}
        unidentified={c.unidentified}
        unidentifiedFiles={c.unidentifiedFiles}
        failed={c.failed}
        notice={c.notice}
        onOpenMissing={() => c.setIssueTab("missing")}
        onClearMissingFiles={handleClearMissingFiles}
        onOpenUnidentified={() => c.setIssueTab("unidentified")}
        onRescan={() => void startLibraryScan(library.id)}
        onRemoveLibrary={() => void removeLibrary()}
      />

      {c.canManageLibraries && (
        <IssueDrawer
          open={c.issueTab}
          onClose={() => c.setIssueTab(null)}
          onSwitchTab={c.setIssueTab}
          libraryId={libraryId}
          missing={c.missing}
          unidentified={c.unidentified}
          movie={library.kind === "movie"}
          onChanged={c.reload}
        />
      )}

      {c.items.length === 0 && c.filtering ? (
        c.failed ? null : (
          <FilterEmptyState
            libraryId={libraryId}
            filter={c.filter}
            onFilterChange={c.applyFilter}
          />
        )
      ) : c.items.length === 0 ? (
        c.failed ? null : (
          <p className="mt-16 text-center text-ui leading-7 text-[var(--text-muted)]">
            这个库还没有内容。
            <br />
            {c.canManageLibraries
              ? "点右上角菜单里的「扫描库」把已有影片识别入库；订阅内容下载完成后也会自动进来。"
              : "订阅内容下载并入库后会显示在这里。"}
          </p>
        )
      ) : (
        <>
          <div
            ref={c.filterBarTop}
            className="scroll-mt-[52px] max-md:scroll-mt-[calc(52px+var(--safe-top))]"
          >
            <LibraryFilterBar
              libraryId={libraryId}
              filter={c.filter}
              onFilterChange={c.applyFilter}
              sortControl={sortControl}
              className="mt-5 px-6 max-md:mt-4 max-md:px-4"
            />
          </div>

          <div ref={c.wallTop} className="mt-6 max-md:mt-4">
            <div className="flex items-start gap-2 px-6 max-md:gap-1 max-md:px-4">
              <div className="min-w-0 flex-1 [overflow-anchor:none]">
                {c.gallery ? (
                  <VideoGalleryWall
                    groups={c.galleryGroups}
                    density={c.wallDensity}
                    grouped={c.galleryGrouped}
                    onOpen={c.setLightboxIndex}
                  />
                ) : (
                  <div ref={c.wallGrid}>
                    <PosterWall
                      items={c.items}
                      libraryIdOf={c.ownLibraryId}
                      wide={c.wideWall}
                      workingLabelOf={c.workingLabelOf}
                      onGeometry={c.onWallGeometry}
                      showRating={c.showRating}
                    />
                  </div>
                )}
                <WallLoadMore
                  hasMore={c.gallery ? c.galleryHasMore : c.wallHasMore}
                  loaded={c.gallery ? c.galleryLoaded.current : c.items.length}
                  start={c.gallery ? c.galleryOrigin : c.wallOrigin}
                  total={library.stats.item_count}
                  onReach={c.gallery ? c.loadMoreGallery : c.loadMore}
                  rootMargin={c.gallery ? GALLERY_LOAD_MARGIN : undefined}
                />
              </div>
              {c.effectiveSort === "title" && !c.gallery && (
                <WallIndexBar
                  index={c.wallIndex}
                  active={c.activeWallInitial}
                  onJump={c.jumpTo}
                  reversed={!c.sortAscending}
                />
              )}
            </div>
          </div>

          {c.gallery && c.lightboxIndex !== null && c.galleryEntries[c.lightboxIndex] && (
            <VideoGalleryLightbox
              entries={c.galleryEntries}
              index={c.lightboxIndex}
              hasMore={c.galleryHasMore}
              onIndexChange={c.setLightboxIndex}
              onReachEnd={c.loadMoreGallery}
              onToggleFavorite={c.toggleGalleryFavorite}
              onClose={c.closeLightbox}
            />
          )}
        </>
      )}

      {c.recallable !== null && (
        <WallRecallPill
          onJump={() => {
            if (c.recallable === null) return;
            c.dismissRecall();
            if (c.gallery) c.jumpGalleryTo(c.recallable);
            else c.jumpTo(c.recallable);
          }}
          onDismiss={c.dismissRecall}
        />
      )}

      {c.coverModalOpen && (
        <LibraryCoverModal
          library={library}
          open={c.coverModalOpen}
          onOpenChange={c.setCoverModalOpen}
          onCoverUpdated={c.reload}
        />
      )}

      {c.canManageLibraries && (
        <LibraryFormDialog
          state={c.editing}
          onClose={() => c.setEditing(null)}
          onSaved={() => {
            c.setEditing(null);
            c.reload();
          }}
        />
      )}

      {c.canManageLibraries && c.mediaInfoOpen && (
        <LibraryMediaInfoDialog
          library={library}
          onClose={() => c.setMediaInfoOpen(false)}
          onSubmitted={(receipt) => {
            toast.success(
              `已提交 ${receipt.queued} 个文件的媒体信息抓取，跳过 ${receipt.skipped} 个`,
            );
            c.reload();
          }}
        />
      )}
    </div>
  );
}

function CenteredNote({ children }: { children: React.ReactNode }) {
  return (
    <div className="mt-16 flex flex-col items-center gap-3 text-center">{children}</div>
  );
}
