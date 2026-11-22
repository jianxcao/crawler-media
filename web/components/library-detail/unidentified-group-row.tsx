"use client";

import { useState } from "react";

import * as DropdownMenu from "@radix-ui/react-dropdown-menu";

import {
  ClaimConfirmPanel,
  ClaimSearchPanel,
  type ClaimSeed,
  searchSeedFromLabel,
} from "@/components/claim-panels";
import { MoreIcon } from "@/components/icons";
import {
  claimUnidentifiedFile,
  startLibraryScan,
  type UnidentifiedGroup,
} from "@/lib/api/libraries";
import { formatBytes } from "@/lib/format";

/* —— 待识别组：整组认领或整组忽略 ——
   一部剧几十集是同一件事，逐集处理既刷屏又折磨人。认领一律走「详情确认
   面板」：点候选/搜索结果先看海报、简介、季数再确认——只看名字不足以在
   同名双版本（正片 46 集 vs 送审版 51 集）之间下判断。候选全不对时走
   TMDB 搜索（按片名检索、支持直接粘 ID），不再让用户手抄 TMDB ID。 */

/* 失败分类 → 标签：整句原因塞进 title，清单上只留一个能扫读的短标签。
   配色只分两级——**黄色专供"系统故障、重扫可自愈"**（TMDB 不可达），
   它有确定解法、不需要用户动脑；其余是正常待办不是错误，用中性色，
   否则满屏黄字等于没有信号。 */
const UNIDENTIFIED_BADGE: Record<
  string,
  { label: (g: UnidentifiedGroup) => string; tone: "warn" | "muted" }
> = {
  tmdb_unreachable: { label: () => "TMDB 不可达", tone: "warn" },
  // 库类型选错也是"有确定解法但需要动手"的一类：认领单个文件没用，得换库
  kind_mismatch: { label: () => "放错库了", tone: "warn" },
  ambiguous: { label: (g) => `${g.candidates.length} 个候选待定`, tone: "muted" },
  no_match: { label: () => "TMDB 无匹配", tone: "muted" },
  unparsable: { label: () => "认不出片名", tone: "muted" },
};

function GroupMoreMenu({
  expanded,
  canExpand,
  onToggleFiles,
}: {
  expanded: boolean;
  canExpand: boolean;
  onToggleFiles: () => void;
}) {
  const itemClass =
    "glass-row nav-item cursor-pointer px-3 py-2 text-ui font-medium outline-none " +
    "data-[highlighted]:!bg-[var(--glass-fill-hover)] data-[highlighted]:!text-[var(--text)] " +
    "data-[disabled]:pointer-events-none data-[disabled]:opacity-40";
  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger asChild>
        <button
          type="button"
          aria-label="更多操作"
          // 常显但压暗：纯 hover 显示的控件在触屏上根本点不到（NAS 常用平板访问）
          className="btn-glass shrink-0 !rounded-full px-1.5 py-1 opacity-45 transition hover:opacity-100 group-hover/card:opacity-100 focus-visible:opacity-100 data-[state=open]:opacity-100"
        >
          <MoreIcon className="size-4" />
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          align="end"
          sideOffset={6}
          collisionPadding={12}
          // Escape 只关菜单：Radix 在 document 上处理，不拦住就会继续冒泡到
          // 抽屉挂在 window 的监听，把整个抽屉一起关掉（菜单照常关闭——
          // stopPropagation 不影响 Radix 自己的 dismiss）
          onEscapeKeyDown={(e) => e.stopPropagation()}
          className="menu-surface z-[60] min-w-[9rem] p-1"
        >
          {canExpand && (
            <DropdownMenu.Item onSelect={onToggleFiles} className={itemClass}>
              {expanded ? "收起文件" : "查看文件"}
            </DropdownMenu.Item>
          )}
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}

function StatusBadge({ group }: { group: UnidentifiedGroup }) {
  // 旧数据没有 code：退回按有无候选粗分，不至于空着
  const meta =
    (group.code && UNIDENTIFIED_BADGE[group.code]) ??
    (group.candidates.length > 0
      ? UNIDENTIFIED_BADGE.ambiguous
      : UNIDENTIFIED_BADGE.no_match);
  return (
    <span
      title={group.reason ?? undefined}
      className={`shrink-0 rounded-md px-1.5 py-0.5 text-caption ${
        meta.tone === "warn"
          ? "bg-[var(--warn)]/[0.16] text-[var(--warn)]"
          : "bg-white/[0.07] text-[var(--text-muted)]"
      }`}
    >
      {meta.label(group)}
    </span>
  );
}

