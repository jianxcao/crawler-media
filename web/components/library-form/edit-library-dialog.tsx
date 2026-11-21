"use client";

import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";

import { ChevronDownIcon } from "@/components/icons";
import { LIBRARY_KIND_META } from "@/components/library-kind-meta";
import { Modal } from "@/components/modal";
import { Tooltip } from "@/components/tooltip";
import {
  getLibraryOperation,
  type LibraryAccessMode,
  type LibraryOperation,
  type LibraryUpdatePayload,
  type MediaLibrary,
  reindexingOperationId,
  retryLibraryOperationAndWait,
  updateLibraryAndWait,
} from "@/lib/api/libraries";
import { listRuleSets, type RuleSet } from "@/lib/api/subscriptions";
import { LibraryOperationError, operationLabel } from "@/lib/library-operations";

import { AccessScopeEditor, accessScopeSummary } from "./access-scope-editor";
import {
  buildMatchRules,
  kindExcludedFromHome,
  LABEL_CLASS,
  normalizeRoots,
  parseMatchRules,
  sameRoots,
  scopeSummary,
  useRoutingOptions,
  validGenres,
} from "./form-helpers";
import { RootsEditor, RootsPicker } from "./roots-editor";
import { ScopeEditor } from "./scope-editor";

/* —— 编辑：分区折叠，每区一行摘要 —— */

type EditSectionId = "basic" | "scan" | "access" | "scope";

/** 开关行：一句话标题 + ⓘ 长说明 + 开关；长篇解释不再常驻表单里。 */
function SwitchRow({
  title,
  detail,
  checked,
  onChange,
}: {
  title: string;
  detail: ReactNode;
  checked: boolean;
  onChange: (next: boolean) => void;
}) {
  return (
    <div className="flex items-center justify-between gap-3 border-t border-white/[0.06] py-2.5 first:border-t-0">
      <div className="flex items-center gap-1.5 text-ui font-medium text-[var(--text)]">
        {title}
        <Tooltip content={detail} openOnClick>
          <button
            type="button"
            aria-label={`${title}的说明`}
            className="grid size-4 place-items-center rounded-full border border-white/[0.14] text-[10px] font-semibold text-[var(--text-faint)] hover:border-[var(--accent-2)] hover:text-white"
          >
            i
          </button>
        </Tooltip>
      </div>
      <button
        type="button"
        role="switch"
        aria-checked={checked}
        aria-label={title}
        onClick={() => onChange(!checked)}
        className="relative h-[22px] w-[38px] shrink-0 rounded-full bg-white/20 transition-colors aria-checked:bg-[var(--ok,#5fd39b)]"
      >
        <span
          className={`absolute left-[3px] top-[3px] size-4 rounded-full bg-white transition-transform ${
            checked ? "translate-x-4" : ""
          }`}
        />
      </button>
    </div>
  );
}

