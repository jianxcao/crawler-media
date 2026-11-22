import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import React, { act } from "react";
import { create } from "react-test-renderer";
import ts from "typescript";

const require = createRequire(import.meta.url);
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const webRoot = fileURLToPath(new URL("../", import.meta.url));

function transpileFile(relPath, resolveImport) {
  const source = readFileSync(path.join(webRoot, relPath), "utf8");
  const js = ts.transpileModule(source, {
    fileName: relPath,
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      jsx: ts.JsxEmit.ReactJSX,
      target: ts.ScriptTarget.ES2022,
    },
  }).outputText;
  const mod = { exports: {} };
  new Function("require", "module", "exports", js)(resolveImport, mod, mod.exports);
  return mod.exports;
}

function withFakeWindow(initialUrl, fn) {
  const prevWindow = globalThis.window;
  let currentUrl = new URL(initialUrl);
  globalThis.window = {
    get location() {
      return {
        href: currentUrl.href,
        search: currentUrl.search,
      };
    },
    history: {
      state: null,
      replaceState(_state, _title, url) {
        currentUrl = new URL(String(url), currentUrl.origin);
      },
    },
  };
  return Promise.resolve()
    .then(() => fn(() => currentUrl.href))
    .finally(() => {
      if (prevWindow === undefined) delete globalThis.window;
      else globalThis.window = prevWindow;
    });
}

function textOf(node) {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  return (node.children ?? []).map(textOf).join("");
}

function buildSearchSystem({
  permissions = { isAdmin: true, canSearch: true },
  configuredSites = [],
  initialPresets = [],
  onUpdatePresets,
} = {}) {
  const calls = { listPresets: 0, listSites: 0, updates: [] };
  const cache = new Map();

  class FakeHttpError extends Error {
    constructor(message, status = 400) {
      super(message);
      this.status = status;
    }
  }

  const resolveImport = (spec) => {
    if (cache.has(spec)) return cache.get(spec);
    let loaded;
    if (spec === "@/lib/categories") {
      loaded = transpileFile("lib/categories.ts", resolveImport);
    } else if (spec === "@/lib/use-tab-param") {
      loaded = transpileFile("lib/use-tab-param.ts", resolveImport);
    } else if (spec === "@/lib/search-prefs") {
      loaded = transpileFile("lib/search-prefs.tsx", resolveImport);
    } else if (spec === "@/components/search-settings") {
      loaded = transpileFile("components/search-settings.tsx", resolveImport);
    } else if (spec === "@/lib/permissions") {
      loaded = { usePermissions: () => permissions };
    } else if (spec === "@/lib/api/search") {
      loaded = {
        listSearchPresets: async () => {
          calls.listPresets += 1;
          return initialPresets;
        },
        updateSearchPresets: async (next) => {
          calls.updates.push(next);
          if (onUpdatePresets) return onUpdatePresets(next);
          return next;
        },
      };
    } else if (spec === "@/lib/api/sites") {
      loaded = {
        listSiteCatalog: async () => [],
        listConfiguredSites: async () => {
          calls.listSites += 1;
          return configuredSites;
        },
        listSiteBoostStats: async () => ({}),
      };
    } else if (spec === "@/lib/use-visible-polling") {
      loaded = { useVisiblePolling: () => {} };
    } else if (spec === "@/lib/backdrop") {
      loaded = { useBackdrop: () => ({ backdrop: null }) };
    } else if (spec === "@/lib/http") {
      loaded = { HttpError: FakeHttpError };
    } else if (spec === "@/components/feedback") {
      loaded = { useConfirm: () => async () => true };
    } else if (spec === "@/components/icons") {
      loaded = {
        PlusIcon: () => React.createElement("span", null, "+"),
        ServerIcon: () => React.createElement("span", null, "S"),
        GripIcon: () => React.createElement("span", null, "::"),
      };
    } else if (spec === "@/vendor/liquid-glass") {
      loaded = {
        LiquidGlassButton: ({ checked, onCheckedChange, children, ...rest }) =>
          React.createElement(
            "button",
            {
              type: "button",
              role: "switch",
              "aria-checked": checked,
              onClick: () => onCheckedChange?.(!checked),
              ...rest,
            },
            children,
          ),
      };
    } else if (spec === "@/components/settings/sites/cdp-browser-section") {
      loaded = {
        CdpBrowserSection: () => React.createElement("div", { "data-testid": "cdp-panel" }, "CDP面板"),
      };
    } else if (spec === "@/components/settings/sites/site-form") {
      loaded = {
        AddSitePanel: () => React.createElement("div", { "data-testid": "add-site-panel" }, "添加站点面板"),
      };
    } else if (spec === "@/components/settings/sites/site-row") {
      loaded = {
        QueueCongestionTip: () => null,
        SiteRow: ({ site }) => React.createElement("div", null, site.name),
      };
    } else if (spec === "nanoid") {
      loaded = { nanoid: () => "fixed123" };
    } else {
      return require(spec);
    }
    cache.set(spec, loaded);
    return loaded;
  };

  const siteConfig = transpileFile("components/site-config-section.tsx", resolveImport);
  const searchPrefs = resolveImport("@/lib/search-prefs");
  const searchSettings = resolveImport("@/components/search-settings");
  return { calls, siteConfig, searchPrefs, searchSettings };
}