export function UnidentifiedGroupRow({
  group,
  movie,
  onChanged,
}: {
  group: UnidentifiedGroup;
  movie: boolean;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(false);
  // 卡片内联面板：确认（点候选/搜索结果后看详情再定）或搜索；同时只开一个
  const [panel, setPanel] = useState<
    { view: "confirm"; seed: ClaimSeed } | { view: "search" } | null
  >(null);

  const act = (fn: () => Promise<unknown>) => {
    setBusy(true);
    setError(null);
    void fn()
      .then(onChanged)
      .catch((e) => setError((e as Error).message))
      .finally(() => setBusy(false));
  };

  const selectedId = panel?.view === "confirm" ? panel.seed.tmdbId : null;
  const unreachable = group.code === "tmdb_unreachable";
  const searchOpen = panel?.view === "search";

  return (
    <div className="group/card rounded-xl bg-white/[0.03] px-3.5 py-2.5">
      {/* 头行一行说完"是谁、什么状态、多大、能做什么"：整句原因收进标签的
          title，次要动作收进 ⋯ 菜单——组多起来时这一行就是全部扫读内容 */}
      <div className="flex items-center gap-2">
        <span
          className="min-w-0 flex-1 truncate text-ui font-medium text-white/85"
          title={`${group.label}\n${group.key}`}
        >
          {group.label}
        </span>
        <StatusBadge group={group} />
        <span className="shrink-0 text-caption text-[var(--text-faint)]">
          {group.file_count} 个 · {formatBytes(group.total_size_bytes)}
        </span>
        {/* TMDB 不可达有确定解法，直接给按钮；其余给搜索入口 */}
        {unreachable ? (
          <button
            type="button"
            disabled={busy}
            onClick={() => act(() => startLibraryScan(group.library_id))}
            className="btn-glass shrink-0 px-2.5 py-1 text-sub font-medium disabled:opacity-40"
          >
            重新扫描
          </button>
        ) : (
          <button
            type="button"
            disabled={busy}
            onClick={() => setPanel((p) => (p?.view === "search" ? null : { view: "search" }))}
            className={`shrink-0 rounded-full px-2.5 py-1 text-sub font-medium transition disabled:opacity-40 ${
              searchOpen ? "bg-white/[0.14] text-white" : "btn-glass"
            }`}
          >
            搜索
          </button>
        )}
        <GroupMoreMenu
          expanded={expanded}
          canExpand={group.file_count > 1}
          onToggleFiles={() => setExpanded((v) => !v)}
        />
      </div>

      {/* no_match 的两条真实出口写在卡片上：条目常常是晚几周才被社区补上
          （待识别行每次扫描本就自动重试），等不及可以自己去补录。不写清楚，
          用户会以为搜不到就只剩忽略一条路 */}
      {group.code === "no_match" && (
        <p className="mt-1 text-caption leading-relaxed text-[var(--text-faint)]">
          每次扫描会自动重试识别。TMDB 是社区维护的数据库，确认缺这个条目可以
          <a
            href={`https://www.themoviedb.org/${movie ? "movie" : "tv"}/new`}
            target="_blank"
            rel="noreferrer"
            className="underline decoration-white/20 underline-offset-2 transition hover:text-white/80"
          >
            去 TMDB 补录 ↗
          </a>
          ，收录后回来搜索认领；自录、花絮这类本就不会有条目的内容用「忽略」即可。
        </p>
      )}

      {/* 候选：机器给出的可能匹配。点击先进详情确认面板，不直接认领 */}
      {group.candidates.length > 0 && (
        <div className="mt-1.5 flex flex-wrap gap-1.5">
          {group.candidates.map((c) => (
            <button
              key={c.tmdb_id}
              type="button"
              disabled={busy}
              onClick={() =>
                setPanel(
                  selectedId === c.tmdb_id
                    ? null
                    : {
                        view: "confirm",
                        seed: {
                          tmdbId: c.tmdb_id,
                          title: c.title,
                          year: c.year,
                          episodeCount: c.episode_count,
                          reasons: c.reasons,
                        },
                      },
                )
              }
              className={`rounded-lg border px-2.5 py-1 text-sub transition disabled:opacity-40 ${
                selectedId === c.tmdb_id
                  ? "border-[var(--accent)] bg-[var(--accent)]/[0.28] text-white"
                  : "border-[var(--accent)]/40 bg-[var(--accent)]/[0.12] text-white/90 hover:bg-[var(--accent)]/[0.24]"
              }`}
            >
              {c.title}
              {c.year ? ` (${c.year})` : ""}
              {c.episode_count ? (
                <span className="ml-1.5 text-caption text-[var(--text-muted)]">
                  {c.episode_count} 集
                </span>
              ) : null}
            </button>
          ))}
        </div>
      )}

      {error && <p className="mt-1.5 text-caption text-red-300">{error}</p>}

      {panel?.view === "search" && (
        <ClaimSearchPanel
          movie={movie}
          initialQuery={searchSeedFromLabel(group.label)}
          onPick={(seed) => setPanel({ view: "confirm", seed })}
        />
      )}
      {panel?.view === "confirm" && (
        <ClaimConfirmPanel
          key={panel.seed.tmdbId}
          seed={panel.seed}
          movie={movie}
          fileCount={group.file_count}
          busy={busy}
          onConfirm={({ title, year }) =>
            act(async () => {
              // 未识别文件按路径认领为新识别作品，逐个提交；任一失败即报错。
              for (const f of group.files) {
                await claimUnidentifiedFile({
                  path: f.file_path,
                  title,
                  kind: movie ? "movie" : "tv",
                  year,
                  tmdb_id: String(panel.seed.tmdbId),
                });
              }
            })
          }
          onCancel={() => setPanel(null)}
        />
      )}

      {expanded && (
        <ul className="mt-2 space-y-1 border-t border-white/[0.06] pt-2">
          {group.files.map((f) => (
            <li
              key={f.id}
              className="truncate font-mono text-caption text-[var(--text-faint)]"
              title={f.file_path}
            >
              {f.file_path}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
