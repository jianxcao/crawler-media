"use client";

import { useEffect, useRef, useState } from "react";

import * as DropdownMenu from "@radix-ui/react-dropdown-menu";

import { useConfirm } from "@/components/feedback";
import { ChevronDownIcon, DownloadIcon, MoreIcon } from "@/components/icons";
import {
  type Downloader,
  deleteDownloader,
  reverifyDownloader,
  setDefaultDownloader,
  updateDownloader,
} from "@/lib/api/downloaders";

import { DetailSection, DetailStat, PathMappingTable, StatGrid } from "./downloader-detail";
import { DownloaderForm } from "./downloader-form";
import { DownloaderLimitsModal } from "./downloader-limits-modal";
import { STATUS_META, TYPE_LABEL } from "./downloader-meta";

/* —— 下载器行：一行一台，P0 常驻（名称 / 默认 / 状态）；点击整行展开详情 ——
   与站点行同构：操作全收进 ⋯ 菜单（启停 / 编辑 / 限速 / 默认 / 重测 / 删除），
   视觉不用玻璃质感与 WebGL 开关，配置页要的是轻和稳。 */

export interface DownloaderRowProps {
  downloader: Downloader;
  /** 体检跳转建议预填的映射本机侧路径（无建议时为 null） */
  suggestMapping: string | null;
  /** 拥堵提示跳转（?limits=<id>）：挂载后自动打开「全局限速」弹窗 */
  autoOpenLimits?: boolean;
  expanded: boolean;
  /** 编辑表单是否亮出（嵌在详情里） */
  editing: boolean;
  onToggle: () => void;
  /** 确保展开并亮出编辑表单（菜单「编辑配置」） */
  onEdit: () => void;
  onCloseEdit: () => void;
  onChanged: (downloader: Downloader) => void;
  onDeleted: (id: string) => void;
  /** 需要整体刷新列表的操作（如设默认会同时改动其他条目）之后调用 */
  onRefresh: () => void;
  onError: (message: string) => void;
}

