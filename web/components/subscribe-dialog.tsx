"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { useNavigate } from "react-router-dom";

import { BrandLoader } from "@/components/brand-loader";
import { CheckIcon } from "@/components/icons";
import { Modal } from "@/components/modal";
import { PosterImage } from "@/components/poster-image";
import { RuleSetEditorDialog, specSummary } from "@/components/rule-sets-panel";
import { SubscriptionCancelDialog } from "@/components/subscription-cancel-dialog";
import { UpgradeRunReportView } from "@/components/upgrade-run-dialog";
import { listLibraries, type MediaLibrary } from "@/lib/api/libraries";
import {
  createSubscription,
  deleteSubscriptionPermanently,
  listRuleSets,
  previewSubscriptionDownloadRouting,
  previewSubscriptionTitle,
  runSubscriptionUpgradeRound,
  unsubscribeFromSubscription,
  type DispatchPreview,
  type PrepareResult,
  type ResolveCandidate,
  type RuleSet,
  type SeasonOverview,
  type SubscriptionRemovalOptions,
  type UpgradeRunReport,
} from "@/lib/api/subscriptions";
import { cachedImageUrl } from "@/lib/image-proxy";
import type { MediaType } from "@/lib/media-types";
import { usePermissions } from "@/lib/permissions";
import { ruleSetSpecFromAtoms } from "@/lib/subscription-ui";

/**
 * 订阅弹层只传递 Discover 签发的 titleRef；来源识别与 TMDB 锚定由后端负责。
 */
export interface SubscribeTarget {
  titleRef: string;
  kind: MediaType;
  title: string;
  year?: number;
  /**
   * 洗版变体（quality-upgrade.md §13.3，库详情「洗版」入口）：季勾选按媒体库
   * 库存预填、规则组只列带洗版目标的组、自动续订默认关；创建成功后自动触发
   * 一轮洗版并展示体检报告。
   */
  upgradeIntent?: boolean;
}

/**
 * 订阅弹层：一次点击完成订阅，复杂度沉到默认值。
 *
 * 流程（对应后端 /subscriptions/title-preview 的三态）：
 *   loading → ready（渲染季选择 + 自动续订开关 + 规则组）
 *           → ambiguous（豆瓣收敛歧义：候选墙确认一次后重新 prepare）
 *           → not_found（TMDB 未收录，无法订阅）
 * 已订阅的条目进入管理态：展示状态并提供取消订阅。
 *
 * 默认值策略：剧集默认勾选全部已播出的正季（特别季 0 须手动勾）、
 * 在播剧默认打开「自动续订」；规则组默认选中系统默认组。
 */
