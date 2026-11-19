import { existsSync, readFileSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import React, { act } from "react";
import { create } from "react-test-renderer";
import ts from "typescript";

const require = createRequire(import.meta.url);
const noop = () => undefined;
const leaf = () => null;
const fallback = new Proxy({}, { get: (_, key) => key === "__esModule" ? false : leaf });
globalThis.IS_REACT_ACT_ENVIRONMENT = true;

export const deferred = () => {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
};

export const item = (id) => ({
  media_item_id: id, library_id: "library", kind: "movie", title: id, year: null,
  poster_url: null, backdrop_url: null, has_poster: false, has_backdrop: false,
  file_count: 1, episode_count: 0, seasons: [], missing: false, max_resolution: null,
  rating: null, studio: null, studio_logo: null,
});
export const page = (id, next_cursor = null) => ({
  items: [item(id)], groups: [{ ...item(id), images: [] }], next_cursor, total: 1000,
});

// Real React effects/state, the real component, sorting and session snapshots.
// API calls and browser preferences are the external seams; visual children
// expose the component's selected rows and pagination controls without a DOM.
export function libraryDetailHarness({
  gallery = false,
  load,
  recallOffset = 500,
  realRecall = false,
  initialSortPref = "title",
  libraryKind = "movie",
} = {}) {
  const storage = new Map();
  if (initialSortPref !== null) {
    storage.set("crawler-media.library.wall-sort", initialSortPref);
  }
  if (realRecall) storage.set("crawler-media.wall-recall", JSON.stringify({
    "library:library": { offset: recallOffset, view: "wall:title:title", updatedAt: Date.now() },
  }));
  globalThis.window = {
    location: { search: "", pathname: "/library/library" },
    history: { replaceState: noop },
    localStorage: {
      getItem: (key) => (storage.has(key) ? storage.get(key) : null),
      setItem: (key, value) => storage.set(key, value),
    },
  };
  let visibleItem;
  const scroller = new globalThis.EventTarget();
  Object.assign(scroller, {
    scrollTop: 0, clientHeight: 800,
    getBoundingClientRect: () => ({ top: 0 }), scrollTo: noop,
  });
  const wall = {
    getBoundingClientRect: () => ({ top: 0 }), scrollIntoView: noop,
    querySelectorAll: () => visibleItem ? [{
      getBoundingClientRect: () => ({ bottom: 100 }), getAttribute: () => visibleItem,
    }] : [],
  };
  if (realRecall) {
    globalThis.document = { addEventListener: noop, removeEventListener: noop };
    globalThis.requestAnimationFrame = () => 0;
    globalThis.cancelAnimationFrame = noop;
  }
  const library = {
    id: "library", name: "Movies", kind: libraryKind, root_paths: ["/movies"], is_default: true,
    stats: { item_count: 1000, file_count: 1000, total_size_bytes: 0, strm_file_count: 1000 },
  };
  let poll;
  let galleryPreference = gallery;
  let setGalleryPreference;
  const indexCalls = [];
  const modules = new Map();
  const mocks = {
    react: React,
    "react/jsx-runtime": require("react/jsx-runtime"),
    "react-router-dom": { Link: "a", useNavigate: () => noop },
    "@/lib/api/libraries": {
      listLibraries: async () => [library],
      listLibraryItems: (_, query) => load("wall", query),
      listLibraryGallery: (_, query) => load("gallery", query),
      listLibraryItemIndex: async (id, sort, filter, order, options) => {
        indexCalls.push({ id, sort, filter, order, options });
        return [
          { initial: "A", count: 500, cursor: null },
          { initial: "M", count: 500, cursor: "title-M" },
        ];
      },
      filterKey: () => "", isFilterEmpty: () => true,
      listMissingLibraryFiles: async () => [], listUnidentifiedLibraryFiles: async () => [],
    },
    "@/lib/permissions": { usePermissions: () => ({ canManageLibraries: false }) },
    "@/components/feedback": { useConfirm: () => noop, useToast: () => ({ success: noop, error: noop }) },
    "@/lib/use-library-removal": { useLibraryRemoval: () => ({ states: {} }) },
    "@/lib/use-scroll-restoration": { useScrollRestoration: () => noop },
    "@/lib/use-visible-polling": { useVisiblePolling: (callback) => { poll = callback; } },
    "@/lib/use-wall-recall": { useWallRecall: () => ({ recallOffset, dismissRecall: noop }) },
    "@/lib/library-wall-recall": { wallRecallScope: () => "library", isReentryAfterAbsence: () => false },
    "@/components/library-kind-meta": { LIBRARY_KIND_META: { movie: { label: "电影" }, tv: { label: "剧集" }, video: { label: "视频" } } },
    "@/components/wall": { usePhotoWallDensity: () => ["normal", noop] },
    "@/components/poster-wall": {
      PosterWall: ({ items }) => React.createElement("output", { id: "rows" }, items.map((row) => row.title).join(",")),
    },
    "@/components/library-filter-bar": {
      LibraryFilterBar: ({ sortControl }) => sortControl,
      WallSortControl: ({ value, onChange }) => React.createElement("select", { value, onChange }),
    },
    "@/components/wall-chrome": {
      WallLoadMore: ({ hasMore, onReach, start }) => React.createElement("button", { id: "more", disabled: !hasMore, onClick: onReach, "data-start": start }),
      WallRecallPill: ({ onJump }) => React.createElement("button", { id: "recall", onClick: onJump }),
    },
    "@/components/video-gallery": {
      useVideoGalleryMode: () => {
        const [value, setValue] = React.useState(galleryPreference);
        setGalleryPreference = (next) => { galleryPreference = next; setValue(next); };
        return [value, setGalleryPreference];
      },
      useVideoGalleryGrouped: () => [false, noop],
      GALLERY_PAGE_SIZE: 60, flattenGallery: (groups) => groups.flatMap((group) => group.images),
      dedupeGalleryGroups: (groups) => [...new Map(groups.map((group) => [group.media_item_id, group])).values()],
      VideoGalleryWall: ({ groups }) => React.createElement("output", { id: "rows" }, groups.map((group) => group.title).join(",")),
    },
  };
  const real = new Set([
    "@/lib/poll-reconcile",
    "@/lib/library-wall-index",
    "@/lib/wall-sort",
    "@/lib/library-detail-snapshot",
    "@/lib/session-snapshot",
    "@/lib/library-detail-helpers",
    "@/lib/library-filter",
  ]);
  if (realRecall) {
    delete mocks["@/lib/use-wall-recall"];
    delete mocks["@/lib/library-wall-recall"];
    real.add("@/lib/use-wall-recall");
    real.add("@/lib/library-wall-recall");
  }
  function resolveFile(baseUrl, specifier) {
    const raw = specifier.startsWith("@/")
      ? new URL(`../../${specifier.slice(2)}`, import.meta.url)
      : new URL(specifier, baseUrl);
    for (const suffix of ["", ".tsx", ".ts", "/index.tsx", "/index.ts"]) {
      const candidate = new URL(`${raw.href}${suffix}`);
      if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
    }
    throw new Error(`Cannot resolve module ${specifier} from ${baseUrl.href}`);
  }
  function loadModule(name, fromUrl = import.meta.url) {
    const baseUrl = typeof fromUrl === "string" ? new URL(fromUrl) : fromUrl;
    if (mocks[name]) return mocks[name];
    const isRelative = name.startsWith("./") || name.startsWith("../");
    if (name !== "component" && !real.has(name) && !isRelative) return fallback;
    const targetUrl =
      name === "component"
        ? new URL("../../components/library-detail-view.tsx", import.meta.url)
        : resolveFile(baseUrl, name);
    if (modules.has(targetUrl.href)) return modules.get(targetUrl.href).exports;
    const source = readFileSync(targetUrl, "utf8");
    const js = ts.transpileModule(source, {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        target: ts.ScriptTarget.ES2022,
        jsx: ts.JsxEmit.ReactJSX,
      },
    }).outputText;
    const module = { exports: {} };
    modules.set(targetUrl.href, module);
    new Function("require", "module", "exports", js)(
      (dep) => loadModule(dep, targetUrl),
      module,
      module.exports,
    );
    return module.exports;
  }
  const { LibraryDetailView } = loadModule("component");
  let renderer;
  return {
    async mount() {
      await act(async () => { renderer = create(React.createElement(LibraryDetailView, { libraryId: "library" }), {
        createNodeMock: (element) => realRecall ? (element.props.className?.includes("scroll-safe") ? scroller : wall) : null,
      }); });
    },
    async unmount() { await act(async () => { renderer.unmount(); }); },
    async click(id) { await act(async () => { renderer.root.findByProps({ id }).props.onClick(); }); },
    async sort(value) { await act(async () => { renderer.root.findByType("select").props.onChange(value); }); },
    async gallery(value) { await act(async () => { setGalleryPreference(value); }); },
    async poll() { await act(async () => { poll(); }); },
    async resolve(pending, value) { await act(async () => { pending.resolve(value); }); },
    async scrollToItem(id) {
      visibleItem = id;
      await act(async () => { scroller.scrollTop += 100; scroller.dispatchEvent(new globalThis.Event("scroll")); });
    },
    savedRecall: () => JSON.parse(storage.get("crawler-media.wall-recall"))["library:library"],
    indexCalls: () => indexCalls,
    rows: () => renderer.root.findByProps({ id: "rows" }).children.join(""),
    start: () => renderer.root.findByProps({ id: "more" }).props["data-start"],
    hasMore: () => !renderer.root.findByProps({ id: "more" }).props.disabled,
  };
}