export function DownloaderRow({
  downloader,
  suggestMapping,
  autoOpenLimits = false,
  expanded,
  editing,
  onToggle,
  onEdit,
  onCloseEdit,
  onChanged,
  onDeleted,
  onRefresh,
  onError,
}: DownloaderRowProps) {
  const confirm = useConfirm();
  const [busy, setBusy] = useState(false);
  const [saving, setSaving] = useState(false);
  // Refs also block callbacks captured before React commits a disabled menu.
  const actionInFlight = useRef(false);
  const saveInFlight = useRef(false);
  const [limitsOpen, setLimitsOpen] = useState(false);
  const [verifying, setVerifying] = useState(false);
  // 配置更新使旧测试失效，迟到的结果不能覆盖新配置的待测试状态。
  const verificationRevision = useRef(0);
  // 拥堵提示跳转落地：打开一次即完成使命，之后开合归用户（关了不复弹）
  useEffect(() => {
    if (autoOpenLimits) setLimitsOpen(true);
  }, [autoOpenLimits]);
  // 「API 通、路径瞎」的体检结论已不在新契约下载器视图上（无 path_health），
  // 路径异常只会在投递时被后端拒绝并给出可读错误
  const meta = STATUS_META[verifying ? "verifying" : downloader.status];
  const failed = !verifying && downloader.status === "failed" && !!downloader.last_error;

  async function guard(fn: () => Promise<void>) {
    if (actionInFlight.current || saveInFlight.current) return;
    actionInFlight.current = true;
    setBusy(true);
    try {
      await fn();
    } catch (e) {
      onError((e as Error).message);
    } finally {
      actionInFlight.current = false;
      setBusy(false);
    }
  }

  // 副标题：类型 + 分类（新契约无版本/上次检查时间）
  const subtitle = [
    TYPE_LABEL[downloader.kind],
    downloader.category ? `分类 ${downloader.category}` : null,
  ]
    .filter(Boolean)
    .join(" · ");

  return (
    <div>
      {/* 行主体：整行是展开热区。桌面单行；移动端错误原因折到第二行 */}
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
        className={`group flex min-h-[56px] cursor-pointer flex-wrap items-center gap-x-3 gap-y-1.5 px-4 py-2.5 transition-colors hover:bg-white/[0.04] focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-inset focus-visible:ring-white/25 sm:px-5 sm:py-3 ${
          expanded ? "bg-white/[0.045]" : ""
        }`}
      >
        {/* P0：图标 + 名称（可点开 WebUI）+ 默认 + 状态 */}
        <div className="flex min-w-0 flex-1 items-center gap-3">
          <span className="icon-chip size-9 shrink-0 !rounded-xl">
            <DownloadIcon className="size-[18px]" />
          </span>
          <div className="min-w-0">
            {/* 窄屏徽章允许折到名称下一行，别把名称挤成两个字 */}
            <div className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
              <a
                href={downloader.url}
                target="_blank"
                rel="noreferrer"
                title="在新窗口打开下载器 WebUI"
                onClick={(e) => e.stopPropagation()}
                className="truncate text-ui font-semibold text-[var(--text)] underline decoration-transparent underline-offset-4 transition-colors hover:text-white/80 hover:decoration-white/50"
              >
                {downloader.name}
              </a>
              {downloader.is_default && (
                <span className="shrink-0 rounded-full border border-white/[0.12] bg-[var(--accent-soft)] px-2 py-0.5 text-caption font-semibold text-[var(--accent)]">
                  默认
                </span>
              )}
              {downloader.enabled === false && downloader.status !== "disabled" && (
                <span className="shrink-0 text-caption text-[var(--text-faint)]">已停用</span>
              )}
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
            </div>
            <p className="mt-0.5 truncate text-caption text-[var(--text-faint)]">{subtitle}</p>
          </div>
        </div>

        {/* P1：连接失败时把原因亮出来——那一刻没有比它更重要的信息 */}
        {failed && (
          <p
            className="order-3 basis-full truncate text-caption text-[var(--danger)] sm:order-none sm:max-w-[40%] sm:basis-auto"
            title={downloader.last_error ?? undefined}
          >
            {downloader.last_error}
          </p>
        )}

        {/* 控制区：展开箭头 + 操作菜单 */}
        <div className="flex shrink-0 items-center gap-0.5">
          <span className="flex size-7 items-center justify-center rounded-full text-[var(--text-faint)] transition-colors group-hover:bg-white/[0.08] group-hover:text-[var(--text-muted)]">
            <ChevronDownIcon
              className={`size-4 transition-transform ${expanded ? "rotate-180" : ""}`}
            />
          </span>
          <DownloaderActionsMenu
            downloader={downloader}
            busy={busy || saving || verifying}
            editing={editing}
            onEdit={editing ? onCloseEdit : onEdit}
            onLimits={() => setLimitsOpen(true)}
            onSetDefault={() =>
              void guard(async () => {
                await setDefaultDownloader(downloader.id);
                // 原默认的标记同时被清掉，整体刷新一次拿到全量新状态
                onRefresh();
              })
            }
            onReverify={() =>
              void guard(async () => {
                const revision = ++verificationRevision.current;
                setVerifying(true);
                try {
                  const result = await reverifyDownloader(downloader.id, downloader);
                  if (revision === verificationRevision.current) onChanged(result);
                } finally {
                  if (revision === verificationRevision.current) {
                    setVerifying(false);
                  }
                }
              })
            }
            onDelete={() =>
              void guard(async () => {
                if (
                  !(await confirm({
                    title: `删除下载器「${downloader.name}」？`,
                    description: "下载器中的任务不受影响，只是本系统不再向它投递。",
                    confirmLabel: "删除",
                    tone: "danger",
                  }))
                ) {
                  return;
                }
                await deleteDownloader(downloader.id);
                onDeleted(downloader.id);
              })
            }
          />
        </div>
      </div>

      {/* 展开详情：连接 / 分类 / 路径映射，以及嵌入的编辑表单 */}
      {expanded && (
        <div className="divide-y divide-white/[0.05] border-t border-white/[0.06] bg-white/[0.02] px-4 py-3 sm:px-5">
          {editing ? (
            <div className="py-1">
              <DownloaderForm
                downloader={downloader}
                suggestMapping={suggestMapping}
                onSubmit={async (payload) => {
                  if (saveInFlight.current) return;
                  saveInFlight.current = true;
                  const revision = ++verificationRevision.current;
                  setVerifying(false);
                  setSaving(true);
                  try {
                    const updated = await updateDownloader(downloader.id, payload);
                    if (revision === verificationRevision.current) {
                      onChanged(updated);
                      onCloseEdit();
                    }
                  } finally {
                    saveInFlight.current = false;
                    setSaving(false);
                  }
                }}
                onCancel={onCloseEdit}
                onError={onError}
              />
            </div>
          ) : (
            <>
              <DetailSection label="连接">
                <StatGrid>
                  <DetailStat
                    label="地址"
                    value={downloader.url}
                    href={downloader.url}
                    title="在新窗口打开下载器 WebUI"
                    wide
                  />
                  <DetailStat label="用户名" value={downloader.username ?? "未设置"} />
                  <DetailStat label="类型" value={TYPE_LABEL[downloader.kind]} />
                </StatGrid>
              </DetailSection>
              <DetailSection label="分类">
                <StatGrid>
                  <DetailStat
                    label="下载分类"
                    value={downloader.category ?? "未设置"}
                    wide
                  />
                </StatGrid>
              </DetailSection>
              <DetailSection label="映射">
                <PathMappingTable
                  mappings={downloader.path_maps.map((m) => ({ local: m.to, remote: m.from }))}
                  health={[]}
                />
              </DetailSection>
            </>
          )}
        </div>
      )}

      <DownloaderLimitsModal
        open={limitsOpen}
        downloader={downloader}
        onClose={() => setLimitsOpen(false)}
      />
    </div>
  );
}

