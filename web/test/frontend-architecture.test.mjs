import assert from "node:assert/strict";
import { existsSync, readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const TEST_DIR = path.dirname(fileURLToPath(import.meta.url));
const WEB_DIR = path.resolve(TEST_DIR, "..");

function collectLibFiles(dir) {
  const results = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      results.push(...collectLibFiles(full));
    } else if (
      entry.isFile() &&
      /\.(ts|tsx|mjs)$/.test(entry.name) &&
      !entry.name.includes(".test.")
    ) {
      results.push(full);
    }
  }
  return results;
}

test("web/lib must not import from components or app layers", () => {
  const libDir = path.join(WEB_DIR, "lib");
  const files = collectLibFiles(libDir);
  const violations = [];
  const forbiddenImport = /(?:from|import)\s*(?:\(\s*)?["'](@\/(?:components|app)\/|\.\.\/components\/)/g;

  for (const file of files) {
    const content = readFileSync(file, "utf8");
    const matches = [...content.matchAll(forbiddenImport)];
    for (const match of matches) {
      violations.push(`${path.relative(WEB_DIR, file)} -> ${match[1]}`);
    }
  }

  assert.deepEqual(violations, []);
});

test("legacy fake mock modules in web/lib are removed", () => {
  assert.equal(existsSync(path.join(WEB_DIR, "lib/mock-data.ts")), false);
  assert.equal(existsSync(path.join(WEB_DIR, "lib/mock-media.ts")), false);
});

test("settings-sections provides pure metadata and role-filtered groups", async () => {
  const mod = await import("../lib/settings-sections.ts");
  assert.equal(mod.settingsSections.length, 11);
  const ids = mod.settingsSections.map((s) => s.id);
  assert.ok(ids.includes("overview"));
  assert.ok(ids.includes("appearance"));
  assert.ok(ids.includes("plugins"));

  const adminIds = mod.settingsSectionGroupsFor("admin").flatMap((g) => g.items.map((i) => i.id));
  assert.ok(adminIds.includes("overview"));
  assert.ok(adminIds.includes("sites"));

  const memberIds = mod.settingsSectionGroupsFor("member").flatMap((g) => g.items.map((i) => i.id));
  assert.deepEqual(memberIds, ["profile", "appearance"]);
});

test("heavy routes use React.lazy and layouts wrap Outlet in Suspense", () => {
  const mainSource = readFileSync(path.join(WEB_DIR, "src/main.tsx"), "utf8");
  const appLayoutSource = readFileSync(path.join(WEB_DIR, "app/(app)/layout.tsx"), "utf8");
  const playLayoutSource = readFileSync(path.join(WEB_DIR, "app/play/layout.tsx"), "utf8");

  for (const routeName of [
    "PlayPage",
    "SettingsSectionPage",
    "SearchPage",
    "ActivityPage",
    "SubscriptionsPageRoute",
    "LibraryDetailPage",
    "LibraryItemDetailPage",
  ]) {
    assert.match(
      mainSource,
      new RegExp(`const\\s+${routeName}\\s*=\\s*lazy\\(`),
      `${routeName} should be loaded via React.lazy`,
    );
  }

  assert.match(appLayoutSource, /<Suspense\b[\s\S]*?<Outlet\s*\/>[\s\S]*?<\/Suspense>/);
  assert.match(playLayoutSource, /<Suspense\b[\s\S]*?<Outlet\s*\/>[\s\S]*?<\/Suspense>/);
});

test("search-results is decomposed without HitView/AttrView shims or duplicate formatBytes", () => {
  const source = readFileSync(path.join(WEB_DIR, "components/search-results.tsx"), "utf8");
  const lines = source.split("\n").length;
  assert.ok(lines <= 500, `Expected search-results.tsx <= 500 lines, got ${lines}`);
  assert.doesNotMatch(source, /\binterface HitView\b/);
  assert.doesNotMatch(source, /\binterface AttrView\b/);
  assert.doesNotMatch(source, /\bfunction formatBytes\b/);
});

test("site-config-section, task-center-view, and api/libraries are decomposed", () => {
  for (const [rel, maxLines] of [
    ["components/site-config-section.tsx", 400],
    ["components/task-center-view.tsx", 400],
    ["lib/api/libraries.ts", 100],
  ]) {
    const source = readFileSync(path.join(WEB_DIR, rel), "utf8");
    const lines = source.split("\n").length;
    assert.ok(lines <= maxLines, `Expected ${rel} <= ${maxLines} lines, got ${lines}`);
  }
});

test("library-form-dialog, rule-sets-panel, and downloader-config-section are decomposed", () => {
  for (const [rel, maxLines] of [
    ["components/library-form-dialog.tsx", 100],
    ["components/rule-sets-panel.tsx", 300],
    ["components/downloader-config-section.tsx", 300],
  ]) {
    const source = readFileSync(path.join(WEB_DIR, rel), "utf8");
    const lines = source.split("\n").length;
    assert.ok(lines <= maxLines, `Expected ${rel} <= ${maxLines} lines, got ${lines}`);
  }
  assert.equal(existsSync(path.join(WEB_DIR, "lib/rule-set-summary.ts")), true);
});

test("library-detail-view and library-item-detail-view are decomposed", () => {
  for (const [rel, maxLines] of [
    ["components/library-detail-view.tsx", 400],
    ["components/library-item-detail-view.tsx", 400],
  ]) {
    const source = readFileSync(path.join(WEB_DIR, rel), "utf8");
    const lines = source.split("\n").length;
    assert.ok(lines <= maxLines, `Expected ${rel} <= ${maxLines} lines, got ${lines}`);
  }
  assert.equal(existsSync(path.join(WEB_DIR, "lib/library-detail-helpers.ts")), true);
  assert.equal(existsSync(path.join(WEB_DIR, "lib/library-item-format.ts")), true);
});

test("subscription-inspector-view, video-player, and player-controls are decomposed", () => {
  for (const [rel, maxLines] of [
    ["components/subscription-inspector-view.tsx", 400],
    ["components/player/video-player.tsx", 400],
    ["components/player/player-controls.tsx", 400],
  ]) {
    const source = readFileSync(path.join(WEB_DIR, rel), "utf8");
    const lines = source.split("\n").length;
    assert.ok(lines <= maxLines, `Expected ${rel} <= ${maxLines} lines, got ${lines}`);
  }
  assert.equal(existsSync(path.join(WEB_DIR, "lib/subscription-inspector.ts")), true);
  assert.equal(existsSync(path.join(WEB_DIR, "lib/player/player-ui.ts")), true);
});

function collectFilesMatching(dir, pattern) {
  const results = [];
  if (!existsSync(dir)) return results;
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      results.push(...collectFilesMatching(full, pattern));
    } else if (entry.isFile() && pattern.test(entry.name)) {
      results.push(full);
    }
  }
  return results;
}

