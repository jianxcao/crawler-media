import { useCallback, useEffect, useRef, useState } from "react";
import {
  listTransfers,
  type TransferFileItem,
  type TransferFilterMode,
  type TransferGroupItem,
  type TransferKindFilter,
  type TransferSummary,
  type TransferViewMode,
} from "@/lib/api/transfers";
import { useToast } from "@/components/feedback";
import { EMPTY_TRANSFER_SUMMARY, recomputeGroupPrimaryMode } from "./transfer-helpers";
import { useGroupExpansion } from "./use-group-expansion";
import { useTransferActions } from "./use-transfer-actions";

const PAGE_SIZE = 50;
const SEARCH_DEBOUNCE_MS = 200;

interface ModePaginationState {
  filteredTotal: number;
  filteredFileTotal: number;
  hasMore: boolean;
  nextOffset: number;
  loaded: boolean;
  filterKey: string;
}

const EMPTY_MODE_PAGINATION: ModePaginationState = {
  filteredTotal: 0,
  filteredFileTotal: 0,
  hasMore: false,
  nextOffset: 0,
  loaded: false,
  filterKey: "",
};

function buildFilterKey(
  debouncedQuery: string,
  modeFilter: TransferFilterMode,
  kindFilter: TransferKindFilter,
): string {
  return `${debouncedQuery}\0${modeFilter}\0${kindFilter}`;
}

