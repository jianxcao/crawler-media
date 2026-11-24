"use client";

import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import * as DropdownMenu from "@radix-ui/react-dropdown-menu";

import { useConfirm, useToast } from "@/components/feedback";
import { ChevronDownIcon, MoreIcon } from "@/components/icons";
import {
  BoostReadout,
  BoostSettingsModal,
} from "@/components/settings/sites/boost-settings-modal";
import { AUTH_TYPE_LABEL, SiteForm } from "@/components/settings/sites/site-form";
import {
  type CatalogItem,
  checkInSite,
  deleteSite,
  reverifySite,
  setSiteEnabled,
  setSiteRatioBoost,
  type Site,
  type SiteBoostStats,
  updateSite,
} from "@/lib/api/sites";
import { useDownloadTasks } from "@/lib/download-tasks";
import { formatBytes } from "@/lib/format";

const STATUS_META = {
  enabled: { label: "已接入", color: "var(--ok)" },
  disabled: { label: "已停用", color: "var(--text-muted)" },
} as const;

/* —— 站点行：一行一站，P0 常驻 + P1 条件徽章；点击展开详情 —— */

interface SiteRowProps {
  item: CatalogItem;
  site: Site;
  boost?: SiteBoostStats;
  expanded: boolean;
  onToggle: () => void;
  /** 确保展开（不切换）：菜单里的「编辑授权」需要先展开详情再亮出表单 */
  onOpen: () => void;
  onChanged: (site: Site) => void;
  onDeleted: (siteId: string) => void;
  onError: (message: string) => void;
}

export function SiteRow({
  item,
  site,
  boost,
  expanded,
  onToggle,
  onOpen,
  onChanged,
  onDeleted,
  onError,
}: SiteRowProps) {
  const confirm = useConfirm();
  const toast = useToast();
  const [busy, setBusy] = useState(false);
  const [editingAuth, setEditingAuth] = useState(false);
  const [boostModal, setBoostModal] = useState<"enable" | "adjust" | null>(null);
  const [actionStatus, setActionStatus] = useState<{
    tone: "ok" | "danger";
    text: string;
  } | null>(null);
  const meta = STATUS_META[site.enabled ? "enabled" : "disabled"];
  const boosting = boost?.boost.enabled === true;

  async function guard(fn: () => Promise<void>) {
    setBusy(true);
    try {
      await fn();
    } catch (e) {
      onError((e as Error).message);
    } finally {
      setBusy(false);
    }
  }

  async function runCheckIn() {
    setBusy(true);
    setActionStatus(null);
    try {
      const res = await checkInSite(site.id);
      onChanged(res);
      setActionStatus({ tone: "ok", text: "已触发签到" });
      toast.success(`「${site.name}」签到成功`);
    } catch (e) {
      const errMsg = (e as Error).message || "签到失败";
      setActionStatus({ tone: "danger", text: errMsg });
      toast.error(`「${site.name}」签到失败：${errMsg}`);
    } finally {
      setBusy(false);
    }
  }

  async function disableBoost() {
    const ok = await confirm({
      title: `关闭「${site.name}」的自动刷分享率？`,
      bullets: [
        "停止抢该站新发布的免费种子",
        "已在做种的刷流任务全部保留，不删除任何数据",
        "站点索引同步回到正常自适应节奏",
        "重新开启时会再次确认预算与保留期",
      ],
      confirmLabel: "关闭刷流",
    });
    if (!ok) return;
    await guard(async () => onChanged(await setSiteRatioBoost(site.id, false)));
  }

  return (
    <div>
      <div
        role="button"
        tabIndex={0}
        aria-expanded={expanded}
        onClick={onToggle}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onToggle();
          }
        }}
        className={`group flex min-h-[52px] cursor-pointer flex-wrap items-center gap-x-3 gap-y-1.5 px-4 py-2.5 transition-colors hover:bg-white/[0.04] focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-white/25 sm:px-5 sm:py-3 ${
          expanded ? "bg-white/[0.045]" : ""
        }`}
      >
        <div className="flex min-w-0 flex-1 items-center gap-2.5">
          <SiteBadge item={item} />
          <span
            className={`truncate text-ui font-semibold ${
              site.enabled ? "text-[var(--text)]" : "text-[var(--text-muted)]"
            }`}
          >
            {site.name}
          </span>
          <span
            className="flex shrink-0 items-center gap-1.5 rounded-full px-2 py-0.5 text-caption font-medium"
            style={{
              background: `color-mix(in oklab, ${meta.color} 12%, transparent)`,
              color: meta.color,
            }}
          >
            <span className="size-1.5 rounded-full" style={{ background: meta.color }} />
            {meta.label}
          </span>
          {actionStatus && (
            <span
              className={`truncate text-caption font-medium ${
                actionStatus.tone === "ok" ? "text-[var(--ok)]" : "text-[var(--danger)]"
              }`}
            >
              {actionStatus.text}
            </span>
          )}
        </div>

        {boosting && (
          <div className="order-3 basis-full sm:order-none sm:basis-auto">
            <BoostReadout boost={boost} />
          </div>
        )}

        <div className="flex shrink-0 items-center gap-0.5">
          <span className="flex size-7 items-center justify-center rounded-full text-[var(--text-faint)] transition-colors group-hover:bg-white/[0.08] group-hover:text-[var(--text-muted)]">
            <ChevronDownIcon
              className={`size-4 transition-transform ${expanded ? "rotate-180" : ""}`}
            />
          </span>
          <SiteActionsMenu
            site={site}
            busy={busy}
            boosting={boosting}
            supportsCheckIn={item.supports_check_in ?? item.profile_id !== "mteam"}
            onSetEnabled={(enabled) =>
              void guard(async () => onChanged(await setSiteEnabled(site.id, enabled)))
            }
            onEnableBoost={() => setBoostModal("enable")}
            onDisableBoost={() => void disableBoost()}
            onBoostSettings={() => setBoostModal("adjust")}
            onEditAuth={() => {
              onOpen();
              setEditingAuth(true);
            }}
            onCheckIn={() => void runCheckIn()}
            onReverify={() =>
              void guard(async () => onChanged(await reverifySite(site.id)))
            }
            onDelete={() =>
              void guard(async () => {
                if (
                  !(await confirm({
                    title: `删除「${site.name}」的配置？`,
                    description:
                      "该站点将不再参与搜索与订阅投递；在池的刷流任务会转出管理并继续做种。可随时重新接入。",
                    confirmLabel: "删除",
                    tone: "danger",
                  }))
                ) {
                  return;
                }
                await deleteSite(site.id);
                onDeleted(site.id);
              })
            }
          />
        </div>
      </div>

      <BoostSettingsModal
        open={boostModal !== null}
        mode={boostModal ?? "adjust"}
        siteName={site.name}
        site={site}
        boost={boost}
        onClose={() => setBoostModal(null)}
        onChanged={onChanged}
      />

      {expanded && (
        <SiteDetail
          item={item}
          site={site}
          boost={boost}
          busy={busy}
          guard={guard}
          onChanged={onChanged}
          editingAuth={editingAuth}
          onCloseEditAuth={() => setEditingAuth(false)}
        />
      )}
    </div>
  );
}

