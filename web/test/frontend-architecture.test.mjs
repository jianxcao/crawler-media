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
  assert.equal(mod.settingsSections.length, 10);
  const ids = mod.settingsSections.map((s) => s.id);
  assert.ok(ids.includes("overview"));
  assert.ok(ids.includes("appearance"));

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
