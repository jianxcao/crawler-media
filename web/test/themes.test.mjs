import assert from "node:assert/strict";
import { test } from "node:test";

import { sidebarGlass } from "../lib/glass.ts";
import { DEFAULT_THEME_ID, normalizeThemeId, THEMES, themeMeta } from "../lib/themes.ts";

test("注册表提供银玻璃、液态玻璃、纸感玻璃与 Netflix 四套主题", () => {
  const ids = THEMES.map((theme) => theme.id);
  assert.deepEqual(ids, ["silver", "liquid", "paper", "netflix"]);
  // Netflix 是结构级主题（换外壳），其余三套玻璃主题共用控制台外壳
  assert.equal(THEMES.find((t) => t.id === "silver")?.structural, false);
  assert.equal(THEMES.find((t) => t.id === "liquid")?.structural, false);
  assert.equal(THEMES.find((t) => t.id === "paper")?.structural, false);
  assert.equal(THEMES.find((t) => t.id === "netflix")?.structural, true);
});

test("normalizeThemeId：未知值 / 非字符串 / undefined 一律兜底为默认主题", () => {
  assert.equal(normalizeThemeId("netflix"), "netflix");
  assert.equal(normalizeThemeId("silver"), "silver");
  assert.equal(normalizeThemeId("liquid"), "liquid");
  assert.equal(normalizeThemeId("paper"), "paper");
  // 老后端不认识 theme 字段时返回 undefined
  assert.equal(normalizeThemeId(undefined), DEFAULT_THEME_ID);
  assert.equal(normalizeThemeId(null), DEFAULT_THEME_ID);
  assert.equal(normalizeThemeId(42), DEFAULT_THEME_ID);
  // 手改缓存 / 未来被下线的主题 id
  assert.equal(normalizeThemeId("hacker-theme"), DEFAULT_THEME_ID);
  assert.equal(normalizeThemeId(""), DEFAULT_THEME_ID);
});

test("themeMeta：支持液态玻璃与纸感玻璃，未知 id 回落默认主题元数据", () => {
  assert.equal(themeMeta("netflix").label, "Netflix");
  assert.equal(themeMeta("liquid").label, "液态玻璃");
  assert.equal(themeMeta("paper").label, "纸感玻璃");
  assert.equal(themeMeta("nope").id, DEFAULT_THEME_ID);
});

test("sidebarGlass：液态玻璃启用曲面光学折射参数，银玻璃保持磨砂默认", () => {
  const basePrefs = { transparency: 0, brightness: 0, depth: 32 };
  const silver = sidebarGlass(basePrefs, "silver");
  const liquid = sidebarGlass(basePrefs, "liquid");

  assert.equal(silver.variant, "frosted");
  assert.equal(silver.settings.bevel, 0);

  assert.equal(liquid.variant, "dome");
  assert.equal(liquid.settings.bevel, 1);
  assert.ok(liquid.settings.refraction > silver.settings.refraction);
  assert.ok(liquid.settings.chromaticAberration > silver.settings.chromaticAberration);
  assert.ok(liquid.settings.specular > silver.settings.specular);
});

test("sidebarGlass：纸感玻璃保持平滑连续的白瓷不透明度曲线，拉满全透时无残留描边与兜底", () => {
  const lowTransparency = sidebarGlass({ transparency: 0.28, brightness: 0, depth: 32 }, "paper");
  assert.ok(
    lowTransparency.fallbackAlpha >= 0.69 && lowTransparency.fallbackAlpha <= 0.9,
    `expected paper fallbackAlpha at 0.28 in [0.69, 0.90], got ${lowTransparency.fallbackAlpha}`,
  );

  const defaultPaper = sidebarGlass({ transparency: 0.49, brightness: 0, depth: 32 }, "paper");
  assert.equal(defaultPaper.variant, "frosted");
  assert.equal(defaultPaper.sampleBackground, 0);
  assert.ok(defaultPaper.fallbackAlpha >= 0.68 && defaultPaper.fallbackAlpha <= 0.85);
  assert.ok(Math.abs(defaultPaper.hairlineAlpha - 0.085 * (1 - 0.49)) < 1e-6);
  assert.ok(Math.abs(defaultPaper.settings.opacity - 0.96 * (1 - 0.49)) < 1e-6);

  const nearFullA = sidebarGlass({ transparency: 0.98, brightness: 0, depth: 32 }, "paper");
  const nearFullB = sidebarGlass({ transparency: 0.99, brightness: 0, depth: 32 }, "paper");
  const fullTransparent = sidebarGlass({ transparency: 1, brightness: 0, depth: 32 }, "paper");
  assert.ok(nearFullA.fallbackAlpha > nearFullB.fallbackAlpha);
  assert.ok(nearFullB.fallbackAlpha > 0 && nearFullB.fallbackAlpha < 0.05);
  assert.ok(nearFullA.fallbackAlpha - nearFullB.fallbackAlpha < 0.05, "no cliff near 0.99");
  assert.equal(fullTransparent.fallbackAlpha, 0);
  assert.equal(fullTransparent.hairlineAlpha, 0);
  assert.equal(fullTransparent.settings.opacity, 0);
});
