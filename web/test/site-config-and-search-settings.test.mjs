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
      loaded = {
        useConfirm: () => async () => true,
        useToast: () => ({ success() {}, error() {}, info() {} }),
      };
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

test("SiteRow avoids duplicate 已停用 badge and surfaces toast + inline feedback on 立即签到", async () => {
  const toasts = { success: [], error: [] };
  let checkInImpl = async () => ({
    id: "site-hda",
    name: "HDArea",
    url: "https://hdarea.club",
    profile_id: "hdarea",
    enabled: false,
    auth_type: "cookie",
    maintenance_via: "http",
  });

  const DropdownStub = {
    Root: ({ children }) => React.createElement("div", null, children),
    Trigger: ({ children }) => React.createElement("div", null, children),
    Portal: ({ children }) => React.createElement("div", null, children),
    Content: ({ children }) => React.createElement("div", null, children),
    Item: ({ children, onSelect, disabled }) =>
      React.createElement(
        "button",
        { type: "button", disabled, onClick: () => onSelect?.() },
        children,
      ),
  };

  const resolveImport = (spec) => {
    if (spec === "@radix-ui/react-dropdown-menu") return DropdownStub;
    if (spec === "react-router-dom") {
      return { Link: ({ children, to }) => React.createElement("a", { href: to }, children) };
    }
    if (spec === "@/components/feedback") {
      return {
        useConfirm: () => async () => true,
        useToast: () => ({
          success: (msg) => toasts.success.push(msg),
          error: (msg) => toasts.error.push(msg),
          info() {},
        }),
      };
    }
    if (spec === "@/components/icons") {
      return {
        ChevronDownIcon: () => React.createElement("span", null, "v"),
        MoreIcon: () => React.createElement("span", null, "..."),
      };
    }
    if (spec === "@/components/settings/sites/boost-settings-modal") {
      return {
        BoostReadout: () => null,
        BoostSettingsModal: () => null,
      };
    }
    if (spec === "@/components/settings/sites/site-form") {
      return {
        AUTH_TYPE_LABEL: { cookie: "Cookie", apikey: "API Key", credential: "账密", none: "无" },
        SiteForm: () => null,
      };
    }
    if (spec === "@/lib/api/sites") {
      return {
        checkInSite: (id) => checkInImpl(id),
        deleteSite: async () => ({ site_id: "site-hda" }),
        reverifySite: async (id) => checkInImpl(id),
        setSiteEnabled: async (_id, enabled) => ({ ...(await checkInImpl(_id)), enabled }),
        setSiteRatioBoost: async (id) => checkInImpl(id),
        updateSite: async (id) => checkInImpl(id),
      };
    }
    if (spec === "@/lib/download-tasks") {
      return { useDownloadTasks: () => ({ tasks: [] }) };
    }
    if (spec === "@/lib/format") {
      return { formatBytes: (n) => `${n} B` };
    }
    return require(spec);
  };

  const { SiteRow } = transpileFile("components/settings/sites/site-row.tsx", resolveImport);

  const disabledSite = {
    id: "site-hda",
    name: "HDArea",
    url: "https://hdarea.club",
    profile_id: "hdarea",
    cookie: null,
    api_key: null,
    rss_url: null,
    proxy: null,
    rate_limit_per_minute: null,
    cdp_url: null,
    downloader_id: null,
    enabled: false,
    auth_type: "cookie",
    maintenance_via: "http",
  };

  const changed = [];
  const errors = [];
  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(SiteRow, {
        item: { profile_id: "hdarea", auth_types: ["cookie"], supports_check_in: true },
        site: disabledSite,
        expanded: false,
        onToggle() {},
        onOpen() {},
        onChanged: (next) => changed.push(next),
        onDeleted() {},
        onError: (msg) => errors.push(msg),
      }),
    );
  });

  try {
    // 1. Disabled site must render "已停用" only ONCE (not duplicated adjacent badges!)
    const rowText = textOf(renderer.root);
    const disabledOccurrences = rowText.match(/已停用/g) ?? [];
    assert.equal(
      disabledOccurrences.length,
      1,
      `Disabled SiteRow must render '已停用' once, got ${disabledOccurrences.length} in: ${rowText}`,
    );

    // 2. Click "立即签到" -> succeeds with toast.success feedback
    const checkInBtn = renderer.root
      .findAllByType("button")
      .find((btn) => textOf(btn).trim() === "立即签到");
    assert.ok(checkInBtn, "SiteRow menu must render 立即签到 action");

    await act(async () => {
      checkInBtn.props.onClick();
    });
    assert.equal(changed.length, 1);
    assert.equal(toasts.success.length, 1, "Successful checkInSite must trigger toast.success");
    assert.match(toasts.success[0], /签到/);
    assert.match(
      textOf(renderer.root),
      /已触发签到/,
      "Successful checkInSite must display inline 已触发签到 actionStatus",
    );

    // 3. Click "立即签到" when checkInSite rejects -> surfaces toast.error feedback
    checkInImpl = async () => {
      throw new Error("Cookie 已失效");
    };
    await act(async () => {
      checkInBtn.props.onClick();
    });
    assert.equal(toasts.error.length, 1, "Failed checkInSite must trigger toast.error");
    assert.match(toasts.error[0], /Cookie 已失效/);
  } finally {
    await act(async () => renderer.unmount());
  }
});

