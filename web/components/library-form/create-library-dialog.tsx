"use client";

import { useEffect, useState } from "react";

import { InfoIcon } from "@/components/icons";
import { LIBRARY_KIND_META } from "@/components/library-kind-meta";
import { Modal } from "@/components/modal";
import {
  createLibrary,
  type LibraryAccessMode,
  type LibraryPayload,
  type MediaLibrary,
} from "@/lib/api/libraries";
import { listRuleSets, type RuleSet } from "@/lib/api/subscriptions";
import type { LibraryKind } from "@/lib/media-types";

import { AccessScopeEditor } from "./access-scope-editor";
import {
  buildMatchRules,
  INPUT_CLASS,
  KIND_CARDS,
  kindExcludedFromHome,
  kindIsScraped,
  LABEL_CLASS,
  scopeSummary,
  useRoutingOptions,
  validGenres,
} from "./form-helpers";
import { RootsEditor, RootsPicker } from "./roots-editor";
import { ScopeEditor } from "./scope-editor";

/* —— 新建：选类型 → 名称与目录 → （影视库）收藏范围 —— */

export function CreateLibraryDialog({
  onClose,
  onSaved,
}: {
  onClose: () => void;
  onSaved: (saved: MediaLibrary) => void;
}) {
  const [step, setStep] = useState<1 | 2 | 3>(1);
  const [kind, setKind] = useState<LibraryKind | null>(null);
  const [name, setName] = useState("");
  const [roots, setRoots] = useState<string[]>([]);
  const [pickerTarget, setPickerTarget] = useState<"add" | number | null>(null);
  const [regions, setRegions] = useState<string[]>([]);
  const [genres, setGenres] = useState<number[]>([]);
  const [accessMode, setAccessMode] = useState<LibraryAccessMode>("everyone");
  const [adminVisible, setAdminVisible] = useState(true);
  const [memberIds, setMemberIds] = useState<string[]>([]);
  const [defaultFilterId, setDefaultFilterId] = useState<string>("");
  const [ruleSets, setRuleSets] = useState<RuleSet[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const routingOptions = useRoutingOptions();

  useEffect(() => {
    void listRuleSets().then(setRuleSets).catch(() => {});
  }, []);

  const hasScope = kind !== null && kindIsScraped(kind);
  const totalSteps = hasScope ? 3 : 2;
  const ready = name.trim().length > 0 && roots.length > 0;
  const scope = kind ? scopeSummary(kind, regions, genres, routingOptions) : null;

  const chooseKind = (next: LibraryKind) => {
    // 名称按类型预填；用户已经改过的名字不动
    const untouched = name.trim() === "" || (kind !== null && name === KIND_CARDS[kind].defaultName);
    setKind(next);
    if (untouched) setName(KIND_CARDS[next].defaultName);
    if (!kindIsScraped(next)) {
      setRegions([]);
      setGenres([]);
    }
    setStep(2);
  };

  const submit = () => {
    if (kind === null || !ready || busy) return;
    setBusy(true);
    setError(null);
    const payload: LibraryPayload = {
      name: name.trim(),
      kind,
      root_paths: roots,
      match_rules: hasScope ? buildMatchRules(validGenres(kind, genres, routingOptions), regions) : [],
      default_filter_id: defaultFilterId ? defaultFilterId : null,
      // 开关全按推荐值：监控开、自动清理关、封面开、场景图开、首页展示；建好后在编辑里调
      realtime_watch: true,
      generate_thumbnails: true,
      extract_chapter_images: true,
      exclude_from_home: kindExcludedFromHome(),
      detect_intros: false,
      enable_fingerprint: false,
      access_mode: accessMode,
      admin_visible: adminVisible,
      member_ids: accessMode === "selected" ? memberIds : [],
    };
    void createLibrary(payload)
      .then(onSaved)
      .catch((e) => setError((e as Error).message))
      .finally(() => setBusy(false));
  };

  // 主按钮随步骤变化：第 2 步对影视库是「下一步」，最后一步保存即扫描
  const primary =
    step === 1
      ? { label: "创建并开始扫描", disabled: true, action: () => {} }
      : step === 2 && hasScope
        ? { label: "下一步：收藏范围", disabled: !ready, action: () => setStep(3) }
        : {
            label: busy
              ? "创建中…"
              : step === 3 && scope !== null && !scope.declared
                ? "跳过，创建并开始扫描"
                : "创建并开始扫描",
            disabled: !ready || busy,
            action: submit,
          };
  const footNote =
    `第 ${step} 步，共 ${totalSteps} 步` + (step === totalSteps ? " · 保存即开始扫描存量文件" : "");

  return (
    <>
      <Modal open onClose={onClose} label="添加媒体库" width="2xl" panelClassName="flex max-h-[86dvh] flex-col">
        <div className="shrink-0 border-b border-white/[0.06] px-6 pt-5 max-md:px-5">
          <h2 className="text-title font-bold text-white">添加媒体库</h2>
          <ol className="mt-3.5 flex gap-5 text-caption font-semibold uppercase tracking-[0.06em]">
            {(
              [
                [1, "1 · 选类型"],
                [2, "2 · 名称与目录"],
                [3, "3 · 收藏范围"],
              ] as const
            )
              .filter(([n]) => n !== 3 || hasScope)
              .map(([n, label]) => (
                <li
                  key={n}
                  aria-current={step === n ? "step" : undefined}
                  className={`relative pb-2.5 ${step === n ? "text-white" : "text-[var(--text-faint)]"}`}
                >
                  {label}
                  {step === n && (
                    <span className="absolute inset-x-0 bottom-0 h-0.5 rounded-full bg-[var(--accent)]" />
                  )}
                </li>
              ))}
          </ol>
        </div>

        <div className="scroll-thin min-h-0 flex-1 space-y-4 overflow-y-auto p-6 max-md:p-5">
          {step === 1 && (
            <>
              <div className="grid grid-cols-2 gap-2.5 max-md:grid-cols-1">
                {(Object.keys(LIBRARY_KIND_META) as LibraryKind[]).map((k) => {
                  const { label, Icon } = LIBRARY_KIND_META[k];
                  const card = KIND_CARDS[k];
                  return (
                    <button
                      key={k}
                      type="button"
                      onClick={() => chooseKind(k)}
                      data-active={kind === k}
                      className="flex min-h-[148px] flex-col gap-2.5 rounded-2xl border border-white/[0.08] bg-white/[0.04] p-4 text-left transition-colors hover:border-white/20 hover:bg-white/[0.075] data-[active=true]:border-[var(--accent-2)] data-[active=true]:bg-white/[0.12]"
                    >
                      <Icon className="size-[22px] text-[var(--accent)]" />
                      <span className="text-body font-semibold text-white">{label}</span>
                      <span className="text-caption leading-relaxed text-[var(--text-muted)]">{card.blurb}</span>
                      <span className="mt-auto text-micro text-[var(--text-faint)]">{card.traits}</span>
                    </button>
                  );
                })}
              </div>
              <p className="text-caption leading-relaxed text-[var(--text-faint)]">
                类型决定这个库怎么扫、怎么摆、能不能订阅，创建后不可更改。
              </p>
            </>
          )}

          {step === 2 && kind !== null && (
            <>
              {(() => {
                const { label, Icon } = LIBRARY_KIND_META[kind];
                return (
                  <div className="flex items-center gap-2.5 rounded-xl border border-white/[0.08] bg-white/[0.04] px-3 py-2.5">
                    <Icon className="size-[18px] shrink-0 text-[var(--accent)]" />
                    <span className="font-semibold text-white">{label}</span>
                    <span className="min-w-0 truncate text-caption text-[var(--text-muted)]">
                      {KIND_CARDS[kind].chosen}
                    </span>
                    <button
                      type="button"
                      onClick={() => setStep(1)}
                      className="ml-auto shrink-0 text-caption text-[var(--text-faint)] underline underline-offset-4 hover:text-white"
                    >
                      换类型
                    </button>
                  </div>
                );
              })()}
              <div>
                <label className={LABEL_CLASS}>名称</label>
                <input
                  type="text"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  onKeyDown={(e) => {
                    // isComposing：中文输入法选词的回车不当提交
                    if (e.key === "Enter" && !e.nativeEvent.isComposing && !primary.disabled) primary.action();
                  }}
                  placeholder="如：电影库 / 动漫库"
                  autoComplete="off"
                  className={INPUT_CLASS}
                />
              </div>
              <div>
                <label className={LABEL_CLASS}>
                  根目录
                  <span className="font-normal text-[var(--text-faint)]">（第一个为主根，新内容落在这里）</span>
                </label>
                <RootsEditor roots={roots} onChange={setRoots} onPick={setPickerTarget} />
              </div>
              <div>
                <label className={LABEL_CLASS}>可见范围</label>
                <AccessScopeEditor
                  mode={accessMode}
                  adminVisible={adminVisible}
                  memberIds={memberIds}
                  onMode={setAccessMode}
                  onAdminVisible={setAdminVisible}
                  onMemberIds={setMemberIds}
                />
              </div>
              <div className="flex items-start gap-2.5 rounded-xl border border-[var(--info)]/20 bg-[var(--info)]/[0.07] px-3.5 py-3 text-caption leading-relaxed text-[var(--text-muted)]">
                <InfoIcon className="mt-0.5 size-4 shrink-0 text-[var(--info)]" />
                <p>
                  <strong className="font-medium text-[var(--text)]">已按推荐值设好：</strong>
                  {KIND_CARDS[kind].defaults}
                </p>
              </div>
            </>
          )}

          {step === 3 && kind !== null && (
            <>
              <p className="text-sub leading-relaxed text-[var(--text-muted)]">
                声明「本库收什么」，订阅与自动入库就会按作品的区域和类型自动选进这个库。
                <strong className="font-medium text-[var(--text)]">全部留空也可以</strong>
                ：作为默认库承接所有未命中的作品；订阅时永远可以手动改库。
              </p>
              <ScopeEditor
                kind={kind}
                regions={regions}
                genres={genres}
                onRegions={setRegions}
                onGenres={setGenres}
                options={routingOptions}
              />
              <p className="text-caption leading-relaxed text-[var(--text-faint)]">
                {scope?.declared
                  ? `当前：收 ${scope.text}；其他作品去该类型的默认库。`
                  : "当前：未声明，本库将承接该类型全部未命中的作品。"}
              </p>

              <div className="pt-2">
                <label className="mb-1.5 block text-caption font-semibold uppercase tracking-[0.06em] text-[var(--text-faint)]">
                  默认过滤规则组（可选）
                </label>
                <p className="mb-2 text-caption leading-relaxed text-[var(--text-muted)]">
                  新订阅归属到该媒体库时，默认继承此规则组（画质/排除项）。不选则使用全局默认规则。
                </p>
                <select
                  value={defaultFilterId}
                  onChange={(e) => setDefaultFilterId(e.target.value)}
                  className="w-full rounded-xl border border-white/[0.08] bg-white/[0.04] px-3 py-2 text-ui text-[var(--text)] outline-none focus:border-[var(--accent)]/60"
                >
                  <option value="">使用系统全局默认规则</option>
                  {ruleSets.map((rs) => (
                    <option key={rs.id} value={String(rs.id)}>
                      {rs.name} {rs.is_default ? "（全局默认）" : ""}
                    </option>
                  ))}
                </select>
              </div>
            </>
          )}
        </div>

        <div className="shrink-0 border-t border-white/[0.06] px-6 py-4 max-md:px-5">
          {error && (
            <p className="mb-3 rounded-lg border border-red-400/25 bg-red-500/10 px-3.5 py-2.5 text-ui leading-6 text-red-200">
              {error}
            </p>
          )}
          <div className="flex items-center justify-between gap-3">
            <p className="min-w-0 truncate text-caption text-[var(--text-faint)]">{footNote}</p>
            <div className="flex shrink-0 items-center gap-3">
              <button
                type="button"
                onClick={step === 1 ? onClose : () => setStep(step === 3 ? 2 : 1)}
                className="btn-glass h-9 px-4 text-ui font-medium"
              >
                {step === 1 ? "取消" : "上一步"}
              </button>
              <button
                type="button"
                onClick={primary.action}
                disabled={primary.disabled}
                className="btn-accent h-9 rounded-full px-5 text-ui font-semibold disabled:opacity-40"
              >
                {primary.label}
              </button>
            </div>
          </div>
        </div>
      </Modal>
      <RootsPicker target={pickerTarget} roots={roots} onChange={setRoots} onClose={() => setPickerTarget(null)} />
    </>
  );
}