/* —— 展开详情：P2 信息与刷流设置 —— */

interface SiteDetailProps {
  item: CatalogItem;
  site: Site;
  boost?: SiteBoostStats;
  busy: boolean;
  guard: (fn: () => Promise<void>) => Promise<void>;
  onChanged: (site: Site) => void;
  editingAuth: boolean;
  onCloseEditAuth: () => void;
}

function SiteDetail({
  item,
  site,
  boost,
  busy,
  guard,
  onChanged,
  editingAuth,
  onCloseEditAuth,
}: SiteDetailProps) {
  return (
    <div className="divide-y divide-white/[0.05] border-t border-white/[0.06] bg-white/[0.02] px-4 py-3 sm:px-5">
      {boost?.boost.enabled && (
        <DetailSection label="刷流">
          <StatGrid>
            <DetailStat
              label="存储预算"
              value={formatBytes(boost.boost.budget_bytes ?? 0)}
            />
            <DetailStat
              label="汰换保留期"
              value={(boost.boost.hold_days ?? 0) > 0 ? `${boost.boost.hold_days} 天` : "不保护"}
            />
          </StatGrid>
        </DetailSection>
      )}

      <DetailSection label="授权">
        {editingAuth ? (
          <SiteForm
            item={item}
            site={site}
            busy={busy}
            onSubmit={(payload) =>
              guard(async () => {
                onChanged(await updateSite(site.id, payload));
                onCloseEditAuth();
              })
            }
          />
        ) : (
          <p className="text-sub leading-6 text-[var(--text-muted)]">
            {AUTH_TYPE_LABEL[site.auth_type]}
          </p>
        )}
      </DetailSection>
    </div>
  );
}

/* —— 下载器拥堵提示条 —— */

export function QueueCongestionTip() {
  const { tasks } = useDownloadTasks();
  const queued = useMemo(() => tasks.filter((t) => t.state === "queued"), [tasks]);
  if (queued.length === 0) return null;

  const counts = new Map<string, { name: string; count: number }>();
  for (const task of queued) {
    if (task.downloader_id == null) continue;
    const entry = counts.get(task.downloader_id) ?? {
      name: task.downloader_name ?? `#${task.downloader_id}`,
      count: 0,
    };
    entry.count += 1;
    counts.set(task.downloader_id, entry);
  }
  const worst = [...counts.entries()].sort((a, b) => b[1].count - a[1].count)[0];
  if (!worst) return null;
  const [downloaderId, { name }] = worst;

  return (
    <div
      className="flex flex-wrap items-center gap-x-3 gap-y-1.5 rounded-xl border px-4 py-3 text-sub"
      style={{
        borderColor: "color-mix(in oklab, var(--warn) 30%, transparent)",
        background: "color-mix(in oklab, var(--warn) 10%, transparent)",
        color: "var(--warn)",
      }}
    >
      <span className="min-w-0 flex-1">
        下载器「{name}」有 {queued.length} 个任务在排队——活动任务位已满，新种子提交受限，
        刷流已自动暂停投放。建议调大「最大活动种子数」等队列上限。
      </span>
      <Link
        to={`/settings/downloaders?limits=${downloaderId}`}
        className="shrink-0 rounded-full border px-3 py-1 font-medium transition hover:bg-[color-mix(in_oklab,var(--warn)_18%,transparent)]"
        style={{ borderColor: "color-mix(in oklab, var(--warn) 40%, transparent)" }}
      >
        去调整
      </Link>
    </div>
  );
}

