import assert from "node:assert/strict";
import { existsSync, readFileSync, statSync } from "node:fs";
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

test("Douban TV detail loads seasons using resolved TMDB item.id and SeasonsSection avoids direct http request", async () => {
  const seasonsSource = readFileSync(
    path.join(webRoot, "components/media-detail/seasons-section.tsx"),
    "utf8",
  );
  assert.doesNotMatch(seasonsSource, /from\s+["']@\/lib\/http["']/);

  const requests = [];
  const icon = () => React.createElement("span");
  const noop = () => undefined;
  const leaf = () => null;
  const fallback = new Proxy({}, { get: (_, key) => (key === "__esModule" ? false : leaf) });

  const resolveFile = (base) => {
    for (const suffix of ["", ".ts", ".tsx", "/index.ts", "/index.tsx"]) {
      const candidate = `${base}${suffix}`;
      if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
    }
    throw new Error(`Cannot resolve module from ${base}`);
  };

  const cache = new Map();
  const real = new Set([
    "@/lib/api/discover",
    "@/lib/media-detail-helpers",
  ]);
  const mocks = {
    react: React,
    "react/jsx-runtime": require("react/jsx-runtime"),
    "@/lib/http": {
      request: async (reqPath) => {
        requests.push(reqPath);
        if (reqPath === "/media/douban/3541415") {
          return {
            ok: true,
            data: {
              item: {
                id: "99999",
                title: "三体",
                year: 2023,
                kind: "tv",
                tmdb_id: "99999",
                poster_url: null,
                backdrop_url: null,
                rating: 8.5,
              },
              info: {
                overview: "sci-fi",
                genres: ["Sci-Fi"],
                cast: [],
                directors: [],
                country: "CN",
                language: "zh",
                released: "2023-01-15",
                aliases: [],
              },
              seasons: [{ season_number: 1, name: "第 1 季", episode_count: 30 }],
              related: [],
              library_links: [],
            },
          };
        }
        if (reqPath === "/media/tv/99999/season/1") {
          return {
            ok: true,
            data: {
              episodes: [{ episode_number: 1, name: "第一集", overview: "开篇", still_url: null }],
            },
          };
        }
        throw new Error(`Unexpected request: ${reqPath}`);
      },
    },
    "@/lib/image-proxy": { cachedImageUrl: (u) => u, imageUrl: (u) => u, upgradedTmdbOriginalUrl: (u) => u },
    "@/lib/discovery-section-state": { normalizeDiscoverySectionError: () => undefined, shouldCacheDiscoverySection: () => true },
    "@/lib/discovery-collection-cache": { shouldUsePreviewCache: () => false },
    "@/components/subscribe-entry": { useSubscribeEntry: () => ({ canSubscribe: false, subscriptionOf: () => null, refresh: noop }) },
    "@/lib/back-navigation": { useBackNavigation: () => noop },
    "@/lib/backdrop": { useBackdrop: () => ({ setOverrideBackdrop: noop }) },
    "@/lib/artwork-loading": { createArtworkImage: () => ({ image: { decode: async () => {} }, load: noop }) },
    "@/lib/discovery-return-path": { buildDiscoveryReturnPath: () => "" },
    "@/lib/douban-app-link": { useDoubanAppHref: () => null },
    "@/lib/media-detail": { getMediaSeed: () => null },
    "@/lib/permissions": { usePermissions: () => ({ canSearch: false }) },
    "@/lib/ui-prefs": { useTheme: () => ({ id: "default" }) },
    "@/lib/use-media-query": { useIsMobile: () => false },
    "@/lib/use-page-title": { usePageTitle: noop },
    "@/components/icons": { ArrowLeftIcon: icon },
    "@/components/brand-loader": { BrandLoader: leaf },
    "@/components/artwork-image": { ArtworkImage: leaf },
    "@/components/cast-row": { CastRow: leaf },
    "@/components/detail-backdrop-slideshow": { DetailBackdropSlideshow: leaf },
    "@/components/media-row": { MediaRow: leaf },
    "@/components/subscribe-dialog": { SubscribeDialog: leaf },
    "@/components/media-detail/media-detail-hero": { MediaDetailHero: leaf },
    "@/components/media-detail/media-detail-scaffold": {
      MediaDetailFallback: ({ loadingLabel }) => React.createElement("div", null, loadingLabel),
      MediaDetailScaffold: ({ lead, children }) => React.createElement("div", null, lead, children),
    },
    "@/components/media-detail/detail-metadata-widgets": {
      ExpandablePlot: leaf,
      SourceLink: leaf,
    },
    "@/components/media-detail/photo-wall": { PhotoWall: leaf },
    "@/components/media-detail/trailer-row": { TrailerRow: leaf },
  };

  const load = (filename) => {
    if (cache.has(filename)) return cache.get(filename).exports;
    const js = ts.transpileModule(readFileSync(filename, "utf8"), {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        jsx: ts.JsxEmit.ReactJSX,
        target: ts.ScriptTarget.ES2022,
      },
    }).outputText;
    const mod = { exports: {} };
    cache.set(filename, mod);
    new Function("require", "module", "exports", js)((name) => {
      if (mocks[name]) return mocks[name];
      if (name.startsWith(".")) {
        const resolved = resolveFile(path.resolve(path.dirname(filename), name));
        const relAlias = `@/${path.relative(webRoot, resolved).replace(/\.(ts|tsx)$/, "")}`;
        if (mocks[relAlias]) return mocks[relAlias];
        return load(resolved);
      }
      if (name.startsWith("@/") && (real.has(name) || name.startsWith("@/components/media-detail/"))) {
        return load(resolveFile(path.join(webRoot, name.slice(2))));
      }
      if (name.startsWith("@/")) return fallback;
      return require(name);
    }, mod, mod.exports);
    return mod.exports;
  };

  const { MediaDetailView } = load(path.join(webRoot, "components/media-detail-view.tsx"));

  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(MediaDetailView, {
        source: "douban",
        id: "3541415",
      }),
    );
  });

  try {
    assert.ok(requests.includes("/media/douban/3541415"));
    assert.ok(
      requests.includes("/media/tv/99999/season/1"),
      `Expected /media/tv/99999/season/1, got requests: ${JSON.stringify(requests)}`,
    );
    assert.equal(requests.includes("/media/tv/3541415/season/1"), false);
    assert.match(JSON.stringify(renderer.toJSON()), /第一集/);
  } finally {
    await act(async () => {
      renderer.unmount();
    });
  }
});

