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

test("discover API always sends explicit source=tmdb or source=douban on page, collection, and filtered requests", async () => {
  const requestedPaths = [];
  const { load } = compileTsRegistry({
    "@/lib/http": {
      request: async (path) => {
        requestedPaths.push(path);
        if (path.includes("/collection/")) {
          return {
            ok: true,
            data: {
              kind: "movie",
              collection_id: "top-rated",
              title: "高分经典",
              items: [],
              page: 1,
              total_pages: 1,
              total_results: 0,
              has_more: false,
            },
          };
        }
        if (path.includes("/filtered?")) {
          return {
            ok: true,
            data: {
              kind: "movie",
              page: 1,
              total_pages: 0,
              total_results: 0,
              has_more: false,
              items: [],
            },
          };
        }
        return {
          ok: true,
          data: {
            kind: "movie",
            tmdb_configured: true,
            sections: [],
          },
        };
      },
    },
    "@/lib/image-proxy": { cachedImageUrl: (url) => url },
  });

  const discover = load("@/lib/api/discover");
  await discover.fetchDiscoveryPage("movie");
  await discover.fetchDiscoveryPage("tv", "tmdb");
  await discover.fetchDiscoveryPage("movie", "douban");
  await discover.browseDiscoveryCollection("tmdb:movie:top-rated", 20, undefined, 1, "full");
  await discover.browseDiscoveryCollection("douban:movie:top-rated", 20, undefined, 1, "full");
  await discover.fetchFilteredDiscovery(
    "movie",
    { genreIds: [28], sort: "popularity" },
    1,
  );

  assert.equal(requestedPaths[0], "/discover/movie?source=tmdb");
  assert.equal(requestedPaths[1], "/discover/tv?source=tmdb");
  assert.equal(requestedPaths[2], "/discover/movie?source=douban");
  assert.equal(requestedPaths[3], "/discover/movie/collection/top-rated?page=1&page_size=20&source=tmdb");
  assert.equal(requestedPaths[4], "/discover/movie/collection/top-rated?page=1&page_size=20&source=douban");
  assert.match(requestedPaths[5], /^\/discover\/movie\/filtered\?/);
  assert.match(requestedPaths[5], /(?:^|[?&])source=tmdb(?:&|$)/);
});

test("discover API maps library_status and in_library onto MediaItem.libraryStatus for PosterCard green 已入库 ribbon", async () => {
  const { load } = compileTsRegistry({
    "@/lib/http": {
      request: async (path) => {
        if (path.includes("/collection/")) {
          return {
            ok: true,
            data: {
              kind: "movie",
              collection_id: "popular",
              title: "热门电影",
              items: [
                {
                  id: "603",
                  title: "黑客帝国",
                  year: 1999,
                  kind: "movie",
                  tmdb_id: "603",
                  poster_url: "/matrix.jpg",
                  in_library: true,
                  library_status: {
                    owned: true,
                    media_item_id: "media-uuid-603",
                    library_count: 1,
                    file_count: 2,
                  },
                },
                {
                  id: "604",
                  title: "黑客帝国2",
                  year: 2003,
                  kind: "movie",
                  tmdb_id: "604",
                  poster_url: "/matrix2.jpg",
                  in_library: false,
                },
              ],
              page: 1,
              total_pages: 1,
              total_results: 2,
              has_more: false,
            },
          };
        }
        return {
          ok: true,
          data: {
            kind: "movie",
            tmdb_configured: true,
            sections: [
              {
                id: "popular",
                title: "热门电影",
                presentation: "poster-row",
                supports_full_listing: true,
                items: [
                  {
                    id: "603",
                    title: "黑客帝国",
                    year: 1999,
                    kind: "movie",
                    tmdb_id: "603",
                    poster_url: "/matrix.jpg",
                    in_library: true,
                    library_status: { owned: true },
                  },
                  {
                    id: "604",
                    title: "黑客帝国2",
                    year: 2003,
                    kind: "movie",
                    tmdb_id: "604",
                    poster_url: "/matrix2.jpg",
                    in_library: false,
                  },
                ],
              },
            ],
          },
        };
      },
    },
    "@/lib/image-proxy": { cachedImageUrl: (url) => url },
    "react-router-dom": {
      Link: ({ to, children, ...rest }) => React.createElement("a", { href: to, ...rest }, children),
    },
    "@/components/artwork-image": {
      ArtworkImage: (props) => React.createElement("img", props),
    },
    "@/components/poster-image": {
      PosterImage: (props) => React.createElement("img", props),
    },
    "@/components/subscribe-entry": {
      useSubscribeEntry: () => ({ canSubscribe: false, open: () => {}, subscriptionOf: () => undefined }),
    },
    "@/lib/use-media-query": { useMediaQuery: () => false },
    "@/lib/use-tap-guard": { useTapGuard: (fn) => ({ onClick: fn ?? (() => {}) }) },
  });

  const discover = load("@/lib/api/discover");
  const { PosterCardVisual } = load("@/components/poster-card");

  const page = await discover.fetchDiscoveryPage("movie", "tmdb");
  assert.equal(page.sections.length, 1);
  assert.equal(page.sections[0].items.length, 2);
  assert.ok(page.sections[0].items[0].libraryStatus, "owned item on wall must populate libraryStatus");
  assert.equal(page.sections[0].items[1].libraryStatus, null);

  const fullListing = await discover.browseDiscoveryCollection("tmdb:movie:popular", 20, undefined, 1, "full");
  assert.deepEqual(fullListing.items[0].libraryStatus, {
    mediaItemId: "media-uuid-603",
    libraryCount: 1,
    fileCount: 2,
  });
  assert.equal(fullListing.items[1].libraryStatus, null);

  let ownedTree;
  act(() => {
    ownedTree = create(React.createElement(PosterCardVisual, { item: page.sections[0].items[0] }));
  });
  const ribbons = ownedTree.root.findAll(
    (node) => node.type === "span" && node.props["aria-label"] === "已入库",
  );
  assert.equal(ribbons.length, 1, "owned Discover item must render green 已入库 ribbon");

  let unownedTree;
  act(() => {
    unownedTree = create(React.createElement(PosterCardVisual, { item: page.sections[0].items[1] }));
  });
  const unownedRibbons = unownedTree.root.findAll(
    (node) => node.type === "span" && node.props["aria-label"] === "已入库",
  );
  assert.equal(unownedRibbons.length, 0);
});

