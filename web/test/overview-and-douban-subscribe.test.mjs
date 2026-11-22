import assert from "node:assert/strict";
import { existsSync, readFileSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import React, { act } from "react";
import { create } from "react-test-renderer";
import ts from "typescript";

const require = createRequire(import.meta.url);
globalThis.IS_REACT_ACT_ENVIRONMENT = true;

function compileTsRegistry(overrides = {}) {
  const cache = new Map();
  const rootUrl = new URL("../", import.meta.url);

  function resolveFile(baseUrl, specifier) {
    const raw = specifier.startsWith("@/")
      ? new URL(specifier.slice(2), rootUrl)
      : new URL(specifier, baseUrl);
    for (const suffix of ["", ".ts", ".tsx", "/index.ts", "/index.tsx"]) {
      const candidate = new URL(`${raw.href}${suffix}`);
      if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
    }
    throw new Error(`Cannot resolve ${specifier} from ${baseUrl.href}`);
  }

  function load(specifier, fromUrl = rootUrl) {
    if (specifier in overrides) return overrides[specifier];
    const targetUrl = resolveFile(fromUrl, specifier);
    if (cache.has(targetUrl.href)) return cache.get(targetUrl.href).exports;
    const mod = { exports: {} };
    cache.set(targetUrl.href, mod);
    const source = readFileSync(targetUrl, "utf8");
    const js = ts.transpileModule(source, {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
        jsx: ts.JsxEmit.ReactJSX,
        esModuleInterop: true,
      },
    }).outputText;
    const leaf = () => null;
    const stubPkg = new Proxy({}, { get: (_t, key) => (key === "__esModule" ? false : leaf) });
    new Function("require", "module", "exports", js)(
      (dep) => {
        if (dep in overrides) return overrides[dep];
        if (dep === "react") return React;
        if (dep === "react/jsx-runtime") return require("react/jsx-runtime");
        if (dep.startsWith("./") || dep.startsWith("../") || dep.startsWith("@/")) {
          return load(dep, targetUrl);
        }
        try {
          return require(dep);
        } catch {
          return stubPkg;
        }
      },
      mod,
      mod.exports,
    );
    return mod.exports;
  }

  return { load };
}

function makeBrowserEventEnv() {
  const docListeners = new Map();
  const winListeners = new Map();
  const add = (map) => (type, fn) => {
    if (!map.has(type)) map.set(type, new Set());
    map.get(type).add(fn);
  };
  const remove = (map) => (type, fn) => {
    map.get(type)?.delete(fn);
  };
  const emit = (map) => (type, event = {}) => {
    for (const fn of [...(map.get(type) ?? [])]) fn(event);
  };
  globalThis.document = {
    visibilityState: "visible",
    addEventListener: add(docListeners),
    removeEventListener: remove(docListeners),
  };
  globalThis.window = {
    location: { pathname: "/settings/overview", search: "", hash: "" },
    addEventListener: add(winListeners),
    removeEventListener: remove(winListeners),
  };
  return {
    emitDoc: emit(docListeners),
    emitWin: emit(winListeners),
  };
}

function makeLibraryPipeline(id, status = "error") {
  return {
    library_id: id,
    name: `库 ${id}`,
    kind: "tv",
    is_default: id === "lib-1",
    mode: "inplace",
    path: `/media/${id}`,
    library_root: `/media/${id}`,
    staging_path: null,
    status,
    narrative: "",
    checks: [],
  };
}

function makeHealth(overrides = {}) {
  return {
    status: "error",
    error_count: 5,
    warn_count: 0,
    site_check: {
      key: "sites",
      label: "资源搜索",
      status: "error",
      detail: "尚未启用站点",
      fix_section: "sites",
    },
    downloader_ok: false,
    sites_configured: false,
    downloaders_configured: false,
    issues: [],
    libraries: [
      makeLibraryPipeline("lib-1", "error"),
      makeLibraryPipeline("lib-2", "error"),
      makeLibraryPipeline("lib-3", "error"),
    ],
    ...overrides,
  };
}

test("createSubscriptionBody forwards source_title_ref when anchoring Douban candidate to TMDB", () => {
  const { load } = compileTsRegistry();
  const { createSubscriptionBody } = load("@/lib/subscription-form");

  const withDoubanSource = createSubscriptionBody({
    title_ref: "tmdb:tv:9001",
    source_title_ref: "douban:35000001",
    coverage: { kind: "tv", season: 1, episode_from: 1 },
    follow_future: true,
  });
  assert.equal(withDoubanSource.title_ref, "tmdb:tv:9001");
  assert.equal(withDoubanSource.source_title_ref, "douban:35000001");

  const withoutSource = createSubscriptionBody({
    title_ref: "tmdb:movie:603",
    source_title_ref: null,
  });
  assert.equal("source_title_ref" in withoutSource, false);
});

