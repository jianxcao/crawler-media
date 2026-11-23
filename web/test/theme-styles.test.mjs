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
    "--accent-ink: #ffffff",
    "--tooltip-bg:",
    "--tooltip-stroke:",
    "--row-fade-bg:",
    "--ok:",
    "--info:",
    "--warn:",
    "--danger:",
    "--color-sky-300:",
    "--color-emerald-300:",
    "--color-amber-200:",
    "--color-violet-200:",
    "--color-purple-300:",
    "--color-red-100:",
    "--color-red-200:",
    "--color-red-400:",
    "--color-orange-200:",
    "--color-blue-300:",
  ]) {
    assert.ok(paper.includes(token), `paper theme missing ${token}`);
  }
  // Paper sidebar bypasses dark-wallpaper WebGL sampling and wins over .panel--sidebar
  assert.match(glassPanel, /glass-panel--paper/);
  assert.match(css, /html\[data-theme="paper"\]\s+\.panel--sidebar:not\(\.glass-panel--paper\)/);
  assert.match(css, /html\[data-theme="paper"\]\s+\.glass-panel--paper\s*\{/);
  // Paper detail pages transition to daylight canvas instead of pitch black #000
  assert.match(css, /html\[data-theme="paper"\]\s+\.detail-ambient\s*\{[\s\S]*?#eef1f6/);
  assert.match(css, /html\[data-theme="paper"\]\s+\.detail-lead\s+\.text-on-image/);
  // Paper styles shared dialog/popover/card-row/sub-hero/player surfaces cleanly
  assert.match(css, /html\[data-theme="paper"\]\s+\.dialog-surface/);
  assert.match(css, /html\[data-theme="paper"\]\s+\.popover-surface/);
  assert.match(css, /html\[data-theme="paper"\]\s+\.card-row-surface\s*\{/);
  assert.match(css, /html\[data-theme="paper"\]\s+\.sub-hero-card\s*\{/);
  assert.match(css, /html\[data-theme="paper"\]\s+\.sub-hero-tint\s*\{/);
  assert.match(css, /html\[data-theme="paper"\]\s+\.page-nav-btn\s*\{/);
  assert.match(css, /html\[data-theme="paper"\]\s+\.player-root\s*\{/);
  assert.match(
    css,
    /html\[data-theme="paper"\]\s+\[class~="bg-white"\]:not\(:is\(\.dark-media-surface,\s*\.player-root\)\s*\*\)/,
  );
  assert.doesNotMatch(css, /\.video-player|\.player-page|\.image-lightbox|\.zoom-lightbox/);
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
    "--accent-ink: #071426",
    "--tooltip-bg:",
    "--tooltip-stroke:",
    "--row-fade-bg:",
    "--ok:",
    "--info:",
    "--warn:",
    "--danger:",
  ]) {
    assert.ok(liquid.includes(token), `liquid theme missing ${token}`);
  }
  assert.match(css, /html\[data-theme="liquid"\]\s+\.dialog-surface/);
  assert.match(css, /html\[data-theme="liquid"\]\s+\.popover-surface/);
  assert.match(css, /html\[data-theme="liquid"\]\s+\.card-row-surface\s*\{/);
  assert.match(css, /html\[data-theme="liquid"\]\s+\.sub-hero-card\s*\{/);
  assert.match(css, /html\[data-theme="liquid"\]\s+\.sub-hero-tint\s*\{/);
  assert.match(css, /html\[data-theme="liquid"\]\s+\.detail-mobile-hero-fade\s*\{[\s\S]*?#06101f/);
  assert.match(css, /html\[data-theme="liquid"\]\s+\.mobile-topbar\s*\{/);
  assert.match(css, /html\[data-theme="liquid"\]\s+\.page-nav-btn\s*\{/);
  assert.match(prefs, /liquid: "#06101f"/);
  assert.match(html, /liquid/);
});

test("silver remains the default and Netflix keeps its own theme block", () => {
  assert.ok(css.includes("--bg: #0a0b10"));
  assert.ok(css.includes("--accent-ink: #141821"));
  assert.ok(css.includes("--row-fade-bg: rgba(20, 23, 31, 0.98)"));
  assert.match(css, /\.dialog-surface\s*\{/);
  assert.match(css, /\.popover-surface\s*\{/);
  assert.match(css, /\.card-row-surface\s*\{/);
  assert.doesNotMatch(css.replace(/\/\*[\s\S]*?\*\//g, ""), /-webkit-backdrop-filter/);
  const netflix = themeBlock("netflix");
  assert.ok(netflix.includes("--bg: #000000"));
  assert.ok(netflix.includes("--accent-ink: #ffffff"));
  assert.ok(netflix.includes("--row-fade-bg: #181818"));
  assert.match(themes, /DEFAULT_THEME_ID = "silver"/);
});

test("GlassPanel keeps a single DOM tree across themes and avoids WebGL teardown between silver and liquid", () => {
  assert.match(glassPanel, /const usesWebgl = theme\.id !== "netflix" && theme\.id !== "paper"/);
  assert.match(glassPanel, /\},\s*\[backgroundImage,\s*apply,\s*usesWebgl\]\)/);
  assert.doesNotMatch(glassPanel, /\},\s*\[backgroundImage,\s*apply,\s*theme\.id\]\)/);
  const contentSlots = glassPanel.match(/glass-panel__content/g) ?? [];
  // Only 1 JSX className occurrence (comments don't mention glass-panel__content)
  assert.equal(contentSlots.length, 1);
});

test("paper and liquid are applied before the first paint and use matching browser chrome colors", () => {
  assert.match(html, /<meta name="theme-color" content="#0a0b10"\s*\/>/);
  assert.match(html, /t==="paper"\|\|t==="netflix"\|\|t==="liquid"/);
  assert.match(html, /setAttribute\("data-theme",t\)/);
  assert.match(html, /t==="paper"\?"#eef1f6":t==="liquid"\?"#06101f":"#000000"/);
  assert.match(prefs, /paper: "#eef1f6"/);
  assert.match(prefs, /liquid: "#06101f"/);
  assert.match(prefs, /netflix: "#000000"/);
  assert.match(prefs, /\[DEFAULT_THEME_ID\]: "#0a0b10"/);
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
  assert.match(css, /:is\(\.btn-accent,\s*\.btn-glass,\s*button\.text-black\):disabled:not\(:has\(img\)\)/);
  assert.match(css, /html\[data-theme="liquid"\]\s+button\.text-black:disabled/);
  assert.doesNotMatch(css, /\.btn-glass:disabled\s*\{[^}]*opacity:\s*0\./);
});


test("settings keeps a saved theme selector for every registered theme", () => {
  assert.match(settingsSectionsSource, /id: "appearance", label: "外观"/);
  assert.match(settingsView, /THEMES\.map\(\(theme\) =>/);
  assert.match(settingsView, /aria-pressed=\{selected\}/);
  assert.match(settingsView, /savePrefs\(\{ \.\.\.prefs, theme: themeId \}\)/);
});