/* —— 徽标 / 分段 / 操作菜单 —— */

function SiteBadge({ item }: { item: CatalogItem }) {
  return (
    <span className="icon-chip size-8 shrink-0 !rounded-lg text-sub font-semibold">
      {item.profile_id.charAt(0).toUpperCase()}
    </span>
  );
}

function DetailSection({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <section className="grid gap-1.5 py-3 first:pt-1 last:pb-1 sm:grid-cols-[46px_minmax(0,1fr)] sm:gap-x-4 sm:gap-y-0">
      <p className="text-micro font-medium tracking-wide text-[var(--text-faint)] sm:pt-2">
        {label}
      </p>
      <div className="min-w-0 space-y-2">{children}</div>
    </section>
  );
}

function StatGrid({ children }: { children: React.ReactNode }) {
  return (
    <div className="grid grid-cols-2 gap-x-5 gap-y-3 sm:grid-cols-3 lg:grid-cols-4">
      {children}
    </div>
  );
}

function DetailStat({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0">
      <p className="truncate text-micro text-[var(--text-faint)]">{label}</p>
      <p className="mt-0.5 truncate text-ui font-semibold tabular-nums text-[var(--text)] max-sm:text-sub">
        {value}
      </p>
    </div>
  );
}

interface SiteActionsMenuProps {
  site: Site;
  busy: boolean;
  boosting: boolean;
  supportsCheckIn: boolean;
  onSetEnabled: (enabled: boolean) => void;
  onEnableBoost: () => void;
  onDisableBoost: () => void;
  onBoostSettings: () => void;
  onEditAuth: () => void;
  onCheckIn: () => void;
  onReverify: () => void;
  onDelete: () => void;
}

function SiteActionsMenu({
  site,
  busy,
  boosting,
  supportsCheckIn,
  onSetEnabled,
  onEnableBoost,
  onDisableBoost,
  onBoostSettings,
  onEditAuth,
  onCheckIn,
  onReverify,
  onDelete,
}: SiteActionsMenuProps) {
  const itemClass =
    "glass-row nav-item cursor-pointer px-3 py-2.5 text-sub font-medium outline-none " +
    "data-[highlighted]:!bg-[var(--glass-fill-hover)] data-[highlighted]:!text-[var(--text)] " +
    "data-[disabled]:pointer-events-none data-[disabled]:opacity-40";

  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger asChild>
        <button
          type="button"
          aria-label="站点操作"
          onClick={(e) => e.stopPropagation()}
          className="glass-row !w-auto p-2 data-[state=open]:!bg-[var(--glass-fill-active)] data-[state=open]:!text-[var(--text)]"
        >
          <MoreIcon className="size-4" />
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          align="end"
          sideOffset={6}
          collisionPadding={12}
          className="menu-surface z-50 min-w-[10rem] p-1"
          onClick={(e) => e.stopPropagation()}
        >
          <DropdownMenu.Item
            onSelect={() => onSetEnabled(!site.enabled)}
            disabled={busy}
            className={itemClass}
          >
            {site.enabled ? "停用站点" : "启用站点"}
          </DropdownMenu.Item>
          <DropdownMenu.Item
            onSelect={boosting ? onDisableBoost : onEnableBoost}
            disabled={busy}
            className={itemClass}
          >
            {boosting ? "关闭刷流…" : "开启刷流…"}
          </DropdownMenu.Item>
          {boosting && (
            <DropdownMenu.Item onSelect={onBoostSettings} disabled={busy} className={itemClass}>
              刷流设置…
            </DropdownMenu.Item>
          )}
          <DropdownMenu.Item onSelect={onEditAuth} disabled={busy} className={itemClass}>
            编辑授权
          </DropdownMenu.Item>
          {supportsCheckIn && (
            <DropdownMenu.Item onSelect={onCheckIn} disabled={busy} className={itemClass}>
              立即签到
            </DropdownMenu.Item>
          )}
          <DropdownMenu.Item onSelect={onReverify} disabled={busy} className={itemClass}>
            重新验证
          </DropdownMenu.Item>
          <DropdownMenu.Item
            onSelect={onDelete}
            disabled={busy}
            className={`${itemClass} !text-[var(--danger)] data-[highlighted]:!bg-[var(--danger)]/10`}
          >
            删除配置
          </DropdownMenu.Item>
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}
