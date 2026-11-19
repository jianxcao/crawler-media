import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { listLibraries, type MediaLibrary } from "@/lib/api/libraries";
import {
  createSubscription,
  deleteSubscriptionPermanently,
  type DispatchPreview,
  listRuleSets,
  type PrepareResult,
  previewSubscriptionDownloadRouting,
  previewSubscriptionTitle,
  type ResolveCandidate,
  type RuleSet,
  runSubscriptionUpgradeRound,
  type SubscriptionRemovalOptions,
  unsubscribeFromSubscription,
  type UpgradeRunReport,
} from "@/lib/api/subscriptions";
import type { MediaType } from "@/lib/media-types";
import { usePermissions } from "@/lib/permissions";
import {
  computeDefaultSelectedSeasons,
  mergeUpgradeRunReports,
} from "@/lib/subscribe-dialog-helpers";

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

export function useSubscribeDialog({
  target,
  onClose,
  onChanged,
}: {
  target: SubscribeTarget | null;
  onClose: () => void;
  onChanged?: () => void;
}) {
  const { canManageSubscriptions, isAdmin } = usePermissions();
  const upgradeMode = !!target?.upgradeIntent;
  const [prepared, setPrepared] = useState<PrepareResult | null>(null);
  const [upgradeReport, setUpgradeReport] = useState<UpgradeRunReport | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [ruleSets, setRuleSets] = useState<RuleSet[]>([]);
  const [libraries, setLibraries] = useState<MediaLibrary[]>([]);
  const [selectedSeasons, setSelectedSeasons] = useState<Set<number>>(new Set());
  const [followFuture, setFollowFuture] = useState(false);
  const [ruleSetId, setRuleSetId] = useState<string | null>(null);
  const [creatingRuleSet, setCreatingRuleSet] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [libraryId, setLibraryId] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [selectedTitleRef, setSelectedTitleRef] = useState("");
  const [dispatchPreview, setDispatchPreview] = useState<DispatchPreview | null>(null);
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
    previewSubscriptionDownloadRouting(
      routingKind,
      libraryId,
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
        const [result, rules] = await Promise.all([
          previewSubscriptionTitle({ title_ref: t.titleRef }),
          canManageSubscriptions || t.upgradeIntent
            ? listRuleSets()
            : Promise.resolve([]),
        ]);
        if (gen !== prepareGenRef.current) return;
        const resolvedKind = result.media?.kind ?? t.kind;
        const libs = canManageSubscriptions ? await listLibraries(resolvedKind) : [];
        if (gen !== prepareGenRef.current) return;
        setLibraries(libs);
        setRuleSets(rules);

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
        const activeLib = libs.find((l) => l.id === pickedId);
        const libFilterId = matchedDefaultFilterId ?? activeLib?.default_filter_id;
        const targetRule =
          (libFilterId ? rules.find((r) => String(r.id) === String(libFilterId)) : undefined) ??
          rules.find((r) => r.is_default) ??
          rules[0];
        if (gen !== prepareGenRef.current) return;
        setRuleSetId(targetRule?.id ?? null);
        setPrepared(result);
        setSelectedSeasons(new Set(computeDefaultSelectedSeasons(result, t.upgradeIntent)));
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

  const handleLibraryChange = (newLibId: string) => {
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
  };

  const submit = async () => {
    if (!target || !prepared?.media) return;
    setBusy(true);
    setError(null);
    try {
      const sortedSeasons = [...selectedSeasons].sort((a, b) => a - b);
      const createdSubscriptions = [];
      const selectedRule =
        canManageSubscriptions && ruleSetId != null
          ? ruleSets.find((r) => r.id === ruleSetId)
          : null;
      const keepOldFromSpec = selectedRule?.keep_old_versions === true;
      const basePayload = {
        title_ref: selectedTitleRef || target.titleRef,
        source_title_ref:
          target.titleRef.startsWith("douban:") && selectedTitleRef !== target.titleRef
            ? target.titleRef
            : null,
        follow_future: followFuture,
        filter_id: canManageSubscriptions ? ruleSetId : null,
        library_id: canManageSubscriptions ? libraryId : null,
        ...(keepOldFromSpec ? { keep_old_versions: true } : {}),
      };

      if (sortedSeasons.length > 0) {
        for (const s of sortedSeasons) {
          createdSubscriptions.push(
            await createSubscription({
              ...basePayload,
              coverage: { kind: "tv", season: s, episode_from: 1 },
            }),
          );
        }
      } else {
        createdSubscriptions.push(await createSubscription(basePayload));
      }
      onChanged?.();
      if (upgradeMode && createdSubscriptions.length > 0) {
        try {
          const outcomes = await Promise.allSettled(
            createdSubscriptions.map((created) =>
              runSubscriptionUpgradeRound(
                created.id,
                ruleSetId != null ? ruleSetId : undefined,
              ),
            ),
          );
          const reports = outcomes.flatMap((result) =>
            result.status === "fulfilled" ? [result.value] : [],
          );
          const failed = outcomes.length - reports.length;
          if (reports.length === 0) {
            throw new Error(`全部 ${failed} 季的洗版检查触发失败，请稍后重试`);
          }
          setUpgradeReport(mergeUpgradeRunReports(reports, outcomes.length));
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

  const selectableRules = useMemo(() => ruleSets, [ruleSets]);

  const canSubmit = useMemo(() => {
    if (!prepared?.media || busy) return false;
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

  return {
    canManageSubscriptions,
    isAdmin,
    upgradeMode,
    prepared,
    upgradeReport,
    error,
    libraries,
    selectedSeasons,
    followFuture,
    setFollowFuture,
    ruleSetId,
    setRuleSetId,
    creatingRuleSet,
    setCreatingRuleSet,
    cancelling,
    setCancelling,
    libraryId,
    busy,
    dispatchPreview,
    routed,
    selectableRules,
    canSubmit,
    toggleSeason,
    pickCandidate,
    handleLibraryChange,
    submit,
    unsubscribe,
    confirmRemoval,
    onRuleSetSaved: (saved: RuleSet) => {
      setCreatingRuleSet(false);
      setRuleSets((prev) => [...prev, saved]);
      setRuleSetId(saved.id);
    },
  };
}
