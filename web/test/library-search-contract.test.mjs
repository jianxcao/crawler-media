import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import ts from "typescript";

const require = createRequire(import.meta.url);
function load(path, mocks) {
  const filename = new URL(path, import.meta.url);
  const code = ts.transpileModule(readFileSync(filename, "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
    fileName: filename.pathname,
  }).outputText;
  const module = { exports: {} };
  new Function("require", "module", "exports", code)(
    (name) => name in mocks ? mocks[name] : require(name), module, module.exports,
  );
  return module.exports;
}

const hit = { library_id: "library", media_item_id: "media", title: "Fixture TV", kind: "tv", year: 2020, matched_by: "title" };

test("library search client retains the cursor and sends it for the next page", async () => {
  const requests = [];
  const api = load("../lib/api/search.ts", {
    "@/lib/http": { request: async (path) => { requests.push(path); return { ok: true, data: [hit], next_cursor: "next cursor" }; } },
    "@/lib/image-proxy": {}, "@/lib/search-history": {},
  });
  const page = await api.searchLibraryItems("fixture", "next cursor");
  assert.deepEqual(page, { items: [hit], next_cursor: "next cursor" });
  const url = new URL(requests[0], "http://local");
  assert.equal(url.searchParams.get("cursor"), "next cursor");
});

test("library search renders TV Media and loads the next page", async () => {
  const api = load("../lib/api/search.ts", {
    "@/lib/http": { request: async (path) => new URL(path, "http://local").searchParams.has("cursor")
      ? { ok: true, data: [{ ...hit, media_item_id: "second", title: "Second TV" }], next_cursor: null }
      : { ok: true, data: [hit], next_cursor: "next" } },
    "@/lib/image-proxy": {}, "@/lib/search-history": {},
  });
  let current;
  const jsx = (type, props) => ({ type, props });
  const react = {
    useState(initial) {
      const index = current.index++;
      const owner = current;
      if (!(index in owner.states)) owner.states[index] = typeof initial === "function" ? initial() : initial;
      return [owner.states[index], (next) => { owner.states[index] = typeof next === "function" ? next(owner.states[index]) : next; }];
    },
    useRef(initial) { return react.useState(() => ({ current: initial }))[0]; },
    useEffect(effect) { if (!current.started) { current.started = true; effect(); } },
  };
  const { LibrarySearchResults } = load("../components/library-search-results.tsx", {
    react, "react/jsx-runtime": { jsx, jsxs: jsx },
    "react-router-dom": { Link: "a" },
    "@/lib/api/search": api,
    "@/lib/media-types": { LIBRARY_KIND_LABELS: { tv: "TV" } },
    "@/lib/use-scroll-restoration": { useScrollRestoration: () => null },
  });
  const owner = { states: [], index: 0, started: false };
  function render() { current = owner; owner.index = 0; return LibrarySearchResults({ keyword: "fixture" }); }
  function nodes(tree) {
    if (Array.isArray(tree)) return tree.flatMap(nodes);
    if (!tree || typeof tree !== "object") return [];
    if (typeof tree.type === "function") return nodes(tree.type(tree.props));
    return [tree, ...nodes(tree.props?.children)];
  }
  render();
  await new Promise((resolve) => setImmediate(resolve));
  const tree = nodes(render());
  assert.ok(tree.some((node) => node.props?.children === "Fixture TV"));
  assert.ok(tree.some((node) => node.props?.to === "/library/library/item/media"));
  tree.find((node) => node.type === "button" && node.props.children === "加载更多").props.onClick();
  await new Promise((resolve) => setImmediate(resolve));
  const next = nodes(render());
  assert.ok(next.some((node) => node.props?.children === "Fixture TV"));
  assert.ok(next.some((node) => node.props?.children === "Second TV"));
  assert.ok(!next.some((node) => node.type === "button" && node.props.children === "加载更多"));
});