test("describeSubscriptionHealthBanner distinguishes missing sites/downloaders from real library errors without inflating 3 libraries into 5", async () => {
  const { load } = compileTsRegistry({
    "react-router-dom": {
      Link: ({ to, children, ...rest }) => React.createElement("a", { href: to, ...rest }, children),
    },
    "@/components/brand-loader": { BrandLoader: () => null },
    "@/components/content-empty-state": { ContentEmptyState: () => null },
    "@/components/h-scroller": { HScroller: ({ children }) => React.createElement("div", null, children) },
    "@/components/poster-image": { PosterImage: () => null },
    "@/components/poster-card": { PosterCardVisual: () => null },
    "@/components/subscribe-entry": {
      useSubscribeEntry: () => ({ subscriptions: [], refresh: async () => true }),
    },
    "@/lib/download-tasks": { useDownloadTasks: () => ({ tasks: [] }) },
    "@/lib/page-chrome": { usePageChrome: () => null },
    "@/lib/permissions": {
      usePermissions: () => ({ canManageSubscriptions: true, canSubscribe: true, isAdmin: true }),
    },
    "@/lib/use-media-query": { useIsMobile: () => false },
    "@/lib/use-scroll-restoration": { useScrollRestoration: () => () => {} },
    "@/lib/use-today-arrivals": {
      useTodaySubscriptionArrivals: () => ({ arrivals: [], failed: false }),
      useTodayArrivalGroups: () => [],
    },
    "@/lib/image-proxy": {
      cachedImageUrl: (u) => u,
      imageUrl: (u) => u,
      brandLogoUrl: (u) => u ?? "",
    },
    "@/lib/api/subscriptions": {
      checkSubscriptionAutomationReadiness: async () => makeHealth(),
      listRuleSets: async () => [],
    },
  });

  const { describeSubscriptionHealthBanner } = load("@/lib/subscription-form");

  assert.equal(describeSubscriptionHealthBanner(makeHealth({ status: "ok" })), null);
  assert.equal(describeSubscriptionHealthBanner(makeHealth({ status: "warn" })), null);

  // 0 sites + 0 downloaders on 3 libraries (error_count = 5)
  assert.equal(
    describeSubscriptionHealthBanner(makeHealth()),
    "尚未接入资源站点与下载器，订阅暂无法自动搜索和下载入库",
  );

  // Missing sites only
  assert.equal(
    describeSubscriptionHealthBanner(
      makeHealth({ sites_configured: false, downloaders_configured: true }),
    ),
    "尚未启用任何资源站点，订阅暂无法自动搜索资源",
  );

  // Missing downloaders only
  assert.equal(
    describeSubscriptionHealthBanner(
      makeHealth({ sites_configured: true, downloaders_configured: false }),
    ),
    "尚未配置默认下载器，订阅暂无法自动投递下载",
  );

  // Sites + downloaders configured, explicit library_error_count = 2 while error_count = 4
  assert.equal(
    describeSubscriptionHealthBanner(
      makeHealth({
        sites_configured: true,
        downloaders_configured: true,
        error_count: 4,
        library_error_count: 2,
      }),
    ),
    "2 个媒体库的入库链路有问题，相关订阅暂时无法自动下载入库（已下达的任务会自动重试）",
  );

  // Sites + downloaders configured, fallback to libraries.filter(status === "error") -> 1 library (not error_count 3)
  assert.equal(
    describeSubscriptionHealthBanner(
      makeHealth({
        sites_configured: true,
        downloaders_configured: true,
        error_count: 3,
        libraries: [
          makeLibraryPipeline("lib-1", "error"),
          makeLibraryPipeline("lib-2", "ok"),
          makeLibraryPipeline("lib-3", "ok"),
        ],
      }),
    ),
    "1 个媒体库的入库链路有问题，相关订阅暂时无法自动下载入库（已下达的任务会自动重试）",
  );

  // Sites + downloaders configured, 0 library errors (e.g. site cookie expired)
  assert.equal(
    describeSubscriptionHealthBanner(
      makeHealth({
        sites_configured: true,
        downloaders_configured: true,
        error_count: 1,
        library_error_count: 0,
        libraries: [makeLibraryPipeline("lib-1", "ok")],
      }),
    ),
    "订阅入库链路存在配置问题，暂时无法自动下载入库（已下达的任务会自动重试）",
  );

  // Both SubscriptionsView and NetflixSubscriptionsPage render the accurate banner
  const { SubscriptionsView } = load("@/components/subscriptions-view");
  const { NetflixSubscriptionsPage } = load("@/components/netflix/subscriptions-page");

  let glassView;
  await act(async () => {
    glassView = create(React.createElement(SubscriptionsView));
  });
  const glassText = JSON.stringify(glassView.toJSON());
  assert.match(glassText, /尚未接入资源站点与下载器，订阅暂无法自动搜索和下载入库/);
  assert.doesNotMatch(glassText, /5 个媒体库/);
  await act(async () => glassView.unmount());

  let netflixView;
  await act(async () => {
    netflixView = create(React.createElement(NetflixSubscriptionsPage));
  });
  const netflixText = JSON.stringify(netflixView.toJSON());
  assert.match(netflixText, /尚未接入资源站点与下载器，订阅暂无法自动搜索和下载入库/);
  assert.doesNotMatch(netflixText, /5 个媒体库/);
  await act(async () => netflixView.unmount());
});

