import type React from "react";
import { useNavigate } from "react-router-dom";

import { useConfirm, useToast } from "@/components/feedback";
import {
  disablePlugin,
  enablePlugin,
  ensureOperationSucceeded,
  grantPermissions,
  grantPluginUiTrust,
  isPluginConfigEditable,
  listPluginTaskReceipts,
  newIdempotencyKey,
  type PluginDetailView as PluginDetailData,
  type PluginGrantSpec,
  type PluginPermissionsView,
  type PluginTaskView,
  preparePermissions,
  preparePluginUiTrust,
  reauthPluginAdmin,
  requeuePluginEvent,
  revokePermissions,
  revokePluginUiTrust,
  runPluginTask,
  type TaskReceiptView,
  type UiDescriptorView,
  type UiTrustPreparationView,
  uninstallPlugin,
  updatePluginTask,
  waitOperationTerminal,
} from "@/lib/api/plugins";
import {
  clearFederationCache,
  evictPluginFederationCache,
  isRemoteLoaded,
  reloadCurrentDocument,
} from "@/lib/plugins/federation-loader";

export interface DetailActionsArgs {
  pluginId: string;
  detail: PluginDetailData | null;
  uiDesc: UiDescriptorView | null;
  perms: PluginPermissionsView | null;
  trustPrep: UiTrustPreparationView | null;
  reauthPassword: string;
  setBusyAction: (v: string | null) => void;
  setTrustPrep: (v: UiTrustPreparationView | null) => void;
  setReauthPassword: (v: string) => void;
  setTrustBusy: (v: boolean) => void;
  setReceiptsByTask: React.Dispatch<React.SetStateAction<Record<string, TaskReceiptView[]>>>;
  loadAll: () => Promise<void>;
}

function usePluginTrustAndLifecycleActions(args: DetailActionsArgs) {
  const { pluginId, detail, uiDesc, trustPrep, reauthPassword, loadAll } = args;
  const navigate = useNavigate();
  const toast = useToast();
  const confirm = useConfirm();

  async function runLifecycle(action: "enable" | "disable" | "uninstall") {
    if (!detail) return;
    if (action === "uninstall") {
      const ok = await confirm({
        title: `卸载插件「${detail.name}」？`,
        description: "卸载后将移除插件版本与授权。",
        confirmLabel: "卸载",
        tone: "danger",
      });
      if (!ok) return;
    }
    args.setBusyAction(action);
    try {
      const wasRemoteLoaded =
        action === "uninstall" && uiDesc ? isRemoteLoaded(uiDesc.remote_name) : false;
      const op =
        action === "enable"
          ? await enablePlugin(pluginId, detail.revision)
          : action === "disable"
            ? await disablePlugin(pluginId, detail.revision, "admin-ui")
            : await uninstallPlugin(pluginId, false, detail.revision);
      ensureOperationSucceeded(await waitOperationTerminal(op));
      toast.success("插件状态已更新");
      if (action === "uninstall") {
        evictPluginFederationCache(pluginId);
        navigate("/settings/plugins", { replace: true });
        if (wasRemoteLoaded) {
          clearFederationCache();
          reloadCurrentDocument("/settings/plugins");
        }
        return;
      }
      await loadAll();
    } catch (err) {
      toast.error((err as Error).message || "生命周期操作失败");
    } finally {
      args.setBusyAction(null);
    }
  }

  async function handlePrepareTrust() {
    if (!detail) return;
    args.setBusyAction("prepare-trust");
    try {
      const prep = await preparePluginUiTrust(pluginId, detail.package_hash);
      args.setTrustPrep(prep);
      args.setReauthPassword("");
    } catch (err) {
      toast.error((err as Error).message || "准备 UI 信任失败");
    } finally {
      args.setBusyAction(null);
    }
  }

  async function handleConfirmTrust(e: React.FormEvent) {
    e.preventDefault();
    if (!trustPrep) return;
    args.setTrustBusy(true);
    try {
      const reauth = await reauthPluginAdmin(reauthPassword, "plugin-ui-trust");
      await grantPluginUiTrust(
        pluginId,
        {
          preparation_id: trustPrep.preparation_id,
          package_hash: trustPrep.package_hash,
          consent_token: trustPrep.consent_token,
          reauth_token: reauth.reauth_token,
        },
        trustPrep.trust_revision,
      );
      args.setTrustPrep(null);
      args.setReauthPassword("");
      toast.success("已授予插件 UI 信任");
      await loadAll();
    } catch (err) {
      toast.error((err as Error).message || "授予 UI 信任失败");
    } finally {
      args.setTrustBusy(false);
    }
  }

  async function handleRevokeTrust() {
    if (!detail || !uiDesc) return;
    const ok = await confirm({
      title: `撤销「${detail.name}」的 UI 信任？`,
      description: "撤销后将立即阻止加载该插件的前端联邦脚本，插件后端与配置文件不受影响。",
      confirmLabel: "撤销信任",
      tone: "danger",
    });
    if (!ok) return;
    args.setBusyAction("revoke-trust");
    try {
      const wasLoaded = isRemoteLoaded(uiDesc.remote_name);
      await revokePluginUiTrust(
        pluginId,
        { package_hash: detail.package_hash, reason: "admin-ui-revoke" },
        uiDesc.trust.revision,
      );
      evictPluginFederationCache(pluginId);
      toast.success("已撤销 UI 信任");
      if (wasLoaded) {
        clearFederationCache();
        reloadCurrentDocument();
        return;
      }
      await loadAll();
    } catch (err) {
      toast.error((err as Error).message || "撤销失败");
    } finally {
      args.setBusyAction(null);
    }
  }

  return { runLifecycle, handlePrepareTrust, handleConfirmTrust, handleRevokeTrust };
}

