"use client";

import { useEffect, useState } from "react";
import { ContentEmptyState } from "@/components/content-empty-state";
import { FolderSyncIcon, RefreshIcon } from "@/components/icons";
import { TransferSummaryCards } from "@/components/transfers/transfer-summary-cards";
import { TransferToolbar } from "@/components/transfers/transfer-toolbar";
import { TransferFileCard } from "@/components/transfers/transfer-file-card";
import { TransferGroupCard } from "@/components/transfers/transfer-group-card";
import { VirtualTransferList } from "@/components/transfers/virtual-transfer-list";
import { TransferDeleteModal } from "@/components/transfers/transfer-delete-modal";
import { useTransferController } from "@/components/transfers/use-transfer-controller";

export function MediaTransferView() {
  const ctl = useTransferController();
  const [scrollElement, setScrollElement] = useState<HTMLDivElement | null>(null);
  const activeListCount =
    ctl.viewMode === "files" ? ctl.fileItems.length : ctl.groupItems.length;

  useEffect(() => {
    scrollElement?.scrollTo?.({ top: 0 });
  }, [scrollElement, ctl.viewMode, ctl.modeFilter, ctl.kindFilter]);

  return (
    <div
      ref={setScrollElement}
      data-scroll-root
      className="scroll-thin scroll-safe h-full flex-1 overflow-y-auto pb-10"
    >
      <div className="mx-auto max-w-6xl px-4 py-8 max-md:px-3.5 max-md:py-5">
      <div className="flex flex-col gap-4 border-b border-white/[0.08] pb-6 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <div className="flex items-center gap-2.5">
            <div className="flex size-9 items-center justify-center rounded-xl bg-white/[0.06] text-[var(--accent-2)] ring-1 ring-white/[0.1]">
              <FolderSyncIcon className="size-5" />
            </div>
            <h1 className="text-xl font-bold tracking-tight text-[var(--text)] sm:text-2xl">
              媒体整理记录
            </h1>
          </div>
          <p className="mt-1.5 text-xs leading-relaxed text-[var(--text-muted)] sm:text-sm">
            查看所有整理入库的影视资产、STRM 云端直链、硬链接零损流转及目录拓扑映射。
          </p>
        </div>

        <div className="flex items-center gap-2.5 self-start sm:self-center">
          <button
            type="button"
            onClick={() => void ctl.refresh()}
            disabled={ctl.loading}
            className="flex items-center gap-1.5 rounded-xl border border-white/[0.08] bg-white/[0.04] px-3.5 py-2 text-xs font-medium text-[var(--text)] transition hover:border-white/[0.16] hover:bg-white/[0.08] disabled:opacity-50 sm:text-sm"
          >
            <RefreshIcon className={`size-3.5 ${ctl.loading ? "animate-spin text-sky-400" : ""}`} />
            <span>{ctl.loading ? "正在同步..." : "刷新记录"}</span>
          </button>
        </div>
      </div>

      <TransferSummaryCards summary={ctl.summary} />

      <TransferToolbar
        viewMode={ctl.viewMode}
        searchTerm={ctl.searchTerm}
        modeFilter={ctl.modeFilter}
        kindFilter={ctl.kindFilter}
        filteredTotal={ctl.filteredTotal}
        filteredFileTotal={ctl.filteredFileTotal}
        onViewModeChange={ctl.changeViewMode}
        onSearchTermChange={ctl.setSearchTerm}
        onClearSearch={ctl.clearSearch}
        onModeFilterChange={ctl.changeModeFilter}
        onKindFilterChange={ctl.changeKindFilter}
        onResetFilters={ctl.resetFilters}
      />

      {ctl.error && (
        <div className="mt-4 rounded-xl border border-red-500/25 bg-red-500/[0.08] p-4 text-xs text-red-400 sm:text-sm">
          {ctl.error}
        </div>
      )}

      {activeListCount === 0 && !ctl.loading ? (
        <div className="mt-6">
          <ContentEmptyState
            variant="library"
            title={ctl.summary.total_files === 0 ? "暂无整理入库记录" : "未找到匹配的入库记录"}
            description={
              ctl.summary.total_files === 0
                ? "当下载任务完成、或通过收件箱/自动归档完成影视入库后，所有的流转记录与路径映射会在此呈现。"
                : "请尝试调整搜索关键词或流转模式、类型过滤条件。"
            }
          />
        </div>
      ) : ctl.viewMode === "files" ? (
        <VirtualTransferList
          items={ctl.fileItems}
          getKey={(item) => item.id}
          defaultItemHeight={148}
          scrollElement={scrollElement}
          overscan={8}
          hasMore={ctl.hasMore}
          loadingMore={ctl.loadingMore}
          onLoadMore={() => void ctl.loadMore()}
          renderItem={(item) => (
            <TransferFileCard
              item={item}
              isRetransferring={ctl.retransferringId === item.id}
              isRefreshing={ctl.refreshingId === item.id}
              deleteDisabled={ctl.isPending}
              onRetransfer={(target) => void ctl.retransfer(target)}
              onRefreshMetadata={(target) =>
                void ctl.triggerMetadataRefresh(
                  target.id,
                  target.library_id,
                  target.media_id,
                  target.media_title,
                )
              }
              onDelete={ctl.openDeleteDialog}
            />
          )}
        />
      ) : (
        <VirtualTransferList
          items={ctl.groupItems}
          getKey={(group) => group.media_id}
          defaultItemHeight={88}
          scrollElement={scrollElement}
          overscan={8}
          hasMore={ctl.hasMore}
          loadingMore={ctl.loadingMore}
          onLoadMore={() => void ctl.loadMore()}
          renderItem={(group) => (
            <TransferGroupCard
              group={group}
              expandedState={ctl.expandedGroups[group.media_id]}
              isRefreshing={ctl.refreshingId === group.media_id}
              retransferringId={ctl.retransferringId}
              deleteDisabled={ctl.isPending}
              onToggleExpand={(target) => void ctl.toggleGroupExpand(target)}
              onLoadMoreGroupFiles={(target) => void ctl.loadMoreGroupFiles(target)}
              onRefreshGroupMetadata={(target) =>
                void ctl.triggerMetadataRefresh(
                  target.media_id,
                  target.library_id,
                  target.media_id,
                  target.media_title,
                )
              }
              onRetransferFile={(file) => void ctl.retransfer(file)}
              onDeleteFile={ctl.openDeleteDialog}
            />
          )}
        />
      )}

      <TransferDeleteModal
        item={ctl.selectedItem}
        deleteMode={ctl.deleteMode}
        isPending={ctl.isPending}
        onChangeMode={ctl.setDeleteMode}
        onClose={ctl.closeDeleteDialog}
        onConfirm={ctl.confirmDelete}
      />
      </div>
    </div>
  );
}