test("NetworkProxyHealthPanel and PipelineHealthPanel deduplicate StrictMode/focus reloads and ignore stale overlapping responses", async () => {
  const { emitDoc, emitWin } = makeBrowserEventEnv();

  const overviewSrc = readFileSync(
    new URL("../components/settings-overview-section.tsx", import.meta.url),
    "utf8",
  );
  const subSettingsSrc = readFileSync(
    new URL("../components/subscription-settings-section.tsx", import.meta.url),
    "utf8",
  );
  assert.match(overviewSrc, /inFlightRef/);
  assert.match(overviewSrc, /reqIdRef/);
  assert.match(subSettingsSrc, /inFlightRef/);
  assert.match(subSettingsSrc, /reqIdRef/);

  const proxyRequests = [];
  const pipelineRequests = [];

  const { load } = compileTsRegistry({
    "react-router-dom": {
      Link: ({ to, children, ...rest }) => React.createElement("a", { href: to, ...rest }, children),
    },
    "@/components/brand-loader": { BrandLoader: () => React.createElement("span", null, "loading") },
    "@/components/rule-sets-panel": { RuleSetsPanel: () => null },
    "@/lib/api/search": { searchTitles: async () => ({ items: [] }) },
    "@/lib/api/metadata": {
      diagnoseProxySettings: () =>
        new Promise((resolve, reject) => {
          proxyRequests.push({ resolve, reject });
        }),
    },
    "@/lib/api/subscriptions": {
      checkSubscriptionAutomationReadiness: () =>
        new Promise((resolve, reject) => {
          pipelineRequests.push({ resolve, reject });
        }),
      previewSubscriptionDownloadRouting: async () => null,
    },
  });

  const { SettingsOverviewSection } = load("@/components/settings-overview-section");

  let view;
  await act(async () => {
    view = create(
      React.createElement(React.StrictMode, null, React.createElement(SettingsOverviewSection)),
    );
  });

  // Immediate focus + visibilitychange storm right after mount must NOT fire duplicate requests
  await act(async () => {
    emitWin("focus");
    emitDoc("visibilitychange");
    emitWin("focus");
  });

  assert.equal(
    proxyRequests.length,
    1,
    `expected 1 proxy diagnose request across StrictMode + focus storm, got ${proxyRequests.length}`,
  );
  assert.equal(
    pipelineRequests.length,
    1,
    `expected 1 pipeline readiness request across StrictMode + focus storm, got ${pipelineRequests.length}`,
  );

  // Resolve initial requests with healthy data
  await act(async () => {
    proxyRequests[0].resolve({
      active_proxy: "http://127.0.0.1:7890",
      douban_bypass: true,
      items: [
        {
          name: "TMDB",
          url: "https://api.themoviedb.org",
          is_proxied: true,
          route_label: "HTTP 代理",
          ok: true,
          latency_ms: 88,
          error: null,
        },
      ],
    });
    pipelineRequests[0].resolve({
      status: "ok",
      error_count: 0,
      library_error_count: 0,
      warn_count: 0,
      site_check: { key: "sites", label: "资源搜索", status: "ok", detail: "正常", fix_section: null },
      downloader_ok: true,
      sites_configured: true,
      downloaders_configured: true,
      issues: [],
      libraries: [makeLibraryPipeline("lib-1", "ok")],
    });
  });

  // Focus right after completion (< 1500ms cooldown) still does not re-probe
  await act(async () => {
    emitWin("focus");
    emitDoc("visibilitychange");
  });
  assert.equal(proxyRequests.length, 1);
  assert.equal(pipelineRequests.length, 1);

  // Manual "重新检测" / "重新体检" buttons pass force=true and bypass cooldown
  const buttons = view.root.findAllByType("button");
  const recheckProxyBtn = buttons.find((b) => JSON.stringify(b.children).includes("重新检测"));
  const recheckPipelineBtn = buttons.find((b) => JSON.stringify(b.children).includes("重新体检"));
  assert.ok(recheckProxyBtn, "must render 重新检测 button");
  assert.ok(recheckPipelineBtn, "must render 重新体检 button");

  await act(async () => {
    recheckProxyBtn.props.onClick();
    recheckPipelineBtn.props.onClick();
  });
  assert.equal(proxyRequests.length, 2, "manual 重新检测 must force a reload");
  assert.equal(pipelineRequests.length, 2, "manual 重新体检 must force a reload");

  // Trigger another forced reload while request #2 is still in flight -> request #3 becomes current
  await act(async () => {
    recheckProxyBtn.props.onClick();
    recheckPipelineBtn.props.onClick();
  });
  assert.equal(proxyRequests.length, 3);
  assert.equal(pipelineRequests.length, 3);

  // Stale request #2 resolves with failing items / error status -> must be ignored (no red flash!)
  await act(async () => {
    proxyRequests[1].resolve({
      active_proxy: null,
      douban_bypass: false,
      items: [
        {
          name: "TMDB",
          url: "https://api.themoviedb.org",
          is_proxied: false,
          route_label: "直连",
          ok: false,
          latency_ms: 0,
          error: "timeout",
        },
      ],
    });
    pipelineRequests[1].reject(new Error("aborted"));
  });
  const midText = JSON.stringify(view.toJSON());
  assert.doesNotMatch(
    midText,
    /部分外部元数据服务网络连接失败或超时/,
    "stale overlapping proxy response must not flash red error banner",
  );
  assert.doesNotMatch(
    midText,
    /体检加载失败/,
    "stale overlapping pipeline error must not flash failure banner",
  );

  // Latest request #3 resolves cleanly
  await act(async () => {
    proxyRequests[2].resolve({
      active_proxy: "http://127.0.0.1:7890",
      douban_bypass: true,
      items: [
        {
          name: "TMDB",
          url: "https://api.themoviedb.org",
          is_proxied: true,
          route_label: "HTTP 代理",
          ok: true,
          latency_ms: 62,
          error: null,
        },
      ],
    });
    pipelineRequests[2].resolve({
      status: "ok",
      error_count: 0,
      library_error_count: 0,
      warn_count: 0,
      site_check: { key: "sites", label: "资源搜索", status: "ok", detail: "正常", fix_section: null },
      downloader_ok: true,
      sites_configured: true,
      downloaders_configured: true,
      issues: [],
      libraries: [makeLibraryPipeline("lib-1", "ok")],
    });
  });
  assert.match(JSON.stringify(view.toJSON()), /62/);

  await act(async () => view.unmount());
});

