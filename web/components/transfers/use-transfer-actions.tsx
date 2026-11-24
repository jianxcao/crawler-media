import { useCallback, useState, useTransition } from "react";
import {
  deleteLedgerRow,
  refreshItemMetadata,
  retransferLedgerRow,
  type TransferFileItem,
} from "@/lib/api/transfers";
import { useToast } from "@/components/feedback";

export function useTransferActions(
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
