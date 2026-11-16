import { useRef, useState } from "react";

import { Modal } from "@/components/modal";
import {
  createMediaInfoRequestId,
  refreshLibraryMediaInfo,
  type MediaInfoRefreshMode,
  type MediaInfoRefreshReceipt,
  type MediaLibrary,
} from "@/lib/api/libraries";

const MODES = [
  {
    value: "missing",
    label: "只抓取缺少的信息",
    description: "补齐未抓取、抓取失败或已失效的媒体信息，保留已有的有效结果。",
  },
  {
    value: "force",
    label: "强制全部重新抓取",
    description: "重新读取本库全部在位文件，文件较多时需要更长时间；新结果写入前保留原有信息。",
  },
] as const;

export function LibraryMediaInfoDialog({ library, onClose, onSubmitted }: {
  library: Pick<MediaLibrary, "id" | "name">;
  onClose: () => void;
  onSubmitted: (receipt: MediaInfoRefreshReceipt) => void;
}) {
  const [mode, setMode] = useState<MediaInfoRefreshMode>("missing");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const submitting = useRef(false);
  const attempt = useRef<{ mode: MediaInfoRefreshMode; id: string } | null>(null);
  const close = () => { if (!submitting.current) onClose(); };
  const submit = async () => {
    if (submitting.current) return;
    submitting.current = true;
    setBusy(true);
    setError(null);
    try {
      if (attempt.current?.mode !== mode) attempt.current = { mode, id: createMediaInfoRequestId() };
      const receipt = await refreshLibraryMediaInfo(library.id, mode, attempt.current.id);
      onSubmitted(receipt);
      onClose();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : "媒体信息抓取提交失败，请重试");
    } finally {
      submitting.current = false;
      setBusy(false);
    }
  };
  return (
    <Modal open onClose={close} label={`媒体信息抓取 · ${library.name}`}>
      <div className="p-5">
        <h2 className="text-title font-bold text-white">媒体信息抓取</h2>
        <p className="mt-2 text-sub leading-6 text-[var(--text-muted)]">
          为「{library.name}」读取分辨率、编码、时长、音轨和字幕等文件信息，选择本次抓取范围：
        </p>
        <fieldset disabled={busy} className="mt-4 space-y-2">
          <legend className="sr-only">抓取范围</legend>
          {MODES.map(option => (
            <label key={option.value} className="flex cursor-pointer items-start gap-3 rounded-xl border border-white/10 p-3 has-[:checked]:border-[var(--accent)] has-[:checked]:bg-white/5">
              <input type="radio" name="media-info-mode" value={option.value}
                checked={mode === option.value} onChange={() => setMode(option.value)}
                className="mt-1 accent-[var(--accent)]" />
              <span>
                <span className="block text-ui font-medium text-white">{option.label}</span>
                <span className="mt-1 block text-sub leading-6 text-[var(--text-muted)]">{option.description}</span>
              </span>
            </label>
          ))}
        </fieldset>
        <p className="mt-3 text-sub leading-6 text-[var(--text-muted)]">
          片头片尾和声纹按媒体库设置处理；此操作不会刷新 TMDB 简介、海报或背景，也不会移动或删除视频文件。
        </p>
        {error && <p role="alert" className="mt-3 text-sub text-[var(--danger)]">{error}</p>}
        <div className="mt-5 flex justify-end gap-2">
          <button type="button" disabled={busy} onClick={close} className="btn-glass px-4 py-2 text-ui font-medium disabled:opacity-40">取消</button>
          <button type="button" disabled={busy} onClick={() => void submit()}
            className="btn-accent rounded-full px-4 py-2 text-ui font-semibold disabled:opacity-40">
            {busy ? "正在提交…" : "开始抓取"}
          </button>
        </div>
      </div>
    </Modal>
  );
}
