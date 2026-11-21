"use client";

import { useCallback, useEffect, useState } from "react";

import { Modal } from "@/components/modal";
import {
  type Downloader,
  type DownloaderLimitsSnapshot,
  getDownloaderLimits,
  setDownloaderLimits,
} from "@/lib/api/downloaders";

/* —— 下载器全局限速弹窗 —— */

const KIB = 1024;

export function DownloaderLimitsModal({
  open,
  downloader,
  onClose,
}: {
  open: boolean;
  downloader: Downloader;
  onClose: () => void;
}) {
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [unavailable, setUnavailable] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // 表单态：限速以 KiB/s 字符串承载（空 = 不限速）
  const [dlKib, setDlKib] = useState("");
  const [upKib, setUpKib] = useState("");

  const applyLimits = useCallback((limits: DownloaderLimitsSnapshot) => {
    setUnavailable(limits.status === "unavailable");
    if (limits.status === "unavailable") {
      setError(`下载器不可用：${limits.error || "无法读取限速状态"}`);
      return;
    }
    setDlKib(
      limits.download_limit_bytes != null
        ? String(Math.round(limits.download_limit_bytes / KIB))
        : "",
    );
    setUpKib(
      limits.upload_limit_bytes != null
        ? String(Math.round(limits.upload_limit_bytes / KIB))
        : "",
    );
  }, []);

  useEffect(() => {
    if (!open) return;
    setLoading(true);
    setUnavailable(false);
    setError(null);
    getDownloaderLimits(downloader.id)
      .then((limits) => applyLimits(limits))
      .catch((e) => {
        setUnavailable(true);
        setError((e as Error).message);
      })
      .finally(() => setLoading(false));
  }, [open, downloader.id, applyLimits]);

  function parseSpeed(raw: string): number | null {
    if (!raw.trim()) return null; // 空 = 不限速
    const kib = Math.round(Number(raw.trim()));
    return Number.isFinite(kib) && kib > 0 ? kib * KIB : null;
  }

  async function save() {
    if (unavailable) return;
    setBusy(true);
    setError(null);
    try {
      const applied = await setDownloaderLimits(downloader.id, {
        download_limit_bytes: parseSpeed(dlKib),
        upload_limit_bytes: parseSpeed(upKib),
      });
      applyLimits(applied); // 回读生效值（下载器可能钳制），表单即时对齐
      onClose();
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal open={open} onClose={onClose} label="全局限速" width="lg">
      {/* 头部常驻 */}
      <div className="border-b border-white/[0.07] px-6 pb-4 pt-6">
        <h2 className="text-title font-bold text-[var(--text)]">
          全局限速 · {downloader.name}
        </h2>
        <p className="mt-1 text-sub leading-5 text-[var(--text-muted)]">
          实时读写下载器的全局下载与上传限速，留空表示不限速。
        </p>
      </div>

      <div className="scroll-thin min-h-0 flex-1 space-y-4 overflow-y-auto px-6 py-4">
        {error && (
          <div className="rounded-xl border border-[#ff6b6b]/30 bg-[#ff6b6b]/10 px-4 py-3 text-body text-[#ff6b6b]">
            {error}
          </div>
        )}

        {loading ? (
          <div className="space-y-2">
            <div className="h-11 animate-pulse rounded-xl bg-white/[0.04]" />
            <div className="h-11 animate-pulse rounded-xl bg-white/[0.04]" />
          </div>
        ) : (
          <div className="space-y-4">
            <div className="grid grid-cols-2 gap-3 max-md:grid-cols-1">
              <LimitInput
                label="全局下载限速"
                value={dlKib}
                unit="KiB/s"
                placeholder="不限"
                disabled={busy || unavailable}
                onChange={setDlKib}
              />
              <LimitInput
                label="全局上传限速"
                value={upKib}
                unit="KiB/s"
                placeholder="不限"
                disabled={busy || unavailable}
                onChange={setUpKib}
              />
            </div>
          </div>
        )}
      </div>

      {/* 底栏常驻 */}
      <div className="flex justify-end gap-2.5 border-t border-white/[0.07] px-6 py-4">
        <button
          type="button"
          onClick={onClose}
          disabled={busy}
          className="btn-glass px-4 py-2 text-sub font-medium"
        >
          取消
        </button>
        <button
          type="button"
          onClick={() => void save()}
          disabled={busy || loading || unavailable}
          className="btn-accent rounded-full px-4 py-2 text-sub font-semibold disabled:opacity-60"
        >
          {busy ? "保存中…" : "保存"}
        </button>
      </div>
    </Modal>
  );
}

/** 带单位后缀的数字输入（与 feedback 层 prompt 的单位内嵌设计同款）。 */
export function LimitInput({
  label,
  value,
  unit,
  placeholder,
  disabled,
  onChange,
}: {
  label: string;
  value: string;
  unit?: string;
  placeholder?: string;
  disabled?: boolean;
  onChange: (value: string) => void;
}) {
  return (
    <div>
      <label className="mb-1.5 block text-caption font-medium text-[var(--text-muted)]">
        {label}
      </label>
      <div className="relative">
        <input
          type="number"
          min={0}
          value={value}
          placeholder={placeholder}
          disabled={disabled}
          onChange={(e) => onChange(e.target.value)}
          className={`w-full rounded-lg border border-white/10 bg-white/[0.06] px-3 py-2 text-ui text-white outline-none transition [appearance:textfield] focus:border-white/25 [&::-webkit-inner-spin-button]:appearance-none [&::-webkit-outer-spin-button]:appearance-none ${
            unit ? "pr-14" : ""
          }`}
        />
        {unit && (
          <span className="pointer-events-none absolute inset-y-0 right-3 flex items-center text-caption font-medium text-[var(--text-faint)]">
            {unit}
          </span>
        )}
      </div>
    </div>
  );
}
