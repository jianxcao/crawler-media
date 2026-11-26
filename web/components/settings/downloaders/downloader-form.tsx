"use client";

import { useState } from "react";

import { DirectoryPicker } from "@/components/directory-picker";
import { FolderIcon, PlusIcon, XIcon } from "@/components/icons";
import type {
  Downloader,
  DownloaderAuthType,
  DownloaderClientType,
  DownloaderPayload,
  PathMapping,
} from "@/lib/api/downloaders";

import { TYPE_LABEL, URL_PLACEHOLDER } from "./downloader-meta";

/* —— 连接表单：类型 + 名称 + 地址 + 可选凭证/保存目录，新增与编辑共用 —— */

export interface DownloaderFormProps {
  /** 编辑对象；null 表示新增 */
  downloader: Downloader | null;
  /** 体检跳转建议预填的映射本机侧路径（无建议时不传） */
  suggestMapping?: string | null;
  onSubmit: (payload: DownloaderPayload) => Promise<void>;
  onCancel: () => void;
  onError: (message: string) => void;
}

/** 已有映射按前缀已覆盖建议路径时不再追加，避免预填出冗余行。 */
function withSuggestedMapping(existing: PathMapping[], suggest: string | null): PathMapping[] {
  if (!suggest) return existing;
  const norm = (p: string) => p.trim().replace(/\/+$/, "") || "/";
  const target = norm(suggest);
  const covered = existing.some((m) => {
    const local = norm(m.local);
    return local !== "/" && (target === local || target.startsWith(local + "/"));
  });
  return covered ? existing : [...existing, { local: suggest, remote: "" }];
}