test("Navigating MediaDetailView between TV shows resets SeasonsSection via key without wasted stale-season request", async () => {
  const libraryDetailViewSource = readFileSync(
    path.join(webRoot, "components/library-item-detail-view.tsx"),
    "utf8",
  );
  assert.match(
    libraryDetailViewSource,
    /<SeasonEpisodesSection[\s\S]*?key=\{`\$\{libraryId\}:\$\{detail\.media_item_id\}:\$\{initialSeason\s*\?\?\s*""\}:\$\{initialEpisode\s*\?\?\s*""\}`\}/,
    "LibraryItemDetailView must key SeasonEpisodesSection by libraryId, media_item_id, initialSeason, and initialEpisode",
  );

  const requests = [];
  const icon = () => React.createElement("span");
  const noop = () => undefined;
  const leaf = () => null;
  const fallback = new Proxy({}, { get: (_, key) => (key === "__esModule" ? false : leaf) });

  const resolveFile = (base) => {
    for (const suffix of ["", ".ts", ".tsx", "/index.ts", "/index.tsx"]) {
      const candidate = `${base}${suffix}`;
      if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
    }
    throw new Error(`Cannot resolve module from ${base}`);
  };

  const cache = new Map();
  const real = new Set(["@/lib/api/discover", "@/lib/media-detail-helpers"]);
  const mocks = {
    react: React,
    "react/jsx-runtime": require("react/jsx-runtime"),
    "@/lib/http": {
      request: async (reqPath) => {
        requests.push(reqPath);
        if (reqPath === "/media/tv/101") {
          return {
            ok: true,
            data: {
              item: { id: "101", title: "剧集A", year: 2022, kind: "tv", tmdb_id: "101", poster_url: null, backdrop_url: null, rating: 8.0 },
              info: { overview: "", genres: [], cast: [], directors: [], country: "US", language: "en", released: null, aliases: [] },
              seasons: [
                { season_number: 1, name: "第 1 季", episode_count: 8 },
                { season_number: 2, name: "第 2 季", episode_count: 8 },
              ],
              related: [],
              library_links: [],
            },
          };
        }
        if (reqPath === "/media/tv/202") {
          return {
            ok: true,
            data: {
              item: { id: "202", title: "剧集B", year: 2024, kind: "tv", tmdb_id: "202", poster_url: null, backdrop_url: null, rating: 8.8 },
              info: { overview: "", genres: [], cast: [], directors: [], country: "US", language: "en", released: null, aliases: [] },
              seasons: [
                { season_number: 1, name: "第 1 季", episode_count: 10 },
                { season_number: 2, name: "第 2 季", episode_count: 10 },
              ],
              related: [],
              library_links: [],
            },
          };
        }
        if (reqPath.startsWith("/media/tv/101/season/") || reqPath.startsWith("/media/tv/202/season/")) {
          return {
            ok: true,
            data: {
              episodes: [{ episode_number: 1, name: `EP-${reqPath}`, overview: "", still_url: null }],
            },
          };
        }
        throw new Error(`Unexpected request: ${reqPath}`);
      },
    },
    "@/lib/image-proxy": { cachedImageUrl: (u) => u, imageUrl: (u) => u, upgradedTmdbOriginalUrl: (u) => u },
    "@/lib/discovery-section-state": { normalizeDiscoverySectionError: () => undefined, shouldCacheDiscoverySection: () => true },
    "@/lib/discovery-collection-cache": { shouldUsePreviewCache: () => false },
    "@/components/subscribe-entry": { useSubscribeEntry: () => ({ canSubscribe: false, subscriptionOf: () => null, refresh: noop }) },
    "@/lib/back-navigation": { useBackNavigation: () => noop },
    "@/lib/backdrop": { useBackdrop: () => ({ setOverrideBackdrop: noop }) },
    "@/lib/artwork-loading": { createArtworkImage: () => ({ image: { decode: async () => {} }, load: noop }) },
    "@/lib/discovery-return-path": { buildDiscoveryReturnPath: () => "" },
    "@/lib/douban-app-link": { useDoubanAppHref: () => null },
    "@/lib/media-detail": { getMediaSeed: () => null },
    "@/lib/permissions": { usePermissions: () => ({ canSearch: false }) },
    "@/lib/ui-prefs": { useTheme: () => ({ id: "default" }) },
    "@/lib/use-media-query": { useIsMobile: () => false },
    "@/lib/use-page-title": { usePageTitle: noop },
    "@/components/icons": { ArrowLeftIcon: icon },
    "@/components/brand-loader": { BrandLoader: leaf },
    "@/components/artwork-image": { ArtworkImage: leaf },
    "@/components/cast-row": { CastRow: leaf },
    "@/components/detail-backdrop-slideshow": { DetailBackdropSlideshow: leaf },
    "@/components/media-row": { MediaRow: leaf },
    "@/components/subscribe-dialog": { SubscribeDialog: leaf },
    "@/components/media-detail/media-detail-hero": { MediaDetailHero: leaf },
    "@/components/media-detail/media-detail-scaffold": {
      MediaDetailFallback: ({ loadingLabel }) => React.createElement("div", null, loadingLabel),
      MediaDetailScaffold: ({ lead, children }) => React.createElement("div", null, lead, children),
    },
    "@/components/media-detail/detail-metadata-widgets": { ExpandablePlot: leaf, SourceLink: leaf },
    "@/components/media-detail/photo-wall": { PhotoWall: leaf },
    "@/components/media-detail/trailer-row": { TrailerRow: leaf },
  };

  const load = (filename) => {
    if (cache.has(filename)) return cache.get(filename).exports;
    const js = ts.transpileModule(readFileSync(filename, "utf8"), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 },
    }).outputText;
    const mod = { exports: {} };
    cache.set(filename, mod);
    new Function("require", "module", "exports", js)((name) => {
      if (mocks[name]) return mocks[name];
      if (name.startsWith(".")) {
        const resolved = resolveFile(path.resolve(path.dirname(filename), name));
        const relAlias = `@/${path.relative(webRoot, resolved).replace(/\.(ts|tsx)$/, "")}`;
        if (mocks[relAlias]) return mocks[relAlias];
        return load(resolved);
      }
      if (name.startsWith("@/") && (real.has(name) || name.startsWith("@/components/media-detail/"))) {
        return load(resolveFile(path.join(webRoot, name.slice(2))));
      }
      if (name.startsWith("@/")) return fallback;
      return require(name);
    }, mod, mod.exports);
    return mod.exports;
  };

  const { MediaDetailView } = load(path.join(webRoot, "components/media-detail-view.tsx"));

  let renderer;
  await act(async () => {
    renderer = create(React.createElement(MediaDetailView, { type: "tv", id: "101", source: "tmdb" }));
  });

  try {
    // Switch show 101 to Season 2
    const season2Btn = renderer.root
      .findAllByType("button")
      .find((btn) => JSON.stringify(btn.children).includes("第 2 季"));
    assert.ok(season2Btn);
    await act(async () => {
      season2Btn.props.onClick();
    });
    assert.ok(requests.includes("/media/tv/101/season/2"));

    // Navigate directly to show 202 (which also has Season 1 & Season 2): must reset to Season 1!
    await act(async () => {
      renderer.update(React.createElement(MediaDetailView, { type: "tv", id: "202", source: "tmdb" }));
    });
    assert.ok(requests.includes("/media/tv/202/season/1"), "Show 202 must default to Season 1");
    assert.equal(
      requests.includes("/media/tv/202/season/2"),
      false,
      "Show 202 must NOT inherit activeSeason=2 from Show 101",
    );
  } finally {
    await act(async () => {
      renderer.unmount();
    });
  }
});
