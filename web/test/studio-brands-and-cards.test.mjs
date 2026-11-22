import assert from "node:assert/strict";
import { existsSync, readFileSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import React, { act } from "react";
import { create } from "react-test-renderer";
import ts from "typescript";
import { loadLibrariesApi } from "./support/load-libraries-api.mjs";

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

test("getLibraryItemDetail parses brands and defaults missing brands to empty array", async () => {
  let payload = {
    media_item_id: "m-1",
    kind: "tv",
    title: "CCTV Show",
    year: 2024,
    brands: [
      {
        name: "CCTV-8",
        logo_url:
          "https://image.tmdb.org/t/p/h60_filter(negate,000,666)/2aXk02xBwG2Nhu4lftabfqSpwY3.png",
        kind: "network",
      },
      { name: "New Classics Media", logo_url: null, kind: "studio" },
    ],
    seasons: [1],
    files: [],
    next_file_cursor: null,
  };
  const api = loadLibrariesApi(
    {
      "@/lib/http": { request: async () => ({ ok: true, data: payload }), resolveRequestUrl: (p) => p },
      "@/lib/library-file-pages": { collectAllFilePages: async (first) => first.files },
    },
    { fallbackEmpty: true },
  );
  const detail = await api.getLibraryItemDetail("lib-1", "m-1");
  assert.deepEqual(detail.brands, [
    {
      name: "CCTV-8",
      logo_url:
        "https://image.tmdb.org/t/p/h60_filter(negate,000,666)/2aXk02xBwG2Nhu4lftabfqSpwY3.png",
      kind: "network",
    },
    { name: "New Classics Media", logo_url: null, kind: "studio" },
  ]);

  payload = { ...payload, brands: undefined };
  const withoutBrands = await api.getLibraryItemDetail("lib-1", "m-1");
  assert.deepEqual(withoutBrands.brands, []);
});

test("fetchDiscoveredTitleDetails parses info.brands for TMDB and Douban details", async () => {
  const { load } = compileTsRegistry({
    "@/lib/http": {
      request: async (path) => {
        if (path.startsWith("/media/douban/")) {
          return {
            ok: true,
            data: {
              item: { id: "35000001", title: "豆瓣剧", kind: "tv" },
              info: { brands: [] },
            },
          };
        }
        return {
          ok: true,
          data: {
            item: { id: "9001", title: "TMDB Show", kind: "tv", year: 2024 },
            info: {
              rating: "8.8",
              genres: ["剧情"],
              brands: [
                {
                  name: "HBO",
                  logo_url: "https://image.tmdb.org/t/p/h60_filter(negate,000,666)/hbo.png",
                  kind: "network",
                },
                { name: "A24", logo_url: null, kind: "studio" },
              ],
            },
          },
        };
      },
      resolveRequestUrl: (p) => `/api/v1/${p.replace(/^\//, "")}`,
    },
  });
  const discover = load("@/lib/api/discover");
  const tmdb = await discover.fetchDiscoveredTitleDetails("tmdb:tv:9001");
  assert.deepEqual(tmdb.info.brands, [
    {
      name: "HBO",
      logo_url: "https://image.tmdb.org/t/p/h60_filter(negate,000,666)/hbo.png",
      kind: "network",
    },
    { name: "A24", logo_url: null, kind: "studio" },
  ]);

  const douban = await discover.fetchDiscoveredTitleDetails("douban:35000001");
  assert.deepEqual(douban.info.brands, []);
});

test("libraryItemToMediaItem forwards rating, studio extent, and proxied studioLogo into MediaRow/PosterCard without brightness-0 invert", async () => {
  const { load } = compileTsRegistry({
    "react-router-dom": {
      Link: ({ to, children, ...rest }) => React.createElement("a", { href: to, ...rest }, children),
    },
    "@/components/artwork-image": {
      ArtworkImage: (props) => React.createElement("img", props),
    },
    "@/components/poster-image": {
      PosterImage: (props) => React.createElement("img", props),
    },
    "@/components/h-scroller": {
      HScroller: ({ children, className }) => React.createElement("div", { className }, children),
    },
    "@/components/subscribe-entry": {
      useSubscribeEntry: () => ({ canSubscribe: false, open: () => {}, subscriptionOf: () => undefined }),
    },
    "@/lib/media-detail": {
      useMediaDetail: () => ({ open: () => {} }),
    },
    "@/lib/use-media-query": {
      useMediaQuery: () => false,
    },
    "@/lib/use-tap-guard": {
      useTapGuard: () => ({
        onPointerDown: () => {},
        onPointerUp: () => {},
        onPointerCancel: () => {},
        onClick: () => {},
      }),
    },
    "@/lib/env": {
      publicEnv: { apiBaseUrl: "/api/v1" },
    },
    "@/lib/http": {
      request: async () => ({ ok: true, data: [] }),
      resolveRequestUrl: (p) => `/api/v1/${p.replace(/^\//, "")}`,
    },
    "@/lib/permissions": {
      usePermissions: () => ({ canManageLibraries: false }),
    },
    "@/lib/ui-prefs": {
      useUiPrefs: () => ({ prefs: { home: { rows: [] } } }),
    },
    "@/lib/use-visible-polling": {
      useVisiblePolling: () => {},
    },
    "@/lib/use-scroll-restoration": {
      useScrollRestoration: () => ({ current: null }),
    },
  });

  const { libraryItemToMediaItem, favoriteItemToMediaItem } = load("@/components/library-view");
  const { MediaRow } = load("@/components/media-row");

  const mapped = libraryItemToMediaItem({
    media_item_id: "item-1",
    kind: "tv",
    library_id: "lib-tv",
    title: "繁花",
    year: 2023,
    rating: 8.7,
    studio: "CCTV-8",
    studio_logo:
      "https://image.tmdb.org/t/p/h60_filter(negate,000,666)/2aXk02xBwG2Nhu4lftabfqSpwY3.png",
    poster_url: "/posters/file-1",
    backdrop_url: null,
    file_count: 30,
    seasons: [1],
    episode_count: 30,
    missing: false,
  });

  assert.equal(mapped.rating, 8.7);
  assert.equal(mapped.extent, "CCTV-8");
  assert.ok(
    mapped.studioLogo?.includes("/api/v1/images/proxy?url="),
    `expected proxied studioLogo, got ${mapped.studioLogo}`,
  );

  const favMapped = favoriteItemToMediaItem({
    media_item_id: "item-1",
    kind: "tv",
    library_id: "lib-tv",
    title: "繁花",
    year: 2023,
    rating: 8.7,
    studio: "CCTV-8",
    studio_logo:
      "https://image.tmdb.org/t/p/h60_filter(negate,000,666)/2aXk02xBwG2Nhu4lftabfqSpwY3.png",
    poster_url: "/posters/file-1",
    backdrop_url: null,
    file_count: 30,
    seasons: [1],
    episode_count: 30,
    missing: false,
    favorite_season_number: 1,
    favorite_episode_number: null,
  });
  assert.equal(favMapped.rating, 8.7);
  assert.equal(favMapped.extent, "CCTV-8");
  assert.equal(favMapped.studioLogo, mapped.studioLogo);

  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(MediaRow, {
        row: { id: "recent-tv", title: "最近添加", items: [mapped] },
        cardAction: "none",
        cardHref: () => "/library/lib-tv/item/item-1",
      }),
    );
  });
  const imgs = renderer.root.findAllByType("img");
  const logoImg = imgs.find((img) => img.props.src === mapped.studioLogo);
  assert.ok(logoImg, "PosterCard inside MediaRow must render the studioLogo image");
  assert.ok(
    !String(logoImg.props.className ?? "").includes("brightness-0"),
    "studioLogo must not apply brightness-0 so colored logos stay colored",
  );
  assert.ok(
    !String(logoImg.props.className ?? "").includes("invert"),
    "studioLogo must not apply invert on client side",
  );
  assert.match(JSON.stringify(renderer.toJSON()), /8\.7/);
  await act(async () => renderer.unmount());
});

