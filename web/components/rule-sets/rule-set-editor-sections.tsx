"use client";

import { useState, type Dispatch, type SetStateAction } from "react";

import type { Site } from "@/lib/api/sites";
import { PLATFORM_OPTIONS, platformLabel } from "@/lib/platforms";
import {
  AUDIO_LANG_OPTIONS,
  HDR_OPTIONS,
  hdrChipText,
  SUB_LANG_OPTIONS,
} from "@/lib/rule-set-summary";
import { CODEC_FAMILIES } from "@/lib/upgrade-ladder";

import {
  CheckRow,
  Field,
  NumberInput,
  Section,
  ToggleChip,
  TriChip,
} from "./rule-set-form-primitives";

export function PreferredSitesField({
  preferredSites,
  configuredSites,
  onPreferredSites,
}: {
  preferredSites: string[];
  configuredSites: Site[];
  onPreferredSites: Dispatch<SetStateAction<string[]>>;
}) {
  const [draggingPreferredSite, setDraggingPreferredSite] = useState<number | null>(null);

  const reorderPreferredSite = (sourceIndex: number, targetIndex: number) =>
    onPreferredSites((current) => {
      if (
        targetIndex < 0 ||
        targetIndex >= current.length ||
        sourceIndex < 0 ||
        sourceIndex >= current.length ||
        sourceIndex === targetIndex
      ) {
        return current;
      }
      const next = [...current];
      const [moved] = next.splice(sourceIndex, 1);
      next.splice(targetIndex, 0, moved);
      return next;
    });

  return (
    <Field
      label="首选站点"
      hint="首选站点有符合质量条件的资源时优先下载；拖动站点行可调整顺序，未命中时仍可回退其他站点"
    >
      <div className="space-y-2">
        {preferredSites.map((siteId, index) => {
          const site = configuredSites.find((candidate) => candidate.id === siteId);
          return (
            <div
              key={siteId}
              draggable
              onDragStart={(event) => {
                setDraggingPreferredSite(index);
                event.dataTransfer.effectAllowed = "move";
                event.dataTransfer.setData("text/plain", String(index));
              }}
              onDragOver={(event) => {
                event.preventDefault();
                event.dataTransfer.dropEffect = "move";
              }}
              onDrop={(event) => {
                event.preventDefault();
                const sourceIndex = Number(event.dataTransfer.getData("text/plain"));
                if (Number.isInteger(sourceIndex)) reorderPreferredSite(sourceIndex, index);
                setDraggingPreferredSite(null);
              }}
              onDragEnd={() => setDraggingPreferredSite(null)}
              className={`flex cursor-grab items-center gap-2 rounded-lg border border-white/10 px-3 py-2 text-sm text-white active:cursor-grabbing ${draggingPreferredSite === index ? "opacity-50" : ""}`}
            >
              <span aria-hidden="true" className="text-[var(--text-muted)]">⠿</span>
              <span className="w-6 text-[var(--text-muted)]">{index + 1}.</span>
              <span className="flex-1">{site?.name ?? siteId}</span>
              <button
                type="button"
                onClick={() =>
                  onPreferredSites((current) => current.filter((id) => id !== siteId))
                }
              >
                移除
              </button>
            </div>
          );
        })}
        <select
          className="w-full rounded-lg border border-[var(--line)] bg-[var(--surface-inset)] px-3 py-2 text-[var(--text)]"
          value=""
          onChange={(event) => {
            const value = event.target.value;
            if (value && !preferredSites.includes(value)) {
              onPreferredSites((current) => [...current, value]);
            }
          }}
        >
          <option value="">添加已配置站点…</option>
          {configuredSites
            .filter((site) => site.enabled && !preferredSites.includes(site.id))
            .map((site) => (
              <option key={site.id} value={site.id}>
                {site.name}
              </option>
            ))}
        </select>
      </div>
    </Field>
  );
}

