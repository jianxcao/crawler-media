"use client";

import { useState } from "react";

import type { UpgradeLadderPreview } from "@/lib/upgrade-ladder";

/**
 * 洗版阶梯预览：把"洗到哪一档"从一句话摊成一条**看得见的路**。
 *
 * 一句"洗到 1080p 蓝光 · DV"读不出洗版真正的两个后果，而它们恰恰是用户
 * 最该在按下保存之前知道的：
 *
 * - **终点之下还有多少档**：每上一档就是一次重复下载，多加一维阶梯的代价
 *   在这里才具体；
 * - **哪些档已经算达标**：分辨率排在第一位时，任意 2160p 资源（哪怕 WEBRip）
 *   都优于 1080p 蓝光，洗版会直接停在那儿——所以"更高档同样算达标"必须写在
 *   展开后的终点卡上，而不是留给用户自己推。
 *
 * **默认折叠**：完整阶梯要占十来行，常驻会把配置本身挤出视野；收起态只留
 * 一行终点（这段里唯一值得常驻的信息），其余进按钮后面。用点击而不是 hover
 * ——这个弹窗在手机上同样要用，触屏没有 hover；且这里是要读几行的解释，
 * 浮层里读长内容不如就地展开。折叠的交互形态与同一弹窗里的「洗版优先级」
 * 「画质与来源」保持一致，不新增第三种。
 *
 * 展开后版式即语义：终点在最上方并高亮（"最好品质"是这条路的目的地），下方
 * 按档位降序排开、每行一个 ↑ 指回终点——从下往上读就是资源逐级被替换的过程。
 */
export function UpgradeLadderPreviewView({
  preview,
  keepOld,
}: {
  preview: UpgradeLadderPreview;
  keepOld: boolean;
}) {
  const [open, setOpen] = useState(false);
  return (
    <div className="mt-2.5 border-t border-white/[0.06] pt-2.5">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        className="flex w-full items-center gap-3 text-left"
      >
        <span className="min-w-0 flex-1 text-sub text-[var(--text-muted)]">
          再一级级洗到{" "}
          <span className="font-semibold text-white">{preview.target}</span>
        </span>
        <span className="shrink-0 text-caption text-[var(--text-faint)]">
          {open ? "收起" : "看会经过哪些档"}
        </span>
        <span
          className={`shrink-0 text-[var(--text-faint)] transition-transform ${
            open ? "rotate-90" : ""
          }`}
        >
          ›
        </span>
      </button>

      {open && (
        <div className="mt-2.5">
          <p className="text-caption leading-relaxed text-[var(--text-faint)]">
            {`按「${preview.dimensions.join(" › ")}」逐维比较，先分出高低的那一维说了算。`}
          </p>

          <div className="mt-2 rounded-lg border border-[var(--accent-2)]/45 bg-[var(--accent-2)]/[0.14] px-3 py-2">
            <div className="flex flex-wrap items-baseline gap-x-2 gap-y-1">
              <span className="rounded-full bg-[var(--accent-2)]/25 px-2 py-0.5 text-micro font-semibold text-white">
                终点
              </span>
              <span className="text-sub font-semibold text-white">{preview.target}</span>
            </div>
            <p className="mt-1 text-caption leading-relaxed text-[var(--text-faint)]">
              到手即停，不再洗版
              {preview.ceiling
                ? `；比它更高的档（最高 ${preview.ceiling}）同样算达标，也会停`
                : ""}
              。{keepOld ? "旧版本保留共存。" : "旧版本将被直接物理删除。"}
            </p>
          </div>

          {preview.below.length > 0 ? (
            <>
              <p className="mt-2.5 text-caption leading-relaxed text-[var(--text-faint)]">
                {"到终点之前，先到手的可能是下面任意一档；之后每遇到更高的一档，就再下一次、换掉旧的："}
              </p>
              <ol className="mt-1.5 space-y-1">
                {preview.below.map((label) => (
                  <li
                    key={label}
                    className="flex items-center gap-2 text-sub text-[var(--text-muted)]"
                  >
                    <span className="text-[var(--text-faint)]">↑</span>
                    <span>{label}</span>
                  </li>
                ))}
                {preview.moreBelow > 0 && (
                  <li className="flex items-center gap-2 text-caption text-[var(--text-faint)]">
                    <span>↑</span>
                    <span>更低的 {preview.moreBelow} 档同理</span>
                  </li>
                )}
              </ol>
            </>
          ) : (
            <p className="mt-2 text-caption text-[var(--text-faint)]">
              终点已是最低档，不会有过渡版本。
            </p>
          )}
        </div>
      )}
    </div>
  );
}
