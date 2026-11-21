"use client";

import { Link } from "react-router-dom";

import { BrandLoader } from "@/components/brand-loader";
import { DetailPageNav } from "@/components/detail-page-nav";
import { ArrowLeftIcon } from "@/components/icons";
import { SubscriptionAdjustDialog } from "@/components/subscription-adjust-dialog";
import { SubscriptionCancelDialog } from "@/components/subscription-cancel-dialog";
import { UpgradeRunDialog } from "@/components/upgrade-run-dialog";
import { pendingFailures } from "@/lib/subscription-inspector";
import { targetLibraryNotice } from "@/lib/subscription-target-library";

import { ActivityLogSection, SearchRoundBar } from "./subscription-inspector/activity-section";
import { RuleSetSwitchDialog } from "./subscription-inspector/rule-set-switch-dialog";
import { SubscriptionHeroCard } from "./subscription-inspector/subscription-hero-card";
import { SubscriptionManageSheet } from "./subscription-inspector/subscription-manage-controls";
import { useSubscriptionInspector } from "./subscription-inspector/use-subscription-inspector";
import { WantedBreakdown } from "./subscription-inspector/wanted-breakdown";

/**
 * 订阅详情分析页（/subscriptions/[id]）。
 */
export function SubscriptionInspectorView({
  id,
  autoOpenUpgradeRun = false,
}: {
  id: string;
  autoOpenUpgradeRun?: boolean;
}) {
  const c = useSubscriptionInspector({ id, autoOpenUpgradeRun });
  const { detail } = c;

  if (c.failed) {
    return (
      <div className="flex h-full flex-col">
        <DetailPageNav title="" fallback={c.navFallback} onBack={c.back} />
        <div className="flex flex-1 flex-col items-center justify-center gap-4">
          <p className="text-body text-[var(--text-muted)]">未能加载该订阅，可能已被删除。</p>
          <Link to="/subscriptions" replace className="btn-glass px-4 py-2 text-ui font-medium">
            <ArrowLeftIcon className="size-4" />
            返回订阅列表
          </Link>
        </div>
      </div>
    );
  }

  if (!detail) {
    return (
      <div className="flex h-full flex-col">
        <DetailPageNav title="" fallback={c.navFallback} onBack={c.back} />
        <div className="flex flex-1 items-center justify-center gap-2.5 text-ui text-[var(--text-muted)]">
          <BrandLoader className="size-5" />
          正在加载订阅详情…
        </div>
      </div>
    );
  }

  const notice = targetLibraryNotice(detail);

  return (
    <div className="scroll-thin scroll-safe flex-1 overflow-y-auto px-6 pb-12 max-md:px-4">
      <DetailPageNav
        title={detail.media.title}
        fallback={c.navFallback}
        onBack={c.back}
        className="-mx-6 max-md:-mx-4"
      />

      {notice && (
        <div
          role="alert"
          className="mt-4 flex flex-wrap items-center justify-between gap-3 rounded-xl border border-amber-400/25 bg-amber-400/10 px-4 py-3 text-sub text-amber-200"
        >
          <div>
            <p className="font-semibold">目标媒体库不可用</p>
            <p className="mt-1">{notice}</p>
            {!c.canManageSubscriptions && <p className="mt-1">请联系管理员重新选择媒体库。</p>}
          </div>
          {c.canSubscribe && c.canManageSubscriptions && (
            <button
              type="button"
              onClick={() => c.setAdjusting(true)}
              className="shrink-0 rounded-lg border border-amber-300/30 px-3 py-2 font-semibold hover:bg-amber-300/10"
            >
              重新选择媒体库
            </button>
          )}
        </div>
      )}

      <SubscriptionHeroCard
        detail={detail}
        detailStatus={c.detailStatus}
        metaColor={c.meta.color}
        statusLabel={c.statusLabel}
        isMovie={c.isMovie}
        poster={c.poster}
        upgradingCount={c.upgradingCount}
        ruleSetName={c.ruleSetName}
        searchJob={c.searchJob}
        busy={c.busy}
        canSubscribe={c.canSubscribe}
        canManageSubscriptions={c.canManageSubscriptions}
        canSearch={c.canSearch}
        upNext={c.upNext}
        forecast={c.forecast}
        seasonMarking={c.seasonMarking}
        onToggleKeepOldVersions={() => void c.toggleKeepOldVersions()}
        onFetchModeChange={c.handleFetchModeChange}
        onSearchIntervalChange={c.handleSearchIntervalChange}
        onMarkWholeSeason={(played) => void c.markWholeSeason(played)}
        onSearchNow={() => void c.searchNow()}
        onAdjust={() => c.setAdjusting(true)}
        onUpgradeRun={() => c.setUpgradeRunning(true)}
        onToggleFollowFuture={() => void c.toggleFollowFuture()}
        onSwitchRule={() => c.setSwitchingRule(true)}
        onTogglePause={() => void c.togglePause()}
        onRemove={() => void c.remove()}
        onOpenManageSheet={() => c.setManaging(true)}
      />

      <div className="mt-7 space-y-4">
        <SearchRoundBar activities={c.activities} wanted={detail.wanted} />
        <WantedBreakdown
          wanted={detail.wanted}
          isMovie={c.isMovie}
          downloads={c.downloadByHash}
          failures={pendingFailures(c.activities)}
        />
        <ActivityLogSection activities={c.activities} />
      </div>

      {(c.canSubscribe || c.canManageSubscriptions) && c.managing && (
        <SubscriptionManageSheet
          open
          busy={c.busy}
          paused={c.detailStatus === "paused"}
          completed={c.detailStatus === "completed"}
          canSubscribe={c.canSubscribe}
          canManageSubscriptions={c.canManageSubscriptions}
          followFuture={c.isMovie ? null : detail.follow_future}
          onClose={() => c.setManaging(false)}
          onAdjust={() => {
            c.setManaging(false);
            c.setAdjusting(true);
          }}
          onUpgradeRun={() => {
            c.setManaging(false);
            c.setUpgradeRunning(true);
          }}
          onToggleFollowFuture={() => {
            c.setManaging(false);
            void c.toggleFollowFuture();
          }}
          onSwitchRule={() => {
            c.setManaging(false);
            c.setSwitchingRule(true);
          }}
          onTogglePause={() => {
            c.setManaging(false);
            void c.togglePause();
          }}
          onRemove={() => {
            c.setManaging(false);
            void c.remove();
          }}
        />
      )}

      {c.canSubscribe && c.adjusting && (
        <SubscriptionAdjustDialog
          detail={detail}
          onClose={() => c.setAdjusting(false)}
          onSaved={() => {
            c.setAdjusting(false);
            c.reload();
            c.refreshSubscriptions();
          }}
        />
      )}

      {c.canSubscribe && c.upgradeRunning && (
        <UpgradeRunDialog
          detail={detail}
          onClose={() => c.setUpgradeRunning(false)}
          onFinished={() => {
            c.setUpgradeRunning(false);
            c.reload();
            c.refreshSubscriptions();
          }}
        />
      )}

      {c.isAdmin && (
        <SubscriptionCancelDialog
          open={c.cancelling}
          subscriptionId={detail.id}
          title={detail.media.title}
          onClose={() => c.setCancelling(false)}
          onConfirm={c.confirmRemoval}
        />
      )}

      {c.canManageSubscriptions && c.switchingRule && (
        <RuleSetSwitchDialog
          ruleSets={c.ruleSets}
          currentId={detail.filter_id ?? null}
          onClose={() => c.setSwitchingRule(false)}
          onPick={c.handleSwitchRuleSet}
        />
      )}
    </div>
  );
}