export function SubscribeDialog({
  target,
  onClose,
  onChanged,
}: {
  target: SubscribeTarget | null;
  onClose: () => void;
  onChanged?: () => void;
}) {
  const { canManageSubscriptions, isAdmin } = usePermissions();
  const navigate = useNavigate();
  const upgradeMode = !!target?.upgradeIntent;
  const [prepared, setPrepared] = useState<PrepareResult | null>(null);
  // 洗版变体：创建成功后自动触发的一轮洗版报告（非空即进入报告段）
  const [upgradeReport, setUpgradeReport] = useState<UpgradeRunReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [ruleSets, setRuleSets] = useState<RuleSet[]>([]);
  const [libraries, setLibraries] = useState<MediaLibrary[]>([]);
  const [selectedSeasons, setSelectedSeasons] = useState<Set<number>>(new Set());
  const [followFuture, setFollowFuture] = useState(false);
  const [ruleSetId, setRuleSetId] = useState<string | null>(null);
  // 快捷新建规则组（编辑器叠在本弹窗之上，保存后自动选中新组）
  const [creatingRuleSet, setCreatingRuleSet] = useState(false);
  // 管理员取消订阅：叠一层弹窗选"要不要连种子/媒体库文件一起删"
  const [cancelling, setCancelling] = useState(false);
  const [libraryId, setLibraryId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [selectedTitleRef, setSelectedTitleRef] = useState("");
  // 投递路由预检：选库即预演"下载会落到哪、能否自动入库"，配置问题当场亮出
  const [dispatchPreview, setDispatchPreview] = useState<DispatchPreview | null>(null);
  // 收藏范围路由的预选结论：打开弹窗时按作品特征算出的默认库 + 中文理由。
  // 规则只决定默认值——用户改选其它库即显式指定，徽标随之消失
  const [routed, setRouted] = useState<{ libraryId: string; reason: string | null } | null>(null);
  const routingKind = prepared?.media?.kind ?? target?.kind;
  const prepareGenRef = useRef(0);

  useEffect(() => {
    if (!canManageSubscriptions || !routingKind || libraryId === null) {
      setDispatchPreview(null);
      return;
    }
    let cancelled = false;
    setDispatchPreview(null);
    // 带上条目身份：后端据此渲染条目目录预览（entry_dir），前端不自己拼名字
    previewSubscriptionDownloadRouting(
      routingKind,
      libraryId as unknown as number | null,
      prepared?.media?.tmdb_id,
      {
        title: prepared?.media?.title,
        year: prepared?.media?.year,
      },
    )
      .then((p) => {
        if (!cancelled) setDispatchPreview(p);
      })
      .catch(() => {
        /* 预检失败静默：只是提示层，不影响订阅主流程 */
      });
    return () => {
      cancelled = true;
    };
  }, [canManageSubscriptions, routingKind, libraryId, prepared?.media]);

  /** 预检并按结果初始化表单默认值（候选确认后会带着 tmdbId 再次进入）。 */
  const runPrepare = useCallback(
    async (t: SubscribeTarget) => {
      const gen = ++prepareGenRef.current;
      setSelectedTitleRef(t.titleRef);
      setPrepared(null);
      setError(null);
      setUpgradeReport(null);
      try {
        // 洗版变体成员也要选「洗到哪一档」（换组由 upgrade-runs 按订阅归属
        // 者权限执行，与后端口径一致），故规则列表不再只对管理员拉取
        const [result, rules] = await Promise.all([
          previewSubscriptionTitle({ title_ref: t.titleRef }),
          canManageSubscriptions || t.upgradeIntent
            ? listRuleSets()
            : Promise.resolve([]),
        ]);
        if (gen !== prepareGenRef.current) return;
        // 豆瓣条目可能没有可靠的前端类型；媒体库和投递路由必须以后端
        // 收敛后的 canonical kind 为准，避免电影/剧集选到错误的库。
        const resolvedKind = result.media?.kind ?? t.kind;
        const libs = canManageSubscriptions ? await listLibraries(resolvedKind) : [];
        if (gen !== prepareGenRef.current) return;
        setLibraries(libs);
        setRuleSets(rules);
        // 洗版变体与普通订阅同一选组规则：新契约规则组不再携带洗版目标字段，
        // 默认组优先、否则第一个组；目标是否配置由后端在触发时校验
        // 默认库 = 收藏范围路由的结论（按作品的类型/区域自动选库，带中文理由）；
        // 预检失败或没有路由结论时回落该类型默认库
        const fallbackId = libs.find((l) => l.is_default)?.id ?? libs[0]?.id ?? null;
        setRouted(null);
        let pickedId = fallbackId;
        let matchedDefaultFilterId: string | null = null;
        if (canManageSubscriptions && result.status === "ready" && result.media) {
          const p = await previewSubscriptionDownloadRouting(
            resolvedKind,
            null,
            result.media.tmdb_id,
          ).catch(() => null);
          if (p?.library_id != null && libs.some((l) => l.id === String(p.library_id))) {
            pickedId = String(p.library_id);
            setRouted({ libraryId: String(p.library_id), reason: p.route_reason });
            if (p.default_filter_id) {
              matchedDefaultFilterId = p.default_filter_id;
            }
          }
        }
        setLibraryId(pickedId);
        // 如果命中的库（或默认库）配置了 default_filter_id，则默认规则组联动使用库的规则
        const activeLib = libs.find((l) => l.id === pickedId);
        const libFilterId = matchedDefaultFilterId ?? activeLib?.default_filter_id;
        const targetRule = (libFilterId
          ? rules.find((r) => String(r.id) === String(libFilterId))
          : undefined)
          ?? rules.find((r) => r.is_default)
          ?? rules[0];
        if (gen !== prepareGenRef.current) return;
        setRuleSetId(targetRule?.id ?? null);
        setPrepared(result);
        // 默认勾选全部已播出的正季；在播剧默认开启自动续订。
        // 洗版变体（§13.3）：改按媒体库库存预填（用户意图是洗手里有的），
        // 自动续订默认关（洗版场景不追新，可自行打开）
        //
        // 豆瓣季条目例外：用户点进的是「中餐厅 第十季」，要订的就是那一季，
        // 勾上整部剧十季显然不是他的意图。服务端只在条目确实季专属时给出
        // suggested_seasons（普通剧名为空），所以这里直接采信即可
        const suggested = result.suggested_seasons ?? [];
        const defaultSeasons = t.upgradeIntent
          ? result.seasons.filter((s) => s.owned_count > 0).map((s) => s.season_number)
          : suggested.length > 0
            ? suggested
            : result.seasons
                .filter((s) => s.season_number > 0 && s.aired_count > 0)
                .map((s) => s.season_number);
        setSelectedSeasons(new Set(defaultSeasons));
        setFollowFuture(
          !t.upgradeIntent &&
            resolvedKind === "tv" &&
            result.media?.status === "Returning Series",
        );
      } catch (e) {
        setError(e instanceof Error ? e.message : "预检失败，请稍后重试");
      }
    },
    [canManageSubscriptions],
  );

  useEffect(() => {
    if (target) void runPrepare(target);
  }, [target, runPrepare]);

  const toggleSeason = (n: number) =>
    setSelectedSeasons((prev) => {
      const next = new Set(prev);
      if (next.has(n)) next.delete(n);
      else next.add(n);
      return next;
    });

  const pickCandidate = (candidate: ResolveCandidate) => {
    if (!target) return;
    void runPrepare({ ...target, titleRef: candidate.title_ref });
  };

  const submit = async () => {
    if (!target || !prepared?.media) return;
    setBusy(true);
    setError(null);
    try {
      const sortedSeasons = [...selectedSeasons].sort((a, b) => a - b);
      const createdSubscriptions = [];
      // D09: 规则组 keep_old_versions 作为默认保留策略，在分支前只计算一次
      const selectedRule = canManageSubscriptions && ruleSetId != null
        ? ruleSets.find((r) => r.id === ruleSetId)
        : null;
      const keepOldFromSpec = selectedRule?.keep_old_versions === true;

      if (sortedSeasons.length > 0) {
        for (const s of sortedSeasons) {
          const created = await createSubscription({
            title_ref: selectedTitleRef || target.titleRef,
            source_title_ref:
              target.titleRef.startsWith("douban:") && selectedTitleRef !== target.titleRef
                ? target.titleRef
                : null,
            selected_seasons: [s],
            follow_future: followFuture,
            rule_set_id: canManageSubscriptions ? (ruleSetId as unknown as number) : null,
            library_id: canManageSubscriptions ? libraryId : null,
            ...(keepOldFromSpec ? { keep_old_versions: true } : {}),
          });
          createdSubscriptions.push(created);
        }
      } else {
        createdSubscriptions.push(await createSubscription({
          title_ref: selectedTitleRef || target.titleRef,
          source_title_ref:
            target.titleRef.startsWith("douban:") && selectedTitleRef !== target.titleRef
              ? target.titleRef
              : null,
          selected_seasons: [],
          follow_future: followFuture,
          rule_set_id: canManageSubscriptions ? (ruleSetId as unknown as number) : null,
          library_id: canManageSubscriptions ? libraryId : null,
          ...(keepOldFromSpec ? { keep_old_versions: true } : {}),
        }));
      }
      onChanged?.();
      if (upgradeMode && createdSubscriptions.length > 0) {
        try {
          const outcomes = await Promise.allSettled(createdSubscriptions.map((created) =>
            runSubscriptionUpgradeRound(
              created.id,
              ruleSetId != null ? (ruleSetId as unknown as number) : undefined,
            ),
          ));
          const reports = outcomes.flatMap((result) => result.status === "fulfilled" ? [result.value] : []);
          const failed = outcomes.length - reports.length;
          if (reports.length === 0) {
            throw new Error(`全部 ${failed} 季的洗版检查触发失败，请稍后重试`);
          }
          const first = reports[0];
          setUpgradeReport({
            ...first,
            summary: failed > 0
              ? `已为 ${reports.length} 季触发洗版检查，另有 ${failed} 季失败，请到订阅详情重试`
              : reports.length === 1 ? first.summary : `已为 ${reports.length} 季触发洗版检查`,
            counts: reports.reduce((counts, report) => {
              for (const key of Object.keys(counts) as Array<keyof typeof counts>) {
                counts[key] += report.counts[key];
              }
              return counts;
            }, { upgradable: 0, at_cutoff: 0, in_flight: 0, not_comparable: 0, missing: 0 }),
            units: reports.flatMap((report) => report.units),
          });
        } catch (e) {
          setError(
            `订阅已创建，但触发洗版失败：${
              e instanceof Error ? e.message : "请稍后到订阅详情里重试"
            }`,
          );
        }
        return;
      }
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : "订阅失败，请稍后重试");
    } finally {
      setBusy(false);
    }
  };

  // 管理员的取消订阅先叠一层弹窗问"种子与媒体库资源要不要一起删"（与订阅
  // 详情页同一个组件、同一套文案）；成员只是取消自己的关注，直接执行。
  const unsubscribe = async () => {
    if (!prepared?.existing_subscription_id) return;
    if (isAdmin) {
      setCancelling(true);
      return;
    }
    setBusy(true);
    try {
      await unsubscribeFromSubscription(prepared.existing_subscription_id);
      onChanged?.();
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : "取消订阅失败");
    } finally {
      setBusy(false);
    }
  };

  const confirmRemoval = async (options: SubscriptionRemovalOptions) => {
    if (!prepared?.existing_subscription_id) return;
    setBusy(true);
    try {
      await deleteSubscriptionPermanently(prepared.existing_subscription_id, options);
      setCancelling(false);
      onChanged?.();
      onClose();
    } catch (e) {
      setCancelling(false);
      setError(e instanceof Error ? e.message : "取消订阅失败");
    } finally {
      setBusy(false);
    }
  };

  // 洗版变体与普通订阅同一组候选：新契约规则组不携带洗版目标字段，
  // 目标是否配置由后端在触发时校验（未配置会给出可读错误）
  const selectableRules = useMemo(() => ruleSets, [ruleSets]);

  const canSubmit = useMemo(() => {
    if (!prepared?.media || busy) return false;
    // 洗版变体必须选中一个带洗版目标的组，否则触发一轮洗版会被后端拒绝
    if (upgradeMode && !selectableRules.some((r) => r.id === ruleSetId)) {
      return false;
    }
    if (prepared.media.kind === "movie") return true;
    return selectedSeasons.size > 0 || followFuture;
  }, [
    prepared,
    busy,
    selectedSeasons,
    followFuture,
    upgradeMode,
    selectableRules,
    ruleSetId,
  ]);

  if (!target) return null;

  if (upgradeReport) {
    return (
      <Modal
        open
        onClose={onClose}
        label={`订阅《${target.title}》`}
        width="lg"
        panelClassName="max-h-[76dvh]"
      >
        {/* 报告态：自带标题与出口按钮，整体滚动即可 */}
        <div className="scroll-thin min-h-0 flex-1 overflow-y-auto p-6 max-md:p-5">
          <UpgradeRunReportView
            title={target.title}
            isMovie={(prepared?.media?.kind ?? target.kind) === "movie"}
            report={upgradeReport}
            onClose={onClose}
          />
        </div>
      </Modal>
    );
  }

  return (
    <Modal
      open
      onClose={onClose}
      label={`订阅《${target.title}》`}
      width="lg"
      panelClassName="max-h-[76dvh]"
    >
      {/* 头部常驻：剧集选季 + 规则摘要能滚很长，标题不该跟着走 */}
      <div className="border-b border-white/[0.07] px-6 pb-4 pt-6 max-md:px-5">
        <h2 className="text-title font-bold text-white">
          {upgradeMode ? "订阅并洗版" : "订阅追踪"}
          <span className="ml-2 text-ui font-normal text-[var(--text-muted)]">
            {target.title}
            {target.year ? ` (${target.year})` : ""}
          </span>
        </h2>
        {upgradeMode && (
          <p className="mt-1 text-sub leading-6 text-[var(--text-muted)]">
            洗版通过订阅持续追踪更好的版本：确认后建立订阅并立即体检库里已有的每一集。
          </p>
        )}
      </div>

      <div className="scroll-thin min-h-0 flex-1 overflow-y-auto p-6 max-md:p-5">
          {/* —— 加载 / 错误 —— */}
          {!prepared && !error && (
            <div className="mt-8 flex items-center justify-center gap-2.5 pb-4 text-ui text-[var(--text-muted)]">
              <BrandLoader className="size-5" />
              正在获取条目信息…
            </div>
          )}
          {error && (
            <p className="mt-4 rounded-lg border border-red-400/25 bg-red-500/10 px-3.5 py-2.5 text-ui leading-6 text-red-200">
              {error}
            </p>
          )}

          {/* —— 豆瓣收敛：未收录 —— */}
          {prepared?.status === "not_found" && (
            <p className="mt-4 text-ui leading-6 text-[var(--text-muted)]">
              TMDB 未收录该条目，暂时无法订阅。订阅依赖 TMDB
              的别名与季集数据来匹配站点资源，可尝试在 TMDB 搜索入口确认条目后再订阅。
            </p>
          )}

          {/* —— 豆瓣收敛：多候选确认 —— */}
          {prepared?.status === "ambiguous" && (
            <div className="mt-4">
              <p className="text-ui text-[var(--text-muted)]">
                找到多个可能的条目，请确认你订阅的是哪一部：
              </p>
              <div className="mt-3 grid grid-cols-4 gap-3 max-md:grid-cols-3 max-md:gap-2">
                {prepared.candidates.map((c) => (
                  <button
                    key={c.tmdb_id}
                    type="button"
                    onClick={() => pickCandidate(c)}
                    className="group text-left"
                  >
                    <div className="aspect-[2/3] overflow-hidden rounded-lg bg-[#141824] ring-1 ring-white/10 transition group-hover:ring-white/40">
                      <PosterImage
                        src={c.poster_url ? cachedImageUrl(c.poster_url) : undefined}
                        alt={c.title}
                        className="size-full"
                      />
                    </div>
                    <p className="mt-1.5 truncate text-sub text-white/90">{c.title}</p>
                    <p className="truncate text-caption text-[var(--text-faint)]">
                      {c.year ?? "年份未知"}
                    </p>
                  </button>
                ))}
              </div>
            </div>
          )}

          {/* —— 已订阅：管理态 —— */}
          {prepared?.status === "ready" && prepared.existing_subscription_id && (
            <div className="mt-4">
              <p className="flex items-center gap-2 text-ui text-white/85">
                <CheckIcon className="size-4 text-[var(--ok)]" />
                该{target.kind === "movie" ? "电影" : "剧集"}已在订阅中，系统
                正在按间隔搜索资源。
              </p>
              <div className="mt-5 flex justify-end gap-3">
                <button
                  type="button"
                  onClick={onClose}
                  className="btn-glass h-9 px-4 text-ui font-medium"
                >
                  好的
                </button>
                {/* 洗版入口进到已有订阅：并入既有订阅（§13.4），去详情触发一轮 */}
                {upgradeMode && (
                  <button
                    type="button"
                    onClick={() => {
                      const id = prepared.existing_subscription_id;
                      onClose();
                      navigate(`/subscriptions/${id}?upgrade-run=1`);
                    }}
                    className="btn-accent h-9 rounded-full px-4 text-ui font-semibold"
                  >
                    去洗一轮版
                  </button>
                )}
                <button
                  type="button"
                  disabled={busy}
                  onClick={unsubscribe}
                  className="h-9 rounded-full border border-red-400/30 bg-red-500/10 px-4 text-ui font-medium text-red-200 transition hover:bg-red-500/20 disabled:opacity-50"
                >
                  取消订阅
                </button>
              </div>
            </div>
          )}

          {/* —— 订阅表单 —— */}
          {prepared?.status === "ready" && !prepared.existing_subscription_id && (
            <div className="mt-4 space-y-5">
              {prepared.movie_owned && (
                <p className="flex items-center gap-2 rounded-xl border border-[var(--ok)]/25 bg-[var(--ok)]/10 px-3.5 py-2.5 text-sub text-[var(--ok)]">
                  <CheckIcon className="size-4 shrink-0" />
                  {upgradeMode
                    ? "媒体库里已有这部电影，将体检现有版本并按需洗版"
                    : "媒体库里已有这部电影，订阅后不会重复下载"}
                </p>
              )}
              {prepared.media?.kind === "tv" && (
                <section>
                  <h3 className="mb-2 text-ui font-semibold text-white/85">
                    选择要收录的季
                    <span className="ml-2 font-normal text-[var(--text-faint)]">
                      勾选即要整季（含未播集）
                    </span>
                  </h3>
                  <div className="space-y-1.5">
                    {prepared.seasons.map((s) => (
                      <SeasonRow
                        key={s.season_number}
                        season={s}
                        checked={selectedSeasons.has(s.season_number)}
                        onToggle={() => toggleSeason(s.season_number)}
                      />
                    ))}
                  </div>

                  <label className="mt-4 flex cursor-pointer items-center justify-between rounded-xl border border-white/[0.08] bg-white/[0.04] px-4 py-3">
                    <span>
                      <span className="block text-ui font-medium text-white/90">
                        自动续订
                      </span>
                      <span className="mt-0.5 block text-caption text-[var(--text-faint)]">
                        之后播出的新集、新一季自动加入追踪
                      </span>
                    </span>
                    <input
                      type="checkbox"
                      checked={followFuture}
                      onChange={(e) => setFollowFuture(e.target.checked)}
                      className="size-4 accent-[var(--accent-2)]"
                    />
                  </label>
                </section>
              )}

              {(upgradeMode || (canManageSubscriptions && ruleSets.length > 0)) && (
                <section>
                  <div className="mb-2 flex items-center justify-between">
                    <h3 className="text-ui font-semibold text-white/85">
                      {upgradeMode ? "洗版规则" : "资源规则"}
                      {upgradeMode && (
                        <span className="ml-2 font-normal text-[var(--text-faint)]">
                          选中的规则组将作为本轮洗版的依据
                        </span>
                      )}
                    </h3>
                    {canManageSubscriptions && (
                      <button
                        type="button"
                        onClick={() => setCreatingRuleSet(true)}
                        className="text-sub font-medium text-[var(--accent)] hover:underline"
                      >
                        + 新建规则组
                      </button>
                    )}
                  </div>
                  {upgradeMode && selectableRules.length === 0 ? (
                    <p className="rounded-xl border border-white/[0.08] bg-white/[0.03] px-4 py-3 text-sub leading-6 text-[var(--text-muted)]">
                      {canManageSubscriptions
                        ? "还没有规则组——点右上角「+ 新建规则组」创建。"
                        : "还没有规则组，请联系管理员在「设置 → 订阅规则 → 规则组」中创建。"}
                    </p>
                  ) : (
                  <select
                    value={ruleSetId ?? undefined}
                    onChange={(e) => setRuleSetId(e.target.value)}
                    className="w-full rounded-xl border border-white/[0.08] bg-white/[0.04] px-3.5 py-2.5 text-ui text-white/90 outline-none focus:border-white/25 [&>option]:bg-[#181c28]"
                  >
                    {selectableRules.map((r) => (
                      <option key={r.id} value={r.id}>
                        {r.name}
                        {r.is_default ? "（默认）" : ""}
                      </option>
                    ))}
                  </select>
                  )}
                  {/* 所选组的条件摘要：选规则不再是「盲选」 */}
                  {(() => {
                    const picked = selectableRules.find((r) => r.id === ruleSetId);
                    if (!picked) return null;
                    const chips = specSummary(ruleSetSpecFromAtoms(picked.atoms));
                    return (
                      <p className="mt-1.5 flex flex-wrap gap-1.5">
                        {chips.length === 0 ? (
                          // 全不限是个危险默认：第一批抓到的可能是低清枪版或
                          // 零做种死种，用琥珀色把风险讲在订阅之前
                          <span className="text-caption text-[var(--warn)]/90">
                            该规则组不限任何条件——可能抓到低画质或无人做种的资源，
                            建议在「设置 → 订阅规则 → 规则组」里加上分辨率与做种数限制
                          </span>
                        ) : (
                          chips.map((chip) => (
                            <span
                              key={chip}
                              className="rounded-md bg-white/[0.07] px-1.5 py-0.5 text-caption text-white/75"
                            >
                              {chip}
                            </span>
                          ))
                        )}
                      </p>
                    );
                  })()}
                </section>
              )}

              {canManageSubscriptions && libraries.length > 0 && (
                <section>
                  <h3 className="mb-2 text-ui font-semibold text-white/85">入库到</h3>
                  <select
                    value={libraryId ?? undefined}
                    onChange={(e) => {
                      const newLibId = e.target.value;
                      setLibraryId(newLibId);
                      setRouted(null);
                      const targetLib = libraries.find((l) => l.id === newLibId);
                      if (targetLib?.default_filter_id) {
                        const targetRule = ruleSets.find(
                          (r) => String(r.id) === String(targetLib.default_filter_id),
                        );
                        if (targetRule) {
                          setRuleSetId(targetRule.id);
                        }
                      }
                    }}
                    className="w-full rounded-xl border border-white/[0.08] bg-white/[0.04] px-3.5 py-2.5 text-ui text-white/90 outline-none focus:border-white/25 [&>option]:bg-[#181c28]"
                  >
                    {libraries.map((l) => (
                      <option key={l.id} value={l.id}>
                        {l.name}
                        {l.is_default ? "（默认）" : ""}
                      </option>
                    ))}
                  </select>
                  {/* 收藏范围路由徽标：说明"为什么默认选了这个库"；用户改库即消失 */}
                  {routed && routed.reason && libraryId === routed.libraryId && (
                    <p className="mt-1.5 text-caption leading-relaxed text-[var(--accent)]/90">
                      自动选库：{routed.reason}
                    </p>
                  )}
                  {/* 投递路由预检：与后端真实投递同源判定，配置问题当场亮出 */}
                  {dispatchPreview &&
                    (dispatchPreview.ok ? (
                      <p className="mt-1.5 text-caption leading-relaxed text-[var(--text-faint)]">
                        {dispatchPreview.mode === "watch" ? (
                          dispatchPreview.staging_path ? (
                            <>
                              将投递到自动入库的监听目录{" "}
                              <span className="font-mono">{dispatchPreview.path}</span>
                              ，下载完成后整理到{" "}
                              <span className="font-mono">{dispatchPreview.staging_path}</span>
                              ，文件进入媒体库根目录后自动入账
                            </>
                          ) : (
                            <>
                              将投递到自动入库的监听目录{" "}
                              <span className="font-mono">{dispatchPreview.path}</span>
                              ，下载完成后自动整理入库
                            </>
                          )
                        ) : (
                          // 条目目录由后端按命名模板渲染（entry_dir）：模板可全局/
                          // 按库自定义，前端自己拼「标题 (年份)」会与真实落点不符
                          <>
                            将直接下载到库内目录{" "}
                            <span className="font-mono">
                              {(dispatchPreview.entry_dir ?? dispatchPreview.path)?.replace(
                                /\/+$/,
                                "",
                              )}
                            </span>
                            ，完成后自动入账
                          </>
                        )}
                      </p>
                    ) : (
                      <p className="mt-1.5 rounded-lg border border-amber-400/25 bg-amber-500/10 px-3 py-2 text-caption leading-relaxed text-amber-200">
                        {dispatchPreview.warning}
                      </p>
                    ))}
                </section>
              )}

            </div>
          )}
      </div>

      {/* 底栏常驻（仅订阅表单态；管理态的按钮短，留在正文里） */}
      {prepared?.status === "ready" && !prepared.existing_subscription_id && (
        <div className="flex justify-end gap-3 border-t border-white/[0.07] px-6 py-4 max-md:px-5">
          <button type="button" onClick={onClose} className="btn-glass h-9 px-4 text-ui font-medium">
            取消
          </button>
          <button
            type="button"
            disabled={!canSubmit}
            onClick={submit}
            className="btn-accent h-9 rounded-full px-5 text-ui font-semibold disabled:opacity-50"
          >
            {busy
              ? upgradeMode
                ? "正在订阅并体检…"
                : "正在订阅…"
              : upgradeMode
                ? "订阅并开始洗版"
                : "确认订阅"}
          </button>
        </div>
      )}

      {isAdmin && prepared?.existing_subscription_id && (
        <SubscriptionCancelDialog
          open={cancelling}
          raised
          subscriptionId={prepared.existing_subscription_id}
          title={prepared.media?.title ?? target?.title ?? ""}
          onClose={() => setCancelling(false)}
          onConfirm={confirmRemoval}
        />
      )}

      {canManageSubscriptions && creatingRuleSet && (
        <RuleSetEditorDialog
          ruleSet={null}
          raised
          onClose={() => setCreatingRuleSet(false)}
          onSaved={(saved) => {
            setCreatingRuleSet(false);
            setRuleSets((prev) => [...prev, saved]);
            // 新建组即选中（新契约规则组不携带洗版目标字段，无需按目标过滤）
            setRuleSetId(saved.id);
          }}
        />
      )}
    </Modal>
  );
}

