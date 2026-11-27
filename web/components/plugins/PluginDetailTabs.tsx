"use client";

import { useState } from "react";
import { Link } from "react-router-dom";

import { Modal } from "@/components/modal";
import {
  type AuditEventView,
  type PluginDetailView as PluginDetailData,
  type PluginEventDeliveryView,
  type PluginGrantSpec,
  type PluginPermissionsView,
  type PluginTaskView,
  type TaskReceiptView,
  type UiDescriptorView,
  type UiTrustPreparationView,
  ensureOperationSucceeded,
  listPluginTaskReceipts,
  newIdempotencyKey,
  runPluginTask,
  updatePluginTask,
  waitOperationTerminal,
} from "@/lib/api/plugins";

export function PluginOverviewSection({
  pluginId,
  detail,
  uiDesc,
  uiAuthExpired,
  busyAction,
  onPrepareTrust,
  onRevokeTrust,
}: {
  pluginId: string;
  detail: PluginDetailData;
  uiDesc: UiDescriptorView | null;
  uiAuthExpired: boolean;
  busyAction: string | null;
  onPrepareTrust: () => void;
  onRevokeTrust: () => void;
}) {
  return (
    <div className="grid gap-5 md:grid-cols-2">
      <section className="space-y-3 rounded-xl border border-white/10 bg-white/[0.03] p-5">
        <h2 className="text-body font-semibold text-[var(--text)]">运行时摘要</h2>
        <dl className="grid grid-cols-2 gap-3 text-caption">
          <div>
            <dt className="text-[var(--text-faint)]">生命周期</dt>
            <dd className="mt-0.5 font-medium text-[var(--text)]">{detail.lifecycle}</dd>
          </div>
          <div>
            <dt className="text-[var(--text-faint)]">健康状态</dt>
            <dd className="mt-0.5 font-medium text-[var(--text)]">{detail.health}</dd>
          </div>
          <div className="col-span-2">
            <dt className="text-[var(--text-faint)]">Package Hash</dt>
            <dd className="mt-0.5 break-all font-mono text-[11px] text-[var(--text-muted)]">
              {detail.package_hash}
            </dd>
          </div>
        </dl>
      </section>

      <section className="space-y-3 rounded-xl border border-white/10 bg-white/[0.03] p-5">
        <h2 className="text-body font-semibold text-[var(--text)]">UI 信任边界</h2>
        {uiAuthExpired ? (
          <div
            role="alert"
            className="space-y-2 rounded-lg border border-amber-400/30 bg-amber-400/10 p-3 text-caption"
          >
            <p className="font-medium text-[var(--text)]">
              Cookie 会话已失效或未建立（HTTP 401），无法校验插件前端界面。
            </p>
            <p className="text-[var(--text-muted)]">
              请重新登录以刷新同源浏览器 Cookie 后再查看或授权插件 UI。
            </p>
            <Link
              to={`/login?next=${encodeURIComponent(`/settings/plugins/${pluginId}`)}`}
              className="btn-accent inline-flex rounded-full px-3.5 py-1 text-caption font-semibold"
            >
              重新登录
            </Link>
          </div>
        ) : uiDesc ? (
          <div className="space-y-3 text-caption">
            <p className="text-[var(--text-muted)]">
              模式：{uiDesc.ui_mode} · 状态：
              <strong className={uiDesc.ui_trusted ? "text-emerald-300" : "text-amber-300"}>
                {uiDesc.ui_trusted ? "已信任" : "未信任"}
              </strong>
              （trust rev #{uiDesc.trust?.revision ?? 0}）
            </p>
            <p className="break-all font-mono text-[11px] text-[var(--text-faint)]">
              bundle_sha256: {uiDesc.bundle_sha256}
            </p>
            <div className="flex flex-wrap gap-2">
              {!uiDesc.ui_trusted ? (
                <button
                  type="button"
                  disabled={busyAction !== null}
                  onClick={onPrepareTrust}
                  className="btn-accent rounded-full px-4 py-1.5 text-caption font-semibold"
                >
                  授予 UI 信任…
                </button>
              ) : (
                <button
                  type="button"
                  disabled={busyAction !== null}
                  onClick={onRevokeTrust}
                  className="btn-glass px-3.5 py-1.5 text-caption font-medium text-[#ff6b6b]"
                >
                  撤销 UI 信任
                </button>
              )}
            </div>
          </div>
        ) : (
          <p className="text-caption text-[var(--text-faint)]">
            纯 schema 模式插件（无前端联邦脚本需要信任）。
          </p>
        )}
      </section>
    </div>
  );
}