test("listTodaySubscriptionArrivals preserves episode_number, air_date, days_ahead, and release_forecast", async () => {
  const tomorrow = new Date();
  tomorrow.setDate(tomorrow.getDate() + 1);
  const tomorrowStr = `${tomorrow.getFullYear()}-${String(tomorrow.getMonth() + 1).padStart(2, "0")}-${String(tomorrow.getDate()).padStart(2, "0")}`;

  const { load } = compileTsRegistry({
    "@/lib/http": {
      request: async (path) => {
        assert.equal(path, "/subscriptions/today-arrivals");
        return {
          ok: true,
          data: [
            {
              subscription_id: "sub-1",
              title: "两京十五日",
              coverage: { kind: "tv", season: 1, episode_from: 1, episode_to: null },
              status: "imported",
              season: 1,
              episode: 5,
            },
            {
              subscription_id: "sub-1",
              title: "两京十五日",
              coverage: { kind: "tv", season: 1, episode_from: 1, episode_to: null },
              status: "wanted",
              season: 1,
              episode: 9,
              air_date: tomorrowStr,
              release_forecast: {
                predicted_at: `${tomorrowStr}T12:00:00Z`,
                window_end: `${tomorrowStr}T16:00:00Z`,
                confidence: "bootstrap",
              },
            },
          ],
        };
      },
      unwrap: (p) => p.then((r) => r.data),
      resolveRequestUrl: (p) => p,
    },
  });

  const { listTodaySubscriptionArrivals } = load("@/lib/api/subscriptions");
  const rows = await listTodaySubscriptionArrivals();
  assert.equal(rows.length, 2);
  assert.equal(rows[0].season_number, 1);
  assert.equal(rows[0].episode_number, 5);
  assert.equal(rows[0].status, "imported");
  assert.equal(rows[0].days_ahead, 0);

  assert.equal(rows[1].season_number, 1);
  assert.equal(rows[1].episode_number, 9);
  assert.equal(rows[1].air_date, tomorrowStr);
  assert.equal(rows[1].expected_day, tomorrowStr);
  assert.equal(rows[1].days_ahead, 1);
  assert.equal(rows[1].release_forecast?.predicted_at, `${tomorrowStr}T12:00:00Z`);
});