export function QualitySourceSection({
  codecFamilies,
  codecExtras,
  onToggleCodecFamily,
  platformsAllow,
  platformsBlock,
  platformExtras,
  onCyclePlatform,
  hdrLevels,
  hdrBlock,
  onCycleHdr,
  groupsAllow,
  onGroupsAllow,
  groupsBlock,
  onGroupsBlock,
}: {
  codecFamilies: Set<string>;
  codecExtras: string[];
  onToggleCodecFamily: (label: string) => void;
  platformsAllow: string[];
  platformsBlock: string[];
  platformExtras: { allow: string[]; block: string[] };
  onCyclePlatform: (value: string) => void;
  hdrLevels: string[];
  hdrBlock: string[];
  onCycleHdr: (value: string) => void;
  groupsAllow: string;
  onGroupsAllow: (value: string) => void;
  groupsBlock: string;
  onGroupsBlock: (value: string) => void;
}) {
  const platformState = (value: string): "off" | "include" | "exclude" =>
    platformsAllow.includes(value)
      ? "include"
      : platformsBlock.includes(value)
        ? "exclude"
        : "off";

  const hdrState = (value: string): "off" | "include" | "exclude" =>
    hdrLevels.includes(value) ? "include" : hdrBlock.includes(value) ? "exclude" : "off";

  const qualitySummary =
    [
      codecFamilies.size ? [...codecFamilies].join("/") : "",
      platformsAllow.length ? platformsAllow.map(platformLabel).join("/") : "",
      platformsBlock.length ? `排除 ${platformsBlock.map(platformLabel).join("/")}` : "",
      hdrLevels.length ? hdrChipText(hdrLevels) : "",
      hdrBlock.length ? `排除 ${hdrBlock.join("/")}` : "",
      groupsAllow.trim() ? `组 ${groupsAllow.trim()}` : "",
      groupsBlock.trim() ? `排除组 ${groupsBlock.trim()}` : "",
    ]
      .filter(Boolean)
      .join(" · ") || "未设置";

  return (
    <Section title="画质与来源" summary={qualitySummary}>
      <Field label="视频编码" hint="按家族选择，等价写法（如 x265 / HEVC）一并计入；不选 = 不限">
        <div className="flex flex-wrap gap-1.5">
          {CODEC_FAMILIES.map((family) => (
            <ToggleChip
              key={family.label}
              active={codecFamilies.has(family.label)}
              onClick={() => onToggleCodecFamily(family.label)}
            >
              {family.label}
            </ToggleChip>
          ))}
          {codecExtras.length > 0 && (
            <span className="self-center text-caption text-[var(--text-faint)]">
              另有自定义值：{codecExtras.join("、")}（保留）
            </span>
          )}
        </div>
      </Field>

      <Field
        label="流媒体平台"
        hint="点一下 = 只要，再点 = 排除，第三下取消。设了「只要」之后，识别不出平台的资源会被排除（与分辨率/编码口径一致）"
      >
        <div className="flex flex-wrap gap-1.5">
          {PLATFORM_OPTIONS.map((id) => (
            <TriChip
              key={id}
              state={platformState(id)}
              onClick={() => onCyclePlatform(id)}
            >
              {platformLabel(id)}
            </TriChip>
          ))}
        </div>
        {(platformExtras.allow.length > 0 || platformExtras.block.length > 0) && (
          <p className="mt-1.5 text-caption text-[var(--text-faint)]">
            另有自定义值：
            {[...platformExtras.allow, ...platformExtras.block].map(platformLabel).join("、")}
            （保留）
          </p>
        )}
      </Field>

      <Field
        label="HDR"
        hint='点一下 = 只要，再点 = 排除，第三下取消。"SDR" 指资源没标注任何 HDR 格式；都不选 = 不限'
      >
        <div className="flex flex-wrap gap-1.5">
          {HDR_OPTIONS.map((value) => (
            <TriChip key={value} state={hdrState(value)} onClick={() => onCycleHdr(value)}>
              {value}
            </TriChip>
          ))}
        </div>
      </Field>

      <Field
        label="制作组白名单"
        hint="只接受这些制作组的资源，逗号或空格分隔（如 FRDS, WiKi）；留空 = 不限"
      >
        <input
          type="text"
          value={groupsAllow}
          onChange={(e) => onGroupsAllow(e.target.value)}
          placeholder="留空不限"
          className="w-full rounded-xl border border-white/[0.08] bg-white/[0.04] px-3.5 py-2.5 text-ui text-[var(--text)] outline-none focus:border-[var(--accent)]/60"
        />
      </Field>
      <Field label="制作组黑名单" hint="这些制作组的资源一律不要">
        <input
          type="text"
          value={groupsBlock}
          onChange={(e) => onGroupsBlock(e.target.value)}
          placeholder="留空不启用"
          className="w-full rounded-xl border border-white/[0.08] bg-white/[0.04] px-3.5 py-2.5 text-ui text-[var(--text)] outline-none focus:border-[var(--accent)]/60"
        />
      </Field>
    </Section>
  );
}