test("CdpBrowserSection persists browser settings before syncCdp/startObscura, polls Obscura status after 202 start & save, and styles toggle thumb with --accent-ink", async () => {
  const sequence = [];
  let statusQueue = [
    {
      installed: false,
      running: false,
      downloading: false,
      port: 0,
      cdp_url: null,
      expected_version: "v0.2.3",
      reported_version: null,
      binary_path: "/bin/obscura",
      install_policy: "preinstalled",
      last_error: null,
      not_started_reason: "switch_off",
    },
  ];
  const scheduledTimers = [];
  const clearedTimers = new Set();
  const prevSetTimeout = globalThis.setTimeout;
  const prevClearTimeout = globalThis.clearTimeout;
  let timerIdSeq = 1000;

  globalThis.setTimeout = (fn, ms) => {
    const id = ++timerIdSeq;
    scheduledTimers.push({ id, fn, ms });
    return id;
  };
  globalThis.clearTimeout = (id) => {
    clearedTimers.add(id);
  };

  class FakeHttpError extends Error {
    constructor(message, status = 400) {
      super(message);
      this.status = status;
    }
  }

  let savedSettings = {
    cdp: { enabled: false, url: "" },
    obscura: { install: "preinstalled" },
    user_agent: "crawler-media/0.1.0",
    managed_browser_sites: 2,
    incompatible_endpoints: {},
  };

  const resolveImport = (spec) => {
    if (spec === "@/lib/http") return { HttpError: FakeHttpError };
    if (spec === "@/lib/api/browser-settings") {
      return {
        fetchBrowserSettings: async () => {
          sequence.push({ op: "fetchBrowserSettings" });
          return savedSettings;
        },
        updateBrowserSettings: async (payload) => {
          sequence.push({ op: "updateBrowserSettings", payload });
          savedSettings = {
            ...savedSettings,
            cdp: { ...payload.cdp },
            obscura: { ...payload.obscura },
            user_agent: payload.user_agent,
          };
          return savedSettings;
        },
        fetchObscuraStatus: async () => {
          sequence.push({ op: "fetchObscuraStatus" });
          if (statusQueue.length > 1) return statusQueue.shift();
          return statusQueue[0];
        },
        startObscura: async () => {
          sequence.push({ op: "startObscura", savedInstall: savedSettings.obscura.install });
          return {
            ...statusQueue[0],
            running: false,
            downloading: true,
            not_started_reason: null,
          };
        },
        stopObscura: async () => {
          sequence.push({ op: "stopObscura" });
          return { ...statusQueue[0], running: false, not_started_reason: "stopped_manually" };
        },
        syncCdp: async () => {
          sequence.push({
            op: "syncCdp",
            savedEnabled: savedSettings.cdp.enabled,
            savedUrl: savedSettings.cdp.url,
          });
          return { total_cookies_scanned: 12, updated_sites: 2, candidate_sites: [] };
        },
      };
    }
    return require(spec);
  };

  const { CdpBrowserSection } = transpileFile(
    "components/settings/sites/cdp-browser-section.tsx",
    resolveImport,
  );

  const flushNextTimer = async () => {
    const next = scheduledTimers.find((t) => !clearedTimers.has(t.id));
    assert.ok(next, "expected a scheduled poll timer");
    clearedTimers.add(next.id);
    await act(async () => {
      await next.fn();
    });
  };

  let renderer;
  try {
    await act(async () => {
      renderer = create(React.createElement(CdpBrowserSection));
    });

    // Check toggle thumb contrast class (Item 5)
    const toggleTrack = renderer.root
      .findAllByType("div")
      .find((d) => String(d.props.className ?? "").includes("peer-checked:bg-[var(--accent)]"));
    assert.ok(toggleTrack, "must render peer toggle track");
    assert.match(
      toggleTrack.props.className,
      /peer-checked:after:bg-\[var\(--accent-ink\)\]/,
      "toggle thumb must switch to var(--accent-ink) when checked",
    );

    // 1. Toggle CDP on + enter endpoint + click syncCdp WITHOUT clicking Save first
    const checkbox = renderer.root.find((n) => n.type === "input" && n.props.type === "checkbox");
    await act(async () => {
      checkbox.props.onChange({ target: { checked: true } });
    });
    const urlInput = renderer.root
      .findAll((n) => n.type === "input" && n.props.type === "text")
      .find((n) => String(n.props.placeholder ?? "").includes("9222"));
    await act(async () => {
      urlInput.props.onChange({ target: { value: "http://127.0.0.1:9222" } });
    });

    sequence.length = 0;
    const syncBtn = renderer.root
      .findAllByType("button")
      .find((btn) => textOf(btn).includes("立即从浏览器扫描并同步 Cookie"));
    assert.ok(syncBtn);
    await act(async () => {
      await syncBtn.props.onClick();
    });

    assert.deepEqual(
      sequence.map((s) => s.op),
      ["updateBrowserSettings", "syncCdp"],
      "handleTriggerSync must save browser settings before calling syncCdp",
    );
    assert.equal(sequence[1].savedEnabled, true);
    assert.equal(sequence[1].savedUrl, "http://127.0.0.1:9222");

    // 2. Clear URL, switch install policy to "download", click "手动启动 Obscura" -> saves first, then polls until running=true
    await act(async () => {
      urlInput.props.onChange({ target: { value: "" } });
    });
    const downloadPolicyBtn = renderer.root
      .findAllByType("button")
      .find((btn) => textOf(btn).includes("自动下载"));
    await act(async () => {
      downloadPolicyBtn.props.onClick();
    });

    sequence.length = 0;
    scheduledTimers.length = 0;
    clearedTimers.clear();
    statusQueue = [
      {
        ...statusQueue[0],
        installed: false,
        running: false,
        downloading: true,
        not_started_reason: null,
      },
      {
        ...statusQueue[0],
        installed: true,
        running: true,
        downloading: false,
        port: 19222,
        cdp_url: "http://127.0.0.1:19222",
        not_started_reason: null,
      },
    ];

    const startBtn = renderer.root
      .findAllByType("button")
      .find((btn) => textOf(btn).includes("手动启动 Obscura"));
    assert.ok(startBtn);
    await act(async () => {
      await startBtn.props.onClick();
    });

    assert.equal(sequence[0]?.op, "updateBrowserSettings");
    assert.equal(sequence[1]?.op, "startObscura");
    assert.equal(
      sequence[1]?.savedInstall,
      "download",
      "startObscura must see newly toggled obscuraInstall persisted in SQLite",
    );
    assert.match(textOf(renderer.root), /下载安装中/);

    // First poll tick -> still downloading; second poll tick -> running=true (port 19222) and stops polling
    await flushNextTimer();
    await flushNextTimer();
    assert.match(textOf(renderer.root), /运行中 \(端口: 19222\)/);
    const activeAfterRunning = scheduledTimers.filter((t) => !clearedTimers.has(t.id));
    assert.equal(activeAfterRunning.length, 0, "polling must stop once running === true");

    // 3. Save browser settings when managed Obscura is starting -> polls until last_error settles, and cancels on unmount
    statusQueue = [
      {
        ...statusQueue[0],
        installed: false,
        running: false,
        downloading: false,
        last_error: null,
        not_started_reason: null,
      },
      {
        ...statusQueue[0],
        installed: false,
        running: false,
        downloading: false,
        last_error: "binary checksum mismatch",
        not_started_reason: null,
      },
    ];
    scheduledTimers.length = 0;
    clearedTimers.clear();
    const saveBtn = renderer.root
      .findAllByType("button")
      .find((btn) => textOf(btn).includes("保存浏览器与网络设置"));
    await act(async () => {
      await saveBtn.props.onClick();
    });
    await flushNextTimer();
    assert.match(textOf(renderer.root), /binary checksum mismatch/);
    assert.equal(
      scheduledTimers.filter((t) => !clearedTimers.has(t.id)).length,
      0,
      "polling must stop once last_error settles",
    );
  } finally {
    if (renderer) {
      await act(async () => renderer.unmount());
    }
    globalThis.setTimeout = prevSetTimeout;
    globalThis.clearTimeout = prevClearTimeout;
  }
});
