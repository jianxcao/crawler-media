"use client";

import type { Dispatch, SetStateAction } from "react";

import type { EditDraft } from "./marker-control-helpers";

export function MarkerEditForm({
  editing,
  setEditing,
  isTv,
  busy,
  onSubmit,
}: {
  editing: EditDraft;
  setEditing: Dispatch<SetStateAction<EditDraft | null>>;
  isTv: boolean;
  busy: string | null;
  onSubmit: () => void;
}) {
  return (
    <div className="mt-3 rounded-xl border border-sky-400/25 bg-sky-500/[0.06] p-3 text-caption">
      <p className="font-medium text-sky-200">
        手工编辑
        {isTv && editing.episode != null ? `第 ${editing.episode} 集` : "正片"}
        （支持填秒数如 90 或分:秒如 01:30，留空表示无）
      </p>
      <div className="mt-2 grid grid-cols-2 gap-2 sm:grid-cols-4">
        <label className="text-white/70">
          片头起
          <input
            type="text"
            aria-label="片头起点"
            value={editing.introStart}
            onChange={(e) =>
              setEditing((prev) => (prev ? { ...prev, introStart: e.target.value } : prev))
            }
            className="mt-1 block w-full rounded-lg border border-white/15 bg-black/40 px-2 py-1 text-white"
          />
        </label>
        <label className="text-white/70">
          片头止
          <input
            type="text"
            aria-label="片头终点"
            value={editing.introEnd}
            onChange={(e) =>
              setEditing((prev) => (prev ? { ...prev, introEnd: e.target.value } : prev))
            }
            className="mt-1 block w-full rounded-lg border border-white/15 bg-black/40 px-2 py-1 text-white"
          />
        </label>
        <label className="text-white/70">
          片尾起
          <input
            type="text"
            aria-label="片尾起点"
            value={editing.outroStart}
            onChange={(e) =>
              setEditing((prev) => (prev ? { ...prev, outroStart: e.target.value } : prev))
            }
            className="mt-1 block w-full rounded-lg border border-white/15 bg-black/40 px-2 py-1 text-white"
          />
        </label>
        <label className="text-white/70">
          片尾止
          <input
            type="text"
            aria-label="片尾终点"
            value={editing.outroEnd}
            onChange={(e) =>
              setEditing((prev) => (prev ? { ...prev, outroEnd: e.target.value } : prev))
            }
            className="mt-1 block w-full rounded-lg border border-white/15 bg-black/40 px-2 py-1 text-white"
          />
        </label>
      </div>
      <label className="mt-2.5 inline-flex items-center gap-2 text-white/80">
        <input
          type="checkbox"
          aria-label="锁定人工标记"
          checked={editing.locked}
          onChange={(e) =>
            setEditing((prev) => (prev ? { ...prev, locked: e.target.checked } : prev))
          }
          className="rounded border-white/20 bg-black/40"
        />
        锁定此标记（防止后续自动检测覆盖）
      </label>
      <div className="mt-2.5 flex items-center gap-2">
        <button
          type="button"
          disabled={busy != null}
          onClick={onSubmit}
          className="rounded-lg bg-sky-500/25 px-3 py-1 font-medium text-sky-100 hover:bg-sky-500/35 disabled:opacity-40"
        >
          {editing.locked ? "保存锁定" : "保存并解锁"}
        </button>
        <button
          type="button"
          disabled={busy != null}
          onClick={() => setEditing(null)}
          className="rounded-lg border border-white/15 px-3 py-1 text-white/70 hover:bg-white/10 disabled:opacity-40"
        >
          取消
        </button>
      </div>
    </div>
  );
}
