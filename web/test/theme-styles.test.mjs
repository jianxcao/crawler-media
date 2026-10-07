import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

const css = await readFile(new URL("../app/globals.css", import.meta.url), "utf8");
const html = await readFile(new URL("../index.html", import.meta.url), "utf8");
const prefs = await readFile(new URL("../lib/ui-prefs.tsx", import.meta.url), "utf8");
const themes = await readFile(new URL("../lib/themes.ts", import.meta.url), "utf8");
const emptyState = await readFile(new URL("../components/content-empty-state.tsx", import.meta.url), "utf8");
const mockData = await readFile(new URL("../lib/mock-data.ts", import.meta.url), "utf8");
const settingsView = await readFile(new URL("../components/settings-view.tsx", import.meta.url), "utf8");

function themeBlock(id) {
  const selector = `html[data-theme="${id}"]`;
  const start = css.indexOf(`${selector} {`);
  assert.notEqual(start, -1, `missing ${selector} token block`);
  const end = css.indexOf("\n}", start);
  assert.notEqual(end, -1, `unterminated ${selector} token block`);
  return css.slice(start, end + 2);
}

test("paper glass maps the shared semantic palette to light surfaces", () => {
  const paper = themeBlock("paper");
  for (const token of [
    "color-scheme: light",
    "--bg: #eef1f6",
    "--surface-sidebar:",
    "--surface-main:",
    "--surface-raised:",
    "--text: #202a3a",
    "--line:",
    "--accent:",
    "--ok:",
    "--info:",
    "--warn:",
    "--danger:",
  ]) {
    assert.ok(paper.includes(token), `paper theme missing ${token}`);
  }
});

test("silver remains the default and Netflix keeps its own theme block", () => {
  assert.ok(css.includes("--bg: #0a0b10"));
  assert.ok(themeBlock("netflix").includes("--bg: #000000"));
  assert.match(themes, /DEFAULT_THEME_ID = "silver"/);
});

test("paper is applied before the first paint and uses a matching browser chrome color", () => {
  assert.match(html, /t==="paper"\|\|t==="netflix"/);
  assert.match(html, /setAttribute\("data-theme",t\)/);
  assert.match(prefs, /paper: "#eef1f6"/);
});

test("empty and disabled states stay neutral while run states use signal tokens", () => {
  assert.match(css, /:where\(button, input, select, textarea\):disabled/);
  assert.match(emptyState, /bg-\[var\(--text-faint\)\]/);
  assert.doesNotMatch(emptyState, /bg-\[var\(--ok\)\]/);
  assert.match(mockData, /running: \{ label: "运行中", color: "var\(--info\)" \}/);
  assert.match(mockData, /done: \{ label: "已完成", color: "var\(--ok\)" \}/);
  assert.match(mockData, /failed: \{ label: "失败", color: "var\(--danger\)" \}/);
});

test("settings keeps a saved theme selector for every registered theme", () => {
  assert.match(mockData, /id: "appearance", label: "外观"/);
  assert.match(settingsView, /THEMES\.map\(\(theme\) =>/);
  assert.match(settingsView, /aria-pressed=\{selected\}/);
  assert.match(settingsView, /savePrefs\(\{ \.\.\.prefs, theme: themeId \}\)/);
});
