"use client";

import * as DropdownMenu from "@radix-ui/react-dropdown-menu";

import { MoreIcon } from "@/components/icons";
import { Modal } from "@/components/modal";

export interface SubscriptionManageActionsProps {
  busy: boolean;
  paused: boolean;
  completed: boolean;
  canSubscribe: boolean;
  canManageSubscriptions: boolean;
  /** null 表示电影订阅，不展示没有业务语义的自动续订动作。 */
  followFuture: boolean | null;
  onAdjust: () => void;
  /** 打开「洗一轮版」弹层（quality-upgrade.md §13.5 的订阅详情入口） */
  onUpgradeRun: () => void;
  onToggleFollowFuture: () => void;
  onSwitchRule: () => void;
  onTogglePause: () => void;
  onRemove: () => void;
}

/** 桌面端管理菜单：Portal 与碰撞检测交给 Radix，避免 Hero 的 overflow-hidden 裁掉菜单。 */
export function SubscriptionManageMenu({
  busy,
  paused,
  completed,
  canSubscribe,
  canManageSubscriptions,
  followFuture,
  onAdjust,
  onUpgradeRun,
  onToggleFollowFuture,
  onSwitchRule,
  onTogglePause,
  onRemove,
}: SubscriptionManageActionsProps) {
  const itemClass =
    "glass-row nav-item cursor-pointer px-3 py-2 text-ui font-medium outline-none " +
    "data-[highlighted]:!bg-[var(--glass-fill-hover)] data-[highlighted]:!text-[var(--text)] " +
    "data-[disabled]:pointer-events-none data-[disabled]:opacity-40";

  return (
    <DropdownMenu.Root>
      <DropdownMenu.Trigger asChild>
        <button
          type="button"
          disabled={busy}
          className="inline-flex h-9 items-center gap-1.5 rounded-full px-3 text-sub font-medium text-white/65 transition hover:bg-white/[0.08] hover:text-white data-[state=open]:bg-white/[0.1] data-[state=open]:text-white disabled:opacity-40"
        >
          <MoreIcon className="size-4" />
          更多
        </button>
      </DropdownMenu.Trigger>
      <DropdownMenu.Portal>
        <DropdownMenu.Content
          align="end"
          sideOffset={6}
          collisionPadding={12}
          className="menu-surface z-50 min-w-[10.5rem] p-1"
        >
          {canSubscribe && (
            <DropdownMenu.Item onSelect={onAdjust} className={itemClass}>
              调整订阅…
            </DropdownMenu.Item>
          )}
          {canSubscribe && (
            <DropdownMenu.Item onSelect={onUpgradeRun} disabled={busy} className={itemClass}>
              洗一轮版…
            </DropdownMenu.Item>
          )}
          {canSubscribe && followFuture !== null && (
            <DropdownMenu.Item
              onSelect={onToggleFollowFuture}
              disabled={busy}
              className={itemClass}
            >
              {followFuture ? "关闭自动续订" : "开启自动续订"}
            </DropdownMenu.Item>
          )}
          {canManageSubscriptions && (
            <DropdownMenu.Item onSelect={onSwitchRule} className={itemClass}>
              更换规则组…
            </DropdownMenu.Item>
          )}
          {canSubscribe && (
            <DropdownMenu.Item
              onSelect={onTogglePause}
              disabled={busy || completed}
              className={itemClass}
            >
              {paused ? "恢复追踪" : "暂停追踪"}
            </DropdownMenu.Item>
          )}
          {canSubscribe && <DropdownMenu.Separator className="my-1 h-px bg-white/[0.07]" />}
          {canSubscribe && (
            <DropdownMenu.Item
              onSelect={onRemove}
              disabled={busy}
              className={`${itemClass} !text-[var(--danger)] data-[highlighted]:!bg-[var(--danger)]/10`}
            >
              取消订阅
            </DropdownMenu.Item>
          )}
        </DropdownMenu.Content>
      </DropdownMenu.Portal>
    </DropdownMenu.Root>
  );
}

/** 移动端管理动作使用底部抽屉。 */
export function SubscriptionManageSheet({
  open,
  busy,
  paused,
  completed,
  canSubscribe,
  canManageSubscriptions,
  followFuture,
  onClose,
  onAdjust,
  onUpgradeRun,
  onToggleFollowFuture,
  onSwitchRule,
  onTogglePause,
  onRemove,
}: SubscriptionManageActionsProps & { open: boolean; onClose: () => void }) {
  const rowClass =
    "flex min-h-12 w-full items-center justify-between gap-3 px-4 py-3 text-left text-ui " +
    "font-medium text-white/85 transition hover:bg-white/[0.07] disabled:opacity-40";

  return (
    <Modal open={open} onClose={onClose} label="管理订阅">
      <div className="p-4 pt-3">
        <div aria-hidden className="mx-auto mb-3 h-1 w-10 rounded-full bg-white/20" />
        <h2 className="px-2 text-title-sm font-bold text-white">管理订阅</h2>
        <div className="mt-3 overflow-hidden rounded-xl bg-white/[0.035]">
          {canSubscribe && (
            <button type="button" onClick={onAdjust} className={rowClass}>
              <span>调整订阅</span>
              <span aria-hidden className="text-white/35">›</span>
            </button>
          )}
          {canSubscribe && (
            <button type="button" disabled={busy} onClick={onUpgradeRun} className={rowClass}>
              <span>洗一轮版</span>
              <span aria-hidden className="text-white/35">›</span>
            </button>
          )}
          {canSubscribe && followFuture !== null && (
            <button
              type="button"
              disabled={busy}
              onClick={onToggleFollowFuture}
              className={rowClass}
            >
              <span>{followFuture ? "关闭自动续订" : "开启自动续订"}</span>
            </button>
          )}
          {canManageSubscriptions && (
            <button type="button" onClick={onSwitchRule} className={rowClass}>
              <span>更换规则组</span>
              <span aria-hidden className="text-white/35">›</span>
            </button>
          )}
          {canSubscribe && (
            <button
              type="button"
              disabled={busy || completed}
              onClick={onTogglePause}
              className={rowClass}
            >
              <span>{paused ? "恢复追踪" : "暂停追踪"}</span>
              <span aria-hidden className="text-white/35">›</span>
            </button>
          )}
          {canSubscribe && (
            <button
              type="button"
              disabled={busy}
              onClick={onRemove}
              className={`${rowClass} border-t border-white/[0.07] !text-[var(--danger)] hover:!bg-[var(--danger)]/10`}
            >
              <span>取消订阅</span>
            </button>
          )}
        </div>
        <button
          type="button"
          onClick={onClose}
          className="btn-glass mt-3 h-11 w-full text-ui font-medium"
        >
          关闭
        </button>
      </div>
    </Modal>
  );
}
