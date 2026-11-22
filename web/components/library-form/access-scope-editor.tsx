"use client";

import { useEffect, useRef, useState } from "react";

import { CheckIcon, XIcon } from "@/components/icons";
import type { LibraryAccessMode } from "@/lib/api/libraries";
import { listMembers, type MemberView } from "@/lib/api/members";

import { INPUT_CLASS } from "./form-helpers";

const ADMIN_OPTION_ID = "admin" as const;
type ViewerOptionId = string;
interface ViewerOption {
  id: ViewerOptionId;
  label: string;
  hint: string;
}

/**
 * 可见范围编辑器：
 * 「所有成员」= 对全部成员自动开放（含以后新建的）；「指定成员」= 只对勾选的
 * 成员开放，名单第一项固定是「超管（我自己）」——超管不是成员，进不了白名单，
 * 单独一位存在 admin_visible 上，但对用户就是同一份名单。
 * 管理权与浏览权分离：超管把自己摘掉后仍能管理这个库，只是看不到内容。
 */
export function AccessScopeEditor({
  mode,
  adminVisible,
  memberIds,
  onMode,
  onAdminVisible,
  onMemberIds,
}: {
  mode: LibraryAccessMode;
  adminVisible: boolean;
  memberIds: string[];
  onMode: (next: LibraryAccessMode) => void;
  onAdminVisible: (next: boolean) => void;
  onMemberIds: (next: string[]) => void;
}) {
  const [members, setMembers] = useState<MemberView[] | null>(null);
  useEffect(() => {
    let cancelled = false;
    listMembers()
      .then((rows) => {
        if (!cancelled) setMembers(rows);
      })
      .catch(() => {
        if (!cancelled) setMembers([]);
      });
    return () => {
      cancelled = true;
    };
  }, []);
  const modeButton = (value: LibraryAccessMode, label: string) => (
    <button
      type="button"
      onClick={() => onMode(value)}
      className={`rounded-md px-3 py-1.5 text-sub font-medium transition-colors ${
        mode === value ? "bg-white/[0.11] text-white" : "text-[var(--text-faint)] hover:text-[var(--text)]"
      }`}
    >
      {label}
    </button>
  );
  const nobody = mode === "selected" && !adminVisible && memberIds.length === 0;
  // 超管与成员同列一份名单：超管用固定 id "admin"，成员用字符串 id
  const options: ViewerOption[] = [
    { id: ADMIN_OPTION_ID, label: "超管（我自己）", hint: "你自己" },
    ...(members ?? []).map((m) => ({
      id: m.id,
      label: m.login,
      hint: `@${m.login}`,
    })),
  ];
  const selected: ViewerOptionId[] = [
    ...(adminVisible ? [ADMIN_OPTION_ID as ViewerOptionId] : []),
    ...memberIds,
  ];
  const setSelected = (next: ViewerOptionId[]) => {
    onAdminVisible(next.includes(ADMIN_OPTION_ID));
    onMemberIds(next.filter((v) => v !== ADMIN_OPTION_ID));
  };
  return (
    <div>
      <div className="inline-flex rounded-lg border border-white/[0.08] bg-white/[0.035] p-1">
        {modeButton("everyone", "所有成员")}
        {modeButton("selected", "指定成员")}
      </div>
      <p className="mt-2 text-caption leading-relaxed text-[var(--text-faint)]">
        {mode === "everyone"
          ? "对全部成员开放，包括以后新建的成员；成员管理页里切到「指定库」的成员除外。"
          : "只有选中的人能浏览这个库的内容；其他人在首页、搜索、最近观看和播放器里都看不到它。你始终可以管理它。"}
      </p>
      {mode === "selected" && (
        <div className="mt-3">
          <ViewerCombobox
            options={options}
            selected={selected}
            onChange={setSelected}
            loading={members === null}
          />
        </div>
      )}
      {nobody && (
        <p className="mt-2.5 text-caption leading-relaxed text-[var(--warn)]">
          当前没有任何人能浏览这个库的内容，包括你自己。你仍然可以在这里管理它。
        </p>
      )}
    </div>
  );
}

/**
 * 可见成员的多选下拉：输入关键字过滤（昵称 / 用户名都匹配），列表里点选或
 * 回车切换，已选的人显示为可移除的标签。与成员管理页的选择方式一致，
 * 成员一多也不会铺成一大片按钮。
 */
