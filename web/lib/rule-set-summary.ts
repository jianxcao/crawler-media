import type { RuleSetAtom } from "@/lib/api/subscriptions";

import { platformLabel } from "./platforms.ts";
import { ruleAtomValue, ruleAtomValues } from "./rule-set-edit.ts";
import {
  CODEC_FAMILIES,
  DEFAULT_LADDER,
  MEDIA_SOURCE_OPTIONS,
} from "./upgrade-ladder.ts";

/** 分辨率可选项（词表归一值里的常用档；其余档位罕见，需要时再扩）。 */
export const RESOLUTION_OPTIONS = ["2160p", "1080p", "720p"];

// 语言值与后端 lang_decl 的 BCP 47 输出对齐；任一命中即过。
// "zh" 前缀匹配简/繁/未标简繁的一切中文声明——日常"要中文字幕"选它就够
export const SUB_LANG_OPTIONS: [string, string][] = [
  ["zh", "中文（不限简繁）"],
  ["zh-Hans", "简体中文"],
  ["zh-Hant", "繁体中文"],
  ["en", "英文"],
];

export const AUDIO_LANG_OPTIONS: [string, string][] = [
  ["cmn", "国语"],
  ["yue", "粤语"],
  ["ja", "日语"],
  ["en", "英语"],
];

/**
 * 洗版目标片源档的三个选项就是全部预设
 * ——没有需要照抄的社区配置。目标分辨率缺省取分辨率偏好的第一位。
 */
export const UPGRADE_OPTIONS: [string, string][] = [
  ["web-dl", "洗到 WEB-DL"],
  ["blu-ray", "洗到蓝光"],
  ["remux", "洗到 Remux"],
];

export const UPGRADE_SOURCE_LABEL: Record<string, string> = {
  "web-dl": "WEB-DL",
  "blu-ray": "蓝光",
  remux: "Remux",
};

/** 片源档规范值 → 展示名；未知值原样返回（与 platformLabel 同一取向）。 */
export const mediaSourceLabel = (value: string): string =>
  MEDIA_SOURCE_OPTIONS.find((option) => option.value === value)?.label ?? value;

/**
 * HDR 值域（顺序即编辑器里的芯片顺序）。"SDR" 是哨兵值 = 资源未标注任何
 * HDR 格式——命名惯例里 HDR 属"缺席即否定"的强标记。
 */
export const HDR_OPTIONS = ["DV", "HDR10+", "HDR10", "HLG", "SDR"];

/**
 * HDR 白名单 → 摘要文案。整族与纯 SDR 这两种最常见的配置（也是旧 hdr 三态
 * 换算出来的形态）说人话，避免清单里出现 "HDR DV/HDR10+/HDR10/HLG" 这种
 * 又长又没信息量的芯片。
 */
export function hdrChipText(levels: string[]): string {
  const family = HDR_OPTIONS.filter((v) => v !== "SDR");
  if (levels.length === family.length && family.every((v) => levels.includes(v))) {
    return "必须 HDR";
  }
  if (levels.length === 1 && levels[0] === "SDR") return "排除 HDR";
  return `HDR ${levels.join("/")}`;
}

export const sameLadder = (a: string[], b: string[]) =>
  a.length === b.length && a.every((v, i) => v === b[i]);

/** 洗版目标的人话标签（摘要芯片 / 订阅详情页规则组 fact 复用）。 */
export function upgradeTargetLabel(atoms: RuleSetAtom[]): string | null {
  const target = ruleAtomValue(atoms, "wash_target")?.split(" ");
  if (!target?.[1]) return null;
  const resolution = target[0] || ruleAtomValues(atoms, "resolution")[0] || "1080p";
  const parts = [`${resolution} ${UPGRADE_SOURCE_LABEL[target[1]] ?? target[1]}`];
  // 与后端 decision.upgrade_target_label 同一口径：只渲染"进了阶梯且配了偏好"
  // 的维度（偏好为空的位后端会自动跳过）
  const ladder = ruleAtomValue(atoms, "upgrade_ladder")?.split(",") ?? DEFAULT_LADDER;
  if (ladder.includes("hdr") && ruleAtomValues(atoms, "hdr").length) {
    parts.push(ruleAtomValues(atoms, "hdr")[0]);
  }
  if (ladder.includes("video_codec") && ruleAtomValues(atoms, "video_codec").length) {
    parts.push(ruleAtomValues(atoms, "video_codec")[0]);
  }
  if (ladder.includes("platform") && ruleAtomValues(atoms, "platform").length) {
    parts.push(platformLabel(ruleAtomValues(atoms, "platform")[0]));
  }
  return parts.join(" · ");
}

