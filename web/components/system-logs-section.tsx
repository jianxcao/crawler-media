"use client";

import { useCallback, useEffect, useRef, useState, useMemo } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import dayjs from "dayjs";

import {
  listSystemLogs,
  clearSystemLogs,
  subscribeSystemLogs,
  exportLogsUrl,
  type SystemLogEntry,
} from "@/lib/api/logs";
import { useToast, useConfirm } from "@/components/feedback";

const LOG_LEVELS = [
  { id: "ALL", label: "全部级别" },
  { id: "ERROR", label: "ERROR", badgeColor: "bg-[var(--danger)]/15 text-[var(--danger)] border-[var(--danger)]/30" },
  { id: "WARN", label: "WARN", badgeColor: "bg-[var(--warn)]/15 text-[var(--warn)] border-[var(--warn)]/30" },
  { id: "INFO", label: "INFO", badgeColor: "bg-[var(--info)]/15 text-[var(--info)] border-[var(--info)]/30" },
  { id: "DEBUG", label: "DEBUG", badgeColor: "bg-[var(--glass-fill)] text-[var(--text-muted)] border-[var(--line)]" },
];

const MODULE_OPTIONS = [
  { id: "ALL", label: "全部模块" },
  { id: "subscribe", label: "订阅/巡检 (subscribe)" },
  { id: "downloader", label: "下载器 (downloader)" },
  { id: "indexer", label: "索引/站点 (indexer)" },
  { id: "library", label: "媒体库/刮削 (library)" },
  { id: "api", label: "API 服务 (api)" },
  { id: "playback", label: "播放 (playback)" },
];

