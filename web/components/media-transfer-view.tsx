"use client";

import { useEffect, useMemo, useState, useTransition } from "react";
import {
  deleteLedgerRow,
  listLedger,
  refreshItemMetadata,
  retransferLedgerRow,
  type LedgerItem,
} from "@/lib/api/transfers";
import { formatBytes } from "@/lib/format";
import { useToast } from "@/components/feedback";
import { Modal } from "@/components/modal";
import { CopyButton } from "@/components/copy-button";
import { ContentEmptyState } from "@/components/content-empty-state";
import {
  ArrowRightIcon,
  FilmIcon,
  FolderSyncIcon,
  RefreshIcon,
  SearchIcon,
  SparkIcon,
  TrashIcon,
  TvIcon,
  XIcon,
} from "@/components/icons";

type FilterMode = "all" | "hardlink" | "copy" | "move" | "strm";

export function MediaTransferView() {
  const [items, setItems] = useState<LedgerItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [searchTerm, setSearchTerm] = useState("");
  const [modeFilter, setModeFilter] = useState<FilterMode>("all");
  const [kindFilter, setKindFilter] = useState<"all" | "movie" | "tv">("all");
  const [isPending, startTransition] = useTransition();
  const toast = useToast();

  const loadData = async () => {
    try {
      setLoading(true);
      const data = await listLedger();
      setItems(data);
      setError(null);
    } catch (e) {
      setError((e as Error).message || "加载整理记录失败");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    loadData();
  }, []);

  const [retransferringId, setRetransferringId] = useState<string | null>(null);
  const [refreshingId, setRefreshingId] = useState<string | null>(null);
  const [selectedItem, setSelectedItem] = useState<LedgerItem | null>(null);
  const [deleteMode, setDeleteMode] = useState<"record_only" | "with_file">("record_only");

  const openDeleteDialog = (item: LedgerItem) => {
    setDeleteMode("record_only");
    setSelectedItem(item);
  };

  const closeDeleteDialog = () => {
    setSelectedItem(null);
    setDeleteMode("record_only");
  };

  // 触发重新生成硬链接
  const handleRetransfer = async (item: LedgerItem) => {
    if (!item.source_path) {
      toast.error("该文件未记录下载源路径，无法从源文件直接重构硬链接");
      return;
    }
    try {
      setRetransferringId(item.id);
      await retransferLedgerRow(item.id);
      await loadData();
      toast.success(`《${item.media_title || "媒体文件"}》硬链接已重新生成成功`);
    } catch (e) {
      toast.error("重新生成失败: " + (e as Error).message);
    } finally {
      setRetransferringId(null);
    }
  };

  // 触发重新刮削
  const handleRefreshMetadata = async (item: LedgerItem) => {
    if (!item.library_id) {
      toast.error("无法确定该文件所属的媒体库");
      return;
    }
    try {
      setRefreshingId(item.id);
      await refreshItemMetadata(item.library_id, item.media_id);
      await loadData();
      toast.success(`已为《${item.media_title}》重新触发元数据刮削`);
    } catch (e) {
      toast.error("元数据刷新失败: " + (e as Error).message);
    } finally {
      setRefreshingId(null);
    }
  };

  // 确认删除入库记录
  const handleDeleteConfirm = () => {
    if (!selectedItem) return;
    const deleteFile = deleteMode === "with_file";
    startTransition(async () => {
      try {
        await deleteLedgerRow(selectedItem.id, deleteFile);
        const title = selectedItem.media_title || "记录";
        closeDeleteDialog();
        await loadData();
        toast.success(
          deleteFile ? `已删除《${title}》的入库记录及物理文件` : `已移除《${title}》的入库记录`,
        );
      } catch (e) {
        toast.error("删除失败: " + (e as Error).message);
      }
    });
  };

  // 过滤逻辑
  const filtered = useMemo(() => {
    return items.filter((item) => {
      if (modeFilter !== "all" && item.transfer_mode !== modeFilter) {
        return false;
      }
      if (kindFilter !== "all" && item.media_kind !== kindFilter) {
        return false;
      }
      if (searchTerm.trim()) {
        const term = searchTerm.toLowerCase();
        const matchTitle = item.media_title?.toLowerCase().includes(term);
        const matchPath = item.path?.toLowerCase().includes(term);
        const matchSource = item.source_path?.toLowerCase().includes(term);
        const matchRes = item.resolution?.toLowerCase().includes(term);
        const matchCodec = item.codec?.toLowerCase().includes(term);
        if (!matchTitle && !matchPath && !matchSource && !matchRes && !matchCodec) {
          return false;
        }
      }
      return true;
    });
  }, [items, modeFilter, kindFilter, searchTerm]);

  // 统计数据
  const totalCount = items.length;
  const hardlinkCount = items.filter((i) => i.transfer_mode === "hardlink").length;
  const totalSizeBytes = items.reduce((acc, curr) => acc + (curr.file_size || 0), 0);
  const hardlinkRate = totalCount > 0 ? Math.round((hardlinkCount / totalCount) * 100) : 0;

  return (
    <div className="mx-auto max-w-6xl px-4 py-8 max-md:px-3.5 max-md:py-5">
      {/* 页面顶栏：标题与状态刷新 */}
      <div className="flex flex-col gap-4 sm:flex-row sm:items-center sm:justify-between border-b border-white/[0.08] pb-6">
        <div>
          <div className="flex items-center gap-2.5">
            <div className="flex size-9 items-center justify-center rounded-xl bg-white/[0.06] text-[var(--accent-2)] ring-1 ring-white/[0.1]">
              <FolderSyncIcon className="size-5" />
            </div>
            <h1 className="text-xl sm:text-2xl font-bold tracking-tight text-[var(--text)]">
              媒体整理记录
            </h1>
          </div>
          <p className="mt-1.5 text-xs sm:text-sm text-[var(--text-muted)] leading-relaxed">
            查看所有整理入库的影视物理资产、硬链接零损流转及目录拓扑映射。
          </p>
        </div>

        <div className="flex items-center gap-2.5 self-start sm:self-center">
          <button
            type="button"
            onClick={loadData}
            disabled={loading}
            className="flex items-center gap-1.5 rounded-xl border border-white/[0.08] bg-white/[0.04] px-3.5 py-2 text-xs sm:text-sm font-medium text-[var(--text)] transition hover:border-white/[0.16] hover:bg-white/[0.08] disabled:opacity-50"
          >
            <RefreshIcon className={`size-3.5 ${loading ? "animate-spin text-sky-400" : ""}`} />
            <span>{loading ? "正在同步..." : "刷新记录"}</span>
          </button>
        </div>
      </div>

      {/* 统计指标卡片网格 */}
      <div className="mt-6 grid grid-cols-1 gap-3 sm:grid-cols-3">
        {/* 指标 1: 已整理文件 */}
        <div className="relative overflow-hidden rounded-2xl border border-white/[0.08] bg-white/[0.02] p-4.5 backdrop-blur-md">
          <div className="text-xs font-medium text-[var(--text-muted)]">已整理文件</div>
          <div className="mt-2 flex items-baseline gap-2">
            <span className="text-2xl sm:text-3xl font-bold tracking-tight text-[var(--text)]">
              {totalCount}
            </span>
            <span className="text-xs text-[var(--text-faint)]">项资产</span>
          </div>
          <div className="mt-2.5 flex items-center gap-2 text-micro text-[var(--text-muted)]">
            <span className="inline-flex items-center gap-1 text-[var(--text-faint)]">
              <FilmIcon className="size-3" />
              {items.filter((i) => i.media_kind === "movie").length} 电影
            </span>
            <span className="text-white/20">·</span>
            <span className="inline-flex items-center gap-1 text-[var(--text-faint)]">
              <TvIcon className="size-3" />
              {items.filter((i) => i.media_kind === "tv").length} 剧集
            </span>
          </div>
        </div>

        {/* 指标 2: 存储流转模式 */}
        <div className="relative overflow-hidden rounded-2xl border border-white/[0.08] bg-white/[0.02] p-4.5 backdrop-blur-md">
          <div className="flex items-center justify-between">
            <span className="text-xs font-medium text-[var(--text-muted)]">硬链接占用占比</span>
            <span className="inline-flex items-center gap-1 rounded-full bg-emerald-500/10 px-2 py-0.5 text-[11px] font-medium text-emerald-400">
              零额外磁盘开销
            </span>
          </div>
          <div className="mt-2 flex items-baseline gap-2">
            <span className="text-2xl sm:text-3xl font-bold tracking-tight text-emerald-400">
              {hardlinkCount}
            </span>
            <span className="text-xs text-emerald-400/80">/ {totalCount} ({hardlinkRate}%)</span>
          </div>
          <div className="mt-3 h-1.5 w-full overflow-hidden rounded-full bg-white/[0.06]">
            <div
              className="h-full rounded-full bg-emerald-500 transition-all duration-500"
              style={{ width: `${hardlinkRate}%` }}
            />
          </div>
        </div>

        {/* 指标 3: 管理总体积 */}
        <div className="relative overflow-hidden rounded-2xl border border-white/[0.08] bg-white/[0.02] p-4.5 backdrop-blur-md">
          <div className="text-xs font-medium text-[var(--text-muted)]">总归档文件体积</div>
          <div className="mt-2 flex items-baseline gap-2">
            <span className="text-2xl sm:text-3xl font-bold tracking-tight text-sky-400">
              {formatBytes(totalSizeBytes)}
            </span>
          </div>
          <div className="mt-2.5 text-micro text-[var(--text-muted)]">
            源下载目录与入库目录双向追踪
          </div>
        </div>
      </div>

      {/* 搜索与多维过滤条 */}
      <div className="mt-6 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        {/* 搜索框 */}
        <div className="relative flex-1 sm:max-w-md">
          <SearchIcon className="pointer-events-none absolute left-3.5 top-1/2 size-4 -translate-y-1/2 text-white/40" />
          <input
            type="text"
            value={searchTerm}
            onChange={(e) => setSearchTerm(e.target.value)}
            placeholder="搜索作品名、文件名、源路径或目标路径..."
            className="w-full rounded-xl border border-white/[0.08] bg-white/[0.04] py-2 pl-9 pr-9 text-xs sm:text-sm text-[var(--text)] placeholder-white/30 outline-none transition focus:border-white/20 focus:bg-white/[0.07]"
          />
          {searchTerm && (
            <button
              type="button"
              onClick={() => setSearchTerm("")}
              className="absolute right-2.5 top-1/2 -translate-y-1/2 rounded-md p-1 text-white/40 hover:text-white"
            >
              <XIcon className="size-3.5" />
            </button>
          )}
        </div>

        {/* 过滤 Pills */}
        <div className="flex flex-wrap items-center gap-2">
          {/* 流转方式 Pills */}
          <div className="flex rounded-xl border border-white/[0.08] bg-white/[0.03] p-0.5 text-xs">
            <button
              type="button"
              onClick={() => setModeFilter("all")}
              className={`rounded-lg px-2.5 py-1 font-medium transition ${
                modeFilter === "all"
                  ? "bg-white/15 text-white shadow-sm"
                  : "text-white/60 hover:text-white"
              }`}
            >
              全部流转
            </button>
            <button
              type="button"
              onClick={() => setModeFilter("hardlink")}
              className={`rounded-lg px-2.5 py-1 font-medium transition ${
                modeFilter === "hardlink"
                  ? "bg-emerald-500/20 text-emerald-300 shadow-sm"
                  : "text-white/60 hover:text-white"
              }`}
            >
              硬链接
            </button>
            <button
              type="button"
              onClick={() => setModeFilter("copy")}
              className={`rounded-lg px-2.5 py-1 font-medium transition ${
                modeFilter === "copy"
                  ? "bg-amber-500/20 text-amber-300 shadow-sm"
                  : "text-white/60 hover:text-white"
              }`}
            >
              复制
            </button>
            <button
              type="button"
              onClick={() => setModeFilter("move")}
              className={`rounded-lg px-2.5 py-1 font-medium transition ${
                modeFilter === "move"
                  ? "bg-sky-500/20 text-sky-300 shadow-sm"
                  : "text-white/60 hover:text-white"
              }`}
            >
              移动
            </button>
            <button
              type="button"
              onClick={() => setModeFilter("strm")}
              className={`rounded-lg px-2.5 py-1 font-medium transition ${
                modeFilter === "strm"
                  ? "bg-purple-500/20 text-purple-300 shadow-sm"
                  : "text-white/60 hover:text-white"
              }`}
            >
              STRM 流媒体
            </button>
          </div>

          {/* 类型 Pills */}
          <div className="flex rounded-xl border border-white/[0.08] bg-white/[0.03] p-0.5 text-xs">
            <button
              type="button"
              onClick={() => setKindFilter("all")}
              className={`rounded-lg px-2.5 py-1 font-medium transition ${
                kindFilter === "all"
                  ? "bg-white/15 text-white shadow-sm"
                  : "text-white/60 hover:text-white"
              }`}
            >
              全部类型
            </button>
            <button
              type="button"
              onClick={() => setKindFilter("movie")}
              className={`rounded-lg px-2.5 py-1 font-medium transition ${
                kindFilter === "movie"
                  ? "bg-white/15 text-white shadow-sm"
                  : "text-white/60 hover:text-white"
              }`}
            >
              电影
            </button>
            <button
              type="button"
              onClick={() => setKindFilter("tv")}
              className={`rounded-lg px-2.5 py-1 font-medium transition ${
                kindFilter === "tv"
                  ? "bg-white/15 text-white shadow-sm"
                  : "text-white/60 hover:text-white"
              }`}
            >
              剧集
            </button>
          </div>
        </div>
      </div>

      {/* 结果计数提示 */}
      <div className="mt-3 flex items-center justify-between text-xs text-[var(--text-muted)]">
        <span>
          共找到 <strong className="font-semibold text-[var(--text)]">{filtered.length}</strong> 条入库记录
        </span>
        {(searchTerm || modeFilter !== "all" || kindFilter !== "all") && (
          <button
            type="button"
            onClick={() => {
              setSearchTerm("");
              setModeFilter("all");
              setKindFilter("all");
            }}
            className="text-sky-400 hover:underline"
          >
            重置所有筛选
          </button>
        )}
      </div>

      {/* 错误提示 */}
      {error && (
        <div className="mt-4 rounded-xl border border-red-500/25 bg-red-500/[0.08] p-4 text-xs sm:text-sm text-red-400">
          {error}
        </div>
      )}

      {/* 列表主体 */}
      {filtered.length === 0 && !loading ? (
        <div className="mt-6">
          <ContentEmptyState
            variant="library"
            title={items.length === 0 ? "暂无整理入库记录" : "未找到匹配的文件记录"}
            description={
              items.length === 0
                ? "当下载任务完成、或通过收件箱/自动归档完成影视入库后，所有的流转记录与路径映射会在此呈现。"
                : "请尝试调整搜索关键词或流转模式、类型过滤条件。"
            }
          />
        </div>
      ) : (
        <div className="mt-4 space-y-3">
          {filtered.map((item) => {
            const epTag =
              item.season != null && item.episode != null
                ? `S${String(item.season).padStart(2, "0")}E${String(item.episode).padStart(2, "0")}`
                : null;
            const isHardlink = item.transfer_mode === "hardlink";
            const isMove = item.transfer_mode === "move";
            const isStrm = item.transfer_mode === "strm";
            const isRetransferring = retransferringId === item.id;
            const isRefreshing = refreshingId === item.id;

            return (
              <div
                key={item.id}
                className="group relative rounded-2xl border border-white/[0.08] bg-white/[0.02] p-4 sm:p-5 transition hover:border-white/[0.14] hover:bg-white/[0.035]"
              >
                {/* 顶部行：媒体名称、标签、规格与移动端/桌面端统一排布 */}
                <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
                  <div className="min-w-0 flex-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="text-sm sm:text-base font-semibold text-[var(--text)]">
                        {item.media_title || "未命名作品"}
                      </span>

                      {/* 剧集集数 */}
                      {epTag && (
                        <span className="rounded-md border border-sky-500/30 bg-sky-500/10 px-2 py-0.5 text-xs font-mono font-medium text-sky-300">
                          {epTag}
                        </span>
                      )}

                      {/* 流转模式 Badge */}
                      <span
                        className={`rounded-md px-2 py-0.5 text-xs font-medium ${
                          isStrm
                            ? "bg-purple-500/15 text-purple-300 border border-purple-500/25"
                            : isHardlink
                              ? "bg-emerald-500/15 text-emerald-300 border border-emerald-500/25"
                              : isMove
                                ? "bg-sky-500/15 text-sky-300 border border-sky-500/25"
                                : "bg-amber-500/15 text-amber-300 border border-amber-500/25"
                        }`}
                      >
                        {isStrm ? "STRM (流媒体代理)" : isHardlink ? "硬链接 (Hardlink)" : isMove ? "移动 (Move)" : "复制 (Copy)"}
                      </span>

                      {/* 分辨率 */}
                      {item.resolution && (
                        <span className="rounded-md bg-white/[0.06] px-2 py-0.5 text-xs font-mono text-white/80">
                          {item.resolution}
                        </span>
                      )}

                      {/* 编码 */}
                      {item.codec && (
                        <span className="rounded-md bg-white/[0.06] px-2 py-0.5 text-xs font-mono text-white/70">
                          {item.codec}
                        </span>
                      )}

                      {/* HDR 规格 */}
                      {item.hdr && (
                        <span className="rounded-md bg-purple-500/15 border border-purple-500/25 px-1.5 py-0.5 text-[11px] font-medium text-purple-300">
                          {item.hdr}
                        </span>
                      )}

                      {/* 文件大小 */}
                      {item.file_size > 0 && (
                        <span className="text-xs text-[var(--text-muted)] font-mono">
                          {formatBytes(item.file_size)}
                        </span>
                      )}
                    </div>
                  </div>

                  {/* 右上操作按钮组 (桌面端并排，移动端自适应) */}
                  <div className="flex flex-wrap items-center gap-2 self-start sm:self-center">
                    <button
                      type="button"
                      onClick={() => handleRetransfer(item)}
                      disabled={isRetransferring || !item.source_path}
                      title={
                        item.source_path
                          ? "从原始下载目录重新生成硬链接到目标媒体库"
                          : "缺少源下载文件路径"
                      }
                      className="flex items-center gap-1 rounded-lg border border-sky-500/25 bg-sky-500/10 px-2.5 py-1.5 text-xs font-medium text-sky-300 transition hover:bg-sky-500/20 disabled:cursor-not-allowed disabled:opacity-40"
                    >
                      <RefreshIcon className={`size-3.5 ${isRetransferring ? "animate-spin" : ""}`} />
                      <span>{isRetransferring ? "生成中..." : "重新生成文件"}</span>
                    </button>

                    <button
                      type="button"
                      onClick={() => handleRefreshMetadata(item)}
                      disabled={isRefreshing || !item.library_id}
                      title={
                        item.library_id
                          ? "重新拉取刮削数据与元数据海报"
                          : "无法确定该文件所属的媒体库"
                      }
                      className="flex items-center gap-1 rounded-lg border border-white/[0.08] bg-white/[0.04] px-2.5 py-1.5 text-xs font-medium text-white/80 transition hover:bg-white/[0.08] hover:text-white disabled:opacity-50"
                    >
                      <SparkIcon className={`size-3.5 text-amber-300 ${isRefreshing ? "animate-pulse" : ""}`} />
                      <span>{isRefreshing ? "刮削中..." : "重新刮削"}</span>
                    </button>

                    <button
                      type="button"
                      onClick={() => openDeleteDialog(item)}
                      disabled={isPending}
                      className="flex items-center gap-1 rounded-lg border border-red-500/20 bg-red-500/10 px-2.5 py-1.5 text-xs font-medium text-red-400 transition hover:bg-red-500/20 hover:text-red-300 disabled:opacity-50"
                    >
                      <TrashIcon className="size-3.5" />
                      <span>删除管理</span>
                    </button>
                  </div>
                </div>

                {/* 路径流转呈现区：源文件 -> 入库目标 */}
                <div className="mt-3.5 space-y-2 rounded-xl border border-white/[0.05] bg-black/25 p-3 font-mono text-xs">
                  {/* 源路径 */}
                  <div className="flex items-center justify-between gap-2">
                    <div className="flex min-w-0 items-center gap-2">
                      <span className="shrink-0 rounded bg-white/[0.07] px-1.5 py-0.5 font-sans text-[10px] font-medium text-white/60">
                        下载源
                      </span>
                      <span
                        className="truncate text-white/60 text-[11px]"
                        title={item.source_path || "未记录下载源路径"}
                      >
                        {item.source_path || "未记录下载源路径（如从外部直接拷贝入库）"}
                      </span>
                    </div>
                    {item.source_path && (
                      <CopyButton
                        text={item.source_path}
                        className="shrink-0 text-white/40 hover:text-white p-1"
                      />
                    )}
                  </div>

                  {/* 指向图标 */}
                  <div className="flex items-center gap-2 pl-4 text-white/20">
                    <ArrowRightIcon className="size-3 rotate-90 sm:rotate-0" />
                  </div>

                  {/* 入库路径 */}
                  <div className="flex items-center justify-between gap-2">
                    <div className="flex min-w-0 items-center gap-2">
                      <span className="shrink-0 rounded bg-sky-500/20 px-1.5 py-0.5 font-sans text-[10px] font-semibold text-sky-300">
                        入库位
                      </span>
                      <span
                        className="truncate text-sky-300/90 text-[11px] font-medium"
                        title={item.path}
                      >
                        {item.path}
                      </span>
                    </div>
                    <CopyButton
                      text={item.path}
                      className="shrink-0 text-sky-400/60 hover:text-sky-300 p-1"
                    />
                  </div>
                </div>
              </div>
            );
          })}
        </div>
      )}

      {/* 删除管理弹窗 (采用全站统一规范的 Modal 组件) */}
      {selectedItem && (
        <Modal
          open={Boolean(selectedItem)}
          onClose={closeDeleteDialog}
          label="删除入库资产记录"
          width="md"
        >
          <div className="space-y-4 p-6 max-md:p-5">
            <h2 className="text-title-sm font-bold text-[var(--text)]">删除入库资产记录</h2>
            <p className="text-xs sm:text-sm text-[var(--text-muted)] leading-relaxed">
              您正在移除《
              <strong className="text-[var(--text)] font-semibold">
                {selectedItem.media_title || "未命名作品"}
              </strong>
              》的入库记录。请选择具体的数据与物理文件处理方式：
            </p>

            <div className="space-y-2.5">
              {/* 模式 1: 仅移除记录 */}
              <label
                className={`flex cursor-pointer items-start gap-3 rounded-xl border p-3.5 transition ${
                  deleteMode === "record_only"
                    ? "border-sky-500/40 bg-sky-500/[0.08]"
                    : "border-white/[0.08] bg-white/[0.02] hover:bg-white/[0.04]"
                }`}
              >
                <input
                  type="radio"
                  name="delete_mode"
                  checked={deleteMode === "record_only"}
                  onChange={() => setDeleteMode("record_only")}
                  className="mt-1 size-4 accent-sky-400"
                />
                <div className="min-w-0 flex-1">
                  <div className="text-xs sm:text-sm font-semibold text-[var(--text)]">
                    仅从系统数据库移除记录 (推荐)
                  </div>
                  <div className="mt-1 text-xs text-[var(--text-muted)] leading-relaxed">
                    仅注销系统的入库记录，保留硬盘上的目标物理文件及原始下载数据不变。
                  </div>
                </div>
              </label>

              {/* 模式 2: 连带删除物理文件 */}
              <label
                className={`flex cursor-pointer items-start gap-3 rounded-xl border p-3.5 transition ${
                  deleteMode === "with_file"
                    ? "border-red-500/50 bg-red-500/[0.1]"
                    : "border-red-500/20 bg-red-500/[0.03] hover:bg-red-500/[0.06]"
                }`}
              >
                <input
                  type="radio"
                  name="delete_mode"
                  checked={deleteMode === "with_file"}
                  onChange={() => setDeleteMode("with_file")}
                  className="mt-1 size-4 accent-red-500"
                />
                <div className="min-w-0 flex-1">
                  <div className="text-xs sm:text-sm font-semibold text-red-400">
                    彻底删除磁盘入库物理文件
                  </div>
                  <div className="mt-1 text-xs text-red-300/70 leading-relaxed">
                    将目标路径对应文件物理删除（不可逆，若为硬链接则源下载文件仍保留；若源文件也被删则数据丢失）。
                  </div>
                </div>
              </label>
            </div>

            {/* 底部按钮栏 */}
            <div className="mt-6 flex items-center justify-end gap-3 pt-2">
              <button
                type="button"
                onClick={closeDeleteDialog}
                className="rounded-xl border border-white/[0.08] px-4 py-2 text-xs sm:text-sm font-medium text-white/70 transition hover:bg-white/[0.08] hover:text-white"
              >
                取消
              </button>
              <button
                type="button"
                onClick={handleDeleteConfirm}
                disabled={isPending}
                className="flex items-center gap-1.5 rounded-xl bg-red-600 px-4 py-2 text-xs sm:text-sm font-semibold text-white shadow-lg transition hover:bg-red-500 disabled:opacity-50"
              >
                <TrashIcon className="size-4" />
                <span>{isPending ? "正在处理..." : "确认删除"}</span>
              </button>
            </div>
          </div>
        </Modal>
      )}
    </div>
  );
}
