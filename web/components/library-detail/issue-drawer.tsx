"use client";

import { useEffect, useState } from "react";
import { createPortal } from "react-dom";

import { useConfirm, useToast } from "@/components/feedback";
import { XIcon } from "@/components/icons";
import {
  clearMissingLibraryRecords,
  type MissingItem,
  type UnidentifiedGroup,
} from "@/lib/api/libraries";

import { UnidentifiedGroupRow } from "./unidentified-group-row";

/* —— 待处理抽屉：缺失 / 待识别 / 身份复核 / 已忽略，右侧滑出，海报墙不再被待办铺满 ——
   「已忽略」不是待办，是**已处理**的归档：默认不打扰，只在有内容时露出 tab，
   给识别器变强之后反悔的机会。 */

export type IssueTab = "missing" | "unidentified";

export function IssueDrawer({
  open,
  onClose,
  onSwitchTab,
  libraryId,
  missing,
  unidentified,
  movie,
  onChanged,
}: {
  open: IssueTab | null;
  onClose: () => void;
  onSwitchTab: (tab: IssueTab) => void;
  libraryId: string;
  missing: MissingItem[];
  unidentified: UnidentifiedGroup[];
  movie: boolean;
  onChanged: () => void;
}) {
  const confirm = useConfirm();
  const toast = useToast();
  const [query, setQuery] = useState("");
  const [busy, setBusy] = useState(false);

  // 打开/切 tab 时清空过滤词；Escape 关闭
  useEffect(() => {
    setQuery("");
  }, [open]);
  useEffect(() => {
    if (open === null) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (open === null || typeof document === "undefined") return null;

  const keyword = query.trim().toLowerCase();
  const missingShown = keyword
    ? missing.filter((m) => m.title.toLowerCase().includes(keyword))
    : missing;
  const unidentifiedShown = keyword
    ? unidentified.filter(
        (g) =>
          g.label.toLowerCase().includes(keyword) ||
          g.files.some((f) => f.file_path.toLowerCase().includes(keyword)),
      )
    : unidentified;
  const missingFileTotal = missing.reduce((n, item) => n + item.files.length, 0);
  const unidentifiedFileTotal = unidentified.reduce((n, g) => n + g.file_count, 0);
  // 「放错库了」的文件数：认领解决不了（TMDB 的 movie/tv 是两套 id 空间），
  // 得挪文件或删库重建，单独给一条修复引导
  const kindMismatchFiles = unidentified.reduce(
    (n, g) => (g.code === "kind_mismatch" ? n + g.file_count : n),
    0,
  );

  const tabClass = (active: boolean) =>
    `rounded-full px-3.5 py-1.5 text-sub font-semibold transition ${
      active ? "bg-white/[0.14] text-white" : "text-[var(--text-muted)] hover:text-white"
    }`;

  return createPortal(
    // bottom 越出视口 --vp-overshoot：遮罩与右侧全高面板铺到屏幕物理底边
    // （iOS 独立 App 的视口矮一截，见 globals.css）；列表内容用加大的 pb 留在视口内
    <div
      className="fixed inset-0 z-50 [bottom:calc(-1*var(--vp-overshoot))]"
      role="dialog"
      aria-modal="true"
      aria-label="待处理"
    >
      <button
        type="button"
        aria-label="关闭"
        onClick={onClose}
        className="absolute inset-0 cursor-default bg-black/50 backdrop-blur-[2px]"
      />
      <div className="popover-surface absolute right-0 top-0 flex h-full w-full max-w-[600px] flex-col !border-y-0 !border-r-0 max-md:max-w-none">
        {/* 头部：tab + 关闭。padding-top 叠 --safe-top：面板 top-0 贴的是屏幕物理顶边，
            iOS 独立 App（black-translucent + viewport-fit=cover）里状态栏正压在这一行上，
            不让位就会与 tab 胶囊糊在一起（见 globals.css 的安全区说明） */}
        <div className="flex items-center justify-between gap-3 border-b border-white/[0.08] px-5 py-3.5 [padding-top:calc(0.875rem+var(--safe-top))]">
          <div className="flex items-center gap-1.5">
            <button
              type="button"
              onClick={() => onSwitchTab("missing")}
              className={tabClass(open === "missing")}
            >
              缺失 {missing.length > 0 ? missing.length : ""}
            </button>
            <button
              type="button"
              onClick={() => onSwitchTab("unidentified")}
              className={tabClass(open === "unidentified")}
            >
              待识别 {unidentifiedFileTotal > 0 ? unidentifiedFileTotal : ""}
            </button>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label="关闭抽屉"
            className="btn-glass flex size-8 items-center justify-center !rounded-full"
          >
            <XIcon className="size-4" />
          </button>
        </div>

        {/* 工具行：过滤 + 批量操作 */}
        <div className="flex items-center gap-2.5 border-b border-white/[0.08] px-5 py-3">
          <input
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={open === "missing" ? "按片名过滤…" : "按文件名过滤…"}
            className="min-w-0 flex-1 rounded-lg border border-white/[0.08] bg-white/[0.04] px-3 py-1.5 text-sub text-[var(--text)] outline-none placeholder:text-white/30 focus:border-[var(--accent)]/60"
          />
          {open === "missing" && missing.length > 0 && (
            <button
              type="button"
              disabled={busy}
              onClick={async () => {
                if (
                  !(await confirm({
                    title: `清理全部 ${missingFileTotal} 条缺失记录？`,
                    description: "复查视频仍缺失后，删除记录及残留的字幕、NFO 和图片；已恢复的文件会保留。",
                    confirmLabel: "全部清理",
                    tone: "danger",
                  }))
                )
                  return;
                setBusy(true);
                void clearMissingLibraryRecords(libraryId)
                  .then(onChanged)
                  .catch((e) =>
                    toast.error(e instanceof Error ? e.message : "清理缺失记录失败"),
                  )
                  .finally(() => setBusy(false));
              }}
              className="btn-glass shrink-0 px-3 py-1.5 text-sub font-medium disabled:opacity-50"
            >
              全部清理
            </button>
          )}
        </div>

        {/* 说明行 */}
        <p className="px-5 pt-3 text-caption leading-5 text-[var(--text-muted)]">
          {open === "missing"
            ? "缺失文件恢复后会自动解除标记。「清理记录」会复查视频是否仍缺失，并删除缺失记录和残留附件；扫描媒体库时也会自动清理仍缺失的记录。"
            : "同一目录的文件聚成一组，认领一次整组生效；点候选可先核对海报简介再确认。状态标签悬停看完整原因。"}
        </p>

        {/* 「放错库了」修复引导：认领解决不了这一类，不给引导用户会在认领里打转 */}
        {open === "unidentified" && kindMismatchFiles > 0 && (
          <p className="mx-5 mt-3 rounded-lg bg-[var(--warn)]/[0.1] px-3 py-2 text-caption leading-5 text-[var(--warn)]">
            有 {kindMismatchFiles} 个文件的实际类型与本库不符（
            {movie ? "剧集文件在电影库" : "电影文件在剧集库"}
            ），认领无法解决。个别文件放错了：把文件移到对应类型的库即可；整库类型建错了：
            删除本库并以正确类型重建（删库不会动磁盘文件），重新扫描即可恢复。
          </p>
        )}

        {/* 列表区：独立滚动。面板底边已铺到物理底边，滚动到底时最后一行
            要停在安全区与视口截差之上，不被 Home 指示条压住 */}
        <div className="scroll-thin min-h-0 flex-1 space-y-2 overflow-y-auto px-5 pt-4 pb-[calc(1rem+var(--safe-bottom)+var(--vp-overshoot))]">
          {open === "missing" &&
            missingShown.map((item) => (
              <MissingRow
                key={item.media_item_id}
                libraryId={libraryId}
                item={item}
                onChanged={onChanged}
              />
            ))}
          {open === "unidentified" &&
            unidentifiedShown.map((group) => (
              <UnidentifiedGroupRow
                key={group.key}
                group={group}
                movie={movie}
                onChanged={onChanged}
              />
            ))}
          {((open === "missing" && missingShown.length === 0) ||
            (open === "unidentified" && unidentifiedShown.length === 0)) && (
            <p className="mt-12 text-center text-ui text-[var(--text-muted)]">
              {keyword ? "没有匹配的条目" : "没有需要处理的了 🎉"}
            </p>
          )}
        </div>
      </div>
    </div>,
    document.body,
  );
}

function MissingRow({
  libraryId,
  item,
  onChanged,
}: {
  libraryId: string;
  item: MissingItem;
  onChanged: () => void;
}) {
  const confirm = useConfirm();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // 清理完成的回执标签：写端随旧清理流程一起删了，这里保持只读（恒 null）
  const [done] = useState<string | null>(null);

  const act = (fn: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    void fn()
      .then(onChanged)
      .catch((e) => setError((e as Error).message))
      .finally(() => setBusy(false));
  };

  // 剧集：按季聚合缺失集数做摘要；电影：单文件
  const summary =
    item.kind === "tv"
      ? Array.from(
          item.files.reduce((m, f) => {
            m.set(f.season_number, (m.get(f.season_number) ?? 0) + 1);
            return m;
          }, new Map<number, number>()),
        )
          .sort(([a], [b]) => a - b)
          .map(([s, n]) => (s === 0 ? `特别篇 ${n} 集` : `第 ${s} 季缺 ${n} 集`))
          .join("、")
      : `${item.files.length} 个文件`;

  return (
    <div className="rounded-xl bg-white/[0.03] px-3.5 py-2.5">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1.5">
        <span className="min-w-0 flex-1 truncate text-ui font-medium text-white/85">
          {item.title}
          {item.year ? ` (${item.year})` : ""}
          <span className="ml-2 text-sub font-normal text-[var(--text-muted)]">{summary}</span>
        </span>
        {done ? (
          <span className="text-sub text-[var(--ok)]">{done}</span>
        ) : (
          <span className="flex shrink-0 items-center gap-2">
            <button
              type="button"
              disabled={busy}
              onClick={async () => {
                const ok = await confirm({
                  title: `清理「${item.title}」的 ${item.files.length} 条缺失记录？`,
                  description: item.subscription_id
                    ? "复查视频仍缺失后，删除记录及残留的字幕、NFO 和图片。该条目有正在追踪的订阅，清理后可能重新下载；已恢复的文件会保留。"
                    : "复查视频仍缺失后，删除记录及残留的字幕、NFO 和图片；已恢复的文件会保留。",
                  confirmLabel: "清理记录",
                  tone: "danger",
                });
                if (!ok) return;
                act(() => clearMissingLibraryRecords(libraryId, item.media_item_id));
              }}
              className="btn-glass px-3 py-1.5 text-sub font-medium disabled:opacity-50"
            >
              清理记录
            </button>
          </span>
        )}
      </div>
      {item.subscription_id && !done && (
        <p className="mt-1 text-caption text-[var(--warn)]/80">该条目有订阅在追踪</p>
      )}
      {error && <p className="mt-1 text-caption text-red-300">{error}</p>}
    </div>
  );
}