test("SiteConfigSection shows right action only on sites/search tabs, hides on cdp, and consumes createPresetNonce once", async () => {
  await withFakeWindow("http://localhost/settings/sites", async (getHref) => {
    const { siteConfig, searchPrefs } = buildSearchSystem({
      configuredSites: [{ id: "site-1", name: "HDArea", profile_id: "hdarea", enabled: true }],
    });

    let renderer;
    await act(async () => {
      renderer = create(
        React.createElement(
          searchPrefs.SearchPrefsProvider,
          null,
          React.createElement(siteConfig.SiteConfigSection),
        ),
      );
    });

    const findButton = (label) =>
      renderer.root
        .findAllByType("button")
        .find((btn) => textOf(btn).includes(label));

    const hasEditorHeading = () =>
      renderer.root
        .findAllByType("h3")
        .some((h) => textOf(h).includes("新建自定义分类"));

    // 1. Default tab = "sites" -> shows "添加站点", hides "新建自定义分类"
    assert.equal(Boolean(findButton("添加站点")), true, "sites tab must render 添加站点 button");
    assert.equal(Boolean(findButton("新建自定义分类")), false);

    // 2. Switch to "cdp" -> neither "添加站点" nor "新建自定义分类" is rendered!
    await act(async () => {
      findButton("CDP & 浏览器").props.onClick();
    });
    assert.equal(getHref(), "http://localhost/settings/sites?tab=cdp");
    assert.equal(Boolean(findButton("添加站点")), false, "cdp tab must not show 添加站点");
    assert.equal(Boolean(findButton("新建自定义分类")), false, "cdp tab must not show 新建自定义分类");

    // 3. Switch to "search" -> shows "新建自定义分类", editor is closed initially
    await act(async () => {
      findButton("搜索分类").props.onClick();
    });
    assert.equal(getHref(), "http://localhost/settings/sites?tab=search");
    assert.equal(Boolean(findButton("新建自定义分类")), true, "search tab must render 新建自定义分类 button");
    assert.equal(Boolean(findButton("添加站点")), false);
    assert.equal(hasEditorHeading(), false, "PresetEditor should not open automatically");

    // 4. Click "新建自定义分类" -> opens PresetEditor
    await act(async () => {
      findButton("新建自定义分类").props.onClick();
    });
    assert.equal(hasEditorHeading(), true, "clicking 新建自定义分类 must open PresetEditor");

    // 5. Switch away to "cdp" and back to "search" -> must NOT re-open blank PresetEditor!
    await act(async () => {
      findButton("CDP & 浏览器").props.onClick();
    });
    assert.equal(hasEditorHeading(), false);
    await act(async () => {
      findButton("搜索分类").props.onClick();
    });
    assert.equal(
      hasEditorHeading(),
      false,
      "switching back to search tab must not re-open blank 新建自定义分类 panel",
    );

    // 6. Clicking "新建自定义分类" again on search tab still opens PresetEditor
    await act(async () => {
      findButton("新建自定义分类").props.onClick();
    });
    assert.equal(hasEditorHeading(), true);

    await act(async () => {
      renderer.unmount();
    });
  });
});

test("legacy /settings/search route redirects to /settings/sites?tab=search", async () => {
  let navigatedTo = null;
  const SettingsSectionPage = transpileFile("app/(app)/settings/[section]/page.tsx", (spec) => {
    if (spec === "react-router-dom") {
      return {
        useParams: () => ({ section: "search" }),
        useSearchParams: () => [new URLSearchParams()],
        Navigate: ({ to, replace }) => {
          navigatedTo = { to, replace };
          return null;
        },
      };
    }
    if (spec === "@/components/settings-view") {
      return { SettingsPanel: () => null };
    }
    if (spec === "@/src/not-found") {
      return { NotFound: () => null };
    }
    if (spec === "@/lib/settings-sections") {
      return { settingsSections: [{ id: "sites" }] };
    }
    return require(spec);
  }).default;

  let renderer;
  await act(async () => {
    renderer = create(React.createElement(SettingsSectionPage));
  });
  assert.deepEqual(navigatedTo, { to: "/settings/sites?tab=search", replace: true });
  await act(async () => {
    renderer.unmount();
  });
});

