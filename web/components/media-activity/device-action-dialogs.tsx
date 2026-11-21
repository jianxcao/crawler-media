"use client";

import { Modal } from "@/components/modal";

export interface RevokeTarget {
  userId: string;
  deviceId: string;
  label: string;
}

/** 结束播放确认：动作可逆（设备再点播放即可），但会打断正在观看的播放。 */
export function EndPlaybackDialog({
  target,
  busy,
  onClose,
  onConfirm,
}: {
  target: RevokeTarget;
  busy: boolean;
  onClose: () => void;
  onConfirm: () => void;
}) {
  return (
    <Modal open onClose={busy ? () => {} : onClose} label="结束播放" topmost>
      <div className="p-6 max-md:p-5">
        <h2 className="text-title-sm font-bold text-white">结束「{target.label}」本次播放？</h2>
        <p className="mt-2 text-sub leading-6 text-[var(--text-muted)]">
          这台设备正在进行的播放会立刻中断，一分钟内不能续播；登录凭据与观看进度都不受影响，
          之后重新点播放即可继续。
        </p>
        <div className="mt-5 flex justify-end gap-2.5">
          <button
            type="button"
            onClick={onClose}
            disabled={busy}
            className="rounded-lg border border-white/10 bg-white/[0.06] px-4 py-2 text-ui text-white/80 transition hover:bg-white/[0.1] disabled:opacity-40"
          >
            取消
          </button>
          <button
            type="button"
            onClick={onConfirm}
            disabled={busy}
            className="rounded-lg bg-white/15 px-4 py-2 text-ui font-medium text-white transition hover:bg-white/25 disabled:opacity-40"
          >
            {busy ? "正在结束…" : "结束播放"}
          </button>
        </div>
      </div>
    </Modal>
  );
}

/** 注销设备确认：登录凭据立即失效，下次使用需要重新登录。 */
export function RevokeDeviceDialog({
  target,
  busy,
  onClose,
  onConfirm,
}: {
  target: RevokeTarget;
  busy: boolean;
  onClose: () => void;
  onConfirm: () => void;
}) {
  return (
    <Modal open onClose={busy ? () => {} : onClose} label="注销播放器设备" topmost>
      <div className="p-6 max-md:p-5">
        <h2 className="text-title-sm font-bold text-white">注销这台设备？</h2>
        <p className="mt-2 text-sub leading-6 text-[var(--text-muted)]">
          「{target.label}」的登录凭据会立刻失效，正在进行的播放与下载一并停止，
          该设备下次使用需要重新登录。
        </p>
        <p className="mt-3 text-caption leading-5 text-white/45">
          已看进度、收藏这些观看记录按账号保存，不会因为注销设备而丢失。
        </p>
        <div className="mt-5 flex justify-end gap-2.5">
          <button
            type="button"
            onClick={onClose}
            disabled={busy}
            className="rounded-lg border border-white/10 bg-white/[0.06] px-4 py-2 text-ui text-white/80 transition hover:bg-white/[0.1] disabled:opacity-40"
          >
            取消
          </button>
          <button
            type="button"
            onClick={onConfirm}
            disabled={busy}
            className="rounded-lg bg-red-500/85 px-4 py-2 text-ui font-medium text-white transition hover:bg-red-500 disabled:opacity-40"
          >
            {busy ? "正在注销…" : "注销设备"}
          </button>
        </div>
      </div>
    </Modal>
  );
}