export function EditLibraryDialog({
  library,
  onClose,
  onSaved,
}: {
  library: MediaLibrary;
  onClose: () => void;
  onSaved: (saved: MediaLibrary) => void;
}) {
  // 新契约的 MediaLibrary 不再带能力位 / 收藏范围 / 扫描开关 / 访问控制字段
  // （对应端点已删），编辑表单按类型形态给安全默认值
  const scraped = library.kind === "movie" || library.kind === "tv";
  // 图片库：条目只看不播，缩略图从原图缩放而来（不是抓帧），开关文案随之换
  const playable = true;
  const [name, setName] = useState(library.name);
  const [roots, setRoots] = useState<string[]>(library.root_paths);
  const [pickerTarget, setPickerTarget] = useState<"add" | number | null>(null);
  const [realtimeWatch, setRealtimeWatch] = useState(library.realtime_watch ?? true);
  const [generateThumbnails, setGenerateThumbnails] = useState(library.generate_thumbnails ?? true);
  const [extractChapterImages, setExtractChapterImages] = useState(library.extract_chapter_images ?? true);
  const [excludeFromHome, setExcludeFromHome] = useState(
    library.exclude_from_home ?? kindExcludedFromHome(),
  );
  const [autoSeriesCollections, setAutoSeriesCollections] = useState(library.auto_series_collections ?? true);
  const [accessMode, setAccessMode] = useState<LibraryAccessMode>("everyone");
  const [adminVisible, setAdminVisible] = useState(true);
  const [memberIds, setMemberIds] = useState<string[]>([]);
  const [detectIntros, setDetectIntros] = useState(library.detect_intros ?? false);
  const [enableFingerprint, setEnableFingerprint] = useState(library.enable_fingerprint ?? false);
  const [defaultFilterId, setDefaultFilterId] = useState<string>(library.default_filter_id ?? "");
  const [ruleSets, setRuleSets] = useState<RuleSet[]>([]);
  const parsedRules = useMemo(() => parseMatchRules(library.match_rules ?? []), [library.match_rules]);
  const [regions, setRegions] = useState<string[]>(parsedRules.regions);
  const [genres, setGenres] = useState<number[]>(parsedRules.genres);
  // 展开的分区（同时只开一个）；默认全收起——摘要行已经把现状说清
  const [open, setOpen] = useState<EditSectionId | null>(null);
  const [busy, setBusy] = useState(false);
  const [pending, setPending] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [failedOperation, setFailedOperation] = useState<LibraryOperation | null>(null);
  const abortRef = useRef<AbortController | null>(null);
  const routingOptions = useRoutingOptions();

  useEffect(() => {
    void listRuleSets().then(setRuleSets).catch(() => {});
  }, []);

  useEffect(
    () => () => {
      abortRef.current?.abort();
    },
    [],
  );

  const ready = name.trim().length > 0 && roots.length > 0;
  const missing = [name.trim() ? null : "名称", roots.length > 0 ? null : "根目录"].filter(
    (v): v is string => v !== null,
  );
  const scope = scopeSummary(library.kind, regions, genres, routingOptions);
  const { label: kindLabel, Icon: KindIcon } = LIBRARY_KIND_META[library.kind];

  const handleOperationError = async (err: unknown, signal: AbortSignal) => {
    if (signal.aborted) return;
    if (err instanceof LibraryOperationError) {
      setFailedOperation(err.operation);
      setError(err.message);
      return;
    }
    const opId = reindexingOperationId(err);
    if (opId) {
      try {
        const operation = await getLibraryOperation(library.id, opId, signal);
        if (signal.aborted) return;
        if (operation.state === "failed") {
          setFailedOperation(operation);
          setError(operation.error ?? (err instanceof Error ? err.message : "媒体库操作失败，可重新尝试"));
          return;
        }
      } catch {
        if (signal.aborted) return;
      }
    }
    setError(err instanceof Error ? err.message : "保存失败");
  };

  const retryOperation = () => {
    if (!failedOperation || busy) return;
    const operationId = failedOperation.operation_id;
    const controller = new AbortController();
    abortRef.current?.abort();
    abortRef.current = controller;
    setBusy(true);
    setError(null);
    setPending("正在重试操作…");
    void retryLibraryOperationAndWait(library.id, operationId, {
      signal: controller.signal,
      onProgress: (operation) =>
        setPending(operation.state === "failed" ? "正在重试操作…" : operationLabel(operation)),
    })
      .then((saved) => {
        if (controller.signal.aborted) return;
        setFailedOperation(null);
        onSaved(saved);
      })
      .catch((err) => handleOperationError(err, controller.signal))
      .finally(() => {
        if (abortRef.current === controller) abortRef.current = null;
        if (!controller.signal.aborted) {
          setBusy(false);
          setPending(null);
        }
      });
  };

  const submit = () => {
    if (failedOperation) {
      retryOperation();
      return;
    }
    if (!ready || busy) return;
    const controller = new AbortController();
    abortRef.current?.abort();
    abortRef.current = controller;
    setBusy(true);
    setError(null);
    setPending(null);
    // 换目录是异步操作（202 + operation_id）：只有 root_paths 真的变了才带上这个字段，
    // 否则后端会把「改名」也冻结整库去迁移文件归属。
    const nextRoots = normalizeRoots(roots);
    const rootsChanged = !sameRoots(nextRoots, library.root_paths);
    const payload: LibraryUpdatePayload = {
      name: name.trim(),
      ...(rootsChanged ? { root_paths: nextRoots } : {}),
      match_rules: scraped ? buildMatchRules(validGenres(library.kind, genres, routingOptions), regions) : [],
      default_filter_id: defaultFilterId ? defaultFilterId : null,
      realtime_watch: realtimeWatch,
      generate_thumbnails: generateThumbnails,
      extract_chapter_images: extractChapterImages,
      exclude_from_home: excludeFromHome,
      auto_series_collections: autoSeriesCollections,
      access_mode: accessMode,
      admin_visible: adminVisible,
      member_ids: memberIds,
      detect_intros: detectIntros,
      enable_fingerprint: enableFingerprint,
    };
    void updateLibraryAndWait(library.id, payload, {
      signal: controller.signal,
      onProgress: (operation) => setPending(operationLabel(operation)),
    })
      .then((saved) => {
        if (controller.signal.aborted) return;
        setFailedOperation(null);
        onSaved(saved);
      })
      .catch((err) => handleOperationError(err, controller.signal))
      .finally(() => {
        if (abortRef.current === controller) abortRef.current = null;
        if (!controller.signal.aborted) {
          setBusy(false);
          setPending(null);
        }
      });
  };

  const dot = (on: boolean, text: string) => (
    <span key={text} className="inline-flex items-center gap-1.5">
      <span className={`size-1.5 rounded-full ${on ? "bg-[var(--ok,#5fd39b)]" : "bg-white/25"}`} />
      {text}
    </span>
  );

  const sections: { id: EditSectionId; title: string; summary: ReactNode; body: ReactNode }[] = [
    ...(scraped
      ? [
          {
            id: "scope" as EditSectionId,
            title: "收藏范围",
            summary: (
              <span className={scope.declared ? "text-[var(--text)]" : "text-[var(--text-faint)]"}>
                {scope.text}
              </span>
            ),
            body: (
              <div className="space-y-4">
                <p className="text-sub leading-relaxed text-[var(--text-muted)]">
                  声明「本库收什么」，订阅与自动入库就会按作品的区域和类型自动选进这个库。
                  <strong className="font-medium text-[var(--text)]">全部留空也可以</strong>
                  ：作为默认库承接所有未命中的作品；订阅时永远可以手动改库。
                </p>
                <ScopeEditor
                  kind={library.kind}
                  regions={regions}
                  genres={genres}
                  onRegions={setRegions}
                  onGenres={setGenres}
                  options={routingOptions}
                />
              </div>
            ),
          },
        ]
      : []),
    {
      id: "scan",
      title: "扫描与监控",
      summary: (
        <>
          {dot(realtimeWatch, "实时监控")}
          {dot(
            generateThumbnails,
            scraped ? "抓帧补图" : playable ? "抓帧封面" : "缩略图",
          )}
          {playable && dot(extractChapterImages, "章节")}
          {dot(!excludeFromHome, "首页展示")}
          {dot(autoSeriesCollections, "系列合集")}
        </>
      ),
      body: (
        <div>
          <SwitchRow
            title="实时监控目录变化"
            checked={realtimeWatch}
            onChange={setRealtimeWatch}
            detail={
              "新文件落盘后自动增量扫描入库。SMB/NFS 等网络挂载收不到远端变化通知、建立监听还很慢，建议关闭；关闭后由定期对账和手动扫描发现新文件，不实时但不会缺失。"
            }
          />

          <SwitchRow
            title={
              scraped
                ? "缺图时从视频抓帧补图"
                : playable
                  ? "从视频抓帧生成封面"
                  : "生成缩略图"
            }
            checked={generateThumbnails}
            onChange={setGenerateThumbnails}
            detail={
              scraped
                ? "未识别文件的封面、以及 TMDB 没有剧照的分集，从视频本身抓一帧顶上；TMDB 有图时始终用 TMDB 的。网络挂载库抓帧需要读取每个文件，介意流量可关闭，关闭后显示占位图。"
                : playable
                  ? "没有在线海报的内容从视频本身抓一帧当封面：优先用同名图片或内嵌封面，没有再抓帧。网络挂载库抓帧需要读取每个文件，介意流量可关闭，关闭后显示占位图。"
                  : "把原图缩到长边 720 像素当相册墙上的缩略图，网格只加载缩略图。关闭后墙会直接加载原图，大照片会很慢；只有网络挂载的大库介意流量时才建议关。"
            }
          />
          {/* 章节是视频的事：图片库（不可播）没有这一项 */}
          {playable && (
            <SwitchRow
              title="生成章节"
              checked={extractChapterImages}
              onChange={setExtractChapterImages}
              detail="每个视频按章节各抓一张画面：条目页出「章节」横排、点一张从那里开始播，Infuse 等播放器也能按章节跳转。扫描后在后台低优先级生成。"
            />
          )}
          {playable && (
            <SwitchRow
              title="识别片头片尾 (TheIntroDB / 章节)"
              checked={detectIntros}
              onChange={setDetectIntros}
              detail="控制 TheIntroDB 云端和视频内嵌章节识别；与下方声纹开关独立。只想关闭 TheIntroDB 云端库时，请在刮削设置中关闭 TheIntroDB。"
            />
          )}
          {playable && library.kind === "tv" && (
            <SwitchRow
              title="无章节剧集音频声纹比对 (Chromaprint)"
              checked={enableFingerprint}
              onChange={setEnableFingerprint}
              detail="独立提取每集配置时长的音频样本，并与同季剧集比对片头片尾；不受 TheIntroDB / 内嵌章节开关影响。STRM 视频会拉取音频流，请根据网络与云盘情况按需开启。"
            />
          )}
          <SwitchRow
            title="在首页展示"
            checked={!excludeFromHome}
            onChange={(next) => setExcludeFromHome(!next)}
            detail="关闭后首页「最近添加」与 Jellyfin 客户端的「最新媒体」都跳过这个库；库卡片仍在，进库内看照常。"
          />
          {/* 系列是"作品的属性"：影视库来自 TMDB，其他库来自视频旁 NFO 的 <set>；照片没有系列 */}
          <SwitchRow
            title="按作品系列自动生成合集"
            checked={autoSeriesCollections}
            onChange={setAutoSeriesCollections}
            detail={
              scraped
                ? "《哈利·波特》《碟中谍》这类系列会在合集页自动成集，按上映顺序排列，还能看出你缺哪几部、一键去补。这只是**展示**偏好：关掉之后系列信息照常入库、NFO 里照常写，Kodi/Emby 那边不受影响，只是我们的合集页不再自动多出几十个系列；重新打开会把已有的补齐，不会重新刮削。"
                : "视频旁的 NFO 里写了系列（<set>）的，会在合集页按系列自动成集。这只是**展示**偏好：关掉之后系列信息照常读取，只是合集页不再自动生成；重新打开会把已有的补齐。改了 NFO 之后，在库的 ⋯ 菜单里点「重新读取 NFO 与封面」生效。"
            }
          />
        </div>
      ),
    },
  ];
  // 可见范围放最后：它决定的是「谁能看」，与库怎么扫、收什么无关，排在配置项之后
  sections.push({
    id: "access",
    title: "可见范围",
    summary: (
      <span className={accessMode === "everyone" ? "text-[var(--text)]" : "text-[var(--warn)]"}>
        {accessScopeSummary(accessMode, adminVisible, memberIds.length)}
      </span>
    ),
    body: (
      <AccessScopeEditor
        mode={accessMode}
        adminVisible={adminVisible}
        memberIds={memberIds}
        onMode={setAccessMode}
        onAdminVisible={setAdminVisible}
        onMemberIds={setMemberIds}
      />
    ),
  });

  return (
    <>
      <Modal open onClose={onClose} label={`编辑「${library.name}」`} width="2xl" panelClassName="flex max-h-[86dvh] flex-col">
        <div className="shrink-0 border-b border-white/[0.06] px-6 pb-4 pt-5 max-md:px-5">
          <div className="flex flex-wrap items-center gap-2.5">
            <h2 className="text-title font-bold text-white">编辑「{library.name}」</h2>
            {/* 类型创建后不可改：只读展示，不再摆一排灰按钮 */}
            <span className="inline-flex items-center gap-1.5 rounded-full border border-white/[0.08] bg-white/[0.04] py-0.5 pl-1.5 pr-2.5 text-caption text-[var(--text-muted)]">
              <KindIcon className="size-3.5 text-[var(--accent)]" />
              {kindLabel}
            </span>
          </div>
        </div>

        <div className="scroll-thin min-h-0 flex-1 space-y-4 overflow-y-auto p-6 max-md:p-5">
          {/* 基本信息：核心字段常驻展开，绝不折叠，点击即聚焦 */}
          <div className="rounded-[14px] border border-white/[0.08] bg-white/[0.04] p-4 space-y-4">
            <h3 className="font-semibold text-white">基本信息</h3>
            <div>
              <label className={LABEL_CLASS}>媒体库名称</label>
              <input
                type="text"
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="请输入媒体库名称"
                autoComplete="off"
                className="w-full rounded-xl border border-white/20 bg-black/40 px-3.5 py-2.5 text-ui font-medium text-white outline-none transition focus:border-amber-400 focus:bg-black/60 focus:ring-1 focus:ring-amber-400/50"
              />
            </div>
            <div>
              <label className={LABEL_CLASS}>
                根目录<span className="font-normal text-[var(--text-faint)]">（第一个为主根，新内容落在这里）</span>
              </label>
              <RootsEditor roots={roots} onChange={setRoots} onPick={setPickerTarget} />
            </div>
            <div>
              <label className={LABEL_CLASS}>
                默认过滤规则组<span className="font-normal text-[var(--text-faint)]">（新订阅归属到此库时默认继承）</span>
              </label>
              <select
                value={defaultFilterId}
                onChange={(e) => setDefaultFilterId(e.target.value)}
                className="w-full rounded-xl border border-white/20 bg-black/40 px-3.5 py-2 text-ui font-medium text-white outline-none transition focus:border-amber-400 focus:bg-black/60 focus:ring-1 focus:ring-amber-400/50"
              >
                <option value="" className="bg-[#1c1d22] text-white">使用系统全局默认规则</option>
                {ruleSets.map((rs) => (
                  <option key={rs.id} value={String(rs.id)} className="bg-[#1c1d22] text-white">
                    {rs.name} {rs.is_default ? "（全局默认）" : ""}
                  </option>
                ))}
              </select>
            </div>
          </div>

          {sections.map((s) => {
            const expanded = open === s.id;
            return (
              <section
                key={s.id}
                data-section={s.id}
                className="overflow-hidden rounded-[14px] border border-white/[0.08] bg-white/[0.04]"
              >
                <button
                  type="button"
                  aria-expanded={expanded}
                  onClick={() => setOpen(expanded ? null : s.id)}
                  className="grid w-full grid-cols-[6.5rem_1fr_auto] items-center gap-3.5 px-3.5 py-3 text-left transition-colors hover:bg-white/[0.075] max-md:grid-cols-[5.5rem_1fr_auto]"
                >
                  <span className="font-semibold text-white">{s.title}</span>
                  <span className="flex min-w-0 flex-wrap items-center gap-x-2.5 gap-y-1 text-caption text-[var(--text-muted)]">
                    {s.summary}
                  </span>
                  <ChevronDownIcon
                    className={`size-3.5 text-[var(--text-faint)] transition-transform ${expanded ? "rotate-180" : ""}`}
                  />
                </button>
                {expanded && (
                  <div className="space-y-4 border-t border-white/[0.06] px-3.5 pb-4 pt-3">{s.body}</div>
                )}
              </section>
            );
          })}
        </div>

        <div className="shrink-0 border-t border-white/[0.06] px-6 py-4 max-md:px-5">
          {error && (
            <p className="mb-3 rounded-lg border border-red-400/25 bg-red-500/10 px-3.5 py-2.5 text-ui leading-6 text-red-200">
              {error}
            </p>
          )}
          <div className="flex items-center justify-between gap-3">
            <p className="min-w-0 truncate text-caption text-[var(--text-faint)]">
              {missing.length > 0 ? `还需填写：${missing.join("、")}` : "类型创建后不可更改"}
            </p>
            <div className="flex shrink-0 items-center gap-3">
              <button type="button" onClick={onClose} className="btn-glass h-9 px-4 text-ui font-medium">
                取消
              </button>
              {failedOperation ? (
                <button
                  type="button"
                  onClick={retryOperation}
                  disabled={busy}
                  className="btn-accent h-9 rounded-full px-5 text-ui font-semibold disabled:opacity-40"
                >
                  {busy ? (pending ?? "重试中…") : "重试操作"}
                </button>
              ) : (
                <button
                  type="button"
                  onClick={submit}
                  disabled={!ready || busy}
                  className="btn-accent h-9 rounded-full px-5 text-ui font-semibold disabled:opacity-40"
                >
                  {busy ? (pending ?? "保存中…") : "保存"}
                </button>
              )}
            </div>
          </div>
        </div>
      </Modal>
      <RootsPicker target={pickerTarget} roots={roots} onChange={setRoots} onClose={() => setPickerTarget(null)} />
    </>
  );
}
