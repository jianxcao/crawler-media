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

test("GlassPanel keeps a single DOM tree across themes and updates backdrop textures in-place without WebGL teardown", async () => {
  assert.match(glassPanel, /const usesWebgl = theme\.id !== "netflix" && theme\.id !== "paper"/);
  assert.match(glassPanel, /\},\s*\[apply,\s*usesWebgl\]\)/);
  assert.doesNotMatch(glassPanel, /\},\s*\[backgroundImage,\s*apply/);
  assert.match(glassPanel, /setImage\(backgroundImage\)/);
  const contentSlots = glassPanel.match(/glass-panel__content/g) ?? [];
  // Only 1 JSX className occurrence (comments don't mention glass-panel__content)
  assert.equal(contentSlots.length, 1);

  // Behavioral verification of LiquidGlassRenderer currentUrl deduplication + 6-entry LRU imageCache
  const { createRequire } = await import("node:module");
  const ts = (await import("typescript")).default;
  const require = createRequire(import.meta.url);

  const rendererTs = await readFile(
    path.join(webDir, "vendor/liquid-glass/core/LiquidGlassRenderer.ts"),
    "utf8",
  );
  const rendererJs = ts.transpileModule(rendererTs, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;

  const createdLoads = [];
  class FakeImage extends globalThis.EventTarget {
    complete = false;
    naturalWidth = 1920;
    naturalHeight = 1080;
    crossOrigin = "";
    _src = "";
    set src(v) {
      // Simulate real browser resolving relative URLs like "/backdrop-default.jpg" to full origin URL!
      this._src = v ? new URL(v, "http://localhost:3334").href : "";
    }
    get src() {
      return this._src;
    }
    get currentSrc() {
      return this._src;
    }
  }

  let loseContextCalls = 0;
  let createProgramCalls = 0;
  const prevWindow = globalThis.window;
  const prevImage = globalThis.Image;
  const prevHTMLVideoElement = globalThis.HTMLVideoElement;
  globalThis.window = {
    devicePixelRatio: 1,
    innerWidth: 1280,
    innerHeight: 800,
    scrollX: 0,
    scrollY: 0,
    setTimeout: globalThis.setTimeout.bind(globalThis),
    requestAnimationFrame: () => 1,
    cancelAnimationFrame() {},
  };
  globalThis.Image = FakeImage;
  globalThis.HTMLVideoElement = class {};

  const mod = { exports: {} };
  new Function("require", "module", "exports", rendererJs)(
    (spec) => {
      if (spec === "./shaders") {
        return { vertexShader: "void main(){}", fragmentShader: "void main(){}" };
      }
      if (spec === "@/lib/artwork-loading") {
        return {
          createArtworkImage: () => {
            const image = new FakeImage();
            return {
              image,
              load(url) {
                createdLoads.push(url);
                image.src = url;
                image.complete = true;
                image.dispatchEvent(new globalThis.Event("load"));
              },
            };
          },
        };
      }
      return require(spec);
    },
    mod,
    mod.exports,
  );

  try {
    const fakeGl = {
      VERTEX_SHADER: 1,
      FRAGMENT_SHADER: 2,
      COMPILE_STATUS: 3,
      LINK_STATUS: 4,
      ARRAY_BUFFER: 5,
      STATIC_DRAW: 6,
      FLOAT: 7,
      TEXTURE0: 8,
      TEXTURE_2D: 9,
      TEXTURE_MIN_FILTER: 10,
      TEXTURE_MAG_FILTER: 11,
      TEXTURE_WRAP_S: 12,
      TEXTURE_WRAP_T: 13,
      LINEAR: 14,
      CLAMP_TO_EDGE: 15,
      UNPACK_FLIP_Y_WEBGL: 16,
      RGBA: 17,
      UNSIGNED_BYTE: 18,
      COLOR_BUFFER_BIT: 19,
      TRIANGLE_STRIP: 20,
      isContextLost: () => false,
      getExtension: (name) =>
        name === "WEBGL_lose_context"
          ? {
              loseContext() {
                loseContextCalls += 1;
              },
              restoreContext() {},
            }
          : null,
      createShader: () => ({}),
      shaderSource() {},
      compileShader() {},
      getShaderParameter: () => true,
      deleteShader() {},
      createProgram: () => {
        createProgramCalls += 1;
        return {};
      },
      attachShader() {},
      linkProgram() {},
      getProgramParameter: () => true,
      useProgram() {},
      deleteProgram() {},
      createBuffer: () => ({}),
      bindBuffer() {},
      bufferData() {},
      deleteBuffer() {},
      getAttribLocation: () => 0,
      enableVertexAttribArray() {},
      vertexAttribPointer() {},
      getUniformLocation: () => ({}),
      createTexture: () => ({}),
      deleteTexture() {},
      activeTexture() {},
      bindTexture() {},
      texParameteri() {},
      texImage2D() {},
      pixelStorei() {},
      uniform1i() {},
      uniform1f() {},
      uniform2f() {},
      uniform3f() {},
      uniform3fv() {},
      viewport() {},
      clearColor() {},
      clear() {},
      drawArrays() {},
    };

    const fakeCanvas = {
      width: 300,
      height: 600,
      clientWidth: 300,
      style: {},
      ownerDocument: { querySelector: () => null },
      addEventListener() {},
      removeEventListener() {},
      getBoundingClientRect: () => ({
        left: 0,
        top: 0,
        right: 300,
        bottom: 600,
        width: 300,
        height: 600,
      }),
      getContext: () => fakeGl,
    };

    const defaultSettings = {
      lensWidth: 300,
      lensHeight: 600,
      radius: 16,
      blur: 10,
      depth: 10,
      refraction: 0.5,
      chromaticAberration: 0.1,
      edgeHighlight: 0.2,
      specular: 0.2,
      fresnel: 0.2,
      brightness: 1,
      saturation: 1,
      shadow: 0.2,
      darkTint: 0.4,
      distortion: 0.2,
      tintStrength: 0.1,
      tint: 0,
      tintColor: [1, 1, 1],
      opacity: 1,
      bevel: 0,
    };

    const { LiquidGlassRenderer } = mod.exports;
    const r = new LiquidGlassRenderer(fakeCanvas, "/backdrop-default.jpg", defaultSettings);
    assert.equal(createProgramCalls, 1);
    assert.deepEqual(createdLoads, ["/backdrop-default.jpg"]);

    // Calling setImage with identical relative URL "/backdrop-default.jpg" MUST be a no-op even though img.src resolved to http://localhost:3334/backdrop-default.jpg!
    r.setImage("/backdrop-default.jpg");
    assert.equal(createdLoads.length, 1, "setImage('/backdrop-default.jpg') must deduplicate via currentUrl");

    // Load 5 more distinct URLs -> cache now holds 6 entries: ["/backdrop-default.jpg", u1, u2, u3, u4, u5]
    for (let i = 1; i <= 5; i += 1) {
      r.setImage(`https://cdn.invalid/backdrop-${i}.jpg`);
    }
    assert.equal(createdLoads.length, 6);
    assert.equal(loseContextCalls, 0);
    assert.equal(createProgramCalls, 1);

    // Hit u1 -> refreshes u1 to MRU; oldest non-default is now u2!
    const loadsBeforeHit = createdLoads.length;
    r.setImage("https://cdn.invalid/backdrop-1.jpg");
    assert.equal(createdLoads.length, loadsBeforeHit, "u1 must be served from cache");

    // Load u6 -> must evict u2 (NOT "/backdrop-default.jpg" and NOT recently-hit u1!)
    r.setImage("https://cdn.invalid/backdrop-6.jpg");
    assert.equal(createdLoads.length, loadsBeforeHit + 1);

    // "/backdrop-default.jpg" and u1 are still cached (0 network loads)
    r.setImage("/backdrop-default.jpg");
    r.setImage("https://cdn.invalid/backdrop-1.jpg");
    assert.equal(createdLoads.length, loadsBeforeHit + 1, "default backdrop and MRU entry must remain cached");

    // u2 was evicted so requesting it again triggers a fresh load
    r.setImage("https://cdn.invalid/backdrop-2.jpg");
    assert.equal(createdLoads.length, loadsBeforeHit + 2, "evicted LRU entry u2 must reload when requested again");

    r.dispose();
    assert.equal(loseContextCalls, 1, "dispose() releases WebGL context once on unmount");
  } finally {
    if (prevWindow === undefined) delete globalThis.window;
    else globalThis.window = prevWindow;
    if (prevImage === undefined) delete globalThis.Image;
    else globalThis.Image = prevImage;
    if (prevHTMLVideoElement === undefined) delete globalThis.HTMLVideoElement;
    else globalThis.HTMLVideoElement = prevHTMLVideoElement;
  }
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

test("paper light theme contrast fixes, --ok-ink, .btn-glass modifiers, and amber disabled buttons", async () => {
  const paper = themeBlock("paper");

  // 1. TrackTone.Text (--color-cyan-100) and TrackTone.Ai contrast on paper .menu-surface
  assert.ok(paper.includes("--color-cyan-100:"), "paper theme must map --color-cyan-100 for TrackTone.Text");
  const mediaTrackFormat = await readFile(path.join(webDir, "lib/media-track-format.ts"), "utf8");
  assert.doesNotMatch(
    mediaTrackFormat,
    /text-\[#eef2f8\]/,
    "TrackTone.Ai must not hardcode silver #eef2f8 text on light paper menus",
  );

  // 2. --ok-ink token & SourceWarning / check badges
  assert.ok(css.includes("--ok-ink:"), ":root must define --ok-ink");
  assert.match(paper, /--ok-ink:\s*#ffffff/i, "paper theme must set --ok-ink: #ffffff for deep #157a48 --ok badges");
  const taskLayout = await readFile(path.join(webDir, "components/task-center/task-layout.tsx"), "utf8");
  assert.doesNotMatch(taskLayout, /\btext-amber-50\b/, "SourceWarning link must not use 1.09:1 text-amber-50");
  assert.match(taskLayout, /text-\[var\(--warn\)\]/);
  assert.doesNotMatch(taskLayout, /text-\[#07120b\]/);
  const seasonEpisodes = await readFile(
    path.join(webDir, "components/library-item/season-episodes-section.tsx"),
    "utf8",
  );
  assert.doesNotMatch(seasonEpisodes, /text-\[#07120c\]/);

  // 3. Torrent search seeders, promo badges, and HDR chip contrast on paper .card-row-surface
  const torrentSearchView = await readFile(path.join(webDir, "lib/torrent-search-view.ts"), "utf8");
  assert.doesNotMatch(torrentSearchView, /text-\[#ff9a9a\]|text-\[#79d193\]/);
  const torrentRow = await readFile(path.join(webDir, "components/search/torrent-row.tsx"), "utf8");
  assert.doesNotMatch(torrentRow, /text-\[#c8a6ff\]/, "HDR chip must not hardcode pastel #c8a6ff on white paper rows");

  // 4. Settings & inspector semantic tokens instead of pastel hexes
  const cdpBrowser = await readFile(
    path.join(webDir, "components/settings/sites/cdp-browser-section.tsx"),
    "utf8",
  );
  assert.doesNotMatch(cdpBrowser, /text-\[#51cf66\]/);
  assert.doesNotMatch(
    cdpBrowser,
    /text-\[#ff6b6b\]/,
    "CdpBrowserSection diagnostics must use semantic text-[var(--danger)] instead of pastel #ff6b6b",
  );

  // Paper .dark-media-surface restores the full silver accent token group so accent badges use dark #141821 ink
  const darkSurfaceMatch = css.match(
    /html\[data-theme="paper"\]\s+:is\(\s*\.dark-media-surface,[\s\S]*?\)\s*\{([\s\S]*?)\n\}/,
  );
  assert.ok(darkSurfaceMatch, "paper theme must define .dark-media-surface override block");
  const darkSurfaceBody = darkSurfaceMatch[1];
  for (const token of [
    "--accent: #cdd6e6;",
    "--accent-strong: #eef2f8;",
    "--accent-2: #9fb0c9;",
    "--accent-ink: #141821;",
    "--accent-soft: rgba(205, 214, 230, 0.14);",
  ]) {
    assert.ok(
      darkSurfaceBody.includes(token),
      `paper .dark-media-surface block must restore ${token}`,
    );
  }

  const settingsOverview = await readFile(
    path.join(webDir, "components/settings-overview-section.tsx"),
    "utf8",
  );
  assert.doesNotMatch(settingsOverview, /text-\[#38bdf8\]/);

  const metadataSettings = await readFile(
    path.join(webDir, "components/metadata-settings-section.tsx"),
    "utf8",
  );
  assert.doesNotMatch(metadataSettings, /text-\[#38bdf8\]/);

  const wantedBreakdown = await readFile(
    path.join(webDir, "components/subscription-inspector/wanted-breakdown.tsx"),
    "utf8",
  );
  assert.doesNotMatch(wantedBreakdown, /text-\[#2dd4bf\]/);

  const membersSection = await readFile(path.join(webDir, "components/members-section.tsx"), "utf8");
  assert.doesNotMatch(membersSection, /!text-\[#ffb36b\]/);

  // 5. SiteRow avoids #c0c4cc low contrast
  const siteRow = await readFile(path.join(webDir, "components/settings/sites/site-row.tsx"), "utf8");
  assert.doesNotMatch(siteRow, /#c0c4cc/);

  // 6. disabled-states.css neutralizes solid amber / #fbbf24 buttons (ProbeQueueCard)
  assert.match(css, /button\[class\*="bg-amber-"\]:disabled/);
  assert.match(css, /button\[class\*="bg-\[#fbbf24\]"\]:disabled/);

  // 7. .btn-glass--accent and .btn-glass--muted defined in base + paper and applied across components
  assert.match(css, /\.btn-glass--accent\s*\{[^}]*color:\s*var\(--accent\)/);
  assert.match(css, /\.btn-glass--muted\s*\{[^}]*color:\s*var\(--text-muted\)/);
  assert.match(css, /html\[data-theme="paper"\]\s+\.btn-glass--accent\s*\{[^}]*color:\s*var\(--accent\)/);
  assert.match(css, /html\[data-theme="paper"\]\s+\.btn-glass--muted\s*\{[^}]*color:\s*var\(--text-muted\)/);

  const reidentifyDialog = await readFile(path.join(webDir, "components/reidentify-dialog.tsx"), "utf8");
  assert.match(reidentifyDialog, /btn-glass--danger/);
  assert.doesNotMatch(reidentifyDialog, /btn-glass[^\n]*text-\[#ffb4b4\]/);

  assert.match(metadataSettings, /btn-glass--danger/);
  assert.match(metadataSettings, /btn-glass--accent/);
  assert.doesNotMatch(metadataSettings, /btn-glass[^\n]*text-\[#ffaaaa\]/);

  assert.match(settingsView, /btn-glass--muted/);

  const filterToolbar = await readFile(
    path.join(webDir, "components/search/torrent-filter-toolbar.tsx"),
    "utf8",
  );
  assert.match(filterToolbar, /btn-glass--muted/);
  assert.match(torrentRow, /btn-glass--muted/);
});
