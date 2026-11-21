"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";

import { useConfirm, useToast } from "@/components/feedback";
import { useSubscribeEntry } from "@/components/subscribe-entry";
import { listUpNext, setPlaybackMarks, type UpNextItem } from "@/lib/api/playback";
import {
  deleteSubscriptionPermanently,
  fetchReleaseForecast,
  getSubscription,
  listActiveSubscriptionDownloads,
  listRuleSets,
  listSubscriptionActivities,
  searchMissingSubscriptionResources,
  setSubscriptionFetchMode,
  setSubscriptionFollowFuture,
  setSubscriptionKeepOldVersions,
  setSubscriptionSearchInterval,
  setSubscriptionTrackingState,
  unsubscribeFromSubscription,
  updateSubscription,
  type ReleaseForecastDay,
  type RuleSet,
  type SubscriptionActivity,
  type SubscriptionDetail,
  type SubscriptionDownload,
  type SubscriptionRemovalOptions,
} from "@/lib/api/subscriptions";
import { useBackNavigation } from "@/lib/back-navigation";
import { imageUrl } from "@/lib/image-proxy";
import { useJobs } from "@/lib/jobs";
import { usePermissions } from "@/lib/permissions";
import { subscriptionStatus, subscriptionStatusMeta } from "@/lib/subscription-ui";
import { usePageTitle } from "@/lib/use-page-title";
import { useVisiblePolling } from "@/lib/use-visible-polling";