/**
 * atoms → 人话芯片列表（清单摘要 / 订阅弹窗与详情页复用；空数组=全不限）。
 * `withoutUpgrade`：编辑器底部把洗版单独渲染成阶梯，那里不再要这枚芯片。
 */
export function ruleSetSummary(
  atoms: RuleSetAtom[],
  {
    withoutUpgrade = false,
    keepOldVersions = false,
  }: { withoutUpgrade?: boolean; keepOldVersions?: boolean } = {},
): string[] {
  const chips: string[] = [];
  if (ruleAtomValues(atoms, "site").length) {
    chips.push(`首选站点 ${ruleAtomValues(atoms, "site").join(" > ")}`);
  }
  if (ruleAtomValues(atoms, "resolution").length) {
    chips.push(ruleAtomValues(atoms, "resolution").join(" > "));
  }
  if (ruleAtomValues(atoms, "source").length) {
    chips.push(ruleAtomValues(atoms, "source").map(mediaSourceLabel).join(" > "));
  }
  const upgradeTarget = withoutUpgrade ? null : upgradeTargetLabel(atoms);
  if (upgradeTarget) {
    chips.push(`洗到 ${upgradeTarget}${keepOldVersions ? "（保留旧版）" : ""}`);
  }
  if (ruleAtomValues(atoms, "video_codec").length) {
    const rest = new Set(ruleAtomValues(atoms, "video_codec"));
    const labels: string[] = [];
    for (const family of CODEC_FAMILIES) {
      if (family.values.some((v) => rest.has(v))) {
        labels.push(family.label);
        family.values.forEach((v) => rest.delete(v));
      }
    }
    labels.push(...rest);
    chips.push(labels.join("/"));
  }
  if (ruleAtomValues(atoms, "platform").length) {
    chips.push(`平台: ${ruleAtomValues(atoms, "platform").map(platformLabel).join("/")}`);
  }
  if (ruleAtomValues(atoms, "platform", true).length) {
    chips.push(`排除平台: ${ruleAtomValues(atoms, "platform", true).map(platformLabel).join("/")}`);
  }
  const hdrLevels = ruleAtomValues(atoms, "hdr");
  const hdrBlock = ruleAtomValues(atoms, "hdr", true);
  if (hdrLevels.length) chips.push(hdrChipText(hdrLevels));
  if (hdrBlock.length) chips.push(`排除 ${hdrBlock.join("/")}`);
  if (ruleAtomValues(atoms, "subtitle_language").length) {
    chips.push(
      `字幕: ${ruleAtomValues(atoms, "subtitle_language").map((v) => SUB_LANG_OPTIONS.find(([code]) => code === v)?.[1] ?? v).join("/")}`,
    );
  }
  if (ruleAtomValues(atoms, "audio_language").length) {
    chips.push(
      `音轨: ${ruleAtomValues(atoms, "audio_language").map((v) => AUDIO_LANG_OPTIONS.find(([code]) => code === v)?.[1] ?? v).join("/")}`,
    );
  }
  if (atoms.some((atom) => atom.kind === "free" && !atom.exclude)) chips.push("仅免费");
  const seeders = ruleAtomValue(atoms, "min_seeders");
  if (seeders != null) chips.push(`做种 ≥ ${seeders}`);
  const size = ruleAtomValue(atoms, "size");
  if (size) {
    const [min, max] = size.split("-");
    chips.push(
      min && min !== "0" && max
        ? `单集 ${min}–${max}MB`
        : max
          ? `单集 ≤ ${max}MB`
          : `单集 ≥ ${min}MB`,
    );
  }
  const hr = atoms.find((atom) => atom.kind === "hr" && atom.exclude);
  if (hr?.kind === "hr") {
    chips.push(hr.unknown === "exclude" ? "排除 H&R（未知也排）" : "排除 H&R");
  }
  if (ruleAtomValues(atoms, "team").length) {
    chips.push(`制作组白名单 ${ruleAtomValues(atoms, "team").length} 个`);
  }
  if (ruleAtomValues(atoms, "team", true).length) {
    chips.push(`制作组黑名单 ${ruleAtomValues(atoms, "team", true).length} 个`);
  }
  return chips;
}