function isAbortError(error: unknown): boolean {
  if (!error || typeof error !== "object") return false;
  const name = "name" in error ? String((error as { name?: unknown }).name) : "";
  return name === "AbortError";
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
  const [paginationByMode, setPaginationByMode] = useState<
    Record<TransferViewMode, ModePaginationState>
  >({
    files: EMPTY_MODE_PAGINATION,
    groups: EMPTY_MODE_PAGINATION,
  });
  const [loading, setLoading] = useState(true);
  const [loadingMore, setLoadingMore] = useState(false);
  const [loadMoreError, setLoadMoreError] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const currentFilterKey = buildFilterKey(debouncedQuery, modeFilter, kindFilter);
  const activePagination = paginationByMode[viewMode];
  const { filteredTotal, filteredFileTotal, hasMore, nextOffset } = activePagination;

  const paginationByModeRef = useRef(paginationByMode);
  paginationByModeRef.current = paginationByMode;
  const loadMoreErrorRef = useRef(false);
  const viewModeRef = useRef<TransferViewMode>(viewMode);
  viewModeRef.current = viewMode;
  const debouncedQueryRef = useRef(debouncedQuery);
  debouncedQueryRef.current = debouncedQuery;
  const fileItemsRef = useRef<TransferFileItem[]>([]);
  fileItemsRef.current = fileItems;
  const groupItemsRef = useRef<TransferGroupItem[]>([]);
  groupItemsRef.current = groupItems;
  const headAbortRef = useRef<AbortController | null>(null);
  const appendAbortRef = useRef<AbortController | null>(null);
  const summaryAbortRef = useRef<AbortController | null>(null);
  const toast = useToast();
  const toastRef = useRef(toast);
  toastRef.current = toast;
  const {
    expandedGroups,
    resetExpanded,
    toggleGroupExpand,
    loadMoreGroupFiles,
    removeExpandedFile,
    updateExpandedFilePath,
  } = useGroupExpansion(modeFilter, kindFilter, debouncedQuery);

  const clearInactiveList = useCallback((activeMode: TransferViewMode) => {
    const inactiveMode: TransferViewMode = activeMode === "files" ? "groups" : "files";
    if (activeMode === "files") {
      setGroupItems([]);
    } else {
      setFileItems([]);
    }
    setPaginationByMode((prev) => ({
      ...prev,
      [inactiveMode]: EMPTY_MODE_PAGINATION,
    }));
  }, []);

  useEffect(() => {
    const trimmed = searchTerm.trim();
    if (!trimmed) {
      if (debouncedQueryRef.current !== "") {
        debouncedQueryRef.current = "";
        resetExpanded();
        clearInactiveList(viewModeRef.current);
        setLoading(true);
        setDebouncedQuery("");
      }
      return undefined;
    }
    const timer = setTimeout(() => {
      if (debouncedQueryRef.current === trimmed) return;
      debouncedQueryRef.current = trimmed;
      resetExpanded();
      clearInactiveList(viewModeRef.current);
      setLoading(true);
      setDebouncedQuery(trimmed);
    }, SEARCH_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [searchTerm, clearInactiveList, resetExpanded]);

  const fetchHeadPage = useCallback(
    async (options: { silent?: boolean } = {}) => {
      const mode = viewMode;
      const requestFilterKey = buildFilterKey(debouncedQuery, modeFilter, kindFilter);
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
          { view: mode, q: debouncedQuery, mode: modeFilter, kind: kindFilter, offset: 0, limit: PAGE_SIZE },
          { signal: controller.signal },
        );
        if (controller.signal.aborted) return;

        const returnedCount = mode === "files" ? page.items.length : page.groups.length;
        setSummary(page.summary);
        if (mode === "files") {
          setFileItems(page.items);
        } else {
          setGroupItems(page.groups);
        }
        setPaginationByMode((prev) => ({
          ...prev,
          [mode]: {
            filteredTotal: page.filtered_total,
            filteredFileTotal: page.filtered_file_total,
            hasMore: returnedCount > 0 && page.has_more,
            nextOffset: returnedCount,
            loaded: true,
            filterKey: requestFilterKey,
          },
        }));
        setError(null);
      } catch (e) {
        if (!isAbortError(e) && !controller.signal.aborted) {
          if (paginationByModeRef.current[mode].filterKey !== requestFilterKey) {
            if (mode === "files") {
              setFileItems([]);
            } else {
              setGroupItems([]);
            }
            setPaginationByMode((prev) =>
              prev[mode].filterKey !== requestFilterKey
                ? { ...prev, [mode]: EMPTY_MODE_PAGINATION }
                : prev,
            );
          }
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
      const mode = viewMode;
      if (
        loading ||
        loadingMore ||
        !hasMore ||
        activePagination.filterKey !== currentFilterKey ||
        (!force && loadMoreErrorRef.current)
      ) {
        return;
      }
      const requestFilterKey = currentFilterKey;
      appendAbortRef.current?.abort();
      const controller = new AbortController();
      appendAbortRef.current = controller;
      setLoadingMore(true);

      try {
        const page = await listTransfers(
          { view: mode, q: debouncedQuery, mode: modeFilter, kind: kindFilter, offset: nextOffset, limit: PAGE_SIZE },
          { signal: controller.signal },
        );
        if (controller.signal.aborted) return;

        loadMoreErrorRef.current = false;
        setLoadMoreError(false);
        setSummary(page.summary);
        const returnedCount = mode === "files" ? page.items.length : page.groups.length;
        if (mode === "files" && page.items.length > 0) {
          setFileItems((prev) => {
            const seen = new Set(prev.map((item) => item.id));
            return [...prev, ...page.items.filter((item) => !seen.has(item.id))];
          });
        } else if (mode === "groups" && page.groups.length > 0) {
          setGroupItems((prev) => {
            const seen = new Set(prev.map((item) => item.media_id));
            return [...prev, ...page.groups.filter((item) => !seen.has(item.media_id))];
          });
        }
        setPaginationByMode((prev) => ({
          ...prev,
          [mode]: {
            filteredTotal: page.filtered_total,
            filteredFileTotal: page.filtered_file_total,
            hasMore: returnedCount > 0 && page.has_more,
            nextOffset: returnedCount > 0 ? page.offset + returnedCount : prev[mode].nextOffset,
            loaded: true,
            filterKey: requestFilterKey,
          },
        }));
      } catch (e) {
        if (!isAbortError(e) && !controller.signal.aborted) {
          loadMoreErrorRef.current = true;
          setLoadMoreError(true);
          toastRef.current.error("加载下一页失败: " + (e as Error).message);
        }
      } finally {
        if (appendAbortRef.current === controller) setLoadingMore(false);
      }
    },
    [
      loading,
      loadingMore,
      hasMore,
      activePagination.filterKey,
      currentFilterKey,
      viewMode,
      debouncedQuery,
      modeFilter,
      kindFilter,
      nextOffset,
    ],
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
      removeExpandedFile(target.media_id, target.id);
      const isStrm = target.is_strm || target.transfer_mode === "strm";
      const targetGroup = groupItemsRef.current.find((g) => g.media_id === target.media_id);
      const groupWillRemove = Boolean(targetGroup && targetGroup.file_count <= 1);
      setGroupItems((prev) =>
        prev
          .map((g) => {
            if (g.media_id !== target.media_id) return g;
            const nextFileCount = Math.max(0, g.file_count - 1);
            const nextStrm = isStrm ? Math.max(0, g.strm_count - 1) : g.strm_count;
            const nextHardlink =
              !isStrm && target.transfer_mode === "hardlink"
                ? Math.max(0, g.hardlink_count - 1)
                : g.hardlink_count;
            const nextCopy =
              !isStrm && target.transfer_mode === "copy"
                ? Math.max(0, (g.copy_count ?? 0) - 1)
                : (g.copy_count ?? 0);
            const nextMove =
              !isStrm && target.transfer_mode === "move"
                ? Math.max(0, (g.move_count ?? 0) - 1)
                : (g.move_count ?? 0);
            return {
              ...g,
              file_count: nextFileCount,
              strm_count: nextStrm,
              hardlink_count: nextHardlink,
              copy_count: nextCopy,
              move_count: nextMove,
              primary_mode:
                nextFileCount > 0
                  ? recomputeGroupPrimaryMode(
                      nextStrm,
                      nextHardlink,
                      nextMove,
                      nextCopy,
                      g.primary_mode,
                    )
                  : g.primary_mode,
              local_size_bytes: !isStrm
                ? Math.max(0, g.local_size_bytes - (target.file_size ?? 0))
                : g.local_size_bytes,
              total_duration_ms: Math.max(0, g.total_duration_ms - (target.duration_ms ?? 0)),
            };
          })
          .filter((g) => g.file_count > 0),
      );
      setPaginationByMode((prev) => ({
        files: prev.files.loaded
          ? {
              ...prev.files,
              filteredTotal: Math.max(0, prev.files.filteredTotal - 1),
              filteredFileTotal: Math.max(0, prev.files.filteredFileTotal - 1),
              nextOffset: hadFile ? Math.max(0, prev.files.nextOffset - 1) : prev.files.nextOffset,
            }
          : prev.files,
        groups: prev.groups.loaded
          ? {
              ...prev.groups,
              filteredTotal: groupWillRemove
                ? Math.max(0, prev.groups.filteredTotal - 1)
                : prev.groups.filteredTotal,
              filteredFileTotal: Math.max(0, prev.groups.filteredFileTotal - 1),
              nextOffset: groupWillRemove
                ? Math.max(0, prev.groups.nextOffset - 1)
                : prev.groups.nextOffset,
            }
          : prev.groups,
      }));
    },
    [removeExpandedFile],
  );

  const handleItemRetransferred = useCallback(
    (target: TransferFileItem, destination?: string) => {
      if (!destination || destination === target.path) return;
      setFileItems((prev) =>
        prev.map((item) => (item.id === target.id ? { ...item, path: destination } : item)),
      );
      updateExpandedFilePath(target.media_id, target.id, destination);
    },
    [updateExpandedFilePath],
  );

  const refreshSummaryOnly = useCallback(async () => {
    const mode = viewMode;
    const summaryFilterKey = buildFilterKey(debouncedQuery, modeFilter, kindFilter);
    summaryAbortRef.current?.abort();
    const controller = new AbortController();
    summaryAbortRef.current = controller;
    try {
      const page = await listTransfers(
        { view: mode, q: debouncedQuery, mode: modeFilter, kind: kindFilter, offset: 0, limit: 1 },
        { signal: controller.signal },
      );
      if (controller.signal.aborted) return;
      setSummary(page.summary);
      const loadedCount =
        mode === "files" ? fileItemsRef.current.length : groupItemsRef.current.length;
      setPaginationByMode((prev) =>
        prev[mode].filterKey !== summaryFilterKey
          ? prev
          : {
              ...prev,
              [mode]: {
                ...prev[mode],
                filteredTotal: page.filtered_total,
                filteredFileTotal: page.filtered_file_total,
                hasMore: prev[mode].hasMore && loadedCount < page.filtered_total,
                loaded: true,
              },
            },
      );
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
    expandedGroups,
    ...actions,
    setSearchTerm,
    changeViewMode: (next: TransferViewMode) => {
      if (next === viewMode) return;
      resetExpanded();
      if (paginationByMode[next].filterKey !== currentFilterKey) {
        if (next === "files") {
          setFileItems([]);
        } else {
          setGroupItems([]);
        }
        setPaginationByMode((prev) => ({
          ...prev,
          [next]: EMPTY_MODE_PAGINATION,
        }));
      }
      setLoading(true);
      setLoadingMore(false);
      setViewMode(next);
    },
    clearSearch: () => {
      setSearchTerm("");
      if (debouncedQueryRef.current !== "") {
        debouncedQueryRef.current = "";
        resetExpanded();
        clearInactiveList(viewMode);
        setLoading(true);
        setDebouncedQuery("");
      }
    },
    changeModeFilter: (next: TransferFilterMode) => {
      if (next === modeFilter) return;
      resetExpanded();
      clearInactiveList(viewMode);
      setLoading(true);
      setModeFilter(next);
    },
    changeKindFilter: (next: TransferKindFilter) => {
      if (next === kindFilter) return;
      resetExpanded();
      clearInactiveList(viewMode);
      setLoading(true);
      setKindFilter(next);
    },
    resetFilters: () => {
      const changed =
        debouncedQueryRef.current !== "" || modeFilter !== "all" || kindFilter !== "all";
      resetExpanded();
      setSearchTerm("");
      if (changed) {
        debouncedQueryRef.current = "";
        clearInactiveList(viewMode);
        setLoading(true);
        setDebouncedQuery("");
        setModeFilter("all");
        setKindFilter("all");
      }
    },
    refresh: () => {
      resetExpanded();
      return fetchHeadPage();
    },
    loadMore,
    retryLoadMore,
    toggleGroupExpand,
    loadMoreGroupFiles,
  };
}