export function DownloadLimitsSection({
  subLangs,
  onSubLangs,
  audioLangs,
  onAudioLangs,
  freeOnly,
  onFreeOnly,
  excludeHr,
  onExcludeHr,
  hrStrict,
  onHrStrict,
  minSeeders,
  onMinSeeders,
  sizeMin,
  onSizeMin,
  sizeMax,
  onSizeMax,
}: {
  subLangs: string[];
  onSubLangs: Dispatch<SetStateAction<string[]>>;
  audioLangs: string[];
  onAudioLangs: Dispatch<SetStateAction<string[]>>;
  freeOnly: boolean;
  onFreeOnly: (value: boolean) => void;
  excludeHr: boolean;
  onExcludeHr: (value: boolean) => void;
  hrStrict: boolean;
  onHrStrict: (value: boolean) => void;
  minSeeders: string;
  onMinSeeders: (value: string) => void;
  sizeMin: string;
  onSizeMin: (value: string) => void;
  sizeMax: string;
  onSizeMax: (value: string) => void;
}) {
  const labelOf = (options: readonly (readonly [string, string])[], values: string[]) =>
    values.map((v) => options.find(([code]) => code === v)?.[1] ?? v).join("/");

  const limitSummary =
    [
      subLangs.length ? `字幕 ${labelOf(SUB_LANG_OPTIONS, subLangs)}` : "",
      audioLangs.length ? `音轨 ${labelOf(AUDIO_LANG_OPTIONS, audioLangs)}` : "",
      freeOnly ? "仅免费" : "",
      excludeHr ? "排除 H&R" : "",
      minSeeders.trim() ? `做种 ≥ ${minSeeders.trim()}` : "",
      sizeMin.trim() || sizeMax.trim()
        ? `体积 ${sizeMin.trim() || "0"}–${sizeMax.trim() || "∞"}MB`
        : "",
    ]
      .filter(Boolean)
      .join(" · ") || "未设置";

  return (
    <Section title="下载与限制" summary={limitSummary}>
      <Field
        label="字幕语言"
        hint="任一命中即通过；按种子标题声明判断——未声明字幕的资源会被排除，不选 = 不限"
      >
        <div className="flex flex-wrap gap-1.5">
          {SUB_LANG_OPTIONS.map(([value, label]) => (
            <ToggleChip
              key={value}
              active={subLangs.includes(value)}
              onClick={() =>
                onSubLangs((prev) =>
                  prev.includes(value) ? prev.filter((v) => v !== value) : [...prev, value],
                )
              }
            >
              {label}
            </ToggleChip>
          ))}
        </div>
      </Field>

      <Field
        label="音轨语言"
        hint="任一命中即通过；按种子标题声明判断——未声明音轨的资源会被排除，不选 = 不限"
      >
        <div className="flex flex-wrap gap-1.5">
          {AUDIO_LANG_OPTIONS.map(([value, label]) => (
            <ToggleChip
              key={value}
              active={audioLangs.includes(value)}
              onClick={() =>
                onAudioLangs((prev) =>
                  prev.includes(value) ? prev.filter((v) => v !== value) : [...prev, value],
                )
              }
            >
              {label}
            </ToggleChip>
          ))}
        </div>
      </Field>

      <div className="grid grid-cols-2 gap-3 max-md:grid-cols-1">
        <CheckRow
          label="只要免费资源"
          hint="促销状态未知的按非免费处理"
          checked={freeOnly}
          onChange={onFreeOnly}
        />
        <CheckRow
          label="排除 H&R 考核种子"
          hint="有做种考核要求的资源不下载"
          checked={excludeHr}
          onChange={onExcludeHr}
        />
      </div>
      {excludeHr && (
        <div className="-mt-2 rounded-xl bg-white/[0.03] px-4 py-2.5">
          <label className="flex cursor-pointer items-center justify-between">
            <span className="text-sub text-[var(--text-muted)]">
              站点未提供 H&R 信息时，保守视作有考核而排除
            </span>
            <input
              type="checkbox"
              checked={hrStrict}
              onChange={(e) => onHrStrict(e.target.checked)}
              className="size-4 accent-[var(--accent-2)]"
            />
          </label>
        </div>
      )}

      <div className="grid grid-cols-3 gap-3 max-md:grid-cols-1">
        <Field label="做种数下限">
          <NumberInput value={minSeeders} onChange={onMinSeeders} placeholder="不限" />
        </Field>
        <Field label="单集体积下限 (MB)">
          <NumberInput value={sizeMin} onChange={onSizeMin} placeholder="不限" />
        </Field>
        <Field label="单集体积上限 (MB)">
          <NumberInput value={sizeMax} onChange={onSizeMax} placeholder="不限" />
        </Field>
      </div>
      <p className="-mt-3 text-caption leading-relaxed text-[var(--text-faint)]">
        体积按「每集均摊」评估：整季包用总体积 ÷ 集数比较，整季合集不会被单集上限误杀。
      </p>
    </Section>
  );
}
