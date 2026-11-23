"use client";

import { CheckIcon } from "@/components/icons";
import { PosterImage } from "@/components/poster-image";
import { ruleSetSummary } from "@/components/rule-sets-panel";
import type { MediaLibrary } from "@/lib/api/libraries";
import type {
  DispatchPreview,
  PrepareResult,
  ResolveCandidate,
  RuleSet,
  SeasonOverview,
} from "@/lib/api/subscriptions";
import { cachedImageUrl } from "@/lib/image-proxy";
import type { MediaType } from "@/lib/media-types";
import { formatSeasonOwned, formatSeasonProgress } from "@/lib/subscribe-dialog-helpers";

/** 豆瓣收敛歧义时展示多个候选供用户确认一次。 */
export function AmbiguousCandidatesGrid({
  candidates,
  onPick,
}: {
  candidates: ResolveCandidate[];
  onPick: (candidate: ResolveCandidate) => void;
}) {
  return (
    <div className="mt-4">
      <p className="text-ui text-[var(--text-muted)]">
        找到多个可能的条目，请确认你订阅的是哪一部：
      </p>
      <div className="mt-3 grid grid-cols-4 gap-3 max-md:grid-cols-3 max-md:gap-2">
        {candidates.map((c) => (
          <button
            key={c.tmdb_id}
            type="button"
            onClick={() => onPick(c)}
            className="group text-left"
          >
            <div className="dark-media-surface aspect-[2/3] overflow-hidden rounded-lg bg-[#141824] ring-1 ring-white/10 transition group-hover:ring-white/40">
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
  );
}

/** 已订阅条目的管理态面板：继续逛 / 跳转洗版 / 取消订阅。 */
export function ExistingSubscriptionNotice({
  kind,
  upgradeMode,
  busy,
  onClose,
  onGoUpgrade,
  onUnsubscribe,
}: {
  kind: MediaType;
  upgradeMode: boolean;
  busy: boolean;
  onClose: () => void;
  onGoUpgrade: () => void;
  onUnsubscribe: () => void;
}) {
  return (
    <div className="mt-4">
      <p className="flex items-center gap-2 text-ui text-white/85">
        <CheckIcon className="size-4 text-[var(--ok)]" />
        该{kind === "movie" ? "电影" : "剧集"}已在订阅中，系统
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
        {upgradeMode && (
          <button
            type="button"
            onClick={onGoUpgrade}
            className="btn-accent h-9 rounded-full px-4 text-ui font-semibold"
          >
            去洗一轮版
          </button>
        )}
        <button
          type="button"
          disabled={busy}
          onClick={onUnsubscribe}
          className="h-9 rounded-full border border-red-400/30 bg-red-500/10 px-4 text-ui font-medium text-red-200 transition hover:bg-red-500/20 disabled:opacity-50"
        >
          取消订阅
        </button>
      </div>
    </div>
  );
}

/** `ready` 且尚未订阅时的表单主体：选季、续订开关、规则组、目标库与投递预检。 */
export function SubscribeReadyForm({
  prepared,
  upgradeMode,
  canManageSubscriptions,
  selectedSeasons,
  onToggleSeason,
  followFuture,
  onFollowFutureChange,
  ruleSetId,
  selectableRules,
  onRuleSetChange,
  onCreateRuleSet,
  libraries,
  libraryId,
  onLibraryChange,
  routed,
  dispatchPreview,
}: {
  prepared: PrepareResult;
  upgradeMode: boolean;
  canManageSubscriptions: boolean;
  selectedSeasons: Set<number>;
  onToggleSeason: (seasonNumber: number) => void;
  followFuture: boolean;
  onFollowFutureChange: (next: boolean) => void;
  ruleSetId: string | null;
  selectableRules: RuleSet[];
  onRuleSetChange: (nextId: string) => void;
  onCreateRuleSet: () => void;
  libraries: MediaLibrary[];
  libraryId: string | null;
  onLibraryChange: (newLibId: string) => void;
  routed: { libraryId: string; reason: string | null } | null;
  dispatchPreview: DispatchPreview | null;
}) {
  const pickedRule = selectableRules.find((r) => r.id === ruleSetId);
  const ruleChips = pickedRule
    ? ruleSetSummary(pickedRule.atoms, { keepOldVersions: pickedRule.keep_old_versions })
    : null;

  return (
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
                onToggle={() => onToggleSeason(s.season_number)}
              />
            ))}
          </div>

          <label className="mt-4 flex cursor-pointer items-center justify-between rounded-xl border border-white/[0.08] bg-white/[0.04] px-4 py-3">
            <span>
              <span className="block text-ui font-medium text-white/90">自动续订</span>
              <span className="mt-0.5 block text-caption text-[var(--text-faint)]">
                之后播出的新集、新一季自动加入追踪
              </span>
            </span>
            <input
              type="checkbox"
              checked={followFuture}
              onChange={(e) => onFollowFutureChange(e.target.checked)}
              className="size-4 accent-[var(--accent-2)]"
            />
          </label>
        </section>
      )}

      {(upgradeMode || (canManageSubscriptions && selectableRules.length > 0)) && (
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
                onClick={onCreateRuleSet}
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
              onChange={(e) => onRuleSetChange(e.target.value)}
              className="w-full rounded-xl border border-white/[0.08] bg-white/[0.04] px-3.5 py-2.5 text-ui text-white/90 outline-none focus:border-white/25"
            >
              {selectableRules.map((r) => (
                <option key={r.id} value={r.id}>
                  {r.name}
                  {r.is_default ? "（默认）" : ""}
                </option>
              ))}
            </select>
          )}
          {ruleChips && (
            <p className="mt-1.5 flex flex-wrap gap-1.5">
              {ruleChips.length === 0 ? (
                <span className="text-caption text-[var(--warn)]/90">
                  该规则组不限任何条件——可能抓到低画质或无人做种的资源，
                  建议在「设置 → 订阅规则 → 规则组」里加上分辨率与做种数限制
                </span>
              ) : (
                ruleChips.map((chip) => (
                  <span
                    key={chip}
                    className="rounded-md bg-white/[0.07] px-1.5 py-0.5 text-caption text-white/75"
                  >
                    {chip}
                  </span>
                ))
              )}
            </p>
          )}
        </section>
      )}

      {canManageSubscriptions && libraries.length > 0 && (
        <section>
          <h3 className="mb-2 text-ui font-semibold text-white/85">入库到</h3>
          <select
            value={libraryId ?? undefined}
            onChange={(e) => onLibraryChange(e.target.value)}
            className="w-full rounded-xl border border-white/[0.08] bg-white/[0.04] px-3.5 py-2.5 text-ui text-white/90 outline-none focus:border-white/25"
          >
            {libraries.map((l) => (
              <option key={l.id} value={l.id}>
                {l.name}
                {l.is_default ? "（默认）" : ""}
              </option>
            ))}
          </select>
          {routed && routed.reason && libraryId === routed.libraryId && (
            <p className="mt-1.5 text-caption leading-relaxed text-[var(--accent)]/90">
              自动选库：{routed.reason}
            </p>
          )}
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
                  <>
                    将直接下载到库内目录{" "}
                    <span className="font-mono">
                      {(dispatchPreview.entry_dir ?? dispatchPreview.path)?.replace(/\/+$/, "")}
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
  );
}

/** 季选择行：季名 + 播出进度；未播季弱化显示但可勾（勾了=要整季）。 */
export function SeasonRow({
  season,
  checked,
  onToggle,
}: {
  season: SeasonOverview;
  checked: boolean;
  onToggle: () => void;
}) {
  const progress = formatSeasonProgress(season);
  const owned = formatSeasonOwned(season);
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
