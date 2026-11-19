"use client";

import { useNavigate } from "react-router-dom";

import { BrandLoader } from "@/components/brand-loader";
import { Modal } from "@/components/modal";
import { RuleSetEditorDialog } from "@/components/rule-sets-panel";
import {
  AmbiguousCandidatesGrid,
  ExistingSubscriptionNotice,
  SeasonRow,
  SubscribeReadyForm,
} from "@/components/subscribe-dialog/subscribe-form-sections";
import {
  type SubscribeTarget,
  useSubscribeDialog,
} from "@/components/subscribe-dialog/use-subscribe-dialog";
import { SubscriptionCancelDialog } from "@/components/subscription-cancel-dialog";
import { UpgradeRunReportView } from "@/components/upgrade-run-dialog";

export { SeasonRow, type SubscribeTarget };

/**
 * 订阅弹层：一次点击完成订阅，复杂度沉到默认值。
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
  const navigate = useNavigate();
  const state = useSubscribeDialog({ target, onClose, onChanged });

  if (!target) return null;

  if (state.upgradeReport) {
    return (
      <Modal
        open
        onClose={onClose}
        label={`订阅《${target.title}》`}
        width="lg"
        panelClassName="max-h-[76dvh]"
      >
        <div className="scroll-thin min-h-0 flex-1 overflow-y-auto p-6 max-md:p-5">
          <UpgradeRunReportView
            title={target.title}
            isMovie={(state.prepared?.media?.kind ?? target.kind) === "movie"}
            report={state.upgradeReport}
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
      <div className="border-b border-white/[0.07] px-6 pb-4 pt-6 max-md:px-5">
        <h2 className="text-title font-bold text-white">
          {state.upgradeMode ? "订阅并洗版" : "订阅追踪"}
          <span className="ml-2 text-ui font-normal text-[var(--text-muted)]">
            {target.title}
            {target.year ? ` (${target.year})` : ""}
          </span>
        </h2>
        {state.upgradeMode && (
          <p className="mt-1 text-sub leading-6 text-[var(--text-muted)]">
            洗版通过订阅持续追踪更好的版本：确认后建立订阅并立即体检库里已有的每一集。
          </p>
        )}
      </div>

      <div className="scroll-thin min-h-0 flex-1 overflow-y-auto p-6 max-md:p-5">
        {!state.prepared && !state.error && (
          <div className="mt-8 flex items-center justify-center gap-2.5 pb-4 text-ui text-[var(--text-muted)]">
            <BrandLoader className="size-5" />
            正在获取条目信息…
          </div>
        )}
        {state.error && (
          <p className="mt-4 rounded-lg border border-red-400/25 bg-red-500/10 px-3.5 py-2.5 text-ui leading-6 text-red-200">
            {state.error}
          </p>
        )}

        {state.prepared?.status === "not_found" && (
          <p className="mt-4 text-ui leading-6 text-[var(--text-muted)]">
            TMDB 未收录该条目，暂时无法订阅。订阅依赖 TMDB
            的别名与季集数据来匹配站点资源，可尝试在 TMDB 搜索入口确认条目后再订阅。
          </p>
        )}

        {state.prepared?.status === "ambiguous" && (
          <AmbiguousCandidatesGrid
            candidates={state.prepared.candidates}
            onPick={state.pickCandidate}
          />
        )}

        {state.prepared?.status === "ready" && state.prepared.existing_subscription_id && (
          <ExistingSubscriptionNotice
            kind={target.kind}
            upgradeMode={state.upgradeMode}
            busy={state.busy}
            onClose={onClose}
            onGoUpgrade={() => {
              const id = state.prepared?.existing_subscription_id;
              onClose();
              navigate(`/subscriptions/${id}?upgrade-run=1`);
            }}
            onUnsubscribe={state.unsubscribe}
          />
        )}

        {state.prepared?.status === "ready" && !state.prepared.existing_subscription_id && (
          <SubscribeReadyForm
            prepared={state.prepared}
            upgradeMode={state.upgradeMode}
            canManageSubscriptions={state.canManageSubscriptions}
            selectedSeasons={state.selectedSeasons}
            onToggleSeason={state.toggleSeason}
            followFuture={state.followFuture}
            onFollowFutureChange={state.setFollowFuture}
            ruleSetId={state.ruleSetId}
            selectableRules={state.selectableRules}
            onRuleSetChange={state.setRuleSetId}
            onCreateRuleSet={() => state.setCreatingRuleSet(true)}
            libraries={state.libraries}
            libraryId={state.libraryId}
            onLibraryChange={state.handleLibraryChange}
            routed={state.routed}
            dispatchPreview={state.dispatchPreview}
          />
        )}
      </div>

      {state.prepared?.status === "ready" && !state.prepared.existing_subscription_id && (
        <div className="flex justify-end gap-3 border-t border-white/[0.07] px-6 py-4 max-md:px-5">
          <button type="button" onClick={onClose} className="btn-glass h-9 px-4 text-ui font-medium">
            取消
          </button>
          <button
            type="button"
            disabled={!state.canSubmit}
            onClick={state.submit}
            className="btn-accent h-9 rounded-full px-5 text-ui font-semibold disabled:opacity-50"
          >
            {state.busy
              ? state.upgradeMode
                ? "正在订阅并体检…"
                : "正在订阅…"
              : state.upgradeMode
                ? "订阅并开始洗版"
                : "确认订阅"}
          </button>
        </div>
      )}

      {state.isAdmin && state.prepared?.existing_subscription_id && (
        <SubscriptionCancelDialog
          open={state.cancelling}
          raised
          subscriptionId={state.prepared.existing_subscription_id}
          title={state.prepared.media?.title ?? target.title ?? ""}
          onClose={() => state.setCancelling(false)}
          onConfirm={state.confirmRemoval}
        />
      )}

      {state.canManageSubscriptions && state.creatingRuleSet && (
        <RuleSetEditorDialog
          ruleSet={null}
          raised
          onClose={() => state.setCreatingRuleSet(false)}
          onSaved={state.onRuleSetSaved}
        />
      )}
    </Modal>
  );
}
