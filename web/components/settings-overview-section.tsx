"use client";

import { useCallback, useEffect, useState } from "react";
import { Link } from "react-router-dom";

import { BrandLoader } from "@/components/brand-loader";
import { ChevronRightIcon, GlobeIcon, RefreshIcon } from "@/components/icons";
import { PipelineHealthPanel } from "@/components/subscription-settings-section";
import {
  diagnoseProxySettings,
  type ProxyDiagnosticResult,
} from "@/lib/api/metadata";

/**
 * 「概览」分区：管理员进设置的落地页（/settings 重定向到这里）。
 *
 * 聚合两大核心体检：
 * 1. 订阅链路体检：逐库预演「搜索 → 下载 → 投递 → 入库」全流程；
 * 2. 元数据网络体检：检测 TMDB、豆瓣、Bangumi、AniList 等外网源的代理走线与延迟，
 *    网络超时或阻断时主动提醒并引导配置代理。
 */
export function SettingsOverviewSection() {
  return (
    <div className="space-y-10">
      <PipelineHealthPanel />
      <NetworkProxyHealthPanel />
    </div>
  );
}

/**
 * 元数据网络与代理连通性体检面板。
 * 呈现外部服务当前走线（代理 / 直连）与往返延迟；若有阻断给出修复引导。
 */
export function NetworkProxyHealthPanel() {
  const [data, setData] = useState<ProxyDiagnosticResult | null>(null);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);

  const reload = useCallback(() => {
    setFailed(false);
    setBusy(true);
    diagnoseProxySettings()
      .then(setData)
      .catch(() => setFailed(true))
      .finally(() => setBusy(false));
  }, []);

  useEffect(() => {
    reload();
  }, [reload]);

  useEffect(() => {
    const onVisible = () => {
      if (document.visibilityState === "visible") reload();
    };
    document.addEventListener("visibilitychange", onVisible);
    window.addEventListener("focus", onVisible);
    return () => {
      document.removeEventListener("visibilitychange", onVisible);
      window.removeEventListener("focus", onVisible);
    };
  }, [reload]);

  const hasIssues = data?.items.some((item) => !item.ok) ?? false;

  return (
    <section>
      <div className="mb-2 flex items-center justify-between">
        <div className="flex items-center gap-2">
          <span className="icon-chip size-7 !rounded-lg text-[var(--accent)]">
            <GlobeIcon className="size-4" />
          </span>
          <h3 className="text-body font-semibold text-white/90">元数据网络与代理体检</h3>
        </div>
        <button
          type="button"
          onClick={reload}
          disabled={busy}
          className="btn-glass flex items-center gap-1.5 px-3 py-1.5 text-sub font-medium disabled:opacity-60"
        >
          {busy && (
            <span className="size-3 animate-spin rounded-full border-2 border-white/20 border-t-white/70" />
          )}
          重新检测
        </button>
      </div>

      <p className="mb-4 text-sub leading-6 text-[var(--text-muted)]">
        实时探测影视元数据服务（TMDB、豆瓣、番组库等）的网络走线与连通性。国内网络遇阻断时，可通过网络代理（HTTP / SOCKS5）一键提速并解决白图与搜索超时。
      </p>

      {failed && (
        <p className="rounded-xl bg-white/[0.03] px-4 py-5 text-center text-ui text-[var(--text-muted)]">
          网络体检探测失败，请重试
        </p>
      )}

      {data === null && !failed && (
        <p className="flex items-center gap-2.5 rounded-xl bg-white/[0.03] px-4 py-5 text-ui text-[var(--text-muted)]">
          <BrandLoader className="size-5" />
          正在探测各元数据服务网络连通性…
        </p>
      )}

      {data !== null && (
        <div className="space-y-4">
          {/* 异常提示卡 */}
          {hasIssues && (
            <div className="flex flex-wrap items-center justify-between gap-3 rounded-2xl border border-[var(--danger)]/30 bg-[var(--danger)]/10 p-4">
              <div>
                <p className="text-ui font-semibold text-[#fee2e2]">部分外部元数据服务网络连接失败或超时</p>
                <p className="mt-0.5 text-sub text-white/70">
                  可能受本地网络 DNS 污染或防火墙阻断影响，建议配置专用的元数据网络代理（支持 SOCKS5 / HTTP）。
                </p>
              </div>
              <Link
                to="/settings/overview"
                onClick={() => {
                  // 点击后切到同一设置界面的元数据源卡片
                  window.location.hash = "#proxy";
                }}
                className="btn-accent flex items-center gap-1 px-3 py-1.5 text-sub font-semibold"
              >
                前往配置代理
                <ChevronRightIcon className="size-3.5" />
              </Link>
            </div>
          )}

          {/* 状态概览横幅 */}
          <div className="flex flex-wrap items-center justify-between gap-3 rounded-xl border border-white/10 bg-white/[0.03] px-4 py-3">
            <div className="flex items-center gap-2.5 text-sub">
              <span className="text-white/60">当前生效网络走线:</span>
              <span
                className={`rounded-md px-2 py-0.5 font-medium ${
                  data.active_proxy
                    ? "bg-[#38bdf8]/15 text-[#38bdf8]"
                    : "bg-white/10 text-white/80"
                }`}
              >
                {data.active_proxy ? `代理模式 (${data.active_proxy})` : "直连模式 (未配代理)"}
              </span>
              {data.douban_bypass && (
                <span className="rounded-md bg-emerald-500/15 px-2 py-0.5 font-medium text-emerald-400">
                  豆瓣已直连绕过
                </span>
              )}
            </div>

            <span className="text-caption text-white/45">
              共检测 {data.items.length} 个核心域名 · 全部测速完成
            </span>
          </div>

          {/* 域名指标卡片网格 */}
          <div className="grid grid-cols-1 gap-3 sm:grid-cols-2 lg:grid-cols-3">
            {data.items.map((item) => (
              <div
                key={item.url}
                className="flex flex-col justify-between rounded-xl border border-white/10 bg-white/[0.02] p-3.5 transition hover:border-white/20"
              >
                <div>
                  <div className="flex items-start justify-between gap-2">
                    <p className="text-ui font-semibold text-white/90">{item.name}</p>
                    <span
                      className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium ${
                        item.is_proxied
                          ? "bg-[#38bdf8]/15 text-[#38bdf8]"
                          : "bg-emerald-500/15 text-emerald-400"
                      }`}
                    >
                      {item.is_proxied ? "走代理" : "直连"}
                    </span>
                  </div>
                  <p className="mt-1 font-mono text-[11px] text-white/40 truncate">{item.url}</p>
                </div>

                <div className="mt-3.5 flex items-center justify-between border-t border-white/5 pt-2.5 text-caption">
                  <span className="text-white/40">{item.route_label}</span>
                  {item.ok ? (
                    <span className="flex items-center gap-1.5 font-medium text-[var(--ok)]">
                      <span className="size-1.5 rounded-full bg-[var(--ok)]" />
                      {item.latency_ms} ms
                    </span>
                  ) : (
                    <span
                      title={item.error ?? undefined}
                      className="flex items-center gap-1.5 font-medium text-[var(--danger)]"
                    >
                      <span className="size-1.5 rounded-full bg-[var(--danger)]" />
                      连接失败
                    </span>
                  )}
                </div>
              </div>
            ))}
          </div>
        </div>
      )}
    </section>
  );
}