test("BrandStrip caps at 4 items, falls back to text pill on null or error, and renders in MediaDetailHero", async () => {
  const { load } = compileTsRegistry({
    "react-router-dom": {
      Link: ({ to, children, ...rest }) => React.createElement("a", { href: to, ...rest }, children),
    },
    "@/components/artwork-image": {
      ArtworkImage: (props) => React.createElement("img", props),
    },
    "@/lib/env": {
      publicEnv: { apiBaseUrl: "/api/v1" },
    },
    "@/lib/http": {
      request: async () => ({ ok: true, data: [] }),
      resolveRequestUrl: (p) => `/api/v1/${p.replace(/^\//, "")}`,
    },
  });

  const { BrandStrip } = load("@/components/media-detail/brand-strip");
  const { MediaDetailHero } = load("@/components/media-detail/media-detail-hero");

  const brands = [
    {
      name: "CCTV-8",
      logo_url:
        "https://image.tmdb.org/t/p/h60_filter(negate,000,666)/2aXk02xBwG2Nhu4lftabfqSpwY3.png",
      kind: "network",
    },
    { name: "Tencent Video", logo_url: "https://cdn.example.com/tencent.png", kind: "network" },
    { name: "New Classics Media", logo_url: null, kind: "studio" },
    { name: "Jet Tone Films", logo_url: null, kind: "studio" },
    { name: "Fifth Overflow Brand", logo_url: null, kind: "studio" },
  ];

  let strip;
  await act(async () => {
    strip = create(React.createElement(BrandStrip, { brands }));
  });
  const badges = strip.root.findAll((node) => node.type === "span" && Boolean(node.props.title));
  assert.equal(badges.length, 4, "BrandStrip must cap displayed brands at 4");
  const logoImgs = strip.root.findAllByType("img");
  assert.equal(logoImgs.length, 2);
  assert.ok(logoImgs[0].props.src.startsWith("/api/v1/images/proxy?url="));
  assert.ok(!JSON.stringify(strip.toJSON()).includes("Fifth Overflow Brand"));

  // Simulate image load error on first logo -> falls back to text pill
  await act(async () => {
    logoImgs[0].props.onError();
  });
  assert.equal(strip.root.findAllByType("img").length, 1);
  assert.ok(JSON.stringify(strip.toJSON()).includes("CCTV-8"));
  await act(async () => strip.unmount());

  // Render inside MediaDetailHero
  let hero;
  await act(async () => {
    hero = create(
      React.createElement(MediaDetailHero, {
        item: {
          id: "9001",
          source: "tmdb",
          type: "tv",
          title: "繁花",
          originalTitle: "Blossoms Shanghai",
          year: 2023,
          rating: 8.7,
          genres: ["剧情"],
          extent: "",
          badges: [],
          overview: "",
          posterUrl: "",
        },
        info: {
          directors: [],
          directorCredits: [],
          cast: [],
          country: "CN",
          language: "zh",
          released: "2023-12-27",
          aliases: [],
          brands: brands.slice(0, 3),
        },
        isMovie: false,
        libraryLinks: [],
        libraryReturnTo: "/media/tv/9001",
        sub: null,
        showSubscribeButton: false,
        showSearchButton: false,
        onOpenSubscribe: () => {},
      }),
    );
  });
  assert.equal(hero.root.findAllByType("img").length, 2);
  assert.ok(JSON.stringify(hero.toJSON()).includes("New Classics Media"));
  await act(async () => hero.unmount());
});

