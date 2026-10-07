"use client";

import { useCallback, useEffect, useState } from "react";

import { BrandLoader } from "@/components/brand-loader";
import { useToast } from "@/components/feedback";
import { GearIcon, PhotoIcon, RefreshIcon } from "@/components/icons";
import {
  getMetadataSettings,
  saveMetadataSettings,
  testMetadataSettings,
  getProxySettings,
  saveProxySettings,
  testProxySettings,
  diagnoseProxySettings,
  type MetadataSettings,
  type ProxySettings,
  type ProxyDomainDiagnostic,
} from "@/lib/api/metadata";

/**
 * 元数据源配置：TMDB Key 存 SQLite settings KV，保存后 catalog 客户端立即读取，
 * 无需重启。自动海报策略：TMDB 优先，无法访问图片 CDN 时提取最新已入库视频帧。
 */
export function MetadataSettingsSection() {
  const toast = useToast();
  const [settings, setSettings] = useState<MetadataSettings | null>(null);
  const [proxySettings, setProxySettings] = useState<ProxySettings | null>(null);
  const [key, setKey] = useState("");
  const [proxyUrl, setProxyUrl] = useState("");
  const [proxyUsername, setProxyUsername] = useState("");
  const [proxyPassword, setProxyPassword] = useState("");
  const [doubanBypass, setDoubanBypass] = useState(true);
  const [allowedDomains, setAllowedDomains] = useState("");
  const [busy, setBusy] = useState(false);
  const [proxyBusy, setProxyBusy] = useState(false);
  const [testing, setTesting] = useState(false);
  const [proxyTesting, setProxyTesting] = useState(false);
  const [diagnostics, setDiagnostics] = useState<ProxyDomainDiagnostic[] | null>(null);
  const [diagnosing, setDiagnosing] = useState(false);

  const reload = useCallback(() => {
    void getMetadataSettings()
      .then(setSettings)
      .catch((error) => toast.error(`读取元数据设置失败：${(error as Error).message}`));
    void getProxySettings()
      .then((res) => {
        setProxySettings(res);
        setProxyUrl(res.proxy_url);
        setProxyUsername(res.username);
        setProxyPassword("");
        setDoubanBypass(res.douban_bypass);
        setAllowedDomains(res.allowed_domains ?? "");
      })
      .catch((error) => toast.error(`读取代理设置失败：${(error as Error).message}`));
  }, [toast]);

  useEffect(() => {
    reload();
  }, [reload]);

  useEffect(() => {
    if (typeof window !== "undefined" && window.location.hash === "#proxy") {
      const timer = setTimeout(() => {
        document.getElementById("proxy")?.scrollIntoView({ behavior: "smooth" });
      }, 100);
      return () => clearTimeout(timer);
    }
  }, []);

  const save = async () => {
    if (!key.trim()) {
      toast.error("请输入 TMDB API Key；清空请点击“清除 Key”");
      return;
    }
    setBusy(true);
    try {
      const next = await saveMetadataSettings({ tmdb_api_key: key.trim() });
      setSettings(next);
      setKey("");
      toast.success("TMDB Key 已保存，后续刷新媒体库将立即生效");
    } catch (error) {
      toast.error(`保存失败：${(error as Error).message}`);
    } finally {
      setBusy(false);
    }
  };

  const clear = async () => {
    setBusy(true);
    try {
      const next = await saveMetadataSettings({ tmdb_api_key: "" });
      setSettings(next);
      toast.success("TMDB Key 已清除；封面将改用本地视频帧兜底");
    } catch (error) {
      toast.error(`清除失败：${(error as Error).message}`);
    } finally {
      setBusy(false);
    }
  };

  const test = async () => {
    setTesting(true);
    try {
      const result = await testMetadataSettings();
      if (result.ok) {
        toast.success(`TMDB 连接成功${result.sample ? `：${result.sample}` : ""}`);
      } else {
        toast.error(`TMDB 测试失败：${result.error ?? "未知错误"}`);
      }
    } catch (error) {
      toast.error(`TMDB 测试失败：${(error as Error).message}`);
    } finally {
      setTesting(false);
    }
  };

  const saveProxy = async () => {
    setProxyBusy(true);
    try {
      const next = await saveProxySettings({
        proxy_url: proxyUrl.trim(),
        username: proxyUsername.trim(),
        ...(proxyPassword.trim() ? { password: proxyPassword.trim() } : {}),
        douban_bypass: doubanBypass,
        allowed_domains: allowedDomains.trim(),
      });
      setProxySettings(next);
      setProxyPassword("");
      toast.success(
        next.proxy_url
          ? "网络代理已保存，外部元数据请求将立即走代理"
          : "已关闭代理，外部元数据请求恢复直连",
      );
    } catch (error) {
      toast.error(`保存代理失败：${(error as Error).message}`);
    } finally {
      setProxyBusy(false);
    }
  };

  const clearProxy = async () => {
    setProxyBusy(true);
    try {
      const next = await saveProxySettings({
        proxy_url: "",
        username: "",
        password: "",
        douban_bypass: true,
      });
      setProxySettings(next);
      setProxyUrl("");
      setProxyUsername("");
      setProxyPassword("");
      setDoubanBypass(true);
      toast.success("已清除代理配置，恢复直连");
    } catch (error) {
      toast.error(`清除代理失败：${(error as Error).message}`);
    } finally {
      setProxyBusy(false);
    }
  };

  const testProxy = async () => {
    setProxyTesting(true);
    try {
      const result = await testProxySettings({
        proxy_url: proxyUrl.trim() || undefined,
        username: proxyUsername.trim() || undefined,
        password: proxyPassword.trim() || undefined,
      });
      if (result.ok) {
        toast.success(`代理连通正常！延迟 ${result.latency_ms} ms`);
      } else {
        toast.error(`代理测试失败：${result.error ?? "连接超时或拒绝"}`);
      }
    } catch (error) {
      toast.error(`代理测试失败：${(error as Error).message}`);
    } finally {
      setProxyTesting(false);
    }
  };

  const runDiagnose = async () => {
    setDiagnosing(true);
    try {
      const result = await diagnoseProxySettings();
      setDiagnostics(result.items);
      toast.success("元数据域名代理生效检测完成");
    } catch (error) {
      toast.error(`检测失败：${(error as Error).message}`);
    } finally {
      setDiagnosing(false);
    }
  };

  if (!settings) {
    return (
      <div className="flex items-center justify-center gap-2 py-14 text-ui text-[var(--text-muted)]">
        <BrandLoader className="size-5" />
        正在读取元数据源设置…
      </div>
    );
  }

  return (
    <div className="space-y-8">
      <div className="css-glass rounded-2xl p-6">
        <div className="flex items-start gap-4">
          <span className="icon-chip size-11 shrink-0 !rounded-2xl">
            <PhotoIcon className="size-5" />
          </span>
          <div>
            <h2 className="text-title-sm font-semibold text-[var(--text)]">封面自动策略</h2>
            <p className="mt-1 text-sub leading-6 text-[var(--text-muted)]">
              默认优先使用 TMDB 海报；若图片服务不可达或条目无元数据，系统从最新已入库的剧集/电影提取画面作为封面。
              在条目详情里可以手动选择或上传自定义海报，手动图片不会被自动刷新覆盖。
            </p>
          </div>
        </div>
      </div>

      <section>
        <h3 className="group-label mb-2.5 px-1">TMDB</h3>
        <div className="css-glass rounded-2xl p-6">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div>
              <p className="text-body font-semibold">TMDB API Key</p>
              <p className="mt-1 text-sub text-[var(--text-muted)]">
                {settings.tmdb.configured
                  ? `已配置：${settings.tmdb.api_key_hint ?? "已隐藏"}`
                  : "未配置：可仍使用本地视频帧自动封面"}
              </p>
            </div>
            <span
              className={`rounded-full px-2.5 py-1 text-caption font-semibold ${
                settings.tmdb.configured
                  ? "bg-[var(--ok)]/15 text-[var(--ok)]"
                  : "bg-white/[0.08] text-white/55"
              }`}
            >
              {settings.tmdb.configured ? "已接入" : "未接入"}
            </span>
          </div>

          <label className="mt-5 block">
            <span className="mb-1.5 flex items-center gap-1.5 text-sub font-medium text-[var(--text-muted)]">
              <GearIcon className="size-3.5" />
              API Key
            </span>
            <input
              type="password"
              value={key}
              onChange={(event) => setKey(event.target.value)}
              placeholder={settings.tmdb.configured ? "填写新 Key 以替换当前配置" : "粘贴 TMDB v3 API Key"}
              autoComplete="off"
              className="glass-input w-full px-3 py-2.5 font-mono text-ui"
            />
          </label>

          <div className="mt-4 flex flex-wrap gap-2">
            <button type="button" disabled={busy} onClick={() => void save()} className="btn-accent px-4 py-2 text-ui font-semibold disabled:opacity-50">
              {busy ? "保存中…" : "保存 Key"}
            </button>
            <button type="button" disabled={testing || !settings.tmdb.configured} onClick={() => void test()} className="btn-glass px-4 py-2 text-ui font-medium disabled:opacity-50">
              <RefreshIcon className="size-4" />
              {testing ? "测试中…" : "测试连接"}
            </button>
            {settings.tmdb.configured && (
              <button type="button" disabled={busy} onClick={() => void clear()} className="btn-glass px-4 py-2 text-ui font-medium text-[#ffaaaa] disabled:opacity-50">
                清除 Key
              </button>
            )}
          </div>
        </div>
      </section>

      <section id="proxy">
        <h3 className="group-label mb-2.5 px-1">网络代理 (HTTP / SOCKS5)</h3>
        <div className="css-glass rounded-2xl p-6">
          <div className="flex flex-wrap items-center justify-between gap-3">
            <div>
              <p className="text-body font-semibold">元数据专属代理</p>
              <p className="mt-1 text-sub text-[var(--text-muted)]">
                解决 TMDB、TVDB、Bangumi、AniList 等因网络阻断导致的搜索慢、海报拉取失败问题。PT 站点请求默认隔离不走此代理。
              </p>
            </div>
            <span
              className={`rounded-full px-2.5 py-1 text-caption font-semibold ${
                proxySettings?.proxy_url
                  ? "bg-[var(--ok)]/15 text-[var(--ok)]"
                  : "bg-white/[0.08] text-white/55"
              }`}
            >
              {proxySettings?.proxy_url ? "已配置代理" : "直连 (无代理)"}
            </span>
          </div>

          <label className="mt-5 block">
            <span className="mb-1.5 flex items-center gap-1.5 text-sub font-medium text-[var(--text-muted)]">
              <GearIcon className="size-3.5" />
              代理地址 (支持 HTTP 与 SOCKS5，亦可直接写 socks5://user:pass@host:port)
            </span>
            <input
              type="text"
              value={proxyUrl}
              onChange={(event) => setProxyUrl(event.target.value)}
              placeholder="例如: socks5://127.0.0.1:1080 或 http://127.0.0.1:7890"
              autoComplete="off"
              className="glass-input w-full px-3 py-2.5 font-mono text-ui"
            />
          </label>

          <div className="mt-4 grid grid-cols-1 gap-4 sm:grid-cols-2">
            <label className="block">
              <span className="mb-1.5 block text-sub font-medium text-[var(--text-muted)]">
                认证用户名 (选填)
              </span>
              <input
                type="text"
                value={proxyUsername}
                onChange={(event) => setProxyUsername(event.target.value)}
                placeholder="无认证请留空"
                autoComplete="off"
                className="glass-input w-full px-3 py-2 font-mono text-ui"
              />
            </label>

            <label className="block">
              <span className="mb-1.5 block text-sub font-medium text-[var(--text-muted)]">
                认证密码 (选填)
              </span>
              <input
                type="password"
                value={proxyPassword}
                onChange={(event) => setProxyPassword(event.target.value)}
                placeholder={
                  proxySettings?.has_password
                    ? "已配置密码 (不修改请留空)"
                    : "无密码请留空"
                }
                autoComplete="off"
                className="glass-input w-full px-3 py-2 font-mono text-ui"
              />
            </label>
          </div>

          <label className="mt-4 flex items-center gap-2.5 text-ui text-[var(--text-muted)] cursor-pointer">
            <input
              type="checkbox"
              checked={doubanBypass}
              onChange={(event) => setDoubanBypass(event.target.checked)}
              className="size-4 rounded border-white/20 bg-white/5 accent-[var(--accent)]"
            />
            <span>豆瓣直连（推荐开启：豆瓣国内直连最快，且不易触发海外 IP 风控验证码）</span>
          </label>

          <label className="mt-4 block">
            <span className="mb-1.5 flex items-center gap-1.5 text-sub font-medium text-[var(--text-muted)]">
              代理生效域名白名单（支持逗号或换行分隔）
            </span>
            <input
              type="text"
              value={allowedDomains}
              onChange={(event) => setAllowedDomains(event.target.value)}
              placeholder="默认内置: themoviedb.org, tmdb.org, thetvdb.com, anilist.co, bgm.tv, theintrodb.org"
              autoComplete="off"
              className="glass-input w-full px-3 py-2 font-mono text-ui"
            />
            <p className="mt-1.5 text-caption text-white/40">
              只有匹配此列表的国外元数据域名才允许走代理；网盘、视频播放流 (STRM)、测试用例及其他未知域名一律强制纯直连。
            </p>
          </label>

          <div className="mt-5 flex flex-wrap gap-2">
            <button
              type="button"
              disabled={proxyBusy}
              onClick={() => void saveProxy()}
              className="btn-accent px-4 py-2 text-ui font-semibold disabled:opacity-50"
            >
              {proxyBusy ? "保存中…" : "保存代理"}
            </button>
            <button
              type="button"
              disabled={proxyTesting || !proxyUrl.trim()}
              onClick={() => void testProxy()}
              className="btn-glass px-4 py-2 text-ui font-medium disabled:opacity-50"
            >
              <RefreshIcon className="size-4" />
              {proxyTesting ? "探测中…" : "测试连通性"}
            </button>
            <button
              type="button"
              disabled={diagnosing}
              onClick={() => void runDiagnose()}
              className="btn-glass px-4 py-2 text-ui font-medium text-[var(--accent)] disabled:opacity-50"
            >
              <RefreshIcon className={`size-4 ${diagnosing ? "animate-spin" : ""}`} />
              {diagnosing ? "正在诊断各域名…" : "检测各域名代理生效情况"}
            </button>
            {proxySettings?.proxy_url && (
              <button
                type="button"
                disabled={proxyBusy}
                onClick={() => void clearProxy()}
                className="btn-glass px-4 py-2 text-ui font-medium text-[#ffaaaa] disabled:opacity-50"
              >
                清除代理
              </button>
            )}
          </div>

          {diagnostics && diagnostics.length > 0 && (
            <div className="mt-6 border-t border-white/10 pt-5">
              <h4 className="text-sub font-semibold text-white/90">各域名实际走线与连通性诊断</h4>
              <p className="mt-0.5 text-caption text-white/50">
                实时探针已发出，下面展示各元数据服务当前实际生效的访问链路与耗时：
              </p>

              <div className="mt-3.5 divide-y divide-white/5 overflow-hidden rounded-xl border border-white/10 bg-black/20">
                {diagnostics.map((item) => (
                  <div
                    key={item.url}
                    className="flex flex-wrap items-center justify-between gap-3 px-4 py-3"
                  >
                    <div>
                      <div className="flex items-center gap-2">
                        <span className="text-ui font-medium text-white">{item.name}</span>
                        <span
                          className={`rounded-md px-2 py-0.5 text-[11px] font-medium ${
                            item.is_proxied
                              ? "bg-[#38bdf8]/15 text-[#38bdf8]"
                              : "bg-emerald-500/15 text-emerald-400"
                          }`}
                        >
                          {item.route_label}
                        </span>
                      </div>
                      <p className="mt-0.5 font-mono text-[11px] text-white/40">{item.url}</p>
                    </div>

                    <div className="flex items-center gap-2.5">
                      {item.ok ? (
                        <span className="flex items-center gap-1.5 text-caption font-semibold text-[var(--ok)]">
                          <span className="size-1.5 rounded-full bg-[var(--ok)]" />
                          连通正常 · {item.latency_ms} ms {item.error ? `(${item.error})` : ""}
                        </span>
                      ) : (
                        <span
                          title={item.error ?? undefined}
                          className="flex items-center gap-1.5 text-caption font-medium text-[var(--danger)]"
                        >
                          <span className="size-1.5 rounded-full bg-[var(--danger)]" />
                          不可达 · {item.error ?? "请求失败"}
                        </span>
                      )}
                    </div>
                  </div>
                ))}
              </div>
            </div>
          )}
        </div>
      </section>
    </div>
  );
}
