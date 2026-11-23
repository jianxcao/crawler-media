"use client";

import { useEffect, useState } from "react";

import {
  fetchBrowserSettings,
  fetchObscuraStatus,
  type ObscuraStatusData,
  startObscura,
  stopObscura,
  syncCdp,
  type SyncCdpData,
  updateBrowserSettings,
} from "@/lib/api/browser-settings";
import { HttpError } from "@/lib/http";

const NOT_STARTED_REASON_LABELS: Record<string, string> = {
  switch_off: "Browser 开关未开启",
  instance_endpoint_configured: "已配置实例 CDP 端点，不需要受管 Obscura",
  no_managed_browser_sites: "没有启用 Browser 维护且未自带端点的站点",
  stopped_manually: "已手动停止",
};

function errorText(e: unknown): string {
  if (e instanceof HttpError) return e.message;
  if (e instanceof Error) return e.message;
  return String(e);
}

/* —— CDP 与 Obscura 浏览器控制面板 —— */
export function CdpBrowserSection() {
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [syncing, setSyncing] = useState(false);
  const [obscuraActing, setObscuraActing] = useState(false);
  const [msg, setMsg] = useState<{ tone: "success" | "error"; text: string } | null>(null);

  const [cdpEnabled, setCdpEnabled] = useState(false);
  const [cdpUrl, setCdpUrl] = useState("");
  const [obscuraInstall, setObscuraInstall] = useState<"download" | "preinstalled">("preinstalled");
  const [userAgent, setUserAgent] = useState("crawler-media/0.1.0");
  const [obscuraStatus, setObscuraStatus] = useState<ObscuraStatusData | null>(null);
  const [managedBrowserSites, setManagedBrowserSites] = useState<number | null>(null);
  const [incompatible, setIncompatible] = useState<Record<string, string>>({});
  const [syncResult, setSyncResult] = useState<SyncCdpData | null>(null);

  async function loadData() {
    try {
      const [settings, status] = await Promise.all([
        fetchBrowserSettings(),
        fetchObscuraStatus(),
      ]);
      if (settings) {
        setCdpEnabled(settings.cdp?.enabled ?? false);
        setCdpUrl(settings.cdp?.url ?? "");
        setUserAgent(settings.user_agent ?? "crawler-media/0.1.0");
        setManagedBrowserSites(settings.managed_browser_sites ?? null);
        setIncompatible(settings.incompatible_endpoints ?? {});
        if (settings.obscura?.install) {
          setObscuraInstall(settings.obscura.install);
        }
      }
      if (status) {
        setObscuraStatus(status);
      }
    } catch (e) {
      setMsg({ tone: "error", text: `加载配置失败: ${errorText(e)}` });
    } finally {
      setLoading(false);
    }
  }

  useEffect(() => {
    loadData();
  }, []);

  async function handleSave() {
    setSaving(true);
    setMsg(null);
    try {
      const data = await updateBrowserSettings({
        cdp: { enabled: cdpEnabled, url: cdpUrl.trim() },
        obscura: { install: obscuraInstall },
        user_agent: userAgent,
      });
      setMsg({ tone: "success", text: "浏览器配置已保存，受管 Obscura 将按需在后台启停" });
      if (data?.user_agent) setUserAgent(data.user_agent);
      await loadData();
    } catch (e) {
      setMsg({ tone: "error", text: errorText(e) || "保存失败" });
    } finally {
      setSaving(false);
    }
  }

  async function handleObscuraAction(action: "start" | "stop") {
    setObscuraActing(true);
    setMsg(null);
    try {
      const data = action === "start" ? await startObscura() : await stopObscura();
      setMsg({
        tone: "success",
        text: action === "start" ? "已提交启动，Obscura 正在后台安装/启动" : "已停止 Obscura 浏览器服务",
      });
      setObscuraStatus(data);
    } catch (e) {
      setMsg({ tone: "error", text: errorText(e) || "操作失败" });
    } finally {
      setObscuraActing(false);
    }
  }

  async function handleTriggerSync() {
    setSyncing(true);
    setMsg(null);
    setSyncResult(null);
    try {
      const data = await syncCdp();
      setSyncResult(data);
      setMsg({
        tone: "success",
        text: `CDP 凭据同步成功：共扫描 ${data.total_cookies_scanned} 个 Cookie，更新已配置站点 ${data.updated_sites} 个！`,
      });
    } catch (e) {
      setMsg({ tone: "error", text: errorText(e) || "同步失败" });
    } finally {
      setSyncing(false);
    }
  }

  if (loading) {
    return <div className="p-8 text-center text-sub text-[var(--text-muted)]">正在加载设置…</div>;
  }

  const currentMode = !cdpEnabled
    ? "已停用（全部请求使用普通 HTTP）"
    : cdpUrl.trim().length > 0
    ? "外部 Chrome CDP（连接配置的调试端点）"
    : "受管 Obscura 引擎（自动拉起轻量防检测浏览器）";

  return (
    <div className="space-y-6">
      {msg && (
        <div
          className={`rounded-xl border px-4 py-3 text-body ${
            msg.tone === "success"
              ? "border-[#51cf66]/30 bg-[#51cf66]/10 text-[#51cf66]"
              : "border-[#ff6b6b]/30 bg-[#ff6b6b]/10 text-[#ff6b6b]"
          }`}
        >
          {msg.text}
        </div>
      )}

      <div className="rounded-xl border border-white/[0.08] bg-white/[0.02] p-4 text-sub text-[var(--text-muted)]">
        浏览器支持单开关设计：开启后，如果配置了外部 CDP 地址则使用外部浏览器；如果未配置则使用内置受管 Obscura 引擎。
        若外部浏览器或受管引擎执行失败，系统将自动回退到普通 HTTP 流程。
        <span className="mt-1 block font-semibold text-white/90">
          注意：搜索与 RSS 种子检索始终使用普通 HTTP，不消耗任何浏览器资源；浏览器仅用于登录和签到保活。
        </span>
      </div>

      <div className="rounded-xl border border-white/[0.08] bg-white/[0.02] p-5">
        <div className="flex items-start justify-between gap-4">
          <div>
            <h3 className="text-ui font-semibold text-[var(--text)]">启用 CDP 浏览器功能</h3>
            <p className="mt-1 text-sub text-[var(--text-muted)]">
              当前运行模式：<span className="font-semibold text-white/90">{currentMode}</span>
            </p>
          </div>
          <label className="relative inline-flex cursor-pointer items-center">
            <input
              type="checkbox"
              checked={cdpEnabled}
              onChange={(e) => setCdpEnabled(e.target.checked)}
              className="peer sr-only"
            />
            <div className="peer h-6 w-11 rounded-full bg-white/10 after:absolute after:left-[2px] after:top-[2px] after:h-5 after:w-5 after:rounded-full after:bg-[#ffffff] after:transition-all after:content-[''] peer-checked:bg-[var(--accent)] peer-checked:after:translate-x-full" />
          </label>
        </div>

        {cdpEnabled && (
          <div className="mt-4 space-y-4 border-t border-white/[0.06] pt-4">
            <div>
              <label className="text-micro font-medium text-[var(--text-faint)]">
                外部 Chrome 远程调试地址 (选填，留空则使用内置 Obscura 引擎)
              </label>
              <input
                type="text"
                value={cdpUrl}
                onChange={(e) => setCdpUrl(e.target.value)}
                placeholder="留空使用受管 Obscura，或填 http://127.0.0.1:9222"
                className="mt-1.5 w-full rounded-xl border border-white/[0.08] bg-white/[0.04] px-3.5 py-2 text-ui text-[var(--text)] outline-none focus:border-[var(--accent)]"
              />
              <p className="mt-1.5 text-micro text-[var(--text-faint)]">
                若需使用本地已登录的 Chrome/Edge，带参数{" "}
                <code className="text-white/80">--remote-debugging-port=9222</code> 启动浏览器并填入该地址。
              </p>
            </div>

            <div className="flex items-center gap-3">
              <button
                type="button"
                onClick={handleTriggerSync}
                disabled={syncing}
                className="btn-accent rounded-full px-4 py-1.5 text-sub font-semibold disabled:opacity-50"
              >
                {syncing ? "正在提取并同步中…" : "立即从浏览器扫描并同步 Cookie"}
              </button>
            </div>

            {(syncResult?.candidate_sites?.length ?? 0) > 0 && (
              <div className="rounded-xl border border-white/[0.08] bg-white/[0.03] p-4 text-sub">
                <p className="font-semibold text-white/90">在浏览器中发现以下支持的未接入站点：</p>
                <div className="mt-2 flex flex-wrap gap-2">
                  {(syncResult?.candidate_sites ?? []).map((c) => (
                    <span
                      key={c.profile_id}
                      className="rounded-lg bg-white/[0.06] px-2.5 py-1 text-caption text-white/80"
                    >
                      {c.profile_id} ({c.domain}) · {c.cookie_count} 项 Cookie
                    </span>
                  ))}
                </div>
              </div>
            )}
          </div>
        )}
      </div>

      <div className="rounded-xl border border-white/[0.08] bg-white/[0.02] p-5 space-y-4">
        <div className="flex items-start justify-between gap-4">
          <div>
            <h3 className="text-ui font-semibold text-[var(--text)]">受管 Obscura 浏览器状态</h3>
            <p className="mt-1 text-sub text-[var(--text-muted)]">
              内置轻量级无头浏览器守护进程。无需任何外部依赖，在未配置外部 CDP 时按需拉起并自动管理生命周期。
            </p>
          </div>
          {obscuraStatus && (
            <div className="flex items-center gap-2">
              <span
                className={`inline-flex items-center gap-1.5 rounded-full px-3 py-1 text-caption font-medium ${
                  obscuraStatus.running
                    ? "bg-[#51cf66]/10 text-[#51cf66]"
                    : "bg-white/10 text-[var(--text-muted)]"
                }`}
              >
                <span
                  className={`h-2 w-2 rounded-full ${
                    obscuraStatus.running ? "bg-[#51cf66]" : "bg-white/30"
                  }`}
                />
                {obscuraStatus.running
                  ? `运行中 (端口: ${obscuraStatus.port})`
                  : obscuraStatus.downloading
                  ? "下载安装中…"
                  : "未启动"}
              </span>
            </div>
          )}
        </div>

        <div className="border-t border-white/[0.06] pt-4 space-y-3">
          {(obscuraStatus?.not_started_reason ||
            obscuraStatus?.last_error ||
            managedBrowserSites !== null ||
            Object.keys(incompatible).length > 0) && (
            <div className="space-y-1 text-sub" data-testid="obscura-diagnostics">
              {managedBrowserSites !== null && (
                <div>
                  <span className="text-[var(--text-faint)]">需要受管 Obscura 的站点: </span>
                  <span className="font-medium text-white/90">{managedBrowserSites}</span>
                </div>
              )}
              {obscuraStatus?.not_started_reason && (
                <div>
                  <span className="text-[var(--text-faint)]">未启动原因: </span>
                  <span className="font-medium text-white/90">
                    {NOT_STARTED_REASON_LABELS[obscuraStatus.not_started_reason] ??
                      obscuraStatus.not_started_reason}
                  </span>
                </div>
              )}
              {obscuraStatus?.last_error && (
                <div className="text-[#ff6b6b]">
                  <span className="text-[var(--text-faint)]">最近错误: </span>
                  {obscuraStatus.last_error}
                </div>
              )}
              {Object.entries(incompatible).map(([endpoint, reason]) => (
                <div key={endpoint} className="text-[#ff6b6b]">
                  <span className="text-[var(--text-faint)]">不兼容端点 {endpoint}: </span>
                  {reason}
                </div>
              ))}
            </div>
          )}
          <div className="grid grid-cols-2 gap-4 text-sub">
            <div>
              <span className="text-[var(--text-faint)]">安装状态: </span>
              <span className="font-medium text-white/90">
                {obscuraStatus?.installed ? "已安装" : "未安装"}
              </span>
            </div>
            <div>
              <span className="text-[var(--text-faint)]">引擎版本: </span>
              <span className="font-medium text-white/90">
                {obscuraStatus?.reported_version ?? obscuraStatus?.expected_version ?? "v0.2.3"}
              </span>
            </div>
          </div>

          <div>
            <label className="text-micro font-medium text-[var(--text-faint)]">
              安装策略 (二进制获取方式)
            </label>
            <div className="mt-1 flex gap-2">
              <button
                type="button"
                onClick={() => setObscuraInstall("preinstalled")}
                data-active={obscuraInstall === "preinstalled"}
                className="glass-row nav-item !w-auto px-3 py-1.5 text-sub font-medium"
              >
                仅预安装 (离线环境)
              </button>
              <button
                type="button"
                onClick={() => setObscuraInstall("download")}
                data-active={obscuraInstall === "download"}
                className="glass-row nav-item !w-auto px-3 py-1.5 text-sub font-medium"
              >
                自动下载 (SHA256 哈希校验)
              </button>
            </div>
          </div>

          <div className="flex items-center gap-3 pt-2">
            {obscuraStatus?.running ? (
              <button
                type="button"
                onClick={() => handleObscuraAction("stop")}
                disabled={obscuraActing}
                className="rounded-full border border-red-500/40 bg-red-500/10 px-4 py-1.5 text-sub font-semibold text-red-400 hover:bg-red-500/20 disabled:opacity-50"
              >
                {obscuraActing ? "停止中…" : "停止 Obscura 服务"}
              </button>
            ) : (
              <button
                type="button"
                onClick={() => handleObscuraAction("start")}
                disabled={obscuraActing || obscuraStatus?.downloading}
                className="rounded-full border border-emerald-500/40 bg-emerald-500/10 px-4 py-1.5 text-sub font-semibold text-emerald-400 hover:bg-emerald-500/20 disabled:opacity-50"
              >
                {obscuraActing ? "启动中…" : "手动启动 Obscura"}
              </button>
            )}
          </div>
        </div>
      </div>

      <div className="rounded-xl border border-white/[0.08] bg-white/[0.02] p-5 space-y-4">
        <div>
          <h3 className="text-ui font-semibold text-[var(--text)]">全局统一 User-Agent 请求标识</h3>
          <p className="mt-1 text-sub text-[var(--text-muted)]">
            统一管理系统对外发起 HTTP 请求（TMDB/TVDB/Bangumi 等）与 STRM 媒体探测（ffprobe/ffmpeg）时的标准 User-Agent。
          </p>
        </div>

        <div className="border-t border-white/[0.06] pt-4">
          <div>
            <label className="text-micro font-medium text-[var(--text-faint)]">
              全局 User-Agent
            </label>
            <input
              type="text"
              value={userAgent}
              onChange={(e) => setUserAgent(e.target.value)}
              placeholder="crawler-media/0.1.0"
              className="mt-1.5 w-full rounded-xl border border-white/[0.08] bg-white/[0.04] px-3.5 py-2 text-ui text-[var(--text)] outline-none focus:border-[var(--accent)] font-mono text-xs"
            />
            <p className="mt-1.5 text-micro text-[var(--text-faint)]">
              默认使用简洁干净的 <code className="text-sky-300">crawler-media/0.1.0</code>。保存后立即热生效，无需重启服务。
            </p>
          </div>
        </div>
      </div>

      <div className="flex justify-end">
        <button
          type="button"
          onClick={handleSave}
          disabled={saving}
          className="btn-accent rounded-full px-5 py-2 text-ui font-semibold disabled:opacity-50"
        >
          {saving ? "保存中…" : "保存浏览器与网络设置"}
        </button>
      </div>
    </div>
  );
}