/** 季选择行：季名 + 播出进度；未播季弱化显示但可勾（勾了=要整季）。
 *  订阅弹窗与调整订阅弹窗（subscription-adjust-dialog）共用。 */
export function SeasonRow({
  season,
  checked,
  onToggle,
}: {
  season: SeasonOverview;
  checked: boolean;
  onToggle: () => void;
}) {
  const total = season.episode_count ?? 0;
  const progress =
    season.aired_count >= total && total > 0
      ? `全 ${total} 集已播完`
      : total > 0
        ? `已播 ${season.aired_count}/${total} 集`
        : season.aired_count > 0
          ? `已播 ${season.aired_count} 集`
          : "未播出";
  // 库存提示（媒体库联通）：已有的集不会重复下载
  const owned =
    season.owned_count > 0
      ? season.owned_count >= total && total > 0
        ? "整季已在库"
        : `库里已有 ${season.owned_count} 集`
      : null;
  return (
    <label
      className={`flex cursor-pointer items-center justify-between rounded-xl border px-4 py-2.5 transition ${
        checked
          ? "border-white/20 bg-white/[0.08]"
          : "border-white/[0.06] bg-white/[0.02] hover:bg-white/[0.05]"
      }`}
    >
      <span className="flex items-baseline gap-2.5">
        <span className="text-ui font-medium text-white/90">
          {season.season_number === 0 ? "特别篇" : `第 ${season.season_number} 季`}
        </span>
        <span className="tnum text-caption text-[var(--text-faint)]">{progress}</span>
        {owned && (
          <span className="tnum text-caption font-medium text-[var(--ok)]/90">{owned}</span>
        )}
      </span>
      <input
        type="checkbox"
        checked={checked}
        onChange={onToggle}
        className="size-4 accent-[var(--accent-2)]"
      />
    </label>
  );
}
