"use client";

import { useMemo } from "react";

import { ChevronRightIcon } from "@/components/icons";
import { OverflowText } from "@/components/overflow-text";
import type { DownloadTask } from "@/lib/api/downloaders";
import { formatBytes } from "@/lib/format";

import { SpeedStat } from "./task-layout";

/**
 * 刷流做种分组：默认折叠的 <details>，头部常显最值得关心的实时汇总——
 * ↑/↓ 总速度与已上传/已下载总量；展开后逐种子一行（站点 + 名称 + 状态 +
 * 各自的速度与累计上传）。刷流种子没有媒体身份与入库流转，刻意不渲染
 * 生命周期，也不提供删除入口（汰换归引擎管，手动删除去下载器按
 * boost 分类操作）。
 */
export function BoostTaskSection({ tasks }: { tasks: DownloadTask[] }) {
  const sum = (pick: (task: DownloadTask) => number | null) =>
    tasks.reduce((total, task) => total + (pick(task) ?? 0), 0);
  const upSpeed = sum((t) => t.upspeed_bytes);
  const downSpeed = sum((t) => t.dlspeed_bytes);
  const uploaded = sum((t) => t.uploaded_bytes);
  const downloaded = sum((t) => t.completed_bytes);
  // 刷流列表最想回答的是"现在哪些种子在出力"，所以按上行速度倒序，正在上传的
  // 永远浮在最前；速度相同（绝大多数是 0）时用累计上传量兜底，让贡献大的靠前，
  // 也让静默种子之间的顺序在刷新之间保持稳定。
  const sorted = useMemo(
    () =>
      [...tasks].sort(
        (a, b) =>
          (b.upspeed_bytes ?? 0) - (a.upspeed_bytes ?? 0) ||
          (b.uploaded_bytes ?? 0) - (a.uploaded_bytes ?? 0),
      ),
    [tasks],
  );

  return (
    <section className="mt-3" aria-label="刷流做种">
      <details className="group border-b border-white/[0.07] py-3.5 last:border-b-0">
        <summary className="flex cursor-pointer list-none flex-wrap items-center gap-x-3 gap-y-1 rounded-lg py-1 text-ui transition [&::-webkit-details-marker]:hidden">
          <span className="font-semibold text-[var(--accent)]">刷流做种</span>
          <span className="tnum text-caption text-white/60">{tasks.length} 个种子</span>
          {/* 实时汇总：速度是"现在"，总量是"战果"——都放头部，折叠时也一眼可见。
              上传走 --ok 绿（刷流的战果就是上传量），下载走 --info 蓝，数值带淡辉光
              从灰色标签里跳出来；标签本身保持浅灰，让数字成为视觉焦点。
              窄屏这四项塞不进标题行，整块换到第二行（order-2 + w-full）独占一行，
              避免和"展开"挤在一起互相把对方顶出去。第二行再排成 2×2 网格：速度一行、
              总量一行——四项自由换行时"已下载"会随速度值的宽窄时而落单成第三行，
              网格让分组头部的高度和对齐不随数字长短抖动。 */}
          <span className="tnum flex flex-wrap items-center gap-x-3 text-caption text-white/45 max-md:order-2 max-md:grid max-md:w-full max-md:grid-cols-2 max-md:gap-y-1">
            {/* 上下行速度常显：为 0 时退成灰色占位，让"现在没在下载"也是一条信息 */}
            <SpeedStat direction="up" bytesPerSecond={upSpeed} glow placeholder="↑ 0 B/s" />
            <SpeedStat direction="down" bytesPerSecond={downSpeed} glow placeholder="↓ 0 B/s" />
            <span>
              已上传 <span className="font-semibold text-[var(--ok)]">{formatBytes(uploaded)}</span>
            </span>
            <span>
              已下载 <span className="font-semibold text-[var(--info)]">{formatBytes(downloaded)}</span>
            </span>
          </span>
          {/* 展开态文案和箭头必须同进同退，所以包在一个不可换行的组里：上一版两者是
              summary 的兄弟节点，窄屏换行时箭头会被单独甩到下一行成为孤儿。
              窄屏 order-1 让它跟在"N 个种子"后面停在标题行右端。 */}
          <span className="ml-auto flex shrink-0 items-center gap-1 whitespace-nowrap text-caption text-white/30 max-md:order-1">
            <span className="group-open:hidden">展开</span>
            <span className="hidden group-open:inline">收起</span>
            <ChevronRightIcon className="size-4 transition-transform group-open:rotate-90" />
          </span>
        </summary>
        <div className="mt-2 divide-y divide-white/[0.05]">
          {sorted.map((task) => (
            <BoostTaskRow key={task.id} task={task} />
          ))}
        </div>
      </details>
    </section>
  );
}

