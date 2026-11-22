import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { test } from "node:test";

const css = await loadBundledCss(new URL("../app/globals.css", import.meta.url));

async function loadBundledCss(entryUrl) {
  const source = await readFile(entryUrl, "utf8");
  const lines = source.split("\n");
  const resolved = [];
  for (const line of lines) {
    const match = line.match(/^@import\s+"(\.[^"]+)";\s*$/);
    if (match) {
      resolved.push(await loadBundledCss(new URL(match[1], entryUrl)));
    } else {
      resolved.push(line);
    }
  }
  return resolved.join("\n");
}
const html = await readFile(new URL("../index.html", import.meta.url), "utf8");
const prefs = await readFile(new URL("../lib/ui-prefs.tsx", import.meta.url), "utf8");
const themes = await readFile(new URL("../lib/themes.ts", import.meta.url), "utf8");
const emptyState = await readFile(new URL("../components/content-empty-state.tsx", import.meta.url), "utf8");
const glassPanel = await readFile(new URL("../components/glass-panel.tsx", import.meta.url), "utf8");
const subscriptionsView = await readFile(new URL("../components/subscriptions-view.tsx", import.meta.url), "utf8");
const settingsSectionsSource = await readFile(new URL("../lib/settings-sections.ts", import.meta.url), "utf8");
const settingsView = await readFile(new URL("../components/settings-view.tsx", import.meta.url), "utf8");

function themeBlock(id) {
  const selector = `html[data-theme="${id}"]`;
  const start = css.indexOf(`${selector} {`);
  assert.notEqual(start, -1, `missing ${selector} token block`);
  const end = css.indexOf("\n}", start);
  assert.notEqual(end, -1, `unterminated ${selector} token block`);
  return css.slice(start, end + 2);
}

test("paper glass maps the shared semantic palette to crisp light surfaces and readable status tokens", () => {
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
    "--color-sky-300:",
    "--color-emerald-300:",
    "--color-amber-200:",
    "--color-violet-200:",
    "--color-purple-300:",
    "--color-red-400:",
  ]) {
    assert.ok(paper.includes(token), `paper theme missing ${token}`);
  }
  // Paper sidebar bypasses dark-wallpaper WebGL sampling and uses daylight frosted glass
  assert.match(glassPanel, /glass-panel--paper/);
  assert.match(css, /\.glass-panel--paper\s*\{/);
  // Paper detail pages transition to daylight canvas instead of pitch black #000
  assert.match(css, /html\[data-theme="paper"\]\s+\.detail-ambient\s*\{[\s\S]*?#eef1f6/);
  // SubscriptionsView no longer hardcodes dark charcoal linear-gradient slabs
  assert.doesNotMatch(subscriptionsView, /rgba\(16,19,29,0\.82\)/);
  assert.doesNotMatch(subscriptionsView, /rgba\(18,21,30,0\.88\)/);
});

test("liquid glass defines optical dark surfaces, specular highlights, and matching pre-paint chrome", () => {
  const liquid = themeBlock("liquid");
  for (const token of [
    "color-scheme: dark",
    "--bg: #06101f",
    "--surface-sidebar:",
    "--surface-main:",
    "--surface-raised:",
    "--text:",
    "--line:",
    "--accent:",
    "--ok:",
    "--info:",
    "--warn:",
    "--danger:",
  ]) {
    assert.ok(liquid.includes(token), `liquid theme missing ${token}`);
  }
  assert.match(prefs, /liquid: "#06101f"/);
  assert.match(html, /liquid/);
});

test("silver remains the default and Netflix keeps its own theme block", () => {
  assert.ok(css.includes("--bg: #0a0b10"));
  assert.ok(themeBlock("netflix").includes("--bg: #000000"));
  assert.match(themes, /DEFAULT_THEME_ID = "silver"/);
});

test("paper and liquid are applied before the first paint and use matching browser chrome colors", () => {
  assert.match(html, /t==="paper"\|\|t==="netflix"\|\|t==="liquid"/);
  assert.match(html, /setAttribute\("data-theme",t\)/);
  assert.match(prefs, /paper: "#eef1f6"/);
  assert.match(prefs, /liquid: "#06101f"/);
});

test("empty and disabled states stay neutral", () => {
  assert.match(css, /:where\(button, input, select, textarea\):disabled/);
  assert.match(emptyState, /bg-\[var\(--text-faint\)\]/);
  assert.doesNotMatch(emptyState, /bg-\[var\(--ok\)\]/);
});

test("disabled buttons neutralize bright fills and avoid double-fading text-faint across themes", () => {
  assert.match(
    css,
    /\.btn-accent:disabled[\s\S]*?\{[^}]*background:\s*rgba\(255,\s*255,\s*255,\s*0\.0[6-9]\)[^}]*color:\s*var\(--text-faint\)[^}]*opacity:\s*1/
  );
  assert.match(
    css,
    /html\[data-theme="paper"\]\s+\.btn-accent:disabled[\s\S]*?\{[^}]*background:\s*rgba\(32,\s*42,\s*58,\s*0\.0[5-8]\)[^}]*color:\s*var\(--text-faint\)[^}]*opacity:\s*1/
  );
  assert.match(
    css,
    /html\[data-theme="liquid"\]\s+\.btn-accent:disabled[\s\S]*?\{[^}]*background:\s*rgba\(255,\s*255,\s*255,\s*0\.0[6-9]\)[^}]*color:\s*var\(--text-faint\)[^}]*opacity:\s*1/
  );
  assert.match(
    css,
    /html\[data-theme="netflix"\]\s+\.btn-accent:disabled[\s\S]*?\{[^}]*background:\s*#404040[^}]*color:\s*var\(--text-faint\)[^}]*opacity:\s*1/
  );
  assert.doesNotMatch(css, /\.btn-glass:disabled\s*\{[^}]*opacity:\s*0\./);
});


test("settings keeps a saved theme selector for every registered theme", () => {
  assert.match(settingsSectionsSource, /id: "appearance", label: "外观"/);
  assert.match(settingsView, /THEMES\.map\(\(theme\) =>/);
  assert.match(settingsView, /aria-pressed=\{selected\}/);
  assert.match(settingsView, /savePrefs\(\{ \.\.\.prefs, theme: themeId \}\)/);
});
