"use client";

import { ChapterStrip } from "@/components/chapter-strip";
import type { ChapterTarget, ItemChapter } from "@/lib/api/libraries";
import { shouldPollFrameTask, type FrameTask } from "@/lib/chapter-frame-task";

export function ItemChaptersSection({
  isTv,
  canManageLibraries,
  chapterTarget,
  chapters,
  frameTask,
  chapterError,
  chaptersBusy,
  probing,
  resumeMs,
  onRefreshChapters,
  onRegenerateChapters,
  onProbeMedia,
  onPlayChapter,
}: {
  isTv: boolean;
  canManageLibraries: boolean;
  chapterTarget: ChapterTarget | null;
  chapters: ItemChapter[];
  frameTask: FrameTask | null;
  chapterError: string | null;
  chaptersBusy: boolean;
  probing: boolean;
  resumeMs: number | null;
  onRefreshChapters: () => void;
  onRegenerateChapters: () => void;
  onProbeMedia: () => void;
  onPlayChapter: (chapter: { start_ms: number }) => void;
}) {
  return (
    <div className="mt-6 px-12 max-md:px-4">
      <div className="mb-2.5 flex flex-wrap items-center justify-between gap-2">
        <h3 className="text-sub font-semibold text-white/70">章节与片头片尾</h3>
        {canManageLibraries && (
          <div className="flex flex-wrap items-center gap-1.5 text-micro">
            <button
              type="button"
              disabled={chaptersBusy || !chapterTarget}
              onClick={onRefreshChapters}
              title="复用有效音频并重新识别片头片尾和章节"
              className="rounded-md border border-white/[0.08] bg-white/[0.03] px-2.5 py-1 text-white/60 transition hover:bg-white/[0.08] hover:text-white/90 disabled:opacity-40"
            >
              {chaptersBusy ? "识别中…" : "重新获取片头/章节"}
            </button>
            <button
              type="button"
              disabled={chaptersBusy || !chapterTarget}
              onClick={onRegenerateChapters}
              className="rounded-md border border-white/[0.08] bg-white/[0.03] px-2.5 py-1 text-white/60 transition hover:bg-white/[0.08] hover:text-white/90 disabled:opacity-40"
            >
              {chaptersBusy
                ? "提交中…"
                : frameTask?.state === "partial" || frameTask?.state === "failed"
                  ? "重试失败场景图"
                  : "生成场景图"}
            </button>
            <button
              type="button"
              disabled={probing}
              onClick={onProbeMedia}
              title="清空该条目全部文件的流信息/章节/片头片尾缓存并重新后台探测（视频流、音轨、字幕、声纹）"
              className="rounded-md border border-white/[0.08] bg-white/[0.03] px-2.5 py-1 text-white/60 transition hover:bg-white/[0.08] hover:text-white/90 disabled:opacity-40"
            >
              {probing ? "探测中…" : "重新探测媒体信息"}
            </button>
          </div>
        )}
      </div>
      {frameTask && (
        <p role="status" className="mb-2 text-caption text-white/60">
          {frameTask.state === "queued"
            ? `场景图已排队 ${frameTask.ready}/${frameTask.total}`
            : frameTask.state === "running"
              ? `正在生成场景图 ${frameTask.ready}/${frameTask.total}`
              : frameTask.state === "cancelled"
                ? "场景图任务已取消"
                : frameTask.state === "superseded"
                  ? "场景图任务已更新"
                  : `已完成 ${frameTask.ready}/${frameTask.total}${frameTask.failed ? `，${frameTask.failed} 张失败` : ""}`}
        </p>
      )}
      {chapters.length === 0 && (
        <div className="rounded-xl border border-white/[0.06] bg-white/[0.02] px-4 py-5 text-caption text-white/45">
          暂未识别到片头片尾或章节数据。
          {isTv ? " 请先在上方选集列表选中一集。" : ""} 若为 STRM / 网盘挂载，建议在「设置 → 刮削设置 →
          片头片尾」中配置 TheIntroDB API Key 后点击右上角「重新获取片头/章节」。
        </div>
      )}
      <ChapterStrip
        chapters={chapters.map((chapter, index) => ({
          index,
          start_ms: chapter.start_ms,
          end_ms: chapter.end_ms,
          frame_ms: chapter.start_ms,
          title: chapter.title,
          synthetic: false,
          image_url: chapter.image_url,
        }))}
        pending={chaptersBusy || shouldPollFrameTask(frameTask)}
        resumeMs={resumeMs}
        onPlay={onPlayChapter}
      />
      {chapterError && <p className="mt-2 text-caption text-amber-300/80">{chapterError}</p>}
    </div>
  );
}