export function useSubscriptionInspector({
  id,
  autoOpenUpgradeRun,
}: {
  id: string;
  autoOpenUpgradeRun: boolean;
}) {
  const navigate = useNavigate();
  const confirm = useConfirm();
  const { canSubscribe, refresh: refreshSubscriptions } = useSubscribeEntry();
  const { canManageSubscriptions, canSearch, isAdmin } = usePermissions();
  const [detail, setDetail] = useState<SubscriptionDetail | null>(null);
  const [activities, setActivities] = useState<SubscriptionActivity[]>([]);
  const [ruleSets, setRuleSets] = useState<RuleSet[]>([]);
  const [failed, setFailed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [switchingRule, setSwitchingRule] = useState(false);
  const [adjusting, setAdjusting] = useState(false);
  const [upgradeRunning, setUpgradeRunning] = useState(autoOpenUpgradeRun);
  const [managing, setManaging] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const toast = useToast();

  const [downloads, setDownloads] = useState<SubscriptionDownload[]>([]);
  const [forecast, setForecast] = useState<ReleaseForecastDay[] | null>(null);
  const [seasonMarking, setSeasonMarking] = useState(false);
  const [upNext, setUpNext] = useState<UpNextItem | null>(null);
  const detailId = detail?.id;
  const mediaId = detail?.media.id;

  const reload = useCallback(() => {
    const subscriptionId = String(id);
    Promise.all([
      getSubscription(subscriptionId),
      listSubscriptionActivities(subscriptionId),
      canManageSubscriptions ? listRuleSets() : Promise.resolve([]),
    ])
      .then(([d, acts, rules]) => {
        if (String(id) !== subscriptionId) return;
        setDetail(d);
        setActivities(acts);
        setRuleSets(rules);
        setFailed(false);
      })
      .catch(() => {
        if (String(id) !== subscriptionId) return;
        setDetail((prev) => {
          if (!prev) setFailed(true);
          return prev;
        });
      });
  }, [canManageSubscriptions, id]);

  useEffect(() => {
    reload();
  }, [reload]);

  useEffect(() => {
    let alive = true;
    if (!detailId || !mediaId) {
      setForecast(null);
      setUpNext(null);
      return () => {
        alive = false;
      };
    }
    void fetchReleaseForecast(detailId)
      .then((data) => alive && setForecast(data.days.length > 0 ? data.days : null))
      .catch(() => alive && setForecast(null));
    void listUpNext(10)
      .then((items) => {
        if (!alive) return;
        setUpNext(items.find((item) => String(item.media_item_id) === String(mediaId)) ?? null);
      })
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [detailId, mediaId]);

  useEffect(() => {
    if (autoOpenUpgradeRun) {
      navigate(`/subscriptions/${id}`, { replace: true });
    }
  }, [autoOpenUpgradeRun, id, navigate]);

  const hasInFlight =
    detail?.wanted.some((w) => w.status === "grabbed" || w.status === "downloaded") ?? false;
  const downloadsPending = useRef(false);
  useVisiblePolling(
    () => {
      if (downloadsPending.current) return;
      downloadsPending.current = true;
      listActiveSubscriptionDownloads(
        (detail?.wanted ?? []).flatMap((w) => (w.info_hash ? [w.info_hash] : [])),
      )
        .then(setDownloads)
        .catch(() => undefined)
        .finally(() => {
          downloadsPending.current = false;
        });
    },
    hasInFlight ? 5000 : null,
    { leading: true },
  );
  useVisiblePolling(reload, hasInFlight ? 30000 : null);

  const downloadByHash = useMemo(() => {
    const map = new Map<string, SubscriptionDownload>();
    for (const d of downloads) map.set(d.info_hash, d);
    return map;
  }, [downloads]);

  const ruleSetName = useMemo(() => {
    const rs = ruleSets.find((r) => r.id === detail?.filter_id);
    if (!rs) return detail?.filter_id ? `#${detail.filter_id}` : "未设置";
    return rs.name;
  }, [ruleSets, detail]);
  const { jobs } = useJobs();
  const searchJob = useMemo(
    () =>
      jobs.find(
        (job) =>
          job.kind === "subscribe_search" &&
          (String(job.name).includes(detail?.id ?? "\0") ||
            JSON.stringify(job.payload ?? {}).includes(detail?.id ?? "\0")),
      ) ?? null,
    [jobs, detail?.id],
  );
  usePageTitle(detail?.media.title);

  const navFallback = { label: "我的订阅", href: "/subscriptions" };
  const back = useBackNavigation(navFallback.href);

  const detailStatus = detail ? subscriptionStatus(detail) : "active";
  const meta = subscriptionStatusMeta[detailStatus];
  const upgradingCount = detail ? detail.wanted.filter((w) => w.upgrade?.active).length : 0;
  const upgradingText =
    detail && detail.progress.total > 1 ? `洗版中（${upgradingCount}）` : "洗版中";
  const statusLabel =
    detail && detailStatus === "completed" && upgradingCount > 0
      ? `已收齐 · ${upgradingText}`
      : detail && detailStatus === "active" && upgradingCount > 0 && detail.progress.missing === 0
        ? `${meta.label} · ${upgradingText}`
        : meta.label;
  const isMovie = detail?.media.kind === "movie";
  const poster = detail?.media.poster_url
    ? imageUrl(detail.media.poster_url, "poster-card")
    : null;

  const togglePause = async () => {
    if (!detail) return;
    const resuming = detailStatus === "paused";
    const ok = await confirm({
      title: resuming
        ? `恢复《${detail.media.title}》的订阅追踪？`
        : `暂停《${detail.media.title}》的订阅追踪？`,
      description: resuming
        ? "恢复后会继续搜索缺失资源，并按当前规则自动投递符合条件的结果。"
        : "暂停后不会继续搜索或投递资源；已经提交的下载不受影响，可随时恢复。",
      confirmLabel: resuming ? "恢复追踪" : "暂停追踪",
      cancelLabel: "返回",
    });
    if (!ok) return;
    setBusy(true);
    try {
      await setSubscriptionTrackingState(detail.id, resuming ? "active" : "paused");
      toast.success(resuming ? "已恢复订阅追踪" : "已暂停订阅追踪");
      reload();
      refreshSubscriptions();
    } finally {
      setBusy(false);
    }
  };

  const toggleFollowFuture = async () => {
    if (!detail) return;
    const enabling = !detail.follow_future;
    setBusy(true);
    try {
      await setSubscriptionFollowFuture(detail.id, enabling);
      toast.success(enabling ? "已开启自动续订" : "已关闭自动续订");
      reload();
      refreshSubscriptions();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : "设置自动续订失败，请稍后重试");
    } finally {
      setBusy(false);
    }
  };

  const toggleKeepOldVersions = async () => {
    if (!detail) return;
    const enabling = !detail.keep_old_versions;
    setBusy(true);
    try {
      await setSubscriptionKeepOldVersions(detail.id, enabling);
      toast.success(enabling ? "已开启保留旧版本（洗版后新旧并存）" : "已关闭保留旧版本");
      reload();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : "设置失败，请稍后重试");
    } finally {
      setBusy(false);
    }
  };

  const markWholeSeason = async (played: boolean) => {
    if (!detail || detail.coverage.kind !== "tv") return;
    setSeasonMarking(true);
    try {
      await setPlaybackMarks(
        { media_item_id: detail.media.id, season_number: detail.coverage.season },
        { played },
      );
      toast.success(played ? `已把第 ${detail.coverage.season} 季标记为看完` : "已重置本季观看记录");
    } catch (e) {
      toast.error(e instanceof Error ? e.message : "标记失败，请稍后重试");
    } finally {
      setSeasonMarking(false);
    }
  };

  const searchNow = async () => {
    if (!detail) return;
    const ok = await confirm({
      title: `立即搜索《${detail.media.title}》的缺失资源？`,
      description:
        "将跳过当前搜索冷却，重新搜索已经可以搜索的缺口。命中当前规则组的资源可能会自动提交下载。",
      confirmLabel: "立即搜索",
      cancelLabel: "返回",
    });
    if (!ok) return;
    setBusy(true);
    try {
      const { reset_count } = await searchMissingSubscriptionResources(detail.id);
      toast.success(`${reset_count} 个缺口已重新排队，正在搜索`);
      reload();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : "触发搜索失败，请稍后重试");
    } finally {
      setBusy(false);
    }
  };

  const afterRemoved = () => {
    refreshSubscriptions();
    navigate("/subscriptions", { replace: true });
  };

  const remove = async () => {
    if (!detail) return;
    if (isAdmin) {
      setCancelling(true);
      return;
    }
    const ok = await confirm({
      title: `取消订阅《${detail.media.title}》？`,
      description: "将取消你的订阅关注；已经下载或入库的文件不会被删除。",
      confirmLabel: "取消订阅",
      cancelLabel: "先不",
      tone: "danger",
    });
    if (!ok) return;
    setBusy(true);
    try {
      await unsubscribeFromSubscription(detail.id);
      afterRemoved();
    } finally {
      setBusy(false);
    }
  };

  const confirmRemoval = async (options: SubscriptionRemovalOptions) => {
    if (!detail) return;
    setBusy(true);
    try {
      await deleteSubscriptionPermanently(detail.id, options);
      setCancelling(false);
      toast.success("已取消订阅");
      afterRemoved();
    } catch (e) {
      toast.error(e instanceof Error ? e.message : "取消订阅失败，请稍后重试");
    } finally {
      setBusy(false);
    }
  };

  const handleFetchModeChange = (mode: "search" | "rss" | "both") => {
    if (!detail) return;
    setBusy(true);
    void setSubscriptionFetchMode(detail.id, mode)
      .then(() => {
        toast.success("已更新搜种模式");
        reload();
      })
      .finally(() => setBusy(false));
  };

  const handleSearchIntervalChange = (secs: number) => {
    if (!detail) return;
    setBusy(true);
    void setSubscriptionSearchInterval(detail.id, secs)
      .then(() => {
        toast.success("已更新搜索间隔");
        reload();
      })
      .finally(() => setBusy(false));
  };

  const handleSwitchRuleSet = async (ruleSetId: string) => {
    if (!detail) return;
    await updateSubscription(detail.id, { filter_id: String(ruleSetId) });
    setSwitchingRule(false);
    reload();
  };

  return {
    canSubscribe,
    canManageSubscriptions,
    canSearch,
    isAdmin,
    detail,
    activities,
    ruleSets,
    failed,
    busy,
    switchingRule,
    setSwitchingRule,
    adjusting,
    setAdjusting,
    upgradeRunning,
    setUpgradeRunning,
    managing,
    setManaging,
    cancelling,
    setCancelling,
    forecast,
    seasonMarking,
    upNext,
    downloadByHash,
    ruleSetName,
    searchJob,
    navFallback,
    back,
    detailStatus,
    meta,
    upgradingCount,
    statusLabel,
    isMovie: Boolean(isMovie),
    poster,
    reload,
    refreshSubscriptions,
    togglePause,
    toggleFollowFuture,
    toggleKeepOldVersions,
    markWholeSeason,
    searchNow,
    remove,
    confirmRemoval,
    handleFetchModeChange,
    handleSearchIntervalChange,
    handleSwitchRuleSet,
  };
}
