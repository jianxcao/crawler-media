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
  onItemRetransferred: (target: TransferFileItem, destination?: string) => void,
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
        const res = await retransferLedgerRow(item.id);
        onItemRetransferred(item, res?.destination);
        await onAfterMutate();
        toast.success(`《${item.media_title || "媒体文件"}》硬链接已重新生成成功`);
      } catch (e) {
        toast.error("重新生成失败: " + (e as Error).message);
      } finally {
        setRetransferringId(null);
      }
    },
    [onItemRetransferred, onAfterMutate, toast],
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
  const [loadMoreError, setLoadMoreError] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadMoreErrorRef = useRef(false);
  const fileItemsRef = useRef<TransferFileItem[]>([]);
  fileItemsRef.current = fileItems;
  const groupItemsRef = useRef<TransferGroupItem[]>([]);
  groupItemsRef.current = groupItems;
  const headAbortRef = useRef<AbortController | null>(null);
  const appendAbortRef = useRef<AbortController | null>(null);
  const summaryAbortRef = useRef<AbortController | null>(null);
  const toast = useToast();
  const expansion = useGroupExpansion(modeFilter, kindFilter, debouncedQuery);

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
      summaryAbortRef.current?.abort();
      const controller = new AbortController();
      headAbortRef.current = controller;
      loadMoreErrorRef.current = false;
      setLoadMoreError(false);

      try {
        if (!options.silent) setLoading(true);
        setLoadingMore(false);
        const page = await listTransfers(
          { view: viewMode, q: debouncedQuery, mode: modeFilter, kind: kindFilter, offset: 0, limit: PAGE_SIZE },
          { signal: controller.signal },
        );
        if (controller.signal.aborted) return;

        const returnedCount = viewMode === "files" ? page.items.length : page.groups.length;
        setSummary(page.summary);
        setFilteredTotal(page.filtered_total);
        setFilteredFileTotal(page.filtered_file_total);
        setHasMore(returnedCount > 0 && page.has_more);
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
      summaryAbortRef.current?.abort();
    };
  }, [fetchHeadPage]);

  const loadMoreInternal = useCallback(
    async (force = false) => {
      if (loading || loadingMore || !hasMore || (!force && loadMoreErrorRef.current)) return;
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

        loadMoreErrorRef.current = false;
        setLoadMoreError(false);
        setSummary(page.summary);
        setFilteredTotal(page.filtered_total);
        setFilteredFileTotal(page.filtered_file_total);
        const returnedCount = viewMode === "files" ? page.items.length : page.groups.length;
        setHasMore(returnedCount > 0 && page.has_more);
        if (viewMode === "files") {
          if (page.items.length > 0) {
            setFileItems((prev) => {
              const seen = new Set(prev.map((item) => item.id));
              return [...prev, ...page.items.filter((item) => !seen.has(item.id))];
            });
            setNextOffset(page.offset + page.items.length);
          }
        } else if (page.groups.length > 0) {
          setGroupItems((prev) => {
            const seen = new Set(prev.map((item) => item.media_id));
            return [...prev, ...page.groups.filter((item) => !seen.has(item.media_id))];
          });
          setNextOffset(page.offset + page.groups.length);
        }
      } catch (e) {
        if (!isAbortError(e) && !controller.signal.aborted) {
          loadMoreErrorRef.current = true;
          setLoadMoreError(true);
          toast.error("加载下一页失败: " + (e as Error).message);
        }
      } finally {
        if (appendAbortRef.current === controller) setLoadingMore(false);
      }
    },
    [loading, loadingMore, hasMore, viewMode, debouncedQuery, modeFilter, kindFilter, nextOffset, toast],
  );

  const loadMore = useCallback(() => loadMoreInternal(false), [loadMoreInternal]);

  const retryLoadMore = useCallback(() => {
    loadMoreErrorRef.current = false;
    setLoadMoreError(false);
    return loadMoreInternal(true);
  }, [loadMoreInternal]);

  const handleItemDeleted = useCallback(
    (target: TransferFileItem) => {
      const hadFile = fileItemsRef.current.some((item) => item.id === target.id);
      setFileItems((prev) => prev.filter((item) => item.id !== target.id));
      expansion.removeExpandedFile(target.media_id, target.id);
      const isStrm = target.is_strm || target.transfer_mode === "strm";
      const targetGroup = groupItemsRef.current.find((g) => g.media_id === target.media_id);
      const groupWillRemove = Boolean(targetGroup && targetGroup.file_count <= 1);
      setGroupItems((prev) =>
        prev
          .map((g) =>
            g.media_id === target.media_id
              ? {
                  ...g,
                  file_count: Math.max(0, g.file_count - 1),
                  strm_count: isStrm ? Math.max(0, g.strm_count - 1) : g.strm_count,
                  hardlink_count:
                    target.transfer_mode === "hardlink" ? Math.max(0, g.hardlink_count - 1) : g.hardlink_count,
                  local_size_bytes: !isStrm
                    ? Math.max(0, g.local_size_bytes - (target.file_size ?? 0))
                    : g.local_size_bytes,
                  total_duration_ms: Math.max(0, g.total_duration_ms - (target.duration_ms ?? 0)),
                }
              : g,
          )
          .filter((g) => g.file_count > 0),
      );
      if (viewMode === "files" && hadFile) {
        setNextOffset((prev) => Math.max(0, prev - 1));
      } else if (viewMode === "groups" && groupWillRemove) {
        setNextOffset((prev) => Math.max(0, prev - 1));
      }
      setFilteredFileTotal((prev) => Math.max(0, prev - 1));
      if (viewMode === "files" || groupWillRemove) {
        setFilteredTotal((prev) => Math.max(0, prev - 1));
      }
    },
    [expansion, viewMode],
  );

  const handleItemRetransferred = useCallback((target: TransferFileItem, destination?: string) => {
    if (!destination || destination === target.path) return;
    setFileItems((prev) =>
      prev.map((item) => (item.id === target.id ? { ...item, path: destination } : item)),
    );
  }, []);

  const refreshSummaryOnly = useCallback(async () => {
    summaryAbortRef.current?.abort();
    const controller = new AbortController();
    summaryAbortRef.current = controller;
    try {
      const page = await listTransfers(
        { view: viewMode, q: debouncedQuery, mode: modeFilter, kind: kindFilter, offset: 0, limit: 1 },
        { signal: controller.signal },
      );
      if (controller.signal.aborted) return;
      setSummary(page.summary);
      setFilteredTotal(page.filtered_total);
      setFilteredFileTotal(page.filtered_file_total);
      const loadedCount =
        viewMode === "files" ? fileItemsRef.current.length : groupItemsRef.current.length;
      setHasMore((prev) => prev && loadedCount < page.filtered_total);
    } catch {
      // 摘要后台刷新失败时保留本地乐观状态
    }
  }, [viewMode, debouncedQuery, modeFilter, kindFilter]);

  const actions = useTransferActions(handleItemDeleted, handleItemRetransferred, refreshSummaryOnly);

  return {
    viewMode,
    searchTerm,
    debouncedQuery,
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
    loadMoreError,
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
    retryLoadMore,
    toggleGroupExpand: expansion.toggleGroupExpand,
    loadMoreGroupFiles: expansion.loadMoreGroupFiles,
  };
}
