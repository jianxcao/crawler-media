"use client";

import { useEffect, useState } from "react";

import { getDirectory, setTransferMode, type TransferMode } from "@/lib/api/directory";
import { useToast } from "@/components/feedback";

const OPTIONS: { value: TransferMode; label: string; hint: string }[] = [
  { value: "hardlink", label: "硬链接", hint: "同盘默认，做种文件保留" },
  { value: "copy", label: "复制", hint: "库里一份独立拷贝" },
  { value: "move", label: "移动", hint: "入库后删除下载器内文件" },
];

/** Instance-wide Transfer mode. Lives next to downloaders because it runs after they finish. */
export function TransferModeCard() {
  const toast = useToast();
  const [mode, setMode] = useState<TransferMode>("hardlink");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    void getDirectory()
      .then((directory) => setMode(directory.transfer_mode))
      .catch(() => {
        /* keep default hardlink */
      });
  }, []);

  return (
    <div className="rounded-2xl border border-white/[0.08] bg-white/[0.03] px-4 py-3">
      <p className="text-ui font-medium text-white/85">入库方式</p>
      <p className="mt-1 text-caption text-[var(--text-muted)]">
        下载完成后，文件如何进入媒体库。同盘默认硬链接；跨盘硬链接失败时会退回复制。
      </p>
      <select
        className="mt-3 h-9 w-full rounded-lg border border-white/10 bg-white/[0.04] px-2 text-ui text-white"
        value={mode}
        disabled={busy}
        onChange={(event) => {
          const next = event.target.value as TransferMode;
          setBusy(true);
          void setTransferMode(next)
            .then((directory) => {
              setMode(directory.transfer_mode);
              toast.success("已更新入库方式");
            })
            .catch((error: unknown) => {
              toast.error(error instanceof Error ? error.message : "保存失败");
            })
            .finally(() => setBusy(false));
        }}
      >
        {OPTIONS.map((option) => (
          <option key={option.value} value={option.value}>
            {option.label}（{option.hint}）
          </option>
        ))}
      </select>
    </div>
  );
}