export function DownloaderForm({
  downloader,
  suggestMapping = null,
  onSubmit,
  onCancel,
  onError,
}: DownloaderFormProps) {
  const [busy, setBusy] = useState(false);
  const [clientType, setClientType] = useState<DownloaderClientType>(
    downloader?.kind ?? "qbittorrent",
  );
  const [authType, setAuthType] = useState<DownloaderAuthType>(
    downloader?.kind !== "transmission" && downloader?.auth_type === "apikey"
      ? "apikey"
      : "password",
  );
  const [name, setName] = useState(downloader?.name ?? "");
  const [url, setUrl] = useState(downloader?.url ?? "");
  const [username, setUsername] = useState(downloader?.username ?? "");
  // 出于安全后端不回传密码，编辑时留空需重新填写（未开鉴权则保持留空）
  const [password, setPassword] = useState("");
  const [apiKey, setApiKey] = useState("");
  // 路径映射：跨容器部署时本机与下载器看同一块盘的两个名字。
  // 体检跳转带建议时预填一行本机侧（右侧留给用户按下载器视角补齐）
  const [mappings, setMappings] = useState<PathMapping[]>(
    withSuggestedMapping(
      downloader?.path_maps.map((m) => ({ local: m.to, remote: m.from })) ?? [],
      suggestMapping,
    ),
  );
  // 已有映射的编辑态或带预填建议时默认展开，否则折叠（绝大多数部署用不到）
  const [mappingsOpen, setMappingsOpen] = useState(
    (downloader?.path_maps?.length ?? 0) > 0 || suggestMapping !== null,
  );
  // 目录弹窗当前服务的字段：数字 = 第 N 条映射的左列
  const [pickerTarget, setPickerTarget] = useState<number | null>(null);

  // 映射行要么删掉要么填完整：下载器侧必须是绝对路径
  const mappingsComplete = mappings.every(
    (m) => m.local.trim().length > 0 && m.remote.trim().startsWith("/"),
  );
  // 两端各自查重（尾部斜杠归一后比较）：同一本机路径两条映射翻译结果
  // 看遍历顺序，两条映射指向同一下载器路径同样是错配
  const normPath = (p: string) => p.trim().replace(/\/+$/, "") || "/";
  const localPaths = mappings.map((m) => normPath(m.local)).filter((p) => p !== "/");
  const remotePaths = mappings.map((m) => normPath(m.remote)).filter((p) => p !== "/");
  const mappingsUnique =
    new Set(localPaths).size === localPaths.length &&
    new Set(remotePaths).size === remotePaths.length;
  const isQbitApiKey = clientType === "qbittorrent" && authType === "apikey";
  const hasValidAuth =
    !isQbitApiKey || apiKey.trim().length > 0 || Boolean(downloader?.has_api_key);
  const canSubmit =
    name.trim().length > 0 &&
    /^https?:\/\/.+/.test(url.trim()) &&
    hasValidAuth &&
    mappingsComplete &&
    mappingsUnique;

  function submit() {
    setBusy(true);
    const keepStoredPassword =
      Boolean(downloader?.has_password) &&
      downloader?.auth_type !== "apikey" &&
      password === "";
    const authPayload: Partial<DownloaderPayload> =
      clientType === "qbittorrent"
        ? authType === "apikey"
          ? { auth_type: "apikey", username: "", password: "", api_key: apiKey.trim() || null }
          : {
              auth_type: "password",
              username: username.trim() || null,
              ...(keepStoredPassword ? {} : { password }),
              api_key: "",
            }
        : {
            username: username.trim() || null,
            ...(keepStoredPassword ? {} : { password }),
            api_key: "",
          };
    void onSubmit({
      name: name.trim(),
      client_type: clientType,
      url: url.trim(),
      ...authPayload,
      path_mappings: mappings.length
        ? mappings.map((m) => ({ local: m.local.trim(), remote: m.remote.trim() }))
        : null,
    })
      .catch((e) => onError((e as Error).message))
      .finally(() => setBusy(false));
  }

  function setMapping(index: number, patch: Partial<PathMapping>) {
    setMappings((prev) => prev.map((m, i) => (i === index ? { ...m, ...patch } : m)));
  }

  const inputClass =
    "w-full rounded-xl border border-white/[0.08] bg-white/[0.04] px-3 py-2 text-ui " +
    "text-[var(--text)] outline-none focus:border-[var(--accent)]/60";
  const labelClass = "mb-1.5 block text-sub font-medium text-[var(--text-muted)]";

  return (
    <div className="space-y-4">
      {/* 下载器类型 */}
      <div>
        <label className={labelClass}>下载器类型</label>
        <div className="flex flex-wrap gap-2">
          {(Object.keys(TYPE_LABEL) as DownloaderClientType[]).map((t) => (
            <button
              key={t}
              type="button"
              onClick={() => setClientType(t)}
              data-active={clientType === t}
              className="glass-row nav-item !w-auto px-3 py-1.5 text-sub font-medium"
            >
              {TYPE_LABEL[t]}
            </button>
          ))}
        </div>
      </div>

      <div>
        <label className={labelClass}>名称</label>
        <input
          type="text"
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="如：家里的 qBittorrent"
          autoComplete="off"
          className={inputClass}
        />
      </div>

      <div>
        <label className={labelClass}>
          {clientType === "qbittorrent" ? "WebUI 地址" : "RPC 地址"}
        </label>
        <input
          type="text"
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder={URL_PLACEHOLDER[clientType]}
          autoComplete="off"
          className={inputClass}
        />
      </div>

      {/* 认证方式（qBittorrent 支持用户名密码或 v5.2+ 原生 API Key） */}
      {clientType === "qbittorrent" && (
        <div>
          <label className={labelClass}>认证方式</label>
          <div className="flex flex-wrap gap-2">
            <button
              type="button"
              onClick={() => setAuthType("password")}
              data-active={authType === "password"}
              className="glass-row nav-item !w-auto px-3 py-1.5 text-sub font-medium"
            >
              用户名与密码
            </button>
            <button
              type="button"
              onClick={() => setAuthType("apikey")}
              data-active={authType === "apikey"}
              className="glass-row nav-item !w-auto px-3 py-1.5 text-sub font-medium"
            >
              API Key（v5.2+）
            </button>
          </div>
        </div>
      )}

      {isQbitApiKey ? (
        <div className="space-y-2">
          <div>
            <label className={labelClass}>API Key</label>
            <input
              type="password"
              value={apiKey}
              onChange={(e) => setApiKey(e.target.value)}
              placeholder={
                downloader?.has_api_key
                  ? "已配置 API Key（留空则保持不变）"
                  : "粘贴 qbt_ 开头的 API Key"
              }
              autoComplete="new-password"
              className={`${inputClass} font-mono`}
            />
          </div>
          <p className="rounded-xl border border-white/[0.06] bg-white/[0.03] px-3 py-2 text-caption leading-relaxed text-[var(--text-faint)]">
            适用于 qBittorrent v5.2.0 及以上版本：请求时直接发送
            <code className="mx-1 font-mono text-[var(--text-muted)]">Authorization: Bearer</code>
            认证头，无需填写用户名/密码，也不依赖 Cookie 会话。请在 qBittorrent WebUI → 设置（选项）→ WebUI → API Key 中生成并复制
            <code className="mx-0.5 font-mono">qbt_</code> 开头的密钥。
          </p>
        </div>
      ) : (
        <div className="grid grid-cols-2 gap-3">
          <div>
            <label className={labelClass}>用户名（可选）</label>
            <input
              type="text"
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              autoComplete="off"
              className={inputClass}
            />
          </div>
          <div>
            <label className={labelClass}>密码（可选）</label>
            <input
              type="password"
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              placeholder={
                downloader?.has_password && downloader?.auth_type !== "apikey"
                  ? "已配置密码（留空则保持不变）"
                  : ""
              }
              autoComplete="new-password"
              className={inputClass}
            />
          </div>
        </div>
      )}

      {/* 路径映射：默认折叠，仅跨容器/跨主机部署需要展开配置 */}
      <div>
        <button
          type="button"
          onClick={() => setMappingsOpen((v) => !v)}
          className="flex items-center gap-1.5 text-sub font-medium text-[var(--text-muted)] transition-colors hover:text-[var(--text)]"
        >
          <span
            className="inline-block transition-transform"
            style={{ transform: mappingsOpen ? "rotate(90deg)" : undefined }}
          >
            ›
          </span>
          路径映射（可选）
          {!mappingsOpen && mappings.length > 0 && (
            <span className="text-[var(--text-faint)]">已配置 {mappings.length} 条</span>
          )}
        </button>
        {mappingsOpen && (
          <div className="mt-2.5 space-y-2.5">
            <p className="text-caption leading-relaxed text-[var(--text-faint)]">
              本机与下载器不在同一容器/主机、同一块盘两边路径不同时才需要：
              提交下载前会把保存目录按前缀翻译成下载器视角。例如本机看到的下载区是
              <code className="mx-0.5 font-mono">/data/downloads</code>、下载器容器里是
              <code className="mx-0.5 font-mono">/downloads</code>，则添加一条对照。留空表示两边路径一致。
              注意：配了映射后，所有下载保存目录（含媒体库目录）都必须被某条映射覆盖，
              否则会拒绝投递以防下载进下载器容器内的孤立路径；下载器能以相同路径直达的目录，
              添加一条两边相同的映射即可。
            </p>
            {suggestMapping && (
              <p className="rounded-lg bg-[var(--accent-soft)] px-3 py-2 text-caption leading-relaxed text-[var(--accent)]">
                已按体检建议预填映射的本机侧 {suggestMapping}——右侧填下载器视角的对应路径；
                下载器可直达同名路径时，点中间的 → 把左侧复制过去即可。
              </p>
            )}
            {mappings.map((mapping, index) => (
              <div key={index} className="flex items-center gap-2">
                <button
                  type="button"
                  onClick={() => setPickerTarget(index)}
                  title="本机上的路径（浏览选择）"
                  className="flex min-w-0 flex-1 items-center gap-2 rounded-xl border border-white/[0.08] bg-white/[0.04] px-3 py-2 text-left transition-colors hover:border-[var(--accent)]/50"
                >
                  <FolderIcon className="size-4 shrink-0 text-[var(--accent)]/80" />
                  {mapping.local ? (
                    <span dir="rtl" className="min-w-0 flex-1 truncate font-mono text-ui text-[var(--text)]">
                      {"‎" + mapping.local + "‎"}
                    </span>
                  ) : (
                    <span className="truncate text-ui text-[var(--text-faint)]">本机上的路径…</span>
                  )}
                </button>
                {/* 箭头即按钮：两边路径一致时点一下把左侧复制到右侧，省去手动输入 */}
                <button
                  type="button"
                  onClick={() => setMapping(index, { remote: mapping.local })}
                  disabled={!mapping.local}
                  title="将左侧路径复制到右侧（两边路径一致时使用）"
                  aria-label="将左侧路径复制到右侧"
                  className="shrink-0 rounded-lg px-1.5 py-1 text-[var(--text-faint)] transition-colors enabled:hover:bg-white/[0.06] enabled:hover:text-[var(--accent)] disabled:cursor-default"
                >
                  →
                </button>
                <input
                  type="text"
                  value={mapping.remote}
                  onChange={(e) => setMapping(index, { remote: e.target.value })}
                  placeholder="下载器上的路径，如 /downloads"
                  autoComplete="off"
                  className={`${inputClass} flex-1 font-mono`}
                />
                <button
                  type="button"
                  onClick={() => setMappings((prev) => prev.filter((_, i) => i !== index))}
                  aria-label="删除这条映射"
                  className="glass-row !w-auto shrink-0 p-2"
                >
                  <XIcon className="size-4" />
                </button>
              </div>
            ))}
            <button
              type="button"
              onClick={() => setMappings((prev) => [...prev, { local: "", remote: "" }])}
              className="btn-glass flex items-center gap-1 px-3 py-1.5 text-sub font-medium"
            >
              <PlusIcon className="size-3.5" />
              添加映射
            </button>
            {!mappingsComplete ? (
              <p className="text-caption text-[var(--warn)]">
                每条映射两边都要填：左边浏览选择，右边填下载器上以 / 开头的绝对路径；不需要的行请删除。
              </p>
            ) : !mappingsUnique ? (
              <p className="text-caption text-[var(--warn)]">
                映射的路径不能重复：同一本机路径或同一下载器路径只能出现一次，请修改或删除重复的行。
              </p>
            ) : null}
          </div>
        )}
      </div>

      <div className="flex items-center justify-end gap-3 pt-1">
        <button type="button" onClick={onCancel} className="btn-glass px-3.5 py-2 text-ui font-medium">
          取消
        </button>
        <button
          type="button"
          onClick={submit}
          disabled={busy || !canSubmit}
          className="btn-accent rounded-full px-4.5 py-2 text-ui font-semibold disabled:opacity-40"
        >
          {busy ? "保存中…" : "保存"}
        </button>
      </div>

      <DirectoryPicker
        open={pickerTarget !== null}
        initialPath={
          pickerTarget != null ? mappings[pickerTarget]?.local || undefined : undefined
        }
        onClose={() => setPickerTarget(null)}
        onSelect={(path) => {
          if (pickerTarget != null) setMapping(pickerTarget, { local: path });
          setPickerTarget(null);
        }}
      />
    </div>
  );
}
