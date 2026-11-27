"use client";

import { useEffect, useState } from "react";

import { useToast } from "@/components/feedback";
import { Modal } from "@/components/modal";
import {
  buildKeepSecrets,
  enablePlugin,
  ensureOperationSucceeded,
  executePluginInstall,
  executePluginUpgrade,
  getPluginConfig,
  getPluginDetail,
  type InstallationPreparationView,
  type PluginGrantSpec,
  type PluginSummaryView,
  preparePluginInstall,
  preparePluginUpgrade,
  reauthPluginAdmin,
  type StagedUploadView,
  stagePluginUpload,
  type UpgradePreparationView,
  waitOperationTerminal,
} from "@/lib/api/plugins";
import { evictPluginFederationCache } from "@/lib/plugins/federation-loader";

export function SignatureStatusBadge({ status, keyId }: { status: string; keyId?: string | null }) {
  if (status === "signed" || status === "trusted" || status === "minisign") {
    return (
      <span className="rounded-full border border-emerald-400/30 bg-emerald-400/15 px-2.5 py-0.5 text-micro font-medium text-emerald-300">
        已签名验证{keyId ? ` · ${keyId}` : ""}
      </span>
    );
  }
  if (status === "development") {
    return (
      <span className="rounded-full border border-amber-400/30 bg-amber-400/15 px-2.5 py-0.5 text-micro font-medium text-amber-200">
        开发者未签名包
      </span>
    );
  }
  return (
    <span className="rounded-full border border-white/15 bg-white/[0.06] px-2.5 py-0.5 text-micro text-[var(--text-muted)]">
      {status}
    </span>
  );
}

