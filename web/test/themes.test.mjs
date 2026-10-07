import assert from "node:assert/strict";
import { test } from "node:test";

import { DEFAULT_THEME_ID, normalizeThemeId, THEMES, themeMeta } from "../lib/themes.ts";

test("注册表保留银玻璃与 Netflix，并提供纸感玻璃主题", () => {
  const ids = THEMES.map((theme) => theme.id);
  assert.deepEqual(ids, ["silver", "paper", "netflix"]);
  // Netflix 是结构级主题（换外壳），银玻璃不是
  assert.equal(THEMES.find((t) => t.id === "silver")?.structural, false);
  assert.equal(THEMES.find((t) => t.id === "netflix")?.structural, true);
  assert.equal(THEMES.find((t) => t.id === "paper")?.structural, false);
});

test("normalizeThemeId：未知值 / 非字符串 / undefined 一律兜底为默认主题", () => {
  assert.equal(normalizeThemeId("netflix"), "netflix");
  assert.equal(normalizeThemeId("silver"), "silver");
  assert.equal(normalizeThemeId("paper"), "paper");
  // 老后端不认识 theme 字段时返回 undefined
  assert.equal(normalizeThemeId(undefined), DEFAULT_THEME_ID);
  assert.equal(normalizeThemeId(null), DEFAULT_THEME_ID);
  assert.equal(normalizeThemeId(42), DEFAULT_THEME_ID);
  // 手改缓存 / 未来被下线的主题 id
  assert.equal(normalizeThemeId("hacker-theme"), DEFAULT_THEME_ID);
  assert.equal(normalizeThemeId(""), DEFAULT_THEME_ID);
});

test("themeMeta：未知 id 回落默认主题元数据", () => {
  assert.equal(themeMeta("netflix").label, "Netflix");
  assert.equal(themeMeta("paper").label, "纸感玻璃");
  assert.equal(themeMeta("nope").id, DEFAULT_THEME_ID);
});