function ViewerCombobox({
  options,
  selected,
  onChange,
  loading,
}: {
  options: ViewerOption[];
  selected: ViewerOptionId[];
  onChange: (next: ViewerOptionId[]) => void;
  loading: boolean;
}) {
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const rootRef = useRef<HTMLDivElement>(null);
  const keyword = query.trim().toLowerCase();
  const matches = options.filter(
    (o) => keyword === "" || o.label.toLowerCase().includes(keyword) || o.hint.toLowerCase().includes(keyword),
  );
  const toggle = (id: ViewerOptionId) =>
    onChange(selected.includes(id) ? selected.filter((v) => v !== id) : [...selected, id]);
  // 点到组件外面就收起
  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open]);
  useEffect(() => setActive(0), [keyword]);
  const chosen = selected
    .map((id) => options.find((o) => o.id === id))
    .filter((o): o is ViewerOption => o !== undefined);

  return (
    <div ref={rootRef} className="relative">
      <div
        className={`${INPUT_CLASS} flex min-h-[42px] flex-wrap items-center gap-1.5 !py-1.5`}
        onClick={() => rootRef.current?.querySelector("input")?.focus()}
      >
        {chosen.map((o) => (
          <span
            key={String(o.id)}
            className="flex items-center gap-1 rounded-md border border-[var(--accent)]/40 bg-[var(--accent-soft)] py-0.5 pl-2 pr-1 text-sub font-medium text-[var(--accent)]"
          >
            {o.label}
            <button
              type="button"
              aria-label={`移除 ${o.label}`}
              onClick={(e) => {
                e.stopPropagation();
                toggle(o.id);
              }}
              className="grid size-4 place-items-center rounded hover:bg-white/15"
            >
              <XIcon className="size-3" />
            </button>
          </span>
        ))}
        <input
          type="text"
          role="combobox"
          aria-expanded={open}
          aria-controls="viewer-combobox-list"
          value={query}
          placeholder={chosen.length === 0 ? "输入昵称或用户名搜索，选择可浏览的人" : "继续添加…"}
          onChange={(e) => {
            setQuery(e.target.value);
            setOpen(true);
          }}
          onFocus={() => setOpen(true)}
          onKeyDown={(e) => {
            if (e.nativeEvent.isComposing) return;
            if (e.key === "ArrowDown") {
              e.preventDefault();
              setOpen(true);
              setActive((i) => Math.min(i + 1, Math.max(matches.length - 1, 0)));
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setActive((i) => Math.max(i - 1, 0));
            } else if (e.key === "Enter") {
              e.preventDefault();
              if (open && matches[active]) toggle(matches[active].id);
            } else if (e.key === "Escape") {
              setOpen(false);
            } else if (e.key === "Backspace" && query === "" && chosen.length > 0) {
              toggle(chosen[chosen.length - 1].id);
            }
          }}
          className="min-w-[8rem] flex-1 bg-transparent text-ui text-[var(--text)] outline-none placeholder:text-[var(--text-faint)]"
        />
      </div>
      {open && (
        <div
          id="viewer-combobox-list"
          role="listbox"
          aria-multiselectable
          // 就地展开而不是浮层：编辑对话框的分区容器带 overflow-hidden，浮层会被裁掉
          className="mt-1.5 max-h-56 overflow-y-auto rounded-xl border border-white/[0.08] bg-white/[0.04] p-1"
        >
          {loading ? (
            <p className="px-3 py-2 text-caption text-[var(--text-faint)]">正在读取成员…</p>
          ) : matches.length === 0 ? (
            <p className="px-3 py-2 text-caption text-[var(--text-faint)]">没有匹配的成员</p>
          ) : (
            matches.map((o, index) => {
              const on = selected.includes(o.id);
              return (
                <button
                  key={String(o.id)}
                  type="button"
                  role="option"
                  aria-selected={on}
                  onMouseEnter={() => setActive(index)}
                  onClick={() => toggle(o.id)}
                  className={`glass-row nav-item flex w-full items-center gap-2.5 px-3 py-2 text-left text-ui ${
                    index === active ? "!bg-[var(--glass-fill-hover)] !text-[var(--text)]" : ""
                  }`}
                >
                  <span
                    className={`grid size-4 shrink-0 place-items-center rounded border ${
                      on ? "border-[var(--accent)] bg-[var(--accent)] text-white" : "border-white/25"
                    }`}
                  >
                    {on && <CheckIcon className="size-3" />}
                  </span>
                  <span className="min-w-0 flex-1 truncate font-medium">{o.label}</span>
                  <span className="shrink-0 text-caption text-[var(--text-faint)]">{o.hint}</span>
                </button>
              );
            })
          )}
        </div>
      )}
    </div>
  );
}

/** 可见范围的一句话摘要（编辑对话框的分区摘要行）。 */
export function accessScopeSummary(
  mode: LibraryAccessMode,
  adminVisible: boolean,
  memberCount: number,
): string {
  if (mode === "everyone") {
    return adminVisible ? "所有成员" : "所有成员（我自己不可见）";
  }
  const parts = [adminVisible ? "我自己" : null, memberCount > 0 ? `${memberCount} 位成员` : null].filter(
    (v): v is string => v !== null,
  );
  return parts.length > 0 ? `指定成员：${parts.join(" + ")}` : "指定成员：暂无任何人可浏览";
}