export function TrustGrantModal({
  trustPrep,
  reauthPassword,
  trustBusy,
  onPasswordChange,
  onClose,
  onConfirm,
}: {
  trustPrep: UiTrustPreparationView;
  reauthPassword: string;
  trustBusy: boolean;
  onPasswordChange: (pw: string) => void;
  onClose: () => void;
  onConfirm: (e: React.FormEvent) => void;
}) {
  return (
    <Modal open onClose={onClose} label="授予插件 UI 信任" width="lg">
      <form onSubmit={onConfirm} className="space-y-4 p-6">
        <h2 className="text-title font-bold text-[var(--text)]">确认授予 UI 信任</h2>
        <p className="text-sub text-[var(--text-muted)]">
          受信任后，该插件的前端联邦脚本将在当前工作台同源会话下执行。请核对包摘要、Bundle 摘要与声明能力：
        </p>
        <div className="space-y-2 rounded-xl border border-white/10 bg-white/[0.03] p-3 text-caption">
          <p>
            插件：{trustPrep.plugin_id} (remote: {trustPrep.remote_name})
          </p>
          <p className="break-all font-mono text-[11px]">bundle_sha256: {trustPrep.bundle_sha256}</p>
          <p>能力：{trustPrep.capabilities.join(", ") || "无"}</p>
          <p>入口：{trustPrep.surfaces.map((s) => `${s.kind}:${s.id}`).join(", ")}</p>
        </div>
        <div>
          <label
            htmlFor="ui-trust-password"
            className="mb-1 block text-caption font-medium text-[var(--text-muted)]"
          >
            管理员密码二次验证
          </label>
          <input
            id="ui-trust-password"
            type="password"
            required
            value={reauthPassword}
            onChange={(e) => onPasswordChange(e.target.value)}
            className="w-full rounded-lg border border-white/10 bg-white/[0.06] px-3 py-2 text-ui text-[var(--text)]"
          />
        </div>
        <div className="flex justify-end gap-2">
          <button type="button" disabled={trustBusy} onClick={onClose} className="btn-glass px-4 py-2 text-sub">
            取消
          </button>
          <button
            type="submit"
            disabled={trustBusy || !reauthPassword}
            className="btn-accent rounded-full px-4 py-2 text-sub font-semibold disabled:opacity-50"
          >
            {trustBusy ? "验证中…" : "确认信任并启用"}
          </button>
        </div>
      </form>
    </Modal>
  );
}

