import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";
import { filterKey, filterQuery, isFilterEmpty } from "../lib/library-filter.ts";
import { keepIfEqual, reconcileList } from "../lib/poll-reconcile.ts";

const noop = () => undefined;
const tick = () => new Promise((resolve) => setImmediate(resolve));
const item = {
  media_item_id: "local-nfo", library_id: "library", kind: "tv", title: "Local NFO",
  year: 2026, poster_url: "/posters/file", backdrop_url: "/fanart/file",
  has_poster: true, has_backdrop: true, file_count: 8, episode_count: 8,
  seasons: [1], missing: false, max_resolution: "1080p", rating: null,
  studio: null, studio_logo: null,
};
const library = {
  id: "library", name: "Local metadata", kind: "tv", root_paths: ["/tv"],
  stats: { item_count: 1, file_count: 8, total_size_bytes: 0, strm_file_count: 8 },
};

async function renderLibrary(unidentified = []) {
  const states = [], effects = [];
  let index = 0, mounting = true;
  const react = {
    useState(initial) {
      const at = index++;
      if (!(at in states)) states[at] = typeof initial === "function" ? initial() : initial;
      return [states[at], (next) => {
        states[at] = typeof next === "function" ? next(states[at]) : next;
      }];
    },
    useRef(initial) { return react.useState(() => ({ current: initial }))[0]; },
    useMemo: (fn) => fn(), useCallback: (fn) => fn, useLayoutEffect: noop,
    useEffect(effect) { if (mounting) effects.push(effect); },
  };
  const jsx = (type, props) => ({ type, props });
  const fallback = new Proxy({}, { get: (_, key) => key === "__esModule" ? false : noop });
  const mocks = {
    react, "react/jsx-runtime": { jsx, jsxs: jsx, Fragment: "fragment" },
    "react-router-dom": { Link: "a", useNavigate: () => noop },
    "@/lib/api/libraries": {
      filterKey, filterQuery, isFilterEmpty,
      listLibraries: async () => [library],
      // The public browse API includes provisional Media unless explicitly filtered.
      listLibraryItems: async (_, query) => ({
        items: query?.identity === "confirmed" ? [] : [item],
        total: 1, next_cursor: null,
      }),
      listLibraryItemIndex: async () => [],
      listMissingLibraryFiles: async () => [],
      listUnidentifiedLibraryFiles: async () => unidentified,
    },
    "@/lib/poll-reconcile": { keepIfEqual, reconcileList },
    "@/lib/permissions": { usePermissions: () => ({ canManageLibraries: true }) },
    "@/components/feedback": { useConfirm: () => noop, useToast: () => ({ success: noop }) },
    "@/lib/use-library-removal": { useLibraryRemoval: () => ({ states: {} }) },
    "@/lib/use-scroll-restoration": { useScrollRestoration: () => noop },
    "@/lib/use-wall-recall": { useWallRecall: () => ({ recallOffset: null }) },
    "@/lib/library-wall-index": { wallBucketOffsets: () => [], wallInitialAtOffset: () => null },
    "@/lib/wall-sort": {
      useWallSortPref: () => [{ pref: "title", reversed: false }, noop, noop, false],
      PREF_TO_SORT: { title: "title" }, SORT_DIRECTIONS: { title: { naturalAsc: true } },
    },
    "@/components/library-kind-meta": { LIBRARY_KIND_META: { tv: { label: "剧集" } } },
    "@/components/wall": { usePhotoWallDensity: () => ["normal", noop] },
    "@/components/poster-wall": { PosterWall: "poster-wall", InventoryCell: "inventory-cell" },
    "@/components/video-gallery": {
      useVideoGalleryMode: () => [false, noop], useVideoGalleryGrouped: () => [false, noop],
      flattenGallery: () => [],
    },
  };
  const source = readFileSync(new URL("../components/library-detail-view.tsx", import.meta.url), "utf8");
  const code = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
  }).outputText;
  const module = { exports: {} };
  vm.runInNewContext(code, {
    module, exports: module.exports, require: (name) => mocks[name] ?? fallback,
    URLSearchParams, window: { location: { search: "" } },
  });
  function render() {
    index = 0;
    return module.exports.LibraryDetailView({ libraryId: "library" });
  }
  render();
  mounting = false;
  effects.forEach((effect) => effect());
  await tick();
  return render();
}

function elements(tree) {
  if (Array.isArray(tree)) return tree.flatMap(elements);
  if (!tree || typeof tree !== "object") return [];
  return [tree, ...elements(tree.props?.children)];
}
function text(tree) {
  if (Array.isArray(tree)) return tree.map(text).join("");
  if (tree && typeof tree === "object") return text(tree.props?.children);
  return typeof tree === "string" || typeof tree === "number" ? String(tree) : "";
}

test("local NFO Media appears once without a false unidentified warning", async () => {
  const tree = await renderLibrary();
  const cards = elements(tree).flatMap((node) =>
    node.type === "poster-wall" ? node.props.items :
      node.type === "inventory-cell" ? [node.props.item] : []);
  assert.deepEqual(cards.map((card) => card.media_item_id), ["local-nfo"]);
  assert.doesNotMatch(text(tree), /未识别|待识别|去待处理认领/);
});

test("actual unidentified files retain their actionable pending count", async () => {
  const tree = await renderLibrary([{ group_key: "/tv/unknown", file_count: 3, files: [] }]);
  const pending = elements(tree).find((node) =>
    node.type === "button" && text(node).includes("3 个文件待识别"));
  assert.ok(pending);
  assert.equal(typeof pending.props.onClick, "function");
});