test("SearchPrefsProvider loads presets for members with canSearch and skips when canSearch is false", async () => {
  const adminPreset = {
    type: "preset",
    id: "p-shared",
    name: "管理员预设",
    visible: true,
    categories: ["movie"],
    site_ids: [],
    poster_mode: false,
    skip_history: false,
  };

  let observedMemberTabs = [];
  const memberSys = buildSearchSystem({
    permissions: { isAdmin: false, canSearch: true },
    initialPresets: [adminPreset],
  });
  function ProbeMember() {
    const { visibleTabs } = memberSys.searchPrefs.useSearchPrefs();
    observedMemberTabs = visibleTabs;
    return null;
  }

  let r1;
  await act(async () => {
    r1 = create(
      React.createElement(
        memberSys.searchPrefs.SearchPrefsProvider,
        null,
        React.createElement(ProbeMember),
      ),
    );
  });
  assert.equal(memberSys.calls.listPresets, 1, "member with canSearch must fetch /search/presets");
  assert.equal(observedMemberTabs.length, 1);
  assert.equal(observedMemberTabs[0].id, "p-shared");
  await act(async () => {
    r1.unmount();
  });

  const restrictedSys = buildSearchSystem({
    permissions: { isAdmin: false, canSearch: false },
    initialPresets: [adminPreset],
  });
  let r2;
  await act(async () => {
    r2 = create(
      React.createElement(
        restrictedSys.searchPrefs.SearchPrefsProvider,
        null,
        React.createElement("div"),
      ),
    );
  });
  assert.equal(restrictedSys.calls.listPresets, 0, "user without canSearch must not fetch presets");
  await act(async () => {
    r2.unmount();
  });
});

test("SearchSection warns on ghost deleted site_ids and PresetEditor lets user remove them without silent widening", async () => {
  const presetWithGhost = {
    type: "preset",
    id: "p-mixed",
    name: "混合专搜",
    visible: true,
    categories: ["movie"],
    site_ids: ["site-live", "site-ghost"],
    poster_mode: false,
    skip_history: false,
  };
  const { calls, searchPrefs, searchSettings } = buildSearchSystem({
    configuredSites: [{ id: "site-live", name: "M-Team", profile_id: "mteam", enabled: true }],
    initialPresets: [presetWithGhost],
  });

  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(
        searchPrefs.SearchPrefsProvider,
        null,
        React.createElement(searchSettings.SearchSection),
      ),
    );
  });

  // 1. Must NOT silently mutate/save on mount; must display "含已删站点" warning
  assert.equal(calls.updates.length, 0, "mounting must not silently wipe ghost site_ids");
  const fullText = textOf(renderer.root);
  assert.match(fullText, /含已删站点/, "preset row must warn when site_ids contains deleted site");

  // 2. Open editor -> shows live site chip AND removable ghost chip ("已删除站点" + "×")
  const editBtn = renderer.root
    .findAllByType("button")
    .find((btn) => textOf(btn).trim() === "编辑");
  assert.ok(editBtn);
  await act(async () => {
    editBtn.props.onClick();
  });

  const ghostChip = renderer.root
    .findAllByType("button")
    .find((btn) => textOf(btn).includes("已删除站点") && textOf(btn).includes("×"));
  assert.ok(ghostChip, "PresetEditor must render removable 已删除站点 chip with ×");

  // 3. Click ghost chip to toggle off, then save -> saves cleaned ["site-live"]
  await act(async () => {
    ghostChip.props.onClick();
  });
  const saveBtn = renderer.root
    .findAllByType("button")
    .find((btn) => textOf(btn).trim() === "保存");
  await act(async () => {
    saveBtn.props.onClick();
  });
  assert.equal(calls.updates.length, 1);
  assert.deepEqual(calls.updates[0][0].site_ids, ["site-live"]);
  await act(async () => {
    renderer.unmount();
  });

  // 4. Even when 0 configured sites remain, ghost site_ids are still removable in PresetEditor
  const onlyGhostPreset = {
    type: "preset",
    id: "p-orphan",
    name: "孤立专搜",
    visible: true,
    categories: [],
    site_ids: ["site-removed-all"],
    poster_mode: false,
    skip_history: false,
  };
  const zeroSitesSys = buildSearchSystem({
    configuredSites: [],
    initialPresets: [onlyGhostPreset],
  });
  let rZero;
  await act(async () => {
    rZero = create(
      React.createElement(
        zeroSitesSys.searchPrefs.SearchPrefsProvider,
        null,
        React.createElement(zeroSitesSys.searchSettings.SearchSection),
      ),
    );
  });
  assert.match(textOf(rZero.root), /含已删站点/);
  const editOrphanBtn = rZero.root
    .findAllByType("button")
    .find((btn) => textOf(btn).trim() === "编辑");
  await act(async () => {
    editOrphanBtn.props.onClick();
  });
  const orphanChip = rZero.root
    .findAllByType("button")
    .find((btn) => textOf(btn).includes("已删除站点") && textOf(btn).includes("×"));
  assert.ok(orphanChip, "PresetEditor must render ghost chip even when configuredSites is empty");
  await act(async () => {
    rZero.unmount();
  });
});