export function PluginPermissionsTab({
  perms,
  pluginEditable = true,
  busyAction,
  onGrantAll,
  onRevokeGrant,
}: {
  perms: PluginPermissionsView;
  pluginEditable?: boolean;
  busyAction: string | null;
  onGrantAll: () => void;
  onRevokeGrant: (grant: PluginGrantSpec) => void;
}) {
  const canGrantAll =
    pluginEditable && busyAction === null && perms.required_capabilities.length > 0;
  return (
    <div className="space-y-4 rounded-xl border border-white/10 bg-white/[0.03] p-5">
      <div className="flex items-center justify-between gap-2">
        <h2 className="text-body font-semibold text-[var(--text)]">能力授予</h2>
        <button
          type="button"
          disabled={!canGrantAll}
          onClick={onGrantAll}
          className="btn-glass px-3 py-1.5 text-caption font-medium disabled:opacity-50"
        >
          同步授予全部声明能力
        </button>
      </div>
      {!pluginEditable && (
        <div className="rounded-lg border border-amber-400/20 bg-amber-400/[0.06] px-3.5 py-2 text-caption text-amber-200">
          请先停用插件并等待排空完成后再同步授予能力。
        </div>
      )}
      <div className="space-y-2">
        {perms.effective_grants.map((g, idx) => (
          <div
            key={`${g.capability}-${idx}`}
            className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-white/10 bg-white/[0.02] px-3.5 py-2.5 text-caption"
          >
            <div>
              <span className="font-mono font-medium text-[var(--text)]">{g.capability}</span>
              <span className="ml-2 text-[var(--text-faint)]">
                {typeof g.scope === "string" ? g.scope : JSON.stringify(g.scope)} · {g.status}
              </span>
            </div>
            {g.status === "granted" ? (
              <button
                type="button"
                disabled={busyAction !== null}
                onClick={() =>
                  onRevokeGrant({
                    capability: g.capability,
                    scope: g.scope,
                    sensitive: g.sensitive,
                  })
                }
                className="text-caption text-[#ff6b6b] hover:underline disabled:opacity-50"
              >
                撤销
              </button>
            ) : (
              <span className="text-caption text-[var(--text-faint)]">已撤销</span>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

async function runDefaultTaskAction(
  pluginId: string,
  task: PluginTaskView,
  kind: "toggle" | "run",
  onTasksChanged?: () => void,
) {
  if (kind === "toggle") {
    const op = await updatePluginTask(
      pluginId,
      task.id,
      {
        enabled: !task.enabled,
        schedule: { interval_secs: task.schedule.interval_secs ?? 3600 },
        config: task.config ?? {},
      },
      task.definition_revision,
      newIdempotencyKey(),
    );
    ensureOperationSucceeded(await waitOperationTerminal(op));
    onTasksChanged?.();
    return;
  }
  let defRev = task.definition_revision;
  if (!task.enabled) {
    const key = newIdempotencyKey();
    const enOp = await updatePluginTask(
      pluginId,
      task.id,
      {
        enabled: true,
        schedule: { interval_secs: task.schedule.interval_secs ?? 3600 },
        config: task.config ?? {},
      },
      defRev,
      `${key}:enable`,
    );
    ensureOperationSucceeded(await waitOperationTerminal(enOp));
    defRev += 1;
  }
  const runOp = await runPluginTask(pluginId, task.id, {}, defRev, newIdempotencyKey());
  ensureOperationSucceeded(await waitOperationTerminal(runOp));
  onTasksChanged?.();
}

export function PluginTasksTab({
  pluginId,
  tasks,
  receiptsByTask,
  onReceiptsLoaded,
  busyAction,
  onRunTask,
  onToggleTask,
  onTasksChanged,
}: {
  pluginId: string;
  tasks: PluginTaskView[];
  receiptsByTask: Record<string, TaskReceiptView[]>;
  onReceiptsLoaded: (taskId: string, items: TaskReceiptView[]) => void;
  busyAction: string | null;
  onRunTask?: (task: PluginTaskView) => void;
  onToggleTask?: (task: PluginTaskView) => void;
  onTasksChanged?: () => void;
}) {
  const [loadingTaskId, setLoadingTaskId] = useState<string | null>(null);
  const [localBusy, setLocalBusy] = useState<string | null>(null);
  const isBusy = busyAction !== null || localBusy !== null;

  async function trigger(task: PluginTaskView, kind: "toggle" | "run") {
    if (kind === "toggle" && onToggleTask) return onToggleTask(task);
    if (kind === "run" && onRunTask) return onRunTask(task);
    setLocalBusy(`${kind}:${task.id}`);
    try {
      await runDefaultTaskAction(pluginId, task, kind, onTasksChanged);
    } finally {
      setLocalBusy(null);
    }
  }

  return (
    <div className="space-y-4">
      {tasks.map((task) => {
        const receipts = receiptsByTask[task.id] ?? [];
        return (
          <div key={task.id} className="space-y-3 rounded-xl border border-white/10 bg-white/[0.03] p-5">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <div>
                <h3 className="text-body font-semibold text-[var(--text)]">{task.id}</h3>
                <p className="text-caption text-[var(--text-faint)]">
                  状态: {task.enabled ? "已启用" : "已停用"} · interval: {task.schedule.interval_secs ?? "-"}s · rev #{task.definition_revision}
                </p>
              </div>
              <div className="flex flex-wrap items-center gap-2">
                <button
                  type="button"
                  disabled={isBusy}
                  onClick={() => void trigger(task, "toggle")}
                  className="btn-glass px-3 py-1.5 text-caption font-medium"
                >
                  {task.enabled ? "停用" : "启用"}
                </button>
                <button
                  type="button"
                  disabled={loadingTaskId === task.id}
                  onClick={async () => {
                    setLoadingTaskId(task.id);
                    try {
                      const rc = await listPluginTaskReceipts(pluginId, task.id);
                      onReceiptsLoaded(task.id, rc.items);
                    } finally {
                      setLoadingTaskId(null);
                    }
                  }}
                  className="btn-glass px-3 py-1.5 text-caption"
                >
                  查看回执
                </button>
                <button
                  type="button"
                  disabled={isBusy}
                  onClick={() => void trigger(task, "run")}
                  className="btn-accent rounded-full px-3.5 py-1.5 text-caption font-semibold"
                >
                  立即运行
                </button>
              </div>
            </div>
            {receipts.length > 0 && (
              <div className="space-y-1 border-t border-white/10 pt-2 text-caption text-[var(--text-muted)]">
                {receipts.slice(0, 5).map((r) => (
                  <div key={r.operation_id} className="flex justify-between gap-3">
                    <span>
                      {r.state} ({r.phase})
                    </span>
                    <span className="text-[var(--text-faint)]">{r.observed_at}</span>
                  </div>
                ))}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

export function PluginAuditTab({
  events,
  audit,
  busyAction,
  onRequeueEvent,
}: {
  events: PluginEventDeliveryView[];
  audit: AuditEventView[];
  busyAction: string | null;
  onRequeueEvent: (eventId: string) => void;
}) {
  return (
    <div className="space-y-6">
      <section className="space-y-3 rounded-xl border border-white/10 bg-white/[0.03] p-5">
        <h2 className="text-body font-semibold text-[var(--text)]">Outbox 事件投递</h2>
        {events.length === 0 ? (
          <p className="text-caption text-[var(--text-faint)]">暂无事件投递记录。</p>
        ) : (
          <div className="space-y-2">
            {events.map((ev) => (
              <div
                key={ev.event_id}
                className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-white/10 bg-white/[0.02] px-3.5 py-2.5 text-caption"
              >
                <div>
                  <span className="font-medium text-[var(--text)]">
                    {ev.event_kind.startsWith(`${ev.event_domain}.`)
                      ? ev.event_kind
                      : `${ev.event_domain}.${ev.event_kind}`}
                  </span>
                  <span className="ml-2 text-[var(--text-muted)]">
                    status: {ev.status} · attempts: {ev.attempt_count}
                  </span>
                  {ev.last_error && <p className="text-[#ff6b6b]">{ev.last_error}</p>}
                </div>
                {(ev.status === "dead_letter" || ev.status === "failed") && (
                  <button
                    type="button"
                    disabled={busyAction !== null}
                    onClick={() => onRequeueEvent(ev.event_id)}
                    className="btn-glass px-3 py-1 text-caption font-medium"
                  >
                    重新入队
                  </button>
                )}
              </div>
            ))}
          </div>
        )}
      </section>

      <section className="space-y-3 rounded-xl border border-white/10 bg-white/[0.03] p-5">
        <h2 className="text-body font-semibold text-[var(--text)]">管理审计日志</h2>
        <div className="space-y-2">
          {audit.map((item) => (
            <div
              key={item.operation_id}
              className="flex flex-wrap items-center justify-between gap-2 rounded-lg border border-white/10 bg-white/[0.02] px-3.5 py-2 text-caption"
            >
              <div>
                <span className="font-medium text-[var(--text)]">{item.action}</span>
                <span className="ml-2 text-[var(--text-muted)]">
                  {item.state} ({item.phase})
                </span>
              </div>
              <span className="text-[var(--text-faint)]">{item.created_at}</span>
            </div>
          ))}
        </div>
      </section>
    </div>
  );
}