function usePluginTaskActions(args: DetailActionsArgs) {
  const { pluginId, loadAll } = args;
  const toast = useToast();

  async function handleToggleTask(task: PluginTaskView) {
    args.setBusyAction(`task-toggle:${task.id}`);
    try {
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
      toast.success(task.enabled ? `已停用任务 ${task.id}` : `已启用任务 ${task.id}`);
      await loadAll();
    } catch (err) {
      toast.error((err as Error).message || "更新任务状态失败");
    } finally {
      args.setBusyAction(null);
    }
  }

  async function handleRunTask(task: PluginTaskView) {
    args.setBusyAction(`task:${task.id}`);
    try {
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
      const op = await runPluginTask(pluginId, task.id, {}, defRev, newIdempotencyKey());
      if (!["failed", "rejected", "cancelled"].includes(op.state)) {
        toast.info(`已触发任务 ${task.id} (${op.state})`);
      }
      ensureOperationSucceeded(await waitOperationTerminal(op));
      const rc = await listPluginTaskReceipts(pluginId, task.id);
      args.setReceiptsByTask((prev) => ({ ...prev, [task.id]: rc.items }));
      await loadAll();
    } catch (err) {
      toast.error((err as Error).message || "触发任务失败");
    } finally {
      args.setBusyAction(null);
    }
  }

  return { handleToggleTask, handleRunTask };
}

function usePluginPermAndEventActions(args: DetailActionsArgs) {
  const { pluginId, detail, perms, loadAll } = args;
  const toast = useToast();

  async function handleGrantMissingPerms() {
    if (!detail || !perms) return;
    if (!isPluginConfigEditable(detail)) {
      toast.error("请先停用插件并等待排空完成后再同步授予能力");
      return;
    }
    const requested: PluginGrantSpec[] = perms.required_capabilities.map((capability) => ({
      capability,
      scope: "Global",
      sensitive: capability === "command:downloader.delete-files",
    }));
    if (requested.length === 0) return;
    args.setBusyAction("grant-perms");
    try {
      const prep = await preparePermissions(pluginId, requested);
      const op = await grantPermissions(
        pluginId,
        { consent_token: prep.consent_token, grants: requested },
        detail.revision,
      );
      ensureOperationSucceeded(await waitOperationTerminal(op));
      toast.success("权限授予已更新");
      await loadAll();
    } catch (err) {
      toast.error((err as Error).message || "权限授予失败");
    } finally {
      args.setBusyAction(null);
    }
  }

  async function handleRevokeSingleGrant(grant: PluginGrantSpec) {
    if (!detail) return;
    const cleanGrant: PluginGrantSpec = {
      capability: grant.capability,
      scope: grant.scope,
      sensitive: grant.sensitive,
    };
    args.setBusyAction(`revoke:${cleanGrant.capability}`);
    try {
      const op = await revokePermissions(
        pluginId,
        { grants: [cleanGrant], reason: "admin-ui-revoke" },
        detail.revision,
      );
      ensureOperationSucceeded(await waitOperationTerminal(op));
      toast.success(`已撤销 ${cleanGrant.capability}`);
      await loadAll();
    } catch (err) {
      toast.error((err as Error).message || "撤销权限失败");
    } finally {
      args.setBusyAction(null);
    }
  }

  async function handleRequeueEvent(eventId: string) {
    args.setBusyAction(`requeue:${eventId}`);
    try {
      await requeuePluginEvent(pluginId, eventId);
      toast.success("事件已重新入队");
      await loadAll();
    } catch (err) {
      toast.error((err as Error).message || "重新入队失败");
    } finally {
      args.setBusyAction(null);
    }
  }

  return { handleGrantMissingPerms, handleRevokeSingleGrant, handleRequeueEvent };
}

export function usePluginDetailActions(args: DetailActionsArgs) {
  return {
    ...usePluginTrustAndLifecycleActions(args),
    ...usePluginTaskActions(args),
    ...usePluginPermAndEventActions(args),
  };
}