test("DiscoverView retains pageCache when some sections succeed even if one section fails", async () => {
  let pageFetches = 0;
  const { load } = compileTsRegistry({
    "@/lib/http": {
      request: async (path) => {
        if (path.startsWith("/discover/movie")) {
          pageFetches += 1;
          return {
            ok: true,
            data: {
              kind: "movie",
              tmdb_configured: true,
              sections: [
                {
                  id: "popular",
                  title: "热门电影",
                  presentation: "poster-row",
                  supports_full_listing: true,
                  items: [
                    {
                      id: "603",
                      title: "黑客帝国",
                      year: 1999,
                      kind: "movie",
                      tmdb_id: "603",
                      poster_url: "/matrix.jpg",
                    },
                  ],
                },
                {
                  id: "upcoming",
                  title: "即将上映",
                  presentation: "poster-row",
                  supports_full_listing: true,
                  items: [],
                  error: { code: "discover.section_failed", message: "分区暂时无法加载，请重试" },
                },
              ],
            },
          };
        }
        throw new Error(`unexpected path: ${path}`);
      },
    },
    "@/lib/image-proxy": { cachedImageUrl: (url) => url },
    "react-router-dom": {
      Link: ({ to, children, ...rest }) => React.createElement("a", { href: to, ...rest }, children),
      useNavigate: () => () => {},
    },
    "@/components/discovery-filter-dialog": { DiscoveryFilterControl: () => null },
    "@/components/filtered-discovery-view": { FilteredDiscoveryView: () => null },
    "@/components/media-row": {
      MediaRow: ({ row }) => React.createElement("div", { "data-row": row.id }, row.title),
    },
    "@/lib/page-chrome": { usePageChrome: () => null },
    "@/lib/ui-prefs": { useTheme: () => ({ id: "atelier" }) },
    "@/lib/use-media-query": { useIsMobile: () => false },
    "@/lib/use-scroll-restoration": { useScrollRestoration: () => ({ current: null }) },
  });

  const { DiscoverView } = load("@/components/discover-view");
  const { EMPTY_DISCOVERY_FILTERS } = load("@/lib/discovery-filters");

  let tree;
  await act(async () => {
    tree = create(
      React.createElement(DiscoverView, {
        mediaType: "movie",
        source: "tmdb",
        filters: EMPTY_DISCOVERY_FILTERS,
        currentYear: 2026,
      }),
    );
    await Promise.resolve();
    await Promise.resolve();
  });
  assert.equal(pageFetches, 1);

  await act(async () => {
    tree.unmount();
  });

  // Remounting DiscoverView must hit pageCache rather than refetching all 20 sections!
  await act(async () => {
    tree = create(
      React.createElement(DiscoverView, {
        mediaType: "movie",
        source: "tmdb",
        filters: EMPTY_DISCOVERY_FILTERS,
        currentYear: 2026,
      }),
    );
    await Promise.resolve();
  });
  assert.equal(pageFetches, 1, "partial section error must not invalidate client pageCache");
});