test("media-detail-view is decomposed and shares MediaDetailScaffold & DetailPageNav", () => {
  const mediaDetailSource = readFileSync(
    path.join(WEB_DIR, "components/media-detail-view.tsx"),
    "utf8",
  );
  const mediaDetailLines = mediaDetailSource.split("\n").length;
  assert.ok(
    mediaDetailLines <= 300,
    `Expected components/media-detail-view.tsx <= 300 lines, got ${mediaDetailLines}`,
  );

  for (const rel of [
    "components/detail-page-nav.tsx",
    "components/media-detail/media-detail-scaffold.tsx",
    "components/media-detail/detail-metadata-widgets.tsx",
    "lib/media-detail-helpers.ts",
  ]) {
    assert.equal(existsSync(path.join(WEB_DIR, rel)), true, `Missing shared file: ${rel}`);
  }

  for (const removedRel of [
    "components/library-item/item-metadata-widgets.tsx",
    "components/library-item/item-detail-fallback.tsx",
  ]) {
    assert.equal(
      existsSync(path.join(WEB_DIR, removedRel)),
      false,
      `Duplicate module should be removed: ${removedRel}`,
    );
  }

  const componentFiles = collectFilesMatching(
    path.join(WEB_DIR, "components"),
    /\.(ts|tsx)$/,
  );
  const defSites = [];
  for (const file of componentFiles) {
    const content = readFileSync(file, "utf8");
    if (/\bfunction\s+(?:ExpandablePlot|SourceLink)\b/.test(content)) {
      defSites.push(path.relative(WEB_DIR, file));
    }
  }
  assert.deepEqual(defSites, ["components/media-detail/detail-metadata-widgets.tsx"]);
});

test("detail views do not branch on theme or NetflixBackButton directly", () => {
  const forbidden = /\b(?:useTheme|NetflixBackButton)\b/;
  for (const rel of [
    "components/library-item-detail-view.tsx",
    "components/library-item/use-library-item-controller.ts",
    "components/media-detail-view.tsx",
    "components/media-detail/use-media-detail.ts",
    "components/subscription-inspector-view.tsx",
    "components/subscription-inspector/use-subscription-inspector.ts",
  ]) {
    const content = readFileSync(path.join(WEB_DIR, rel), "utf8");
    assert.doesNotMatch(
      content,
      forbidden,
      `${rel} should use DetailPageNav / MediaDetailScaffold instead of useTheme / NetflixBackButton`,
    );
  }
});