test("LibraryItemDetailView renders BrandStrip in the hero metadata block", async () => {
  const noop = () => undefined;
  const leaf = () => null;
  const children = ({ children: c }) => c;
  globalThis.window = {
    location: { pathname: "/library/lib-1/item/m-1", search: "" },
    sessionStorage: { setItem: noop },
  };
  const { load } = compileTsRegistry({
    "react-router-dom": {
      Link: ({ to, children: c, ...rest }) => React.createElement("a", { href: to, ...rest }, c),
      useNavigate: () => noop,
    },
    "@/components/artwork-image": {
      ArtworkImage: (props) => React.createElement("img", props),
    },
    "@/components/cast-row": { CastRow: leaf },
    "@/components/collections-add-button": { CollectionsAddButton: leaf },
    "@/components/media-row": { MediaRow: leaf },
    "@/components/media-track-rows": { MediaTrackRows: leaf },
    "@/components/feedback": {
      useConfirm: () => async () => false,
      useToast: () => ({ info: noop, success: noop, error: noop }),
    },
    "@/components/subscribe-entry": {
      useSubscribeEntry: () => ({ canSubscribe: false, subscriptionOf: () => undefined, open: noop }),
    },
    "@/components/page-nav": { PAGE_NAV_BUTTON_CLASS: "nav-btn", PageNav: leaf },
    "@/components/modal": { Modal: leaf },
    "@/components/tooltip": { Tooltip: children },
    "@/components/h-scroller": { HScroller: children },
    "@/lib/library-detail-snapshot": { invalidateLibraryDetailSnapshot: noop },
    "@/lib/back-navigation": { useBackNavigation: () => noop },
    "@/lib/backdrop": { useBackdrop: () => ({ setOverrideBackdrop: noop }) },
    "@/lib/discovery-return-path": { getDiscoveryReturnPath: () => null },
    "@/lib/permissions": { usePermissions: () => ({ canManageLibraries: false }) },
    "@/lib/similar-read": { waitForSimilar: async () => [] },
    "@/lib/use-media-query": { useIsMobile: () => false },
    "@/lib/use-page-title": { usePageTitle: noop },
    "@/lib/ui-prefs": { useTheme: () => ({ id: "default" }) },
    "@/lib/env": { publicEnv: { apiBaseUrl: "/api/v1" } },
    "@/lib/http": {
      request: async () => ({ data: { played: false, is_favorite: false, unplayed_count: null } }),
      resolveRequestUrl: (p) => `/api/v1/${p.replace(/^\//, "")}`,
    },
    "@/lib/api/playback": {
      clearPlaybackHistory: async () => ({ deleted_states: 0, deleted_metrics: 0 }),
      fetchPlaybackMarks: async () => ({ played: false, is_favorite: false, unplayed_count: null }),
      fetchResumeState: async () => ({
        position_ms: 0,
        played: false,
        play_count: 0,
        duration_ms: null,
        audio_track: null,
        subtitle_track: null,
      }),
      setPlaybackMarks: async () => ({ played: false, is_favorite: false, unplayed_count: null }),
    },
    "@/lib/api/libraries": {
      fetchItemSimilar: async () => [],
      fetchItemChapters: async () => [],
      fetchChapterFrameStatus: async () => ({ task: null, empty: true }),
      fetchItemProbeStatus: async () => ({ active: false, job: null }),
      getLibrary: async () => ({ id: "lib-1", name: "剧集库", kind: "tv" }),
      getItemEpisodes: async () => ({ season_number: 1, episodes: [] }),
      refreshItemMetadata: async () => ({ refreshed: 1 }),
      getLibraryItemDetail: async () => ({
        media_item_id: "m-1",
        kind: "tv",
        title: "繁花",
        year: 2023,
        original_title: null,
        tmdb_id: "9001",
        poster_url: null,
        backdrop_url: null,
        overview: "剧情简介",
        rating: "8.7",
        content_rating: null,
        runtime_minutes: "45",
        genres: ["剧情"],
        cast: [],
        brands: [
          {
            name: "CCTV-8",
            logo_url:
              "https://image.tmdb.org/t/p/h60_filter(negate,000,666)/2aXk02xBwG2Nhu4lftabfqSpwY3.png",
            kind: "network",
          },
          { name: "New Classics Media", logo_url: null, kind: "studio" },
        ],
        seasons: [],
        files: [],
      }),
    },
  });

  const { LibraryItemDetailView } = load("@/components/library-item-detail-view");
  let view;
  await act(async () => {
    view = create(
      React.createElement(LibraryItemDetailView, { libraryId: "lib-1", mediaItemId: "m-1" }),
    );
  });
  const imgs = view.root.findAllByType("img");
  assert.ok(
    imgs.some((img) => img.props.alt === "CCTV-8"),
    "LibraryItemDetailView must render BrandStrip logo",
  );
  assert.ok(
    JSON.stringify(view.toJSON()).includes("New Classics Media"),
    "LibraryItemDetailView must render BrandStrip text pill",
  );
  await act(async () => view.unmount());
});
