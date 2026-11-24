"use client";

import { useCallback, useEffect, useRef, useState } from "react";

import { DownloadIcon, PlusIcon } from "@/components/icons";
import { TransferModeCard } from "@/components/transfer-mode-card";
import {
  type Downloader,
  createDownloader,
  listDownloaders,
} from "@/lib/api/downloaders";

import { DownloaderForm } from "./settings/downloaders/downloader-form";
import { DownloaderRow } from "./settings/downloaders/downloader-row";

/**
 * 「下载器」设置分区。
 *
 * 与站点配置同构：列表展示已接入的下载器，「添加下载器」展开表单；
 * 保存只保存配置，不自动测试；pending/unverified 表示尚未测试，不是异步任务。
 * 手动测试结果仅在当前页面展示，不轮询 GET 覆盖。搜索结果里的"提交下载"以这里配置的实例为目标。
 */
export function DownloaderConfigSection() {
  const [downloaders, setDownloaders] = useState<Downloader[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // 是否展开「添加下载器」面板
  const [adding, setAdding] = useState(false);
  // 当前展开详情的下载器（单开手风琴，与站点列表同款交互）
  const [expanded, setExpanded] = useState<string | null>(null);
  // 当前亮出编辑表单的下载器 id（表单嵌在详情里；菜单「编辑配置」会先展开详情）
  const [editing, setEditing] = useState<string | null>(null);

  const load = useCallback(async () => {
    setError(null);
    try {
      setDownloaders(await listDownloaders());
    } catch (e) {
      setError((e as Error).message);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // 体检修复卡跳转带来的映射建议（?suggest_mapping=/xx）：自动展开默认
  // 下载器的编辑表单并预填一条映射的本机侧，用户只需补下载器视角的路径
  const [suggestMapping, setSuggestMapping] = useState<string | null>(null);
  useEffect(() => {
    const suggested = new URLSearchParams(window.location.search).get("suggest_mapping");
    if (suggested) setSuggestMapping(suggested);
  }, []);
  const suggestConsumed = useRef(false);
  useEffect(() => {
    if (!suggestMapping || loading || suggestConsumed.current || downloaders.length === 0) return;
    suggestConsumed.current = true;
    const target = downloaders.find((d) => d.is_default) ?? downloaders[0];
    setExpanded(target.id);
    setEditing(target.id);
  }, [suggestMapping, loading, downloaders]);

  // 拥堵提示跳转（?limits=<id>，与 suggest_mapping 同款参数惯例）：自动打开
  // 对应下载器的「全局限速」弹窗，用户落地即在要调的设置上
  const [autoLimitsId, setAutoLimitsId] = useState<string | null>(null);
  useEffect(() => {
    const raw = new URLSearchParams(window.location.search).get("limits");
    if (raw) setAutoLimitsId(raw);
  }, []);

  // 原地替换已有条目、新条目追加到末尾（保持列表顺序稳定，避免操作后跳位）
  const upsert = useCallback((next: Downloader) => {
    setDownloaders((prev) => {
      const idx = prev.findIndex((d) => d.id === next.id);
      if (idx === -1) return [...prev, next];
      const copy = [...prev];
      copy[idx] = next;
      return copy;
    });
  }, []);

  const usableCount = downloaders.filter((d) => d.enabled !== false && d.status === "active").length;

  return (
    <div className="space-y-5">
      <TransferModeCard />
      {error && (
        <div className="rounded-xl border border-[var(--danger)]/30 bg-[var(--danger)]/10 px-4 py-3 text-body text-[var(--danger)]">
          {error}
        </div>
      )}

      <div className="flex items-center justify-between gap-4">
        <p className="text-sub text-[var(--text-muted)]">
          {loading
            ? "加载中…"
            : downloaders.length === 0
              ? "接入你自己部署的下载软件，资源将由它们完成下载。"
              : `已接入 ${downloaders.length} 个下载器，${usableCount} 个可用。`}
        </p>
        <div className="flex shrink-0 items-center gap-2.5">
          <button
            type="button"
            onClick={() => void load()}
            disabled={loading}
            className="btn-glass px-3.5 py-1.5 text-sub font-medium"
          >
            刷新
          </button>
          <button
            type="button"
            onClick={() => setAdding((v) => !v)}
            disabled={loading}
            className="btn-accent flex items-center gap-1 rounded-full py-1.5 pl-2.5 pr-3.5 text-sub font-semibold disabled:opacity-60"
          >
            <PlusIcon className="size-4" />
            添加下载器
          </button>
        </div>
      </div>

      {/* 「添加下载器」面板：与站点页的添加面板同款扁平容器 */}
      {adding && (
        <div className="rounded-xl border border-white/[0.08] bg-white/[0.03] p-4">
          <DownloaderForm
            downloader={null}
            onSubmit={async (payload) => {
              upsert(await createDownloader(payload));
              setAdding(false);
            }}
            onCancel={() => setAdding(false)}
            onError={setError}
          />
        </div>
      )}

      {/* 已配置下载器列表：扁平面板容器，行式布局 */}
      {loading ? (
        <div className="space-y-px overflow-hidden rounded-xl border border-white/[0.08]">
          <div className="h-14 animate-pulse bg-white/[0.04]" />
          <div className="h-14 animate-pulse bg-white/[0.04]" />
        </div>
      ) : downloaders.length === 0 ? (
        <div className="flex flex-col items-center gap-3 rounded-xl border border-white/[0.08] bg-white/[0.03] px-6 py-12 text-center">
          <span className="icon-chip size-12 !rounded-2xl">
            <DownloadIcon className="size-6" />
          </span>
          <div>
            <p className="text-body font-medium text-[var(--text)]">还没有接入任何下载器</p>
            <p className="mt-1 text-sub text-[var(--text-muted)]">
              点击右上角「添加下载器」，支持 qBittorrent 和 Transmission。
            </p>
          </div>
        </div>
      ) : (
        <div className="divide-y divide-white/[0.06] overflow-hidden rounded-xl border border-white/[0.08] bg-white/[0.03]">
          {downloaders.map((downloader) => (
            <DownloaderRow
              key={downloader.id}
              downloader={downloader}
              suggestMapping={suggestMapping}
              autoOpenLimits={autoLimitsId === downloader.id}
              expanded={expanded === downloader.id}
              editing={editing === downloader.id}
              onToggle={() =>
                setExpanded((cur) => (cur === downloader.id ? null : downloader.id))
              }
              onEdit={() => {
                setExpanded(downloader.id);
                setEditing(downloader.id);
              }}
              onCloseEdit={() => setEditing((cur) => (cur === downloader.id ? null : cur))}
              onChanged={upsert}
              onDeleted={(id) => {
                setDownloaders((prev) => prev.filter((d) => d.id !== id));
                setExpanded((cur) => (cur === id ? null : cur));
                setEditing((cur) => (cur === id ? null : cur));
                // 删除默认时后端会把默认让给另一台，整体刷新拿到新归属
                void load();
              }}
              onRefresh={() => void load()}
              onError={setError}
            />
          ))}
        </div>
      )}
    </div>
  );
}
