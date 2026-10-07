"use client";

import { useEffect, useState } from "react";

import { DirectoryPicker } from "@/components/directory-picker";
import { FolderIcon, XIcon } from "@/components/icons";
import { useToast } from "@/components/feedback";
import {
  getDirectory,
  setTransferMode,
  setWatchIntake,
  type TransferMode,
} from "@/lib/api/directory";

const OPTIONS: { value: TransferMode; label: string; hint: string }[] = [
  { value: "hardlink", label: "硬链接", hint: "同盘默认，做种文件保留" },
  { value: "copy", label: "复制", hint: "库里一份独立拷贝" },
  { value: "move", label: "移动", hint: "入库后删除下载器内文件" },
];

/** Instance-wide Transfer mode and external Watch Intake. */
export function TransferModeCard() {
  const toast = useToast();
  const [mode, setMode] = useState<TransferMode>("hardlink");
  const [intakePath, setIntakePath] = useState("");
  const [pickerOpen, setPickerOpen] = useState(false);
  const [busyMode, setBusyMode] = useState(false);
  const [busyIntake, setBusyIntake] = useState(false);

  useEffect(() => {
    void getDirectory()
      .then((directory) => {
        setMode(directory.transfer_mode);
        setIntakePath(directory.watch_intake ?? "");
      })
      .catch(() => {
        /* keep defaults */
      });
  }, []);

  const handleSaveIntake = async (path: string | null) => {
    setBusyIntake(true);
    try {
      const res = await setWatchIntake(path);
      setIntakePath(res.watch_intake ?? "");
      toast.success(res.watch_intake ? "已更新监控入库目录" : "已停用监控入库");
    } catch (error: unknown) {
      toast.error(error instanceof Error ? error.message : "保存失败");
    } finally {
      setBusyIntake(false);
    }
  };

  return (
    <div className="space-y-4">
      <div className="rounded-2xl border border-white/[0.08] bg-white/[0.03] px-4 py-3">
        <p className="text-ui font-medium text-white/85">入库方式</p>
        <p className="mt-1 text-caption text-[var(--text-muted)]">
          下载完成后，文件如何进入媒体库。同盘默认硬链接；跨盘硬链接失败时会退回复制。
        </p>
        <select
          className="mt-3 h-9 w-full rounded-lg border border-white/10 bg-white/[0.04] px-2 text-ui text-white"
          value={mode}
          disabled={busyMode}
          onChange={(event) => {
            const next = event.target.value as TransferMode;
            setBusyMode(true);
            void setTransferMode(next)
              .then((directory) => {
                setMode(directory.transfer_mode);
                toast.success("已更新入库方式");
              })
              .catch((error: unknown) => {
                toast.error(error instanceof Error ? error.message : "保存失败");
              })
              .finally(() => setBusyMode(false));
          }}
        >
          {OPTIONS.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}（{option.hint}）
            </option>
          ))}
        </select>
      </div>

      <div className="rounded-2xl border border-white/[0.08] bg-white/[0.03] px-4 py-3">
        <div className="flex items-center justify-between gap-2">
          <p className="text-ui font-medium text-white/85">外部下载监控入库（Watch Intake）</p>
          {intakePath && (
            <button
              type="button"
              disabled={busyIntake}
              onClick={() => void handleSaveIntake(null)}
              className="inline-flex items-center gap-1 rounded-md px-2 py-0.5 text-caption text-red-400/80 transition hover:bg-red-500/10 hover:text-red-300 disabled:opacity-40"
            >
              <XIcon className="size-3" />
              清空并停用
            </button>
          )}
        </div>
        <p className="mt-1 text-caption text-[var(--text-muted)] leading-relaxed">
          监控外部第三方工具（如迅雷、网盘、Aria2）的下载暂存目录。发现新视频后会自动识别剧集并整理/移动进影视库并刮削。
        </p>
        <div className="mt-2 rounded-lg border border-amber-400/20 bg-amber-400/5 px-2.5 py-1.5 text-caption text-amber-200/90 leading-relaxed">
          ⚠️ 提示：系统内部订阅下载（qBittorrent / Transmission）会自动触发整理入库，<b>无需配置此监控</b>。请勿将下载器的默认保存目录直接填入此处。
        </div>

        <div className="mt-3 flex items-center gap-2">
          <input
            type="text"
            className="h-9 flex-1 rounded-lg border border-white/10 bg-white/[0.04] px-3 font-mono text-ui text-white outline-none placeholder:text-white/30 focus:border-white/25"
            placeholder="留空表示不启用（例如 /downloads/incoming）"
            value={intakePath}
            disabled={busyIntake}
            onChange={(e) => setIntakePath(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                void handleSaveIntake(intakePath);
              }
            }}
          />
          <button
            type="button"
            disabled={busyIntake}
            onClick={() => setPickerOpen(true)}
            className="inline-flex h-9 shrink-0 items-center gap-1.5 rounded-lg border border-white/10 bg-white/[0.06] px-3 text-ui text-white/90 transition hover:bg-white/[0.1] active:scale-[0.98] disabled:opacity-50"
          >
            <FolderIcon className="size-4 text-[var(--accent)]" />
            浏览
          </button>
          <button
            type="button"
            disabled={busyIntake}
            onClick={() => void handleSaveIntake(intakePath)}
            className="inline-flex h-9 shrink-0 items-center rounded-lg bg-[var(--accent)] px-3.5 text-ui font-medium text-black transition hover:opacity-90 active:scale-[0.98] disabled:opacity-50"
          >
            保存
          </button>
        </div>
      </div>

      <DirectoryPicker
        open={pickerOpen}
        initialPath={intakePath || undefined}
        onClose={() => setPickerOpen(false)}
        onSelect={(path) => {
          setPickerOpen(false);
          setIntakePath(path);
          void handleSaveIntake(path);
        }}
      />
    </div>
  );
}