/**
 * 刷流单行。数字列走固定宽度而不是跟着内容伸缩——上一版把站点、名称、数字全塞进
 * 一个 flex 行，每行数字个数不同就把名称挤成不同长度，180 行下来像没对齐的乱码。
 *
 * 现在的分工：站点与名称占左半区（名称吃掉全部剩余宽度），上行速度 / 累计上传 /
 * 体积三列右对齐且逐行等宽；缺值补破折号，保证列不塌陷。窄屏时数字整块换到第二行，
 * 让名称在小屏也有完整宽度。
 *
 * 下行速度刻意不占固定列：刷流种子绝大多数只做种不下载，给它留一列的结果是整张列表
 * 挂着一排毫无信息量的破折号，还把上行速度挤到要换行。它改成和进度百分比一起跟在
 * 名称后面，只在真的在下载时出现。
 */
function BoostTaskRow({ task }: { task: DownloadTask }) {
  const downloading = task.state === "downloading";
  const percent = task.progress == null ? null : Math.floor(task.progress * 100);
  const showDownloadNote = (downloading && percent != null) || (task.dlspeed_bytes ?? 0) > 0;
  const name = (
    <OverflowText className="min-w-0 flex-1 text-ui text-white/80">
      {task.name || task.info_hash}
    </OverflowText>
  );
  return (
    <div className="flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 py-2">
      <div className="flex w-[4.5rem] shrink-0">
      </div>
      {name}
      {/* 窄屏字号比桌面大一档（--text-caption 11px → 13px），5.5rem 装不下
          "↑ 63.95 KB/s"，速度会被折成两行。窄屏改用按内容需求配比的弹性列
          （速度 : 累计 : 体积 ≈ 9 : 10 : 7），列宽随屏宽缩放而比例不变——既保住逐行
          对齐，也让 360px 的小屏和 430px 的大屏都留有余量；nowrap 再兜一道底 */}
      <div className="tnum grid shrink-0 grid-cols-[5.5rem_7rem_5rem] items-center gap-x-3 whitespace-nowrap text-right text-caption text-white/40 max-md:w-full max-md:grid-cols-[9fr_10fr_7fr] max-md:gap-x-2">
        <SpeedStat direction="up" bytesPerSecond={task.upspeed_bytes} placeholder="—" />
        <span>
          {task.uploaded_bytes != null && task.uploaded_bytes > 0
            ? `累计 ↑${formatBytes(task.uploaded_bytes)}`
            : "—"}
        </span>
        <span>{task.size_bytes != null ? formatBytes(task.size_bytes) : "—"}</span>
      </div>
      {/* 下载中的刷流种子是少数，进度与下行速度不占固定列，另起一行挂在数字列下方：
          既不因为多出两个值把逐行对齐搞乱，也保证 ↑ 永远排在 ↓ 前面，和分组汇总
          头部（↑ 速度 → ↓ 速度 → 已上传 → 已下载）的"上传在前"口径一致 */}
      {showDownloadNote && (
        <div className="flex w-full items-center justify-end gap-x-2 text-caption text-white/35">
          {downloading && percent != null && <span className="tnum">{percent}%</span>}
          <SpeedStat direction="down" bytesPerSecond={task.dlspeed_bytes} />
        </div>
      )}
    </div>
  );
}
