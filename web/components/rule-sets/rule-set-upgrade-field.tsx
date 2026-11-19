"use client";

import { useState } from "react";

import { RESOLUTION_OPTIONS, UPGRADE_OPTIONS } from "@/lib/rule-set-summary";
import { LADDER_OPTIONS } from "@/lib/upgrade-ladder";

import { Field, ToggleChip } from "./rule-set-form-primitives";

export function RuleSetUpgradeField({
  resolutions,
  mediaSources,
  upgradeSource,
  onUpgradeSource,
  cutoffResolution,
  onCutoffResolution,
  upgradeKeepOld,
  onUpgradeKeepOld,
  upgradeLadder,
  onToggleLadderDim,
  ladderText,
  ladderUnconfigured,
  platformNotLast,
}: {
  resolutions: string[];
  mediaSources: string[];
  upgradeSource: string;
  onUpgradeSource: (value: string) => void;
  cutoffResolution: string;
  onCutoffResolution: (value: string) => void;
  upgradeKeepOld: boolean;
  onUpgradeKeepOld: (value: boolean) => void;
  upgradeLadder: string[];
  onToggleLadderDim: (value: string) => void;
  ladderText: string;
  ladderUnconfigured: (dim: string) => boolean;
  platformNotLast: boolean;
}) {
  // 洗版优先级默认收起：只想"洗到 Remux"的人不该被维度排序打断，
  // 但也不能藏起来——折叠头直接显示当前顺序，需要时一点就开
  const [ladderOpen, setLadderOpen] = useState(false);

  return (
    <Field
      label="洗版"
      hint="收齐后继续追更高版本，直到达到目标档位为止。新版本入库后旧版本将被物理删除，做种中的任务不受影响；不开启 = 下到即止"
    >
      <div className="flex flex-wrap gap-1.5">
        <ToggleChip active={upgradeSource === ""} onClick={() => onUpgradeSource("")}>
          不洗版
        </ToggleChip>
        {UPGRADE_OPTIONS.filter(
          ([value]) => !mediaSources.length || mediaSources.includes(value),
        ).map(([value, label]) => (
          <ToggleChip
            key={value}
            active={upgradeSource === value}
            onClick={() => onUpgradeSource(value)}
          >
            {label}
          </ToggleChip>
        ))}
      </div>
      {upgradeSource !== "" && (
        <div className="mt-2 rounded-xl bg-white/[0.03] px-4 py-2.5">
          <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-1.5">
            <span className="text-sub text-[var(--text-muted)]">目标分辨率</span>
            <div className="flex flex-wrap gap-1.5">
              {(resolutions.length ? resolutions : RESOLUTION_OPTIONS).map((option) => {
                const effective = cutoffResolution || resolutions[0] || "1080p";
                return (
                  <ToggleChip
                    key={option}
                    active={effective === option}
                    onClick={() =>
                      onCutoffResolution(option === (resolutions[0] ?? "") ? "" : option)
                    }
                  >
                    {option}
                  </ToggleChip>
                );
              })}
            </div>
          </div>
          <p className="mt-1.5 text-caption text-[var(--text-faint)]">
            {resolutions.length > 0
              ? "缺省跟随上方分辨率偏好的第一位"
              : "未限定分辨率时缺省洗到 1080p（避免意外进入 4K 的磁盘占用）"}
          </p>
          {/* 旧版本处置（library-file-recycle.md §9）：配置洗版的那一刻
              就是询问旧版去留的合适时机；保留共存服务收藏家场景 */}
          <label className="mt-2.5 flex cursor-pointer items-center justify-between gap-3 border-t border-white/[0.06] pt-2.5">
            <span>
              <span className="block text-sub text-[var(--text-muted)]">
                洗到新版本后保留旧版本
              </span>
              <span className="mt-0.5 block text-caption text-[var(--text-faint)]">
                多版本共存（收藏家模式）；关闭 = 旧版本将被直接物理删除
              </span>
            </span>
            <input
              type="checkbox"
              checked={upgradeKeepOld}
              onChange={(e) => onUpgradeKeepOld(e.target.checked)}
              className="size-4 accent-[var(--accent-2)]"
            />
          </label>

          {/* 洗版优先级：交互与上方分辨率偏好一致——点击依次入列、序号即
              位次。多一位就是多一轮潜在的重复下载，所以缺省只有前两位 */}
          <div className="mt-2.5 border-t border-white/[0.06] pt-2.5">
            <button
              type="button"
              onClick={() => setLadderOpen((v) => !v)}
              className="flex w-full items-center gap-3 text-left"
            >
              <span className="shrink-0 text-sub text-[var(--text-muted)]">洗版优先级</span>
              <span className="min-w-0 flex-1 truncate text-right text-caption text-[var(--text-faint)]">
                {ladderText}
              </span>
              <span
                className={`shrink-0 text-[var(--text-faint)] transition-transform ${
                  ladderOpen ? "rotate-90" : ""
                }`}
              >
                ›
              </span>
            </button>
            {ladderOpen && (
              <>
                <div className="mt-2 flex flex-wrap items-center justify-between gap-x-4 gap-y-1.5">
                  <span className="text-caption text-[var(--text-faint)]">点击依次选择</span>
                  <div className="flex flex-wrap gap-1.5">
                    {LADDER_OPTIONS.map((dim) => {
                      const index = upgradeLadder.indexOf(dim.value);
                      const unconfigured = ladderUnconfigured(dim.value);
                      return (
                        <ToggleChip
                          key={dim.value}
                          active={index >= 0}
                          onClick={() => onToggleLadderDim(dim.value)}
                        >
                          {index >= 0 && upgradeLadder.length > 1 && (
                            <span className="mr-1.5 inline-flex size-4 items-center justify-center rounded-full bg-white/20 text-micro font-semibold">
                              {index + 1}
                            </span>
                          )}
                          {dim.label}
                          {index >= 0 && unconfigured && (
                            <span className="ml-1 text-[var(--text-faint)]">· 未配置</span>
                          )}
                        </ToggleChip>
                      );
                    })}
                  </div>
                </div>
                <p className="mt-1.5 text-caption leading-relaxed text-[var(--text-faint)]">
                  按顺序逐维度比较，先分出高低的那一维说了算。每多一维，就多一轮
                  潜在的重复下载——缺省只比分辨率与片源。标「未配置」的维度会被
                  自动跳过；全被跳过时按缺省的「分辨率 › 片源」比。
                </p>
              </>
            )}
            {platformNotLast && (
              <p className="mt-1.5 rounded-lg border border-amber-400/25 bg-amber-500/10 px-3 py-2 text-caption leading-relaxed text-amber-200">
                很多资源的标题里根本没写平台。把平台排在前面，等于要求「先比平台
                再比别的」——没写平台的资源就全都分不出高低，洗版会大面积停住。
                建议把平台放到最后一位。
              </p>
            )}
          </div>
        </div>
      )}
    </Field>
  );
}