export function PreparationDetailsBox({
  installPrep,
  upgradePrep,
  fingerprint,
  selectedCaps,
  onToggleCap,
}: {
  installPrep: InstallationPreparationView | null;
  upgradePrep: UpgradePreparationView | null;
  fingerprint?: string | null;
  selectedCaps: Set<string>;
  onToggleCap: (capability: string) => void;
}) {
  const prep = installPrep ?? upgradePrep;
  if (!prep) return null;
  const isSensitive = (cap: string) =>
    cap === "command:downloader.delete-files" || cap.startsWith("hooks:") || cap.startsWith("http");

  return (
    <div className="space-y-3">
      <div className="space-y-1.5 rounded-xl border border-white/10 bg-white/[0.03] p-3.5 text-caption">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <span className="font-semibold text-[var(--text)]">{prep.plugin_id} · v{prep.version}</span>
          <SignatureStatusBadge status={prep.signature_status} keyId={prep.signing_key_id} />
        </div>
        <p className="break-all font-mono text-[11px] text-[var(--text-faint)]">SHA-256: {prep.package_hash}</p>
        {fingerprint && (
          <p className="break-all font-mono text-[11px] text-[var(--text-faint)]">Key Fingerprint: {fingerprint}</p>
        )}
        <p className="text-[var(--text-muted)]">{prep.impact.summary}</p>
      </div>

      {upgradePrep && (
        <div className="space-y-1.5 rounded-xl border border-white/10 bg-white/[0.03] p-3.5 text-caption">
          <div className="font-medium text-[var(--text)]">状态与配置迁移预览</div>
          <p className="text-[var(--text-muted)]">
            State Schema: v{upgradePrep.state_diff.old_state_schema} → v{upgradePrep.state_diff.new_state_schema}
            （策略：{upgradePrep.state_policy === "reset" ? "重置状态" : "保留状态"}）
          </p>
          <p className="text-[var(--text-muted)]">
            Config Schema: {upgradePrep.config_diff.old_schema_revision ? `${upgradePrep.config_diff.old_schema_revision.slice(0, 8)} → ` : ""}
            {upgradePrep.config_diff.new_schema_revision.slice(0, 8)}
            {upgradePrep.config_diff.existing_valid ? " · 现有配置兼容" : " · 需要更新/默认配置"}
          </p>
          {upgradePrep.state_diff.reset_preview && (
            <p className="text-amber-200">
              将清空 KV {upgradePrep.state_diff.reset_preview.kv_entries} 项 / Blob {upgradePrep.state_diff.reset_preview.blob_entries} 项
            </p>
          )}
        </div>
      )}

      <div className="space-y-2 rounded-xl border border-white/10 bg-white/[0.03] p-3.5">
        <div className="flex items-center justify-between">
          <span className="text-caption font-medium text-[var(--text)]">
            权限授予确认（{selectedCaps.size}/{prep.grant_diff.required.length}）
          </span>
          {prep.grant_diff.removed.length > 0 && (
            <span className="text-micro text-[var(--text-faint)]">将移除 {prep.grant_diff.removed.length} 项旧能力</span>
          )}
        </div>
        {prep.grant_diff.required.length === 0 ? (
          <p className="text-caption text-[var(--text-faint)]">该插件未声明额外宿主机能力。</p>
        ) : (
          <div className="max-h-40 space-y-1.5 overflow-y-auto pr-1 scroll-thin">
            {prep.grant_diff.required.map((cap) => {
              const isNew = prep.grant_diff.added.some((g) => g.capability === cap);
              return (
                <label key={cap} className="flex cursor-pointer items-center justify-between gap-2 rounded-lg border border-white/[0.06] bg-white/[0.02] px-2.5 py-1.5 text-caption">
                  <span className="flex min-w-0 items-center gap-2">
                    <input type="checkbox" checked={selectedCaps.has(cap)} onChange={() => onToggleCap(cap)} className="size-3.5 accent-[var(--accent-2)]" />
                    <span className="truncate font-mono text-[var(--text)]">{cap}</span>
                  </span>
                  <span className="flex shrink-0 items-center gap-1.5">
                    {isSensitive(cap) && <span className="rounded bg-amber-400/15 px-1.5 py-0.5 text-micro text-amber-200">敏感</span>}
                    {upgradePrep && isNew && <span className="rounded bg-sky-400/15 px-1.5 py-0.5 text-micro text-sky-200">新增</span>}
                  </span>
                </label>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}

export function buildSelectedGrants(
  selectedCaps: Set<string>,
  diff: InstallationPreparationView["grant_diff"] | UpgradePreparationView["grant_diff"],
): PluginGrantSpec[] {
  const byCap = new Map<string, PluginGrantSpec>();
  for (const g of [...diff.kept, ...diff.added]) {
    if (selectedCaps.has(g.capability) && !byCap.has(g.capability)) {
      byCap.set(g.capability, { capability: g.capability, scope: g.scope, sensitive: g.sensitive });
    }
  }
  for (const cap of selectedCaps) {
    if (!byCap.has(cap)) {
      byCap.set(cap, { capability: cap, scope: "Global", sensitive: cap === "command:downloader.delete-files" });
    }
  }
  return Array.from(byCap.values());
}

export async function prepareUpgradeWithSavedConfig(
  pluginId: string,
  source: { upload_id?: string; repo_id?: string; version?: string },
  statePolicy: "preserve" | "reset",
  useDefaultCfg: boolean,
): Promise<{ prep: UpgradePreparationView; baseRevision: number }> {
  const [detail, cfg] = await Promise.all([
    getPluginDetail(pluginId),
    useDefaultCfg ? Promise.resolve(null) : getPluginConfig(pluginId).catch(() => null),
  ]);
  const op = await preparePluginUpgrade(
    pluginId,
    {
      ...source,
      state_policy: statePolicy,
      target_config: { schema_revision: "", values: cfg?.values ?? {}, secrets: cfg ? buildKeepSecrets(cfg) : {} },
    },
    detail.revision,
  );
  const term = ensureOperationSucceeded(await waitOperationTerminal(op, 60_000));
  return { prep: term.result as UpgradePreparationView, baseRevision: detail.revision };
}

export async function runPreparedInstallOrUpgrade(args: {
  installPrep: InstallationPreparationView | null;
  upgradePrep: UpgradePreparationView | null;
  upgradeBaseRev: number | null;
  selectedCaps: Set<string>;
  reauthPassword: string;
  autoEnable: boolean;
}): Promise<{ pluginId: string; upgraded: boolean; enableWarning: string | null }> {
  let pluginId = "";
  let upgraded = false;
  if (args.installPrep) {
    pluginId = args.installPrep.plugin_id;
    const grants = buildSelectedGrants(args.selectedCaps, args.installPrep.grant_diff);
    const op = await executePluginInstall({
      preparation_id: args.installPrep.preparation_id,
      consent_token: args.installPrep.consent_token,
      grants,
    });
    ensureOperationSucceeded(await waitOperationTerminal(op, 60_000));
  } else if (args.upgradePrep && args.upgradeBaseRev !== null) {
    upgraded = true;
    pluginId = args.upgradePrep.plugin_id;
    const reauthTok = args.upgradePrep.impact.requires_reauth
      ? (await reauthPluginAdmin(args.reauthPassword, "plugin:upgrade")).reauth_token
      : undefined;
    const grants = buildSelectedGrants(args.selectedCaps, args.upgradePrep.grant_diff);
    const op = await executePluginUpgrade(
      pluginId,
      {
        preparation_id: args.upgradePrep.preparation_id,
        consent_token: args.upgradePrep.consent_token,
        grants,
        state_policy: args.upgradePrep.state_policy as "preserve" | "reset",
        target_config_digest: args.upgradePrep.target_config_digest,
        reauth_token: reauthTok,
      },
      args.upgradeBaseRev,
    );
    ensureOperationSucceeded(await waitOperationTerminal(op, 60_000));
    evictPluginFederationCache(pluginId);
  } else {
    throw new Error("缺少有效的安装或升级预检凭据");
  }

  let enableWarning: string | null = null;
  if (args.autoEnable && pluginId) {
    try {
      const fresh = await getPluginDetail(pluginId);
      if (!fresh.enabled) {
        const enOp = await enablePlugin(pluginId, fresh.revision);
        ensureOperationSucceeded(await waitOperationTerminal(enOp, 30_000));
      }
    } catch (enableErr) {
      enableWarning = (enableErr as Error).message;
    }
  }
  return { pluginId, upgraded, enableWarning };
}

export function PluginUploadModal({
  open,
  onClose,
  installedPlugins,
  initialUpgradePlugin,
  onCompleted,
}: {
  open: boolean;
  onClose: () => void;
  installedPlugins: PluginSummaryView[];
  initialUpgradePlugin?: PluginSummaryView | null;
  onCompleted: () => Promise<void> | void;
}) {
  const toast = useToast();
  const upgradablePlugins = installedPlugins.filter((p) => p.source !== "builtin");
  const [mode, setMode] = useState<"install" | "upgrade">("install");
  const [targetPluginId, setTargetPluginId] = useState("");
  const [packageFile, setPackageFile] = useState<File | null>(null);
  const [signatureFile, setSignatureFile] = useState<File | null>(null);
  const [expectedSha256, setExpectedSha256] = useState("");
  const [uploadPassword, setUploadPassword] = useState("");
  const [statePolicy, setStatePolicy] = useState<"preserve" | "reset">("preserve");
  const [resetConfigDefaults, setResetConfigDefaults] = useState(false);

  const [step, setStep] = useState<"select" | "review">("select");
  const [staged, setStaged] = useState<StagedUploadView | null>(null);
  const [installPrep, setInstallPrep] = useState<InstallationPreparationView | null>(null);
  const [upgradePrep, setUpgradePrep] = useState<UpgradePreparationView | null>(null);
  const [upgradeBaseRev, setUpgradeBaseRev] = useState<number | null>(null);
  const [selectedCaps, setSelectedCaps] = useState<Set<string>>(new Set());
  const [resetPassword, setResetPassword] = useState("");
  const [autoEnable, setAutoEnable] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setMode(initialUpgradePlugin ? "upgrade" : "install");
    setTargetPluginId(initialUpgradePlugin?.id ?? upgradablePlugins[0]?.id ?? "");
    setPackageFile(null); setSignatureFile(null); setExpectedSha256(""); setUploadPassword("");
    setStatePolicy("preserve"); setResetConfigDefaults(false); setStep("select");
    setStaged(null); setInstallPrep(null); setUpgradePrep(null); setUpgradeBaseRev(null);
    setSelectedCaps(new Set()); setResetPassword(""); setAutoEnable(true); setBusy(false); setError(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, initialUpgradePlugin?.id]);

  async function runPrepareUpgrade(pluginId: string, uploadId: string, policy: "preserve" | "reset", useDefaultCfg: boolean) {
    const { prep, baseRevision } = await prepareUpgradeWithSavedConfig(pluginId, { upload_id: uploadId }, policy, useDefaultCfg);
    setUpgradePrep(prep);
    setUpgradeBaseRev(baseRevision);
    setSelectedCaps(new Set(prep.grant_diff.required));
    setStep("review");
  }

  async function handleStageAndPrepare(e: React.FormEvent) {
    e.preventDefault();
    if (!packageFile) return setError("请选择 `.cmplugin` 插件包文件");
    if (mode === "upgrade" && !targetPluginId) return setError("请选择要升级的目标插件");
    setBusy(true);
    setError(null);
    try {
      const reauth = await reauthPluginAdmin(uploadPassword, "plugin:upload");
      const up = await stagePluginUpload(packageFile, signatureFile ?? undefined, {
        reauthToken: reauth.reauth_token,
        expectedSha256: expectedSha256.trim() || undefined,
      });
      setUploadPassword("");
      setStaged(up);
      if (mode === "install") {
        const op = await preparePluginInstall({ upload_id: up.upload_id });
        const term = ensureOperationSucceeded(await waitOperationTerminal(op, 60_000));
        const prep = term.result as InstallationPreparationView;
        setInstallPrep(prep);
        setUpgradePrep(null);
        setSelectedCaps(new Set(prep.grant_diff.required));
        setStep("review");
      } else {
        setInstallPrep(null);
        await runPrepareUpgrade(targetPluginId, up.upload_id, statePolicy, resetConfigDefaults);
      }
    } catch (err) {
      setError((err as Error).message || "上传或预检失败");
    } finally {
      setBusy(false);
    }
  }

  async function handleRetryUpgrade(nextPolicy: "preserve" | "reset", nextDefaultCfg: boolean) {
    if (!staged || !targetPluginId) return;
    setBusy(true); setError(null); setStatePolicy(nextPolicy); setResetConfigDefaults(nextDefaultCfg);
    try {
      await runPrepareUpgrade(targetPluginId, staged.upload_id, nextPolicy, nextDefaultCfg);
    } catch (err) {
      setError((err as Error).message || "升级预检失败");
    } finally {
      setBusy(false);
    }
  }

  async function handleSwitchToUpgrade() {
    if (!staged || !installPrep) return;
    setMode("upgrade"); setTargetPluginId(installPrep.plugin_id); setInstallPrep(null); setBusy(true); setError(null);
    try {
      await runPrepareUpgrade(installPrep.plugin_id, staged.upload_id, statePolicy, resetConfigDefaults);
    } catch (err) {
      setError((err as Error).message || "切换升级预检失败");
    } finally {
      setBusy(false);
    }
  }

  async function handleExecute(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true); setError(null);
    try {
      const res = await runPreparedInstallOrUpgrade({
        installPrep, upgradePrep, upgradeBaseRev, selectedCaps, reauthPassword: resetPassword, autoEnable,
      });
      if (res.enableWarning) toast.info(`已${res.upgraded ? "升级" : "安装"} ${res.pluginId}，但未能自动启用：${res.enableWarning}`);
      else toast.success(`已${res.upgraded ? "升级" : "安装"}插件 ${res.pluginId}`);
      await onCompleted();
      onClose();
    } catch (err) {
      setError((err as Error).message || "执行安装/升级失败");
    } finally {
      setBusy(false);
    }
  }

  const alreadyInstalled = Boolean(installPrep && installedPlugins.some((p) => p.id === installPrep.plugin_id));
  const hasPrepared = Boolean(installPrep || upgradePrep);
  const upgradeRetryActions =
    staged && mode === "upgrade" ? (
      <div className="flex flex-wrap gap-2">
        <button type="button" disabled={busy} onClick={() => void handleRetryUpgrade("reset", resetConfigDefaults)} className="btn-glass px-3 py-1 text-micro font-medium text-[var(--text)]">
          改为重置状态重试
        </button>
        <button type="button" disabled={busy} onClick={() => void handleRetryUpgrade(statePolicy, true)} className="btn-glass px-3 py-1 text-micro font-medium text-[var(--text)]">
          使用新版默认配置重试
        </button>
      </div>
    ) : null;

  return (
    <Modal open={open} onClose={busy ? () => {} : onClose} label="本地上传安装或升级插件" width="lg">
      {step === "select" ? (
        <form onSubmit={(e) => void handleStageAndPrepare(e)} className="space-y-4 p-6">
          <div className="flex items-center justify-between gap-2">
            <h2 className="text-title font-bold text-[var(--text)]">
              {mode === "upgrade" ? "本地上传升级 (.cmplugin)" : "本地上传安装 (.cmplugin)"}
            </h2>
            {upgradablePlugins.length > 0 && (
              <div className="segmented-glass inline-flex rounded-full p-0.5 text-caption">
                {(["install", "upgrade"] as const).map((m) => (
                  <button key={m} type="button" onClick={() => { setMode(m); setError(null); }} aria-pressed={mode === m} className="segmented-glass__item rounded-full px-3 py-1 font-medium">
                    {m === "install" ? "新装" : "升级"}
                  </button>
                ))}
              </div>
            )}
          </div>

          {error && (
            <div role="alert" className="space-y-2 rounded-xl border border-[#ff6b6b]/30 bg-[#ff6b6b]/10 px-3.5 py-2.5 text-caption text-[#ff6b6b]">
              <p>{error}</p>
              {upgradeRetryActions}
            </div>
          )}

          {mode === "upgrade" && (
            <div>
              <label className="mb-1 block text-caption font-medium text-[var(--text-muted)]">目标插件</label>
              <select value={targetPluginId} onChange={(e) => setTargetPluginId(e.target.value)} className="input-glass w-full rounded-lg px-3 py-2 text-ui">
                {upgradablePlugins.map((p) => (
                  <option key={p.id} value={p.id}>{p.name} ({p.id} · v{p.version})</option>
                ))}
              </select>
              <div className="mt-2 flex flex-wrap gap-4 text-caption text-[var(--text-muted)]">
                <label className="flex items-center gap-1.5">
                  <input type="radio" name="upload-state-policy" checked={statePolicy === "preserve"} onChange={() => setStatePolicy("preserve")} />
                  保留插件私有状态
                </label>
                <label className="flex items-center gap-1.5">
                  <input type="radio" name="upload-state-policy" checked={statePolicy === "reset"} onChange={() => setStatePolicy("reset")} />
                  重置私有状态 (需二次密码验证)
                </label>
              </div>
            </div>
          )}

          <div className="space-y-3">
            <div>
              <label className="mb-1 block text-caption font-medium text-[var(--text-muted)]">插件归档文件 (.cmplugin) *</label>
              <input type="file" accept=".cmplugin" required onChange={(e) => setPackageFile(e.target.files?.[0] ?? null)} className="input-glass w-full rounded-lg px-3 py-2 text-caption file:mr-3 file:rounded-full file:border-0 file:bg-white/10 file:px-3 file:py-1 file:text-caption file:text-[var(--text)]" />
            </div>
            <div>
              <label className="mb-1 block text-caption font-medium text-[var(--text-muted)]">分离签名文件 (.minisig，开发者模式下可选)</label>
              <input type="file" accept=".minisig,.txt" onChange={(e) => setSignatureFile(e.target.files?.[0] ?? null)} className="input-glass w-full rounded-lg px-3 py-2 text-caption file:mr-3 file:rounded-full file:border-0 file:bg-white/10 file:px-3 file:py-1 file:text-caption file:text-[var(--text)]" />
            </div>
            <div>
              <label className="mb-1 block text-caption font-medium text-[var(--text-muted)]">预期 SHA-256（可选，64 位十六进制）</label>
              <input type="text" value={expectedSha256} onChange={(e) => setExpectedSha256(e.target.value)} placeholder="留空则跳过额外摘要比对" className="input-glass w-full rounded-lg px-3 py-2 font-mono text-caption" />
            </div>
            <div>
              <label htmlFor="plugin-upload-reauth" className="mb-1 block text-caption font-medium text-[var(--text-muted)]">管理员密码二次验证 *</label>
              <input id="plugin-upload-reauth" type="password" required value={uploadPassword} onChange={(e) => setUploadPassword(e.target.value)} placeholder="上传并暂存插件包需要管理员密码" className="input-glass w-full rounded-lg px-3 py-2 text-ui" />
            </div>
          </div>

          <div className="flex justify-end gap-2 pt-2">
            <button type="button" disabled={busy} onClick={onClose} className="btn-glass px-4 py-2 text-sub">取消</button>
            <button type="submit" disabled={busy || !packageFile || !uploadPassword} className="btn-accent rounded-full px-4 py-2 text-sub font-semibold disabled:opacity-50">
              {busy ? "上传并校验中…" : "上传并预检"}
            </button>
          </div>
        </form>
      ) : (
        <form onSubmit={(e) => void handleExecute(e)} className="space-y-4 p-6">
          <h2 className="text-title font-bold text-[var(--text)]">{upgradePrep || mode === "upgrade" ? "确认升级插件" : "确认安装插件"}</h2>
          {error && (
            <div role="alert" className="space-y-2 rounded-xl border border-[#ff6b6b]/30 bg-[#ff6b6b]/10 px-3.5 py-2.5 text-caption text-[#ff6b6b]">
              <p>{error}</p>
              {upgradeRetryActions}
            </div>
          )}
          {alreadyInstalled && installPrep && (
            <div className="flex flex-wrap items-center justify-between gap-2 rounded-xl border border-amber-400/30 bg-amber-400/10 px-3.5 py-2.5 text-caption text-amber-200">
              <span>插件 {installPrep.plugin_id} 已存在，请切换为升级流程继续。</span>
              <button type="button" disabled={busy} onClick={() => void handleSwitchToUpgrade()} className="btn-accent rounded-full px-3 py-1 text-micro font-semibold">转为升级预览</button>
            </div>
          )}

          <PreparationDetailsBox
            installPrep={installPrep}
            upgradePrep={upgradePrep}
            fingerprint={staged?.fingerprint}
            selectedCaps={selectedCaps}
            onToggleCap={(cap) => setSelectedCaps((prev) => { const next = new Set(prev); if (next.has(cap)) next.delete(cap); else next.add(cap); return next; })}
          />

          {upgradePrep?.impact.requires_reauth && (
            <div>
              <label htmlFor="plugin-upgrade-reset-reauth" className="mb-1 block text-caption font-medium text-[var(--text-muted)]">管理员密码二次验证（重置状态需要） *</label>
              <input id="plugin-upgrade-reset-reauth" type="password" required value={resetPassword} onChange={(e) => setResetPassword(e.target.value)} className="input-glass w-full rounded-lg px-3 py-2 text-ui" />
            </div>
          )}

          <label className="flex cursor-pointer items-center gap-2 text-caption text-[var(--text-muted)]">
            <input type="checkbox" checked={autoEnable} onChange={(e) => setAutoEnable(e.target.checked)} className="size-3.5 accent-[var(--accent-2)]" />
            <span>完成后自动启用插件</span>
          </label>

          <div className="flex justify-end gap-2 pt-1">
            <button type="button" disabled={busy} onClick={() => { setStep("select"); setError(null); }} className="btn-glass px-4 py-2 text-sub">返回</button>
            <button type="submit" disabled={busy || alreadyInstalled || !hasPrepared || (Boolean(upgradePrep?.impact.requires_reauth) && !resetPassword)} className="btn-accent rounded-full px-4 py-2 text-sub font-semibold disabled:opacity-50">
              {busy ? "执行中…" : upgradePrep || mode === "upgrade" ? "确认升级" : "确认安装"}
            </button>
          </div>
        </form>
      )}
    </Modal>
  );
}