export interface RuleSetDraftInput {
  resolutions: string[];
  mediaSources: string[];
  codecFamilies: Set<string>;
  codecExtras: string[];
  platformsAllow: string[];
  platformsBlock: string[];
  platformExtras: { allow: string[]; block: string[] };
  groupsAllow: string;
  groupsBlock: string;
  hdrLevels: string[];
  hdrBlock: string[];
  subLangs: string[];
  audioLangs: string[];
  preferredSites: string[];
  freeOnly: boolean;
  excludeHr: boolean;
  hrStrict: boolean;
  minSeeders: string;
  sizeMin: string;
  sizeMax: string;
  upgradeSource: string;
  cutoffResolution: string;
  upgradeLadder: string[];
}

/**
 * 表单状态 → atoms 的**唯一**构造点：保存与底部"这条规则会这样筛选"共用它，
 * 保证用户看到的摘要就是将要存下去的东西。校验错误随返回值给出。
 */
export function buildRuleSetDraft(
  input: RuleSetDraftInput,
): { atoms: RuleSetAtom[]; error: string | null } {
  const next: RuleSetAtom[] = [];
  const add = (kind: Exclude<RuleSetAtom["kind"], "hr">, values: string[], exclude = false) => {
    values.forEach((value, index) =>
      next.push({ kind, value, exclude, priority: (values.length - index) * 50 }),
    );
  };
  const parseGroups = (text: string) =>
    text
      .split(/[,，、\s]+/)
      .map((value) => value.trim())
      .filter(Boolean);
  const number = (text: string) => {
    const value = Number.parseInt(text, 10);
    return value > 0 ? value : undefined;
  };
  add("resolution", input.resolutions);
  add("source", input.mediaSources);
  add("video_codec", [
    ...CODEC_FAMILIES.filter((family) => input.codecFamilies.has(family.label)).flatMap(
      (family) => family.values,
    ),
    ...input.codecExtras,
  ]);
  add("platform", [...input.platformsAllow, ...input.platformExtras.allow]);
  add("platform", [...input.platformsBlock, ...input.platformExtras.block], true);
  add("team", parseGroups(input.groupsAllow));
  add("team", parseGroups(input.groupsBlock), true);
  add("hdr", input.hdrLevels);
  add("hdr", input.hdrBlock, true);
  add("subtitle_language", input.subLangs);
  add("audio_language", input.audioLangs);
  add("site", input.preferredSites);
  if (input.freeOnly) next.push({ kind: "free", value: null, priority: 50 });
  if (input.excludeHr) {
    next.push({
      kind: "hr",
      value: null,
      priority: 100,
      exclude: true,
      unknown: input.hrStrict ? "exclude" : "allow",
    });
  }
  const seeders = number(input.minSeeders);
  if (seeders) add("min_seeders", [String(seeders)]);
  const min = number(input.sizeMin);
  const max = number(input.sizeMax);
  if (min || max) add("size", [`${min ?? 0}-${max ?? ""}`]);
  if (min && max && min > max) return { atoms: next, error: "体积下限不能大于上限" };
  if (input.upgradeSource) {
    if (input.mediaSources.length && !input.mediaSources.includes(input.upgradeSource)) {
      return { atoms: next, error: "洗版目标片源必须在允许的片源范围内" };
    }
    if (
      input.cutoffResolution &&
      input.resolutions.length &&
      !input.resolutions.includes(input.cutoffResolution)
    ) {
      return { atoms: next, error: "洗版目标分辨率必须在允许的分辨率范围内" };
    }
    add("wash_target", [
      `${input.cutoffResolution || input.resolutions[0] || "1080p"} ${input.upgradeSource}`,
    ]);
    if (!sameLadder(input.upgradeLadder, DEFAULT_LADDER)) {
      add("upgrade_ladder", [input.upgradeLadder.join(",")]);
    }
  }
  return { atoms: next, error: null };
}