test("globals.css is split into modular stylesheets under app/styles", () => {
  const globalsPath = path.join(WEB_DIR, "app/globals.css");
  const globalsSource = readFileSync(globalsPath, "utf8");
  const globalsLines = globalsSource.split("\n").length;
  assert.ok(globalsLines <= 50, `Expected app/globals.css <= 50 lines, got ${globalsLines}`);

  const imports = [...globalsSource.matchAll(/@import\s+"(\.\/styles\/[^"]+)";/g)].map(
    (m) => m[1],
  );
  assert.ok(
    imports.length >= 6,
    `Expected >= 6 @import "./styles/..." modules, got ${imports.length}`,
  );
  for (const relImport of imports) {
    const target = path.resolve(path.dirname(globalsPath), relImport);
    assert.equal(existsSync(target), true, `Missing stylesheet module: ${relImport}`);
  }

  const styleFiles = collectFilesMatching(path.join(WEB_DIR, "app/styles"), /\.css$/);
  for (const styleFile of styleFiles) {
    const lines = readFileSync(styleFile, "utf8").split("\n").length;
    assert.ok(
      lines <= 800,
      `Expected ${path.relative(WEB_DIR, styleFile)} <= 800 lines, got ${lines}`,
    );
  }
});

test("modularized component subdirectories stay under 600 lines per file and eslint enforces guardrails", () => {
  const componentsDir = path.join(WEB_DIR, "components");
  const subdirs = readdirSync(componentsDir, { withFileTypes: true })
    .filter((entry) => entry.isDirectory())
    .map((entry) => path.join(componentsDir, entry.name));

  const oversized = [];
  for (const subdir of subdirs) {
    for (const file of collectFilesMatching(subdir, /\.(ts|tsx)$/)) {
      const lines = readFileSync(file, "utf8").split("\n").length;
      if (lines > 600) {
        oversized.push(`${path.relative(WEB_DIR, file)}: ${lines}`);
      }
    }
  }
  assert.deepEqual(oversized, []);

  const eslintConfig = readFileSync(path.join(WEB_DIR, "eslint.config.mjs"), "utf8");
  assert.match(eslintConfig, /["']max-lines["']/);
  assert.match(eslintConfig, /["']no-restricted-imports["']/);
});

test("phase 5a: watch-stats-panel, media-track-rows, api/subscriptions, media-activity-section, and discover-view are decomposed", () => {
  for (const [rel, maxLines] of [
    ["components/watch-stats-panel.tsx", 300],
    ["components/media-track-rows.tsx", 550],
    ["lib/api/subscriptions.ts", 550],
    ["components/media-activity-section.tsx", 400],
    ["components/discover-view.tsx", 450],
  ]) {
    const source = readFileSync(path.join(WEB_DIR, rel), "utf8");
    const lines = source.split("\n").length;
    assert.ok(lines <= maxLines, `Expected ${rel} <= ${maxLines} lines, got ${lines}`);
  }

  for (const rel of [
    "lib/api/subscription-types.ts",
    "lib/media-track-format.ts",
    "lib/watch-stats-helpers.ts",
    "lib/media-activity-helpers.ts",
  ]) {
    assert.equal(existsSync(path.join(WEB_DIR, rel)), true, `Missing pure helper: ${rel}`);
  }
});

test("phase 5b: playback api, library-manage-view, library-filter-bar, search-command, and subscribe-dialog are decomposed and zero files exceed 800 lines", () => {
  for (const [rel, maxLines] of [
    ["lib/api/playback.ts", 520],
    ["components/library-manage-view.tsx", 550],
    ["components/library-filter-bar.tsx", 380],
    ["components/search-command.tsx", 480],
    ["components/subscribe-dialog.tsx", 420],
  ]) {
    const source = readFileSync(path.join(WEB_DIR, rel), "utf8");
    const lines = source.split("\n").length;
    assert.ok(lines <= maxLines, `Expected ${rel} <= ${maxLines} lines, got ${lines}`);
  }

  for (const rel of [
    "lib/api/playback-types.ts",
    "lib/library-manage-helpers.ts",
    "lib/library-filter-helpers.ts",
    "lib/search-palette.ts",
    "lib/subscribe-dialog-helpers.ts",
  ]) {
    assert.equal(existsSync(path.join(WEB_DIR, rel)), true, `Missing pure helper: ${rel}`);
  }

  const eslintConfig = readFileSync(path.join(WEB_DIR, "eslint.config.mjs"), "utf8");
  assert.doesNotMatch(eslintConfig, /\b1150\b/);

  const oversized = [];
  for (const dirName of ["app", "components", "lib"]) {
    for (const file of collectFilesMatching(path.join(WEB_DIR, dirName), /\.(ts|tsx)$/)) {
      const lines = readFileSync(file, "utf8").split("\n").length;
      if (lines > 800) {
        oversized.push(`${path.relative(WEB_DIR, file)}: ${lines}`);
      }
    }
  }
  assert.deepEqual(oversized, []);
});
