"use client";

/** 减季保存后的显式清理选择。当前预览没有按季计数，不显示全作品总数。 */

import { useState } from "react";

import { Modal } from "@/components/modal";
import type {
  SubscriptionRemovalOptions,
} from "@/lib/api/subscriptions";

const CANCEL_BTN_CLS =
  "rounded-lg border border-white/10 bg-white/[0.06] px-4 py-2 text-ui text-white/80 transition hover:bg-white/[0.1]";
const DANGER_BTN_CLS =
  "rounded-lg bg-red-500/85 px-4 py-2 text-ui font-medium text-white transition hover:bg-red-500 disabled:opacity-50";

/** 「第 2 季」/「第 2、4 季」——与后端 _season_text 同一口径。 */
function seasonText(seasons: number[]): string {
  return `第 ${[...seasons].sort((a, b) => a - b).join("、")} 季`;
}

export function SeasonCleanupDialog({
  open,
  title,
  seasons,
  onClose,
  onConfirm,
}: {
  open: boolean;
  /** 作品名 */
  title: string;
  /** 本次移出订阅范围的季号 */
  seasons: number[];
  /** 用户选择「保留内容」或关窗：什么都不清理 */
  onClose: () => void;
  /** 用户确认清理：带上两个开关的最终取值 */
  onConfirm: (options: SubscriptionRemovalOptions) => Promise<void> | void;
}) {
  const [deleteTorrents, setDeleteTorrents] = useState(false);
  const [deleteLibraryFiles, setDeleteLibraryFiles] = useState(false);
  const [busy, setBusy] = useState(false);

  const submit = async () => {
    setBusy(true);
    try {
      await onConfirm({ deleteTorrents, deleteLibraryFiles });
    } finally {
      setBusy(false);
    }
  };

  const label = seasonText(seasons);
  // 多季一起减时「这一季」读不通；范围本身已由标题和每个开关点明
  const them = seasons.length > 1 ? "这几季" : "这一季";

  return (
    <Modal open={open} onClose={busy ? () => {} : onClose} label={`清理${label}的内容`}>
      <div className="p-6 max-md:p-5">
        <h2 className="text-title-sm font-bold text-white">{label}已移出订阅</h2>
        <p className="mt-2 text-sub leading-6 text-[var(--text-muted)]">
          《{title}》的{them}不再追了。要不要把已经下载的内容也清理掉？不清理也没关系，
          种子和文件都原样留着。
        </p>

        <div className="mt-4 space-y-3">
          <CleanupToggle
            checked={deleteTorrents}
            disabled={busy}
            onChange={setDeleteTorrents}
            label={`同时删除季号匹配${label}的下载任务`}
            description={`从下载器移除匹配任务并删除下载目录里的文件，不可恢复。跨季种子也可能被选中；以后重新勾选${them}可能需要重新下载。`}
          />
          <CleanupToggle
            checked={deleteLibraryFiles}
            disabled={busy}
            onChange={setDeleteLibraryFiles}
            label={`同时删除包含${label}内容的媒体库文件`}
            description="文件将直接从磁盘物理删除（包含 .strm 指针），无法从回收站恢复。跨季共用的整个文件也会删除，其中其他季的内容会一并丢失。"
          />
        </div>

        {(deleteTorrents || deleteLibraryFiles) && (
          <p className="mt-4 rounded-lg bg-white/[0.04] px-3 py-2.5 text-xs leading-5 text-[var(--text-muted)]">
            确认后立即执行所选清理，请等待处理结果。
          </p>
        )}

        <div className="mt-5 flex justify-end gap-2.5">
          <button type="button" onClick={onClose} disabled={busy} className={CANCEL_BTN_CLS}>
            保留内容
          </button>
          <button
            type="button"
            onClick={() => void submit()}
            disabled={busy || (!deleteTorrents && !deleteLibraryFiles)}
            className={DANGER_BTN_CLS}
          >
            {busy ? "处理中…" : `清理${label}`}
          </button>
        </div>
      </div>
    </Modal>
  );
}

/** 一个清理开关：标题 + 后果说明 +（勾上后才出现的）风险警示。 */
function CleanupToggle({
  checked,
  disabled,
  onChange,
  label,
  description,
}: {
  checked: boolean;
  disabled: boolean;
  onChange: (value: boolean) => void;
  label: string;
  description?: string;
}) {
  return (
    <label
      className={`flex items-start gap-2.5 ${disabled ? "cursor-not-allowed opacity-45" : "cursor-pointer"}`}
    >
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
        className="mt-1 size-4 shrink-0 accent-[var(--accent-2)]"
      />
      <span className="min-w-0">
        <span className="block text-sub leading-6 text-white/90">{label}</span>
        {description && (
          <span className="block text-xs leading-5 text-[var(--text-muted)]">{description}</span>
        )}

      </span>
    </label>
  );
}
