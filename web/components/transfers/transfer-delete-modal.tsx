import type { TransferFileItem } from "@/lib/api/transfers";
import { Modal } from "@/components/modal";
import { TrashIcon } from "@/components/icons";

interface TransferDeleteModalProps {
  item: TransferFileItem | null;
  deleteMode: "record_only" | "with_file";
  isPending: boolean;
  onChangeMode: (mode: "record_only" | "with_file") => void;
  onClose: () => void;
  onConfirm: () => void;
}

export function TransferDeleteModal({
  item,
  deleteMode,
  isPending,
  onChangeMode,
  onClose,
  onConfirm,
}: TransferDeleteModalProps) {
  if (!item) return null;
  const isStrm = item.is_strm || item.transfer_mode === "strm";

  return (
    <Modal open={Boolean(item)} onClose={onClose} label="删除入库资产记录" width="md">
      <div className="space-y-4 p-6 max-md:p-5">
        <h2 className="text-title-sm font-bold text-[var(--text)]">删除入库资产记录</h2>
        <p className="text-xs sm:text-sm text-[var(--text-muted)] leading-relaxed">
          您正在移除《
          <strong className="text-[var(--text)] font-semibold">
            {item.media_title || "未命名作品"}
          </strong>
          》的入库记录。请选择具体的数据与文件处理方式：
        </p>

        <div className="space-y-2.5">
          <label
            className={`flex cursor-pointer items-start gap-3 rounded-xl border p-3.5 transition ${
              deleteMode === "record_only"
                ? "border-sky-500/40 bg-sky-500/[0.08]"
                : "border-white/[0.08] bg-white/[0.02] hover:bg-white/[0.04]"
            }`}
          >
            <input
              type="radio"
              name="delete_mode"
              checked={deleteMode === "record_only"}
              onChange={() => onChangeMode("record_only")}
              className="mt-1 size-4 accent-sky-400"
            />
            <div className="min-w-0 flex-1">
              <div className="text-xs sm:text-sm font-semibold text-[var(--text)]">
                仅从系统数据库移除记录 (推荐)
              </div>
              <div className="mt-1 text-xs text-[var(--text-muted)] leading-relaxed">
                仅注销系统的入库记录，保留媒体库目录中的目标文件不变。
              </div>
            </div>
          </label>

          <label
            className={`flex cursor-pointer items-start gap-3 rounded-xl border p-3.5 transition ${
              deleteMode === "with_file"
                ? "border-red-500/50 bg-red-500/[0.1]"
                : "border-red-500/20 bg-red-500/[0.03] hover:bg-red-500/[0.06]"
            }`}
          >
            <input
              type="radio"
              name="delete_mode"
              checked={deleteMode === "with_file"}
              onChange={() => onChangeMode("with_file")}
              className="mt-1 size-4 accent-red-500"
            />
            <div className="min-w-0 flex-1">
              <div className="text-xs sm:text-sm font-semibold text-red-400">
                {isStrm ? "同时删除本地 .strm 直链文件" : "彻底删除磁盘入库物理文件"}
              </div>
              <div className="mt-1 text-xs text-red-300/70 leading-relaxed">
                {isStrm
                  ? "删除媒体库目录内的 .strm 指针文件（不影响云端网盘源视频）。"
                  : "将目标路径对应文件物理删除（不可逆，若为硬链接则源下载文件仍保留；若源文件也被删则数据丢失）。"}
              </div>
            </div>
          </label>
        </div>

        <div className="mt-6 flex items-center justify-end gap-3 pt-2">
          <button
            type="button"
            onClick={onClose}
            className="rounded-xl border border-white/[0.08] px-4 py-2 text-xs sm:text-sm font-medium text-white/70 transition hover:bg-white/[0.08] hover:text-white"
          >
            取消
          </button>
          <button
            type="button"
            onClick={onConfirm}
            disabled={isPending}
            className="flex items-center gap-1.5 rounded-xl bg-red-600 px-4 py-2 text-xs sm:text-sm font-semibold text-white shadow-lg transition hover:bg-red-500 disabled:opacity-50"
          >
            <TrashIcon className="size-4" />
            <span>{isPending ? "正在处理..." : "确认删除"}</span>
          </button>
        </div>
      </div>
    </Modal>
  );
}
