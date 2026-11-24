import { useCallback, useEffect, useRef, useState } from "react";
import {
  listTransfers,
  type TransferFilterMode,
  type TransferGroupItem,
  type TransferKindFilter,
} from "@/lib/api/transfers";
import { useToast } from "@/components/feedback";
import type { ExpandedGroupState } from "./transfer-group-card";

const GROUP_FILES_PAGE_SIZE = 200;
const EMPTY_EXPANDED_GROUPS: Record<string, ExpandedGroupState> = {};

function buildExpansionFilterKey(
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

export function useGroupExpansion(
  modeFilter: TransferFilterMode,
  kindFilter: TransferKindFilter,
  debouncedQuery: string,
) {
  const currentFilterKey = buildExpansionFilterKey(debouncedQuery, modeFilter, kindFilter);
  const filterKeyRef = useRef(currentFilterKey);
  filterKeyRef.current = currentFilterKey;

  const [expandedState, setExpandedState] = useState<{
    filterKey: string;
    groups: Record<string, ExpandedGroupState>;
  }>(() => ({
    filterKey: currentFilterKey,
    groups: EMPTY_EXPANDED_GROUPS,
  }));

  const expandedGroups =
    expandedState.filterKey === currentFilterKey ? expandedState.groups : EMPTY_EXPANDED_GROUPS;
  const expandedGroupsRef = useRef<Record<string, ExpandedGroupState>>(EMPTY_EXPANDED_GROUPS);
  expandedGroupsRef.current = expandedGroups;

  const generationRef = useRef(0);
  const inFlightRef = useRef<Map<string, AbortController>>(new Map());
  const toast = useToast();
  const toastRef = useRef(toast);
  toastRef.current = toast;

  const abortAllInFlight = useCallback(() => {
    generationRef.current += 1;
    for (const controller of inFlightRef.current.values()) {
      controller.abort();
    }
    inFlightRef.current.clear();
  }, []);

  const resetExpanded = useCallback(() => {
    abortAllInFlight();
    expandedGroupsRef.current = EMPTY_EXPANDED_GROUPS;
    setExpandedState({ filterKey: filterKeyRef.current, groups: EMPTY_EXPANDED_GROUPS });
  }, [abortAllInFlight]);

  const prevFilterKeyRef = useRef(currentFilterKey);
  useEffect(() => {
    if (prevFilterKeyRef.current !== currentFilterKey) {
      prevFilterKeyRef.current = currentFilterKey;
      abortAllInFlight();
      expandedGroupsRef.current = EMPTY_EXPANDED_GROUPS;
      setExpandedState({ filterKey: currentFilterKey, groups: EMPTY_EXPANDED_GROUPS });
    }
  }, [currentFilterKey, abortAllInFlight]);

  useEffect(() => {
    return () => {
      abortAllInFlight();
    };
  }, [abortAllInFlight]);

  const updateGroups = useCallback(
    (
      expectedFilterKey: string,
      expectedGeneration: number | null,
      updater: (prev: Record<string, ExpandedGroupState>) => Record<string, ExpandedGroupState>,
    ) => {
      setExpandedState((prevState) => {
        if (
          filterKeyRef.current !== expectedFilterKey ||
          (expectedGeneration !== null && generationRef.current !== expectedGeneration)
        ) {
          return prevState;
        }
        const base =
          prevState.filterKey === expectedFilterKey ? prevState.groups : EMPTY_EXPANDED_GROUPS;
        const nextGroups = updater(base);
        if (nextGroups === base && prevState.filterKey === expectedFilterKey) {
          return prevState;
        }
        return { filterKey: expectedFilterKey, groups: nextGroups };
      });
    },
    [],
  );

  const toggleGroupExpand = useCallback(
    async (group: TransferGroupItem) => {
      const current = expandedGroupsRef.current[group.media_id];
      const activeFilterKey = filterKeyRef.current;
      if (current?.open) {
        const activeController = inFlightRef.current.get(group.media_id);
        if (activeController) {
          activeController.abort();
          inFlightRef.current.delete(group.media_id);
        }
        updateGroups(activeFilterKey, null, (prev) =>
          prev[group.media_id]
            ? { ...prev, [group.media_id]: { ...prev[group.media_id], open: false, loading: false } }
            : prev,
        );
        return;
      }
      if (current && current.items.length > 0) {
        updateGroups(activeFilterKey, null, (prev) =>
          prev[group.media_id]
            ? { ...prev, [group.media_id]: { ...prev[group.media_id], open: true } }
            : prev,
        );
        return;
      }

      inFlightRef.current.get(group.media_id)?.abort();
      const requestGeneration = generationRef.current;
      const controller = new AbortController();
      inFlightRef.current.set(group.media_id, controller);

      updateGroups(activeFilterKey, requestGeneration, (prev) => ({
        ...prev,
        [group.media_id]: { open: true, loading: true, items: [], hasMore: false },
      }));

      try {
        const page = await listTransfers(
          {
            view: "files",
            media_id: group.media_id,
            mode: modeFilter,
            kind: kindFilter,
            q: debouncedQuery,
            offset: 0,
            limit: GROUP_FILES_PAGE_SIZE,
          },
          { signal: controller.signal },
        );
        if (
          controller.signal.aborted ||
          generationRef.current !== requestGeneration ||
          filterKeyRef.current !== activeFilterKey
        ) {
          return;
        }
        updateGroups(activeFilterKey, requestGeneration, (prev) => ({
          ...prev,
          [group.media_id]: {
            open: true,
            loading: false,
            items: page.items,
            hasMore: page.items.length > 0 && page.has_more,
          },
        }));
      } catch (e) {
        if (
          isAbortError(e) ||
          controller.signal.aborted ||
          generationRef.current !== requestGeneration ||
          filterKeyRef.current !== activeFilterKey
        ) {
          return;
        }
        toastRef.current.error("加载作品文件失败: " + (e as Error).message);
        updateGroups(activeFilterKey, requestGeneration, (prev) => ({
          ...prev,
          [group.media_id]: { open: false, loading: false, items: [], hasMore: false },
        }));
      } finally {
        if (inFlightRef.current.get(group.media_id) === controller) {
          inFlightRef.current.delete(group.media_id);
        }
      }
    },
    [modeFilter, kindFilter, debouncedQuery, updateGroups],
  );

  const loadMoreGroupFiles = useCallback(
    async (group: TransferGroupItem) => {
      const current = expandedGroupsRef.current[group.media_id];
      if (!current || current.loading || !current.hasMore) return;

      const activeFilterKey = filterKeyRef.current;
      inFlightRef.current.get(group.media_id)?.abort();
      const requestGeneration = generationRef.current;
      const controller = new AbortController();
      inFlightRef.current.set(group.media_id, controller);
      const offset = current.items.length;

      updateGroups(activeFilterKey, requestGeneration, (prev) =>
        prev[group.media_id]
          ? { ...prev, [group.media_id]: { ...prev[group.media_id], loading: true } }
          : prev,
      );

      try {
        const page = await listTransfers(
          {
            view: "files",
            media_id: group.media_id,
            mode: modeFilter,
            kind: kindFilter,
            q: debouncedQuery,
            offset,
            limit: GROUP_FILES_PAGE_SIZE,
          },
          { signal: controller.signal },
        );
        if (
          controller.signal.aborted ||
          generationRef.current !== requestGeneration ||
          filterKeyRef.current !== activeFilterKey
        ) {
          return;
        }
        updateGroups(activeFilterKey, requestGeneration, (prev) => {
          const latest = prev[group.media_id];
          if (!latest) return prev;
          const seen = new Set(latest.items.map((item) => item.id));
          return {
            ...prev,
            [group.media_id]: {
              open: latest.open,
              loading: false,
              items: [...latest.items, ...page.items.filter((item) => !seen.has(item.id))],
              hasMore: page.items.length > 0 && page.has_more,
            },
          };
        });
      } catch (e) {
        if (
          isAbortError(e) ||
          controller.signal.aborted ||
          generationRef.current !== requestGeneration ||
          filterKeyRef.current !== activeFilterKey
        ) {
          return;
        }
        toastRef.current.error("加载剩余剧集失败: " + (e as Error).message);
        updateGroups(activeFilterKey, requestGeneration, (prev) =>
          prev[group.media_id]
            ? { ...prev, [group.media_id]: { ...prev[group.media_id], loading: false } }
            : prev,
        );
      } finally {
        if (inFlightRef.current.get(group.media_id) === controller) {
          inFlightRef.current.delete(group.media_id);
        }
      }
    },
    [modeFilter, kindFilter, debouncedQuery, updateGroups],
  );

  const removeExpandedFile = useCallback(
    (mediaId: string, fileId: string) => {
      updateGroups(filterKeyRef.current, null, (prev) => {
        const groupState = prev[mediaId];
        if (!groupState) return prev;
        return {
          ...prev,
          [mediaId]: {
            ...groupState,
            items: groupState.items.filter((item) => item.id !== fileId),
          },
        };
      });
    },
    [updateGroups],
  );

  const updateExpandedFilePath = useCallback(
    (mediaId: string, fileId: string, destination: string) => {
      updateGroups(filterKeyRef.current, null, (prev) => {
        const groupState = prev[mediaId];
        if (!groupState) return prev;
        let changed = false;
        const nextItems = groupState.items.map((item) => {
          if (item.id !== fileId || item.path === destination) return item;
          changed = true;
          return { ...item, path: destination };
        });
        if (!changed) return prev;
        return {
          ...prev,
          [mediaId]: {
            ...groupState,
            items: nextItems,
          },
        };
      });
    },
    [updateGroups],
  );

  return {
    expandedGroups,
    resetExpanded,
    toggleGroupExpand,
    loadMoreGroupFiles,
    removeExpandedFile,
    updateExpandedFilePath,
  };
}
