import { useCallback, useEffect, useRef, useState, useTransition } from "react";
import {
  deleteLedgerRow,
  listTransfers,
  refreshItemMetadata,
  retransferLedgerRow,
  type TransferFileItem,
  type TransferFilterMode,
  type TransferGroupItem,
  type TransferKindFilter,
  type TransferSummary,
  type TransferViewMode,
} from "@/lib/api/transfers";
import { useToast } from "@/components/feedback";
import { EMPTY_TRANSFER_SUMMARY } from "./transfer-helpers";
import { useGroupExpansion } from "./use-group-expansion";

const PAGE_SIZE = 50;
const SEARCH_DEBOUNCE_MS = 200;

function isAbortError(error: unknown): boolean {
  if (!error || typeof error !== "object") return false;
  const name = "name" in error ? String((error as { name?: unknown }).name) : "";
  return name === "AbortError";
}

function useTransferActions(
  onItemDeleted: (target: TransferFileItem) => void,
  onAfterMutate: () => Promise<void>,
) {
  const [retransferringId, setRetransferringId] = useState<string | null>(null);
  const [refreshingId, setRefreshingId] = useState<string | null>(null);
  const [selectedItem, setSelectedItem] = useState<TransferFileItem | null>(null);
  const [deleteMode, setDeleteMode] = useState<"record_only" | "with_file">("record_only");
  const [isPending, startTransition] = useTransition();
  const toast = useToast();

  const retransfer = useCallback(
    async (item: TransferFileItem) => {
      if (item.is_strm || item.transfer_mode === "strm") return;
      if (!item.source_path) {
        toast.error("该文件未记录下载源路径，无法从源文件直接重构硬链接");
        return;
      }
      try {
        setRetransferringId(item.id);
        await retransferLedgerRow(item.id);
        await onAfterMutate();
        toast.success(`《${item.media_title || "媒体文件"}》硬链接已重新生成成功`);
      } catch (e) {
        toast.error("重新生成失败: " + (e as Error).message);
      } finally {
        setRetransferringId(null);
      }
    },
    [onAfterMutate, toast],
  );

  const triggerMetadataRefresh = useCallback(
    async (targetId: string, libraryId: string | null, mediaId: string, title: string) => {
      if (!libraryId) {
        toast.error("无法确定该作品所属的媒体库");
        return;
      }
      try {
        setRefreshingId(targetId);
        await refreshItemMetadata(libraryId, mediaId);
        toast.success(`已为《${title || "作品"}》重新触发元数据刮削`);
      } catch (e) {
        toast.error("元数据刷新失败: " + (e as Error).message);
      } finally {
        setRefreshingId(null);
      }
    },
    [toast],
  );

  const confirmDelete = useCallback(() => {
    if (!selectedItem) return;
    const target = selectedItem;
    const deleteFile = deleteMode === "with_file";
    startTransition(async () => {
      try {
        await deleteLedgerRow(target.id, deleteFile);
        setSelectedItem(null);
        setDeleteMode("record_only");
        onItemDeleted(target);
        void onAfterMutate();
        const title = target.media_title || "记录";
        toast.success(
          deleteFile ? `已删除《${title}》的入库记录及目标文件` : `已移除《${title}》的入库记录`,
        );
      } catch (e) {
        toast.error("删除失败: " + (e as Error).message);
      }
    });
  }, [selectedItem, deleteMode, onItemDeleted, onAfterMutate, toast]);

  return {
    retransferringId,
    refreshingId,
    selectedItem,
    deleteMode,
    isPending,
    setDeleteMode,
    openDeleteDialog: (item: TransferFileItem) => {
      setDeleteMode("record_only");
      setSelectedItem(item);
    },
    closeDeleteDialog: () => {
      setSelectedItem(null);
      setDeleteMode("record_only");
    },
    retransfer,
    triggerMetadataRefresh,
    confirmDelete,
  };
}

