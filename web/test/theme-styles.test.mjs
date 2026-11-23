import assert from "node:assert/strict";
import { readdir, readFile } from "node:fs/promises";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { sidebarGlass } from "../lib/glass.ts";

const webDir = fileURLToPath(new URL("../", import.meta.url));
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

async function collectTsxFiles(dir) {
  const entries = await readdir(dir, { withFileTypes: true });
  const files = [];
  for (const entry of entries) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      files.push(...(await collectTsxFiles(full)));
    } else if (entry.isFile() && /\.(ts|tsx)$/.test(entry.name)) {
      files.push(full);
    }
  }
  return files;
}

test("unlayered surface modifiers and active states work across all 4 themes without dead utility overrides", async () => {
  // .card-row-surface--danger & .card-row-surface--warn in base + paper + liquid + netflix
  assert.match(css, /\.card-row-surface--danger\s*\{[^}]*border-color:/);
  assert.match(css, /\.card-row-surface--warn\s*\{[^}]*border-color:/);
  for (const theme of ["paper", "liquid", "netflix"]) {
    assert.match(css, new RegExp(`html\\[data-theme="${theme}"\\]\\s+\\.card-row-surface--danger\\s*\\{`));
    assert.match(css, new RegExp(`html\\[data-theme="${theme}"\\]\\s+\\.card-row-surface--warn\\s*\\{`));
    assert.match(
      css,
      new RegExp(
        `html\\[data-theme="${theme}"\\]\\s+\\.page-nav-btn\\[data-active="true"\\],[\\s\\S]*?html\\[data-theme="${theme}"\\]\\s+\\.page-nav-btn\\[data-state="open"\\]`,
      ),
    );
    assert.match(
      css,
      new RegExp(
        `html\\[data-theme="${theme}"\\]\\s+\\.segmented-glass__item:is\\(\\[aria-pressed="true"\\],\\s*\\[aria-selected="true"\\],\\s*\\[aria-checked="true"\\]\\)`,
      ),
    );
  }
  assert.match(css, /\.page-nav-btn\[data-active="true"\],\s*\.page-nav-btn\[data-state="open"\]/);
  assert.match(
    css,
    /\.segmented-glass__item:is\(\[aria-pressed="true"\],\s*\[aria-selected="true"\],\s*\[aria-checked="true"\]\)/,
  );
  assert.match(css, /\.btn-glass--ok\s*\{[^}]*color:\s*var\(--ok\)/);
  assert.match(css, /\.btn-glass--danger\s*\{[^}]*color:\s*var\(--danger\)/);
  assert.match(css, /html\[data-theme="paper"\]\s+\.btn-glass--ok\s*\{[^}]*color:\s*var\(--ok\)/);
  assert.match(css, /html\[data-theme="paper"\]\s+\.btn-glass--danger\s*\{[^}]*color:\s*var\(--danger\)/);
  assert.match(css, /\.input-glass\s*\{/);

  const jobCenter = await readFile(path.join(webDir, "components/job-center.tsx"), "utf8");
  assert.match(jobCenter, /card-row-surface--danger/);
  assert.match(jobCenter, /card-row-surface--warn/);

  const posterGrid = await readFile(
    path.join(webDir, "components/search/torrent-poster-grid.tsx"),
    "utf8",
  );
  assert.match(posterGrid, /card-row-surface card-row-surface--interactive/);
  assert.doesNotMatch(posterGrid, /hover:border-\[var\(--line-strong\)\]/);

  const favoritesView = await readFile(path.join(webDir, "components/favorites-view.tsx"), "utf8");
  assert.match(favoritesView, /data-active=\{gallery \? "true" : undefined\}/);
  assert.doesNotMatch(favoritesView, /data-\[state=open\]:bg-black\/55/);

  const systemLogs = await readFile(path.join(webDir, "components/system-logs-section.tsx"), "utf8");
  assert.match(systemLogs, /btn-glass--ok/);
  assert.match(systemLogs, /btn-glass--danger/);
});

test("search page, DiscoverEpisodeCard, PlayLayout, and disabled solid buttons remain contrast-safe in paper theme", async () => {
  const searchPage = await readFile(path.join(webDir, "app/(app)/search/page.tsx"), "utf8");
  assert.match(searchPage, /segmented-glass\b/);
  assert.match(searchPage, /segmented-glass__item\b/);
  assert.doesNotMatch(searchPage, /rgba\(243,\s*245,\s*249,/);

  const seasonsSection = await readFile(
    path.join(webDir, "components/media-detail/seasons-section.tsx"),
    "utf8",
  );
  assert.match(seasonsSection, /dark-media-surface[^\n]*bg-\[#141824\]/);
  assert.doesNotMatch(seasonsSection, /dark-media-surface[^\n]*bg-white\/\[0\.04\]/);
  assert.match(seasonsSection, /bg-emerald-950\/80/);
  assert.match(seasonsSection, /bg-sky-950\/80/);
  assert.match(seasonsSection, /bg-orange-950\/80/);

  const playLayout = await readFile(path.join(webDir, "app/play/layout.tsx"), "utf8");
  assert.match(playLayout, /viewport-overlay-height/);
  assert.doesNotMatch(playLayout, /\bh-dvh\b/);

  // Paper /play dark canvas isolation & dark-media-surface token + scrollbar restoration
  assert.match(css, /html\[data-theme="paper"\]:has\(\.player-root\)\s+body\s*\{[^}]*background:\s*#000000/);
  assert.match(css, /html\[data-theme="paper"\]:has\(\.player-root\)\s+body::before\s*\{[^}]*display:\s*none/);
  assert.match(
    css,
    /html\[data-theme="paper"\]\s+:is\([\s\S]*?\.dark-media-surface[\s\S]*?\.player-root[\s\S]*?\.media-lightbox[\s\S]*?\)\s*\{[\s\S]*?color-scheme:\s*dark[\s\S]*?--text:\s*#f3f5f9/,
  );
  assert.match(
    css,
    /html\[data-theme="paper"\]\s+:is\(\.dark-media-surface,\s*\.player-root,\s*\.media-lightbox\)\s+\.scroll-thin::-webkit-scrollbar-thumb/,
  );

  // Solid danger & colored button disabled neutralization + ProbeQueueCard amber fill
  assert.match(css, /\.btn-danger\s*\{[^}]*background:\s*var\(--danger-solid\)/);
  assert.match(css, /button\[class\*="bg-red-"\]:disabled/);
  assert.match(css, /button\[class\*="bg-blue-"\]:disabled/);
  assert.match(css, /button\[class\*="bg-sky-"\]:disabled/);
  assert.match(css, /button\[class\*="bg-\[var\(--danger-solid\)\]"\]:disabled/);

  const probeQueueCard = await readFile(
    path.join(webDir, "components/task-center/probe-queue-card.tsx"),
    "utf8",
  );
  assert.match(probeQueueCard, /bg-\[#fbbf24\]/);
  assert.doesNotMatch(probeQueueCard, /bg-amber-400\/90/);
});

test("LiquidGlassRenderer tracks DPR + top-aligns backdrop UV and sidebarGlass avoids transparency cliff", async () => {
  const rendererSource = await readFile(
    path.join(webDir, "vendor/liquid-glass/core/LiquidGlassRenderer.ts"),
    "utf8",
  );
  assert.match(rendererSource, /private\s+lastDpr\s*=\s*0/);
  assert.match(rendererSource, /1\s*-\s*imageRatio\s*\/\s*viewRatio/);

  for (let step = 0; step < 100; step++) {
    const a = sidebarGlass({ transparency: step / 100, brightness: 0, depth: 32 }, "paper").fallbackAlpha;
    const b = sidebarGlass({ transparency: (step + 1) / 100, brightness: 0, depth: 32 }, "paper").fallbackAlpha;
    assert.ok(a >= b, `paper fallbackAlpha must be monotonic at ${step / 100}`);
    assert.ok(a - b < 0.05, `paper fallbackAlpha must not have a cliff at ${step / 100}: ${a} -> ${b}`);
  }
});

test("source guardrails forbid hardcoded dark <option> backgrounds, unguarded silver rgba text, and dead card-row-surface border utilities", async () => {
  const sourceFiles = [
    ...(await collectTsxFiles(path.join(webDir, "app"))),
    ...(await collectTsxFiles(path.join(webDir, "components"))),
  ];

  const optionViolations = [];
  const rgbaViolations = [];
  const cardRowBorderViolations = [];

  for (const file of sourceFiles) {
    const rel = path.relative(webDir, file);
    const content = await readFile(file, "utf8");

    if (/\[&>option\]:bg-\[#|<option[^>]*bg-\[#/i.test(content)) {
      optionViolations.push(rel);
    }

    const lines = content.split("\n");
    lines.forEach((line, idx) => {
      if (
        /rgba\(\s*243,\s*245,\s*249,/.test(line) &&
        !line.includes("dark-media-surface")
      ) {
        rgbaViolations.push(`${rel}:${idx + 1}`);
      }
      if (
        line.includes("card-row-surface") &&
        /(?:^|[\s"'`])(?:hover:)?border-(?!0\b|none\b|collapse\b|separate\b|spacing|t\b|b\b|l\b|r\b|x\b|y\b)/.test(
          line,
        )
      ) {
        cardRowBorderViolations.push(`${rel}:${idx + 1}`);
      }
    });
  }

  assert.deepEqual(optionViolations, [], `Hardcoded dark <option> classes found: ${optionViolations.join(", ")}`);
  assert.deepEqual(rgbaViolations, [], `Unguarded rgba(243,245,249,...) found: ${rgbaViolations.join(", ")}`);
  assert.deepEqual(
    cardRowBorderViolations,
    [],
    `.card-row-surface paired with @layer utilities border color override: ${cardRowBorderViolations.join(", ")}`,
  );

  // Also check dark media frames carry dark-media-surface and UI chrome avoids hardcoded dark hexes
  for (const rel of [
    "components/library-cover-modal.tsx",
    "components/cast-row.tsx",
    "components/discovered-person-detail-view.tsx",
    "components/subscribe-dialog/subscribe-form-sections.tsx",
    "components/media-detail/photo-wall.tsx",
    "components/library-view.tsx",
    "components/library-manage-row.tsx",
    "components/poster-wall.tsx",
  ]) {
    const src = await readFile(path.join(webDir, rel), "utf8");
    assert.match(src, /dark-media-surface/, `${rel} must guard its dark artwork surface with dark-media-surface`);
  }

  const healthPage = await readFile(path.join(webDir, "app/health/page.tsx"), "utf8");
  assert.doesNotMatch(healthPage, /bg-\[#111111\]|bg-white\/80/);

  const notFound = await readFile(path.join(webDir, "src/not-found.tsx"), "utf8");
  assert.doesNotMatch(notFound, /bg-\[#0a0b10\]/);
});
