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
  const [expandedGroups, setExpandedGroups] = useState<Record<string, ExpandedGroupState>>({});
  const expandedGroupsRef = useRef<Record<string, ExpandedGroupState>>({});
  expandedGroupsRef.current = expandedGroups;
  const versionRef = useRef(0);
  const inFlightRef = useRef<Map<string, AbortController>>(new Map());
  const toast = useToast();

  const abortAllInFlight = useCallback(() => {
    versionRef.current += 1;
    for (const controller of inFlightRef.current.values()) {
      controller.abort();
    }
    inFlightRef.current.clear();
  }, []);

  const resetExpanded = useCallback(() => {
    abortAllInFlight();
    setExpandedGroups({});
  }, [abortAllInFlight]);

  useEffect(() => {
    abortAllInFlight();
    setExpandedGroups({});
    return () => {
      abortAllInFlight();
    };
  }, [modeFilter, kindFilter, debouncedQuery, abortAllInFlight]);

  const toggleGroupExpand = useCallback(
    async (group: TransferGroupItem) => {
      const current = expandedGroupsRef.current[group.media_id];
      if (current?.open) {
        const activeController = inFlightRef.current.get(group.media_id);
        if (activeController) {
          activeController.abort();
          inFlightRef.current.delete(group.media_id);
        }
        setExpandedGroups((prev) =>
          prev[group.media_id]
            ? { ...prev, [group.media_id]: { ...prev[group.media_id], open: false, loading: false } }
            : prev,
        );
        return;
      }
      if (current && current.items.length > 0) {
        setExpandedGroups((prev) =>
          prev[group.media_id]
            ? { ...prev, [group.media_id]: { ...prev[group.media_id], open: true } }
            : prev,
        );
        return;
      }

      inFlightRef.current.get(group.media_id)?.abort();
      const requestVersion = versionRef.current;
      const controller = new AbortController();
      inFlightRef.current.set(group.media_id, controller);

      setExpandedGroups((prev) => ({
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
        if (controller.signal.aborted || versionRef.current !== requestVersion) return;
        setExpandedGroups((prev) => ({
          ...prev,
          [group.media_id]: {
            open: true,
            loading: false,
            items: page.items,
            hasMore: page.items.length > 0 && page.has_more,
          },
        }));
      } catch (e) {
        if (isAbortError(e) || controller.signal.aborted || versionRef.current !== requestVersion) {
          return;
        }
        toast.error("加载作品文件失败: " + (e as Error).message);
        setExpandedGroups((prev) => ({
          ...prev,
          [group.media_id]: { open: false, loading: false, items: [], hasMore: false },
        }));
      } finally {
        if (inFlightRef.current.get(group.media_id) === controller) {
          inFlightRef.current.delete(group.media_id);
        }
      }
    },
    [modeFilter, kindFilter, debouncedQuery, toast],
  );

  const loadMoreGroupFiles = useCallback(
    async (group: TransferGroupItem) => {
      const current = expandedGroupsRef.current[group.media_id];
      if (!current || current.loading || !current.hasMore) return;

      inFlightRef.current.get(group.media_id)?.abort();
      const requestVersion = versionRef.current;
      const controller = new AbortController();
      inFlightRef.current.set(group.media_id, controller);
      const offset = current.items.length;

      setExpandedGroups((prev) =>
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
        if (controller.signal.aborted || versionRef.current !== requestVersion) return;
        setExpandedGroups((prev) => {
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
        if (isAbortError(e) || controller.signal.aborted || versionRef.current !== requestVersion) {
          return;
        }
        toast.error("加载剩余剧集失败: " + (e as Error).message);
        setExpandedGroups((prev) =>
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
    [modeFilter, kindFilter, debouncedQuery, toast],
  );

  const removeExpandedFile = useCallback((mediaId: string, fileId: string) => {
    setExpandedGroups((prev) => {
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
  }, []);

  const updateExpandedFilePath = useCallback(
    (mediaId: string, fileId: string, destination: string) => {
      setExpandedGroups((prev) => {
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
    [],
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