export function SystemLogsSection() {
  const [logs, setLogs] = useState<SystemLogEntry[]>([]);
  const [level, setLevel] = useState("ALL");
  const [moduleFilter, setModuleFilter] = useState("ALL");
  const [searchQuery, setSearchQuery] = useState("");
  const [autoScroll, setAutoScroll] = useState(true);
  const [loading, setLoading] = useState(true);

  const parentRef = useRef<HTMLDivElement>(null);
  const toast = useToast();
  const toastRef = useRef(toast);
  toastRef.current = toast;
  const confirm = useConfirm();

  // Load initial logs
  const loadLogs = useCallback(async () => {
    try {
      setLoading(true);
      const res = await listSystemLogs({ limit: 3000 });
      setLogs(res.entries || []);
    } catch (err) {
      toastRef.current.error(err instanceof Error ? err.message : "加载系统日志失败");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadLogs();
  }, [loadLogs]);

  // SSE real-time log subscription
  useEffect(() => {
    const unsub = subscribeSystemLogs({
      onLog: (newEntry) => {
        setLogs((prev) => {
          const next = [...prev, newEntry];
          if (next.length > 3000) {
            return next.slice(next.length - 3000);
          }
          return next;
        });
      },
    });

    return () => {
      unsub();
    };
  }, []);

  // Filter logs locally for instant feedback
  const filteredLogs = useMemo(() => {
    const q = searchQuery.trim().toLowerCase();
    const l = level === "ALL" ? null : level;
    const m = moduleFilter === "ALL" ? null : moduleFilter.toLowerCase();

    return logs.filter((item) => {
      if (l && item.level !== l) return false;
      if (m && !item.target.toLowerCase().includes(m)) return false;
      if (q && !item.message.toLowerCase().includes(q) && !item.target.toLowerCase().includes(q)) {
        return false;
      }
      return true;
    });
  }, [logs, level, moduleFilter, searchQuery]);

  // Virtual scrolling
  const rowVirtualizer = useVirtualizer({
    count: filteredLogs.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => 28,
    getItemKey: (index) => filteredLogs[index]?.id ?? index,
    overscan: 20,
  });

  const tailLogId = filteredLogs[filteredLogs.length - 1]?.id ?? null;

  // Auto-scroll to bottom when new logs arrive if enabled
  useEffect(() => {
    if (autoScroll && filteredLogs.length > 0) {
      rowVirtualizer.scrollToIndex(filteredLogs.length - 1, { align: "end" });
    }
  }, [filteredLogs.length, tailLogId, autoScroll, rowVirtualizer]);

  const handleClear = async () => {
    const ok = await confirm({
      title: "清空日志",
      description: "确定要清空当前的内存运行日志吗？这不会删除历史磁盘日志文件。",
      confirmLabel: "清空",
      tone: "danger",
    });
    if (!ok) return;

    try {
      await clearSystemLogs();
      setLogs([]);
      toast.success("日志已清空");
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "清空日志失败");
    }
  };

  const handleExport = async () => {
    try {
      const url = exportLogsUrl();
      const token = typeof window !== "undefined" ? window.localStorage.getItem("mc_token") : null;
      const res = await fetch(url, {
        signal: AbortSignal.timeout(15_000),
        headers: token ? { Authorization: `Bearer ${token}` } : {},
      });
      if (!res.ok) throw new Error(`导出失败 (${res.status})`);
      const blob = await res.blob();
      const blobUrl = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = blobUrl;
      a.download = `crawler-media-logs-${Date.now()}.txt`;
      document.body.appendChild(a);
      a.click();
      a.remove();
      URL.revokeObjectURL(blobUrl);
    } catch (err) {
      toast.error(err instanceof Error ? err.message : "导出日志失败");
    }
  };

  return (
    <div className="flex flex-col gap-4">
      {/* 顶部过滤工具栏 */}
      <div className="css-glass rounded-2xl p-3.5 flex flex-wrap items-center justify-between gap-3">
        {/* 左侧筛选条件 */}
        <div className="flex flex-wrap items-center gap-2.5">
          {/* Level selector */}
          <select
            value={level}
            onChange={(e) => setLevel(e.target.value)}
            className="input-glass cursor-pointer rounded-lg px-2.5 py-1 text-ui font-medium text-[var(--text)]"
          >
            {LOG_LEVELS.map((item) => (
              <option key={item.id} value={item.id}>
                {item.label}
              </option>
            ))}
          </select>

          {/* Module selector */}
          <select
            value={moduleFilter}
            onChange={(e) => setModuleFilter(e.target.value)}
            className="input-glass cursor-pointer rounded-lg px-2.5 py-1 text-ui font-medium text-[var(--text)]"
          >
            {MODULE_OPTIONS.map((item) => (
              <option key={item.id} value={item.id}>
                {item.label}
              </option>
            ))}
          </select>

          {/* Search input */}
          <input
            type="text"
            placeholder="检索日志关键词..."
            value={searchQuery}
            onChange={(e) => setSearchQuery(e.target.value)}
            className="input-glass w-48 rounded-lg px-3 py-1 text-ui text-[var(--text)] placeholder:text-[var(--text-muted)] focus:w-64 transition-all"
          />
        </div>

        {/* 右侧操作按钮 */}
        <div className="flex items-center gap-2">
          {/* 实时滚动开关 */}
          <button
            type="button"
            onClick={() => setAutoScroll(!autoScroll)}
            className={`btn-glass px-2.5 py-1 text-sub ${
              autoScroll ? "btn-glass--ok font-semibold" : "btn-glass--muted"
            }`}
            title="新日志进入时自动滚到底部"
          >
            <span className={`inline-block size-2 rounded-full mr-1.5 ${autoScroll ? "bg-[var(--ok)] animate-pulse" : "bg-zinc-500"}`} />
            自动滚动
          </button>

          {/* 导出 */}
          <button
            type="button"
            onClick={handleExport}
            className="btn-glass px-2.5 py-1 text-sub"
          >
            导出日志
          </button>

          {/* 清空 */}
          <button
            type="button"
            onClick={handleClear}
            className="btn-glass btn-glass--danger px-2.5 py-1 text-sub"
          >
            清空
          </button>
        </div>
      </div>

      {/* 日志控制台主视窗 (虚拟滚动) */}
      <div className="css-glass rounded-2xl relative overflow-hidden flex flex-col h-[600px]">
        {/* 控制台顶栏状态说明 */}
        <div className="code-well flex items-center justify-between !border-x-0 !border-t-0 px-4 py-2 text-sub text-[var(--text-muted)] font-mono">
          <div className="flex items-center gap-2">
            <span className="size-2.5 rounded-full bg-[var(--ok)]" />
            <span>实时日志流已接入</span>
          </div>
          <div className="flex items-center gap-3">
            <span>显示 {filteredLogs.length} / {logs.length} 条</span>
            <span>最大留存 3,000 条</span>
          </div>
        </div>

        {/* 滚动容器 */}
        <div
          ref={parentRef}
          className="scroll-thin flex-1 overflow-y-auto p-3 font-mono text-[13px] leading-relaxed select-text"
        >
          {loading ? (
            <div className="flex h-full items-center justify-center text-[var(--text-muted)]">
              加载日志中...
            </div>
          ) : filteredLogs.length === 0 ? (
            <div className="flex h-full items-center justify-center text-[var(--text-muted)]">
              暂无匹配的日志条目
            </div>
          ) : (
            <div
              style={{
                height: `${rowVirtualizer.getTotalSize()}px`,
                width: "100%",
                position: "relative",
              }}
            >
              {rowVirtualizer.getVirtualItems().map((virtualRow) => {
                const item = filteredLogs[virtualRow.index];
                const timeStr = dayjs(item.timestamp).format("YYYY-MM-DD HH:mm:ss");
                const isErr = item.level === "ERROR";
                const isWarn = item.level === "WARN";
                const isInfo = item.level === "INFO";

                const levelBadgeClass = isErr
                  ? "text-[var(--danger)] font-bold"
                  : isWarn
                  ? "text-[var(--warn)] font-medium"
                  : isInfo
                  ? "text-[var(--info)]"
                  : "text-[var(--text-faint)]";

                return (
                  <div
                    key={item.id || virtualRow.index}
                    data-index={virtualRow.index}
                    ref={rowVirtualizer.measureElement}
                    className="absolute left-0 top-0 flex w-full items-start gap-2.5 py-0.5 hover:bg-white/[0.04] rounded px-1.5 transition-colors"
                    style={{
                      transform: `translateY(${virtualRow.start}px)`,
                    }}
                  >
                    <span className="shrink-0 text-[var(--text-faint)] select-none">
                      {timeStr}
                    </span>
                    <span className={`w-12 shrink-0 select-none font-semibold ${levelBadgeClass}`}>
                      {item.level}
                    </span>
                    <span className="shrink-0 rounded bg-white/[0.06] px-1.5 py-0.2 text-[11px] text-[var(--text-muted)] select-none max-w-[150px] truncate">
                      {item.target}
                    </span>
                    <span className={`flex-1 break-all ${isErr ? "text-[var(--danger)]" : isWarn ? "text-[var(--warn)]" : "text-[var(--text)]"}`}>
                      {item.message}
                    </span>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
