import { useCallback, useState } from "react";
import {
  listTransfers,
  type TransferFilterMode,
  type TransferGroupItem,
} from "@/lib/api/transfers";
import { useToast } from "@/components/feedback";
import type { ExpandedGroupState } from "./transfer-group-card";

const GROUP_FILES_PAGE_SIZE = 200;

export function useGroupExpansion(modeFilter: TransferFilterMode, debouncedQuery: string) {
  const [expandedGroups, setExpandedGroups] = useState<Record<string, ExpandedGroupState>>({});
  const toast = useToast();

  const resetExpanded = useCallback(() => setExpandedGroups({}), []);

  const toggleGroupExpand = useCallback(
    async (group: TransferGroupItem) => {
      const current = expandedGroups[group.media_id];
      if (current?.open) {
        setExpandedGroups((prev) => ({ ...prev, [group.media_id]: { ...current, open: false } }));
        return;
      }
      if (current && current.items.length > 0) {
        setExpandedGroups((prev) => ({ ...prev, [group.media_id]: { ...current, open: true } }));
        return;
      }
      setExpandedGroups((prev) => ({
        ...prev,
        [group.media_id]: { open: true, loading: true, items: [], hasMore: false },
      }));

      try {
        const page = await listTransfers({
          view: "files",
          media_id: group.media_id,
          mode: modeFilter,
          q: debouncedQuery,
          offset: 0,
          limit: GROUP_FILES_PAGE_SIZE,
        });
        setExpandedGroups((prev) => ({
          ...prev,
          [group.media_id]: {
            open: true,
            loading: false,
            items: page.items,
            hasMore: page.has_more,
          },
        }));
      } catch (e) {
        toast.error("加载作品文件失败: " + (e as Error).message);
        setExpandedGroups((prev) => ({
          ...prev,
          [group.media_id]: { open: false, loading: false, items: [], hasMore: false },
        }));
      }
    },
    [expandedGroups, modeFilter, debouncedQuery, toast],
  );

  const loadMoreGroupFiles = useCallback(
    async (group: TransferGroupItem) => {
      const current = expandedGroups[group.media_id];
      if (!current || current.loading || !current.hasMore) return;
      setExpandedGroups((prev) => ({
        ...prev,
        [group.media_id]: { ...current, loading: true },
      }));

      try {
        const page = await listTransfers({
          view: "files",
          media_id: group.media_id,
          mode: modeFilter,
          q: debouncedQuery,
          offset: current.items.length,
          limit: GROUP_FILES_PAGE_SIZE,
        });
        setExpandedGroups((prev) => {
          const latest = prev[group.media_id] ?? current;
          const seen = new Set(latest.items.map((item) => item.id));
          return {
            ...prev,
            [group.media_id]: {
              open: true,
              loading: false,
              items: [...latest.items, ...page.items.filter((item) => !seen.has(item.id))],
              hasMore: page.has_more,
            },
          };
        });
      } catch (e) {
        toast.error("加载剩余剧集失败: " + (e as Error).message);
        setExpandedGroups((prev) => ({
          ...prev,
          [group.media_id]: { ...current, loading: false },
        }));
      }
    },
    [expandedGroups, modeFilter, debouncedQuery, toast],
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

  return {
    expandedGroups,
    resetExpanded,
    toggleGroupExpand,
    loadMoreGroupFiles,
    removeExpandedFile,
  };
}