/* —— 下载器操作折叠菜单（与站点卡片同款 Radix DropdownMenu） —— */

interface DownloaderActionsMenuProps {
  downloader: Downloader;
  busy: boolean;
  /** 编辑表单已亮出时菜单项变成「收起编辑」 */
  editing: boolean;
  onEdit: () => void;
  onLimits: () => void;
  onSetDefault: () => void;
  onReverify: () => void;
  onDelete: () => void;
}

export function DownloaderActionsMenu({
  downloader,
  busy,
  editing,
  onEdit,
  onLimits,
  onSetDefault,
  onReverify,
  onDelete,
}: DownloaderActionsMenuProps) {
  // Radix DropdownMenu：菜单渲染进 body Portal 并做碰撞检测；
  // 菜单项加大内边距保证移动端触控目标
  const itemClass =
    "glass-row nav-item cursor-pointer px-3 py-2.5 text-sub font-medium outline-none " +
    "data-[highlighted]:!bg-[var(--glass-fill-hover)] data-[highlighted]:!text-[var(--text)] " +
    "data-[disabled]:pointer-events-none data-[disabled]:opacity-40";
  // 未测试和停用都不代表测试正在执行；只有本地 busy 禁止重复操作。

  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger asChild>
        <button
          type="button"
          aria-label="下载器操作"
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
          <DropdownMenu.Item onSelect={onEdit} className={itemClass}>
            {editing ? "收起编辑" : "编辑配置"}
          </DropdownMenu.Item>
          <DropdownMenu.Item onSelect={onLimits} disabled={busy} className={itemClass}>
            全局限速…
          </DropdownMenu.Item>
          <DropdownMenu.Item
            onSelect={onSetDefault}
            disabled={busy || downloader.is_default}
            className={itemClass}
          >
            设为默认
          </DropdownMenu.Item>
          <DropdownMenu.Item
            onSelect={onReverify}
            disabled={busy}
            className={itemClass}
          >
            重新测试连接
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