export function useTransferController() {
  const [viewMode, setViewMode] = useState<TransferViewMode>("files");
  const [searchTerm, setSearchTerm] = useState("");
  const [debouncedQuery, setDebouncedQuery] = useState("");
  const [modeFilter, setModeFilter] = useState<TransferFilterMode>("all");
  const [kindFilter, setKindFilter] = useState<TransferKindFilter>("all");

  const [summary, setSummary] = useState<TransferSummary>(EMPTY_TRANSFER_SUMMARY);
  const [fileItems, setFileItems] = useState<TransferFileItem[]>([]);
  const [groupItems, setGroupItems] = useState<TransferGroupItem[]>([]);
  const [filteredTotal, setFilteredTotal] = useState(0);
  const [filteredFileTotal, setFilteredFileTotal] = useState(0);
  const [nextOffset, setNextOffset] = useState(0);
  const [hasMore, setHasMore] = useState(false);
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const headAbortRef = useRef<AbortController | null>(null);
  const appendAbortRef = useRef<AbortController | null>(null);
  const toast = useToast();
  const expansion = useGroupExpansion(modeFilter, debouncedQuery);

  useEffect(() => {
    const trimmed = searchTerm.trim();
    if (!trimmed) {
      setDebouncedQuery("");
      return undefined;
    }
    const timer = setTimeout(() => setDebouncedQuery(trimmed), SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [searchTerm]);

  const fetchHeadPage = useCallback(
    async (options: { silent?: boolean } = {}) => {
      headAbortRef.current?.abort();
      appendAbortRef.current?.abort();
      const controller = new AbortController();
      headAbortRef.current = controller;

      try {
        if (!options.silent) setLoading(true);
        setLoadingMore(false);
        const page = await listTransfers(
          { view: viewMode, q: debouncedQuery, mode: modeFilter, kind: kindFilter, offset: 0, limit: PAGE_SIZE },
          { signal: controller.signal },
        );
        if (controller.signal.aborted) return;

        setSummary(page.summary);
        setFilteredTotal(page.filtered_total);
        setFilteredFileTotal(page.filtered_file_total);
        setHasMore(page.has_more);
        if (viewMode === "files") {
          setFileItems(page.items);
          setNextOffset(page.items.length);
        } else {
          setGroupItems(page.groups);
          setNextOffset(page.groups.length);
        }
        setError(null);
      } catch (e) {
        if (!isAbortError(e) && !controller.signal.aborted) {
          setError((e as Error).message || "加载整理记录失败");
        }
      } finally {
        if (headAbortRef.current === controller) setLoading(false);
      }
    },
    [viewMode, debouncedQuery, modeFilter, kindFilter],
  );

  useEffect(() => {
    void fetchHeadPage();
    return () => {
      headAbortRef.current?.abort();
      appendAbortRef.current?.abort();
    };
  }, [fetchHeadPage]);

  const loadMore = useCallback(async () => {
    if (loading || loadingMore || !hasMore) return;
    appendAbortRef.current?.abort();
    const controller = new AbortController();
    appendAbortRef.current = controller;
    setLoadingMore(true);

    try {
      const page = await listTransfers(
        { view: viewMode, q: debouncedQuery, mode: modeFilter, kind: kindFilter, offset: nextOffset, limit: PAGE_SIZE },
        { signal: controller.signal },
      );
      if (controller.signal.aborted) return;

      setSummary(page.summary);
      setFilteredTotal(page.filtered_total);
      setFilteredFileTotal(page.filtered_file_total);
      setHasMore(page.has_more);
      if (viewMode === "files") {
        setFileItems((prev) => {
          const seen = new Set(prev.map((item) => item.id));
          return [...prev, ...page.items.filter((item) => !seen.has(item.id))];
        });
        setNextOffset(page.offset + page.items.length);
      } else {
        setGroupItems((prev) => {
          const seen = new Set(prev.map((item) => item.media_id));
          return [...prev, ...page.groups.filter((item) => !seen.has(item.media_id))];
        });
        setNextOffset(page.offset + page.groups.length);
      }
    } catch (e) {
      if (!isAbortError(e) && !controller.signal.aborted) {
        toast.error("加载下一页失败: " + (e as Error).message);
      }
    } finally {
      if (appendAbortRef.current === controller) setLoadingMore(false);
    }
  }, [loading, loadingMore, hasMore, viewMode, debouncedQuery, modeFilter, kindFilter, nextOffset, toast]);

  const handleItemDeleted = useCallback(
    (target: TransferFileItem) => {
      setFileItems((prev) => prev.filter((item) => item.id !== target.id));
      expansion.removeExpandedFile(target.media_id, target.id);
      setGroupItems((prev) =>
        prev
          .map((g) => (g.media_id === target.media_id ? { ...g, file_count: Math.max(0, g.file_count - 1) } : g))
          .filter((g) => g.file_count > 0),
      );
    },
    [expansion],
  );

  const silentRefresh = useCallback(() => fetchHeadPage({ silent: true }), [fetchHeadPage]);
  const actions = useTransferActions(handleItemDeleted, silentRefresh);

  return {
    viewMode,
    searchTerm,
    modeFilter,
    kindFilter,
    summary,
    fileItems,
    groupItems,
    filteredTotal,
    filteredFileTotal,
    hasMore,
    loading,
    loadingMore,
    error,
    expandedGroups: expansion.expandedGroups,
    ...actions,
    setSearchTerm,
    changeViewMode: (next: TransferViewMode) => {
      expansion.resetExpanded();
      setViewMode(next);
    },
    clearSearch: () => {
      setSearchTerm("");
      setDebouncedQuery("");
    },
    changeModeFilter: (next: TransferFilterMode) => {
      expansion.resetExpanded();
      setModeFilter(next);
    },
    changeKindFilter: (next: TransferKindFilter) => {
      expansion.resetExpanded();
      setKindFilter(next);
    },
    resetFilters: () => {
      expansion.resetExpanded();
      setSearchTerm("");
      setDebouncedQuery("");
      setModeFilter("all");
      setKindFilter("all");
    },
    refresh: () => fetchHeadPage(),
    loadMore,
    toggleGroupExpand: expansion.toggleGroupExpand,
    loadMoreGroupFiles: expansion.loadMoreGroupFiles,
  };
}
