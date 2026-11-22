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

function createModuleLoader(overrides = {}) {
  const resolveFile = (base) => {
    for (const suffix of ["", ".ts", ".tsx", "/index.ts", "/index.tsx"]) {
      const candidate = `${base}${suffix}`;
      if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
    }
    throw new Error(`Cannot resolve module from ${base}`);
  };
  const cache = new Map();
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
    new Function("require", "module", "exports", js)(
      (name) => {
        if (name in overrides) return overrides[name];
        if (name.startsWith("@/")) return load(resolveFile(path.join(webRoot, name.slice(2))));
        if (name.startsWith(".")) return load(resolveFile(path.resolve(path.dirname(filename), name)));
        return require(name);
      },
      mod,
      mod.exports,
    );
    return mod.exports;
  };
  return (relPath) => load(resolveFile(path.join(webRoot, relPath)));
}

function collectText(node) {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(collectText).join("");
  if (node.children) return node.children.map(collectText).join("");
  return "";
}

test("computeVirtualRange renders <= 25 items for 8,230 rows at any scroll offset with exact spacer heights", () => {
  const load = createModuleLoader();
  const { computeVirtualRange } = load("components/transfers/transfer-helpers.tsx");

  const count = 8230;
  const defaultItemHeight = 148;
  const gap = 12;
  const viewportHeight = 900;
  const overscan = 6;
  const expectedTotal = count * defaultItemHeight + (count - 1) * gap;

  const offsetsToTest = [0, 320, 4800, 65000, 640000, expectedTotal - 900, expectedTotal + 500];
  for (const scrollTop of offsetsToTest) {
    const range = computeVirtualRange({
      count,
      scrollTop,
      viewportHeight,
      listOffsetTop: 240,
      overscan,
      defaultItemHeight,
      gap,
    });

    assert.ok(
      range.visibleCount > 0 && range.visibleCount <= 25,
      `scrollTop=${scrollTop}: expected 1..25 rendered items, got ${range.visibleCount}`,
    );
    assert.equal(range.endIndex - range.startIndex, range.visibleCount);
    assert.equal(range.totalHeightPx, expectedTotal);

    const renderedSliceHeight =
      range.visibleCount * defaultItemHeight + (range.visibleCount - 1) * gap;
    assert.equal(
      range.topSpacerPx + renderedSliceHeight + range.bottomSpacerPx,
      expectedTotal,
      `scrollTop=${scrollTop}: top + slice + bottom must equal totalHeightPx`,
    );
  }

  const head = computeVirtualRange({
    count,
    scrollTop: 0,
    viewportHeight,
    listOffsetTop: 0,
    overscan,
    defaultItemHeight,
    gap,
  });
  assert.equal(head.startIndex, 0);
  assert.equal(head.topSpacerPx, 0);

  const tail = computeVirtualRange({
    count,
    scrollTop: expectedTotal,
    viewportHeight,
    listOffsetTop: 0,
    overscan,
    defaultItemHeight,
    gap,
  });
  assert.equal(tail.endIndex, count);
  assert.equal(tail.bottomSpacerPx, 0);

  // Variable measured heights (e.g. expanded group row among collapsed group rows)
  const measured = new Map([
    [10, 420],
    [11, 310],
  ]);
  const variableRange = computeVirtualRange({
    count,
    scrollTop: 950,
    viewportHeight: 800,
    listOffsetTop: 0,
    overscan: 5,
    defaultItemHeight: 88,
    gap: 12,
    getItemHeight: (idx) => measured.get(idx),
  });
  assert.ok(variableRange.visibleCount <= 25);
  let sliceHeight = 0;
  for (let i = variableRange.startIndex; i < variableRange.endIndex; i += 1) {
    sliceHeight += measured.get(i) ?? 88;
  }
  sliceHeight += (variableRange.visibleCount - 1) * 12;
  assert.equal(
    variableRange.topSpacerPx + sliceHeight + variableRange.bottomSpacerPx,
    variableRange.totalHeightPx,
  );
});

test("listTransfers sends server-paginated query parameters while preserving legacy exports", async () => {
  const requests = [];
  const load = createModuleLoader({
    "@/lib/http": {
      request: async (url, init) => {
        requests.push({ url, init });
        return { ok: true, data: { items: [], groups: [] } };
      },
    },
  });
  const api = load("lib/api/transfers.ts");

  assert.equal(typeof api.listLedger, "function");
  assert.equal(typeof api.deleteLedgerRow, "function");
  assert.equal(typeof api.retransferLedgerRow, "function");
  assert.equal(typeof api.refreshItemMetadata, "function");
  assert.equal(typeof api.listTransfers, "function");

  const controller = new AbortController();
  await api.listTransfers(
    {
      view: "files",
      q: "  知否  ",
      mode: "strm",
      kind: "tv",
      offset: 50,
      limit: 50,
    },
    { signal: controller.signal },
  );

  assert.equal(requests.length, 1);
  const parsed = new URL(requests[0].url, "http://local");
  assert.equal(parsed.pathname, "/ledger/transfers");
  assert.equal(parsed.searchParams.get("view"), "files");
  assert.equal(parsed.searchParams.get("q"), "知否");
  assert.equal(parsed.searchParams.get("mode"), "strm");
  assert.equal(parsed.searchParams.get("kind"), "tv");
  assert.equal(parsed.searchParams.get("offset"), "50");
  assert.equal(parsed.searchParams.get("limit"), "50");
  assert.equal(requests[0].init.signal, controller.signal);
});

test("STRM summary cards and file card render cloud duration & 100% zero-overhead without 128 B", async () => {
  const iconStub = () => React.createElement("span");
  const load = createModuleLoader({
    "@/components/icons": new Proxy({}, { get: () => iconStub }),
    "@/components/copy-button": {
      CopyButton: () => React.createElement("button", { type: "button" }, "copy"),
    },
  });

  const { formatSummaryDuration, formatItemDuration } = load(
    "components/transfers/transfer-helpers.tsx",
  );
  const { TransferSummaryCards } = load("components/transfers/transfer-summary-cards.tsx");
  const { TransferFileCard } = load("components/transfers/transfer-file-card.tsx");

  assert.equal(formatSummaryDuration(0), "云端按需直连");
  assert.equal(formatSummaryDuration(38 * 60_000), "38 分钟");
  assert.equal(formatSummaryDuration(4521 * 3_600_000), "4,521 小时");
  assert.equal(formatItemDuration(2_760_000), "46 分钟");
  assert.equal(formatItemDuration((60 + 52) * 60_000), "1 小时 52 分");

  let summaryRenderer;
  await act(async () => {
    summaryRenderer = create(
      React.createElement(TransferSummaryCards, {
        summary: {
          total_files: 8230,
          media_count: 312,
          movie_files: 120,
          tv_files: 8110,
          strm_count: 8230,
          hardlink_count: 0,
          copy_count: 0,
          move_count: 0,
          local_size_bytes: 0,
          probed_duration_ms: 4521 * 3_600_000,
          strm_probed_count: 8100,
        },
      }),
    );
  });

  const summaryText = collectText(summaryRenderer.toJSON());
  assert.match(summaryText, /8,230/);
  assert.match(summaryText, /312/);
  assert.match(summaryText, /100%/);
  assert.match(summaryText, /STRM 直链\s*8,230\s*·\s*硬链接\s*0/);
  assert.match(summaryText, /云端直链总片长/);
  assert.match(summaryText, /本地零视频占用/);
  assert.match(summaryText, /4,521 小时/);
  assert.doesNotMatch(summaryText, /128 B/);
  assert.doesNotMatch(summaryText, /\(0%\)/);

  let cardRenderer;
  await act(async () => {
    cardRenderer = create(
      React.createElement(TransferFileCard, {
        item: {
          id: "file-strm-1",
          media_id: "media-1",
          library_id: "lib-1",
          media_title: "知否知否应是绿肥红瘦",
          media_kind: "tv",
          path: "/media/tv/知否/S01E01.strm",
          source_path: "/cloud/115/S01E01.mkv",
          units: [{ season: 1, episode: 1 }],
          resolution: "2160p",
          codec: "hevc",
          hdr: "HDR10",
          quality_source: "probe",
          confidence: "high",
          filter_score: null,
          is_strm: true,
          transfer_mode: "strm",
          file_size: null,
          duration_ms: 2_760_000,
        },
        isRetransferring: false,
        isRefreshing: false,
        deleteDisabled: false,
        onRetransfer() {},
        onRefreshMetadata() {},
        onDelete() {},
      }),
    );
  });

  const cardText = collectText(cardRenderer.toJSON());
  assert.match(cardText, /知否知否应是绿肥红瘦/);
  assert.match(cardText, /S01E01/);
  assert.match(cardText, /46 分钟/);
  assert.match(cardText, /云端直链/);
  assert.doesNotMatch(cardText, /128 B/);

  const retransferBtn = cardRenderer.root
    .findAllByType("button")
    .find((btn) => collectText(btn).includes("重新生成文件"));
  assert.ok(retransferBtn);
  assert.equal(retransferBtn.props.disabled, true);
  assert.equal(retransferBtn.props.title, "STRM 云端直链无需重建本地硬链接");

  await act(async () => {
    summaryRenderer.unmount();
    cardRenderer.unmount();
  });
});

test("MediaTransferView fetches first 50 rows, toggles to grouped view, and expands work episodes lazily", async () => {
  const iconStub = () => React.createElement("span");
  const apiCalls = [];
  const load = createModuleLoader({
    "@/components/icons": new Proxy({}, { get: () => iconStub }),
    "@/components/copy-button": {
      CopyButton: () => React.createElement("button", { type: "button" }, "copy"),
    },
    "@/components/feedback": {
      useToast: () => ({ success() {}, error() {} }),
    },
    "@/components/modal": {
      Modal: ({ open, children }) => (open ? React.createElement("div", null, children) : null),
    },
    "@/lib/api/transfers": {
      listTransfers: async (params) => {
        apiCalls.push(params);
        if (params.view === "groups") {
          return {
            summary: {
              total_files: 4,
              media_count: 2,
              movie_files: 1,
              tv_files: 3,
              strm_count: 3,
              hardlink_count: 1,
              copy_count: 0,
              move_count: 0,
              local_size_bytes: 4_000_000_000,
              probed_duration_ms: 2_760_000,
              strm_probed_count: 1,
            },
            view: "groups",
            filtered_total: 2,
            filtered_file_total: 4,
            offset: 0,
            limit: 50,
            has_more: false,
            items: [],
            groups: [
              {
                media_id: "tv-media-1",
                media_title: "知否知否应是绿肥红瘦",
                media_kind: "tv",
                library_id: "lib-tv",
                file_count: 3,
                strm_count: 3,
                hardlink_count: 0,
                local_size_bytes: 0,
                total_duration_ms: 2_760_000,
                primary_resolution: "2160p",
                primary_mode: "strm",
                sample_dir: "/media/tv/知否",
              },
            ],
          };
        }
        return {
          summary: {
            total_files: 4,
            media_count: 2,
            movie_files: 1,
            tv_files: 3,
            strm_count: 3,
            hardlink_count: 1,
            copy_count: 0,
            move_count: 0,
            local_size_bytes: 4_000_000_000,
            probed_duration_ms: 2_760_000,
            strm_probed_count: 1,
          },
          view: "files",
          filtered_total: 3,
          filtered_file_total: 3,
          offset: 0,
          limit: params.limit ?? 50,
          has_more: false,
          items: [
            {
              id: "ep-1",
              media_id: "tv-media-1",
              library_id: "lib-tv",
              media_title: "知否知否应是绿肥红瘦",
              media_kind: "tv",
              path: "/media/tv/知否/S01E01.strm",
              source_path: null,
              units: [{ season: 1, episode: 1 }],
              resolution: "2160p",
              codec: "hevc",
              hdr: null,
              quality_source: "probe",
              confidence: "high",
              filter_score: null,
              is_strm: true,
              transfer_mode: "strm",
              file_size: null,
              duration_ms: 2_760_000,
            },
          ],
          groups: [],
        };
      },
      deleteLedgerRow: async () => ({ deleted: true, path: "", file_deleted: false }),
      retransferLedgerRow: async () => ({ retransferred: true, source: "", destination: "" }),
      refreshItemMetadata: async () => ({ ok: true }),
    },
  });

  const { MediaTransferView } = load("components/media-transfer-view.tsx");

  let viewRenderer;
  await act(async () => {
    viewRenderer = create(React.createElement(MediaTransferView));
  });

  assert.equal(apiCalls.length, 1);
  assert.equal(apiCalls[0].view, "files");
  assert.equal(apiCalls[0].limit, 50);

  const initialText = collectText(viewRenderer.toJSON());
  assert.match(initialText, /本地归档视频体积/);
  assert.match(initialText, /另含\s*3\s*项 STRM 云端直链/);

  const groupToggle = viewRenderer.root
    .findAllByType("button")
    .find((btn) => collectText(btn).includes("按作品聚合"));
  await act(async () => {
    groupToggle.props.onClick();
  });

  assert.equal(apiCalls.at(-1).view, "groups");
  assert.match(collectText(viewRenderer.toJSON()), /3\s*个文件/);

  const expandBtn = viewRenderer.root
    .findAllByType("button")
    .find((btn) => collectText(btn).includes("展开文件"));
  await act(async () => {
    expandBtn.props.onClick();
  });

  assert.equal(apiCalls.at(-1).view, "files");
  assert.equal(apiCalls.at(-1).media_id, "tv-media-1");
  assert.equal(apiCalls.at(-1).limit, 200);
  assert.match(collectText(viewRenderer.toJSON()), /S01E01\.strm/);

  const rootJson = viewRenderer.toJSON();
  assert.equal(rootJson.props["data-scroll-root"], true);
  assert.match(String(rootJson.props.className ?? ""), /overflow-y-auto/);

  await act(async () => {
    viewRenderer.unmount();
  });
});

test("VirtualTransferList tracks inner [data-scroll-root] container scroll when window.scrollY stays 0", async () => {
  const iconStub = () => React.createElement("span");
  const load = createModuleLoader({
    "@/components/icons": new Proxy({}, { get: () => iconStub }),
  });
  const { VirtualTransferList } = load("components/transfers/virtual-transfer-list.tsx");

  const scrollListeners = new Set();
  const prevWindow = globalThis.window;
  const mockWin = {
    scrollY: 0,
    innerHeight: 900,
    setTimeout: globalThis.setTimeout.bind(globalThis),
    clearTimeout: globalThis.clearTimeout.bind(globalThis),
    requestAnimationFrame(cb) {
      cb(0);
      return 1;
    },
    cancelAnimationFrame() {},
    addEventListener() {},
    removeEventListener() {},
  };
  globalThis.window = mockWin;

  const fakeScrollRoot = {
    scrollTop: 0,
    clientWidth: 1000,
    clientHeight: 800,
    ownerDocument: { defaultView: mockWin },
    getBoundingClientRect: () => ({ top: 40, bottom: 840, left: 0, right: 1000, width: 1000, height: 800 }),
    addEventListener(type, fn) {
      if (type === "scroll") scrollListeners.add(fn);
    },
    removeEventListener(type, fn) {
      if (type === "scroll") scrollListeners.delete(fn);
    },
  };

  const items = Array.from({ length: 120 }, (_, i) => ({ id: `row-${i}`, label: `ROW_${i}_END` }));
  let containerTopAtZero = 140;

  let renderer;
  try {
    await act(async () => {
      renderer = create(
        React.createElement(VirtualTransferList, {
          items,
          getKey: (it) => it.id,
          defaultItemHeight: 148,
          overscan: 2,
          gap: 12,
          hasMore: false,
          loadingMore: false,
          onLoadMore() {},
          renderItem: (it) => React.createElement("div", null, it.label),
        }),
        {
          createNodeMock(element) {
            if (element.type === "div") {
              return {
                offsetHeight: 148,
                ownerDocument: { defaultView: mockWin },
                getAttribute(attr) {
                  const val = element.props?.[attr];
                  return val === undefined ? null : String(val);
                },
                closest(sel) {
                  return sel === "[data-scroll-root]" ? fakeScrollRoot : null;
                },
                getBoundingClientRect() {
                  const top = containerTopAtZero - fakeScrollRoot.scrollTop;
                  return { top, bottom: top + 19200, left: 0, right: 1000, width: 1000, height: 19200 };
                },
              };
            }
            return null;
          },
        },
      );
    });

    assert.match(collectText(renderer.toJSON()), /ROW_0_END/);
    assert.doesNotMatch(collectText(renderer.toJSON()), /ROW_30_END/);

    // Scroll inner container to 4800px while window.scrollY remains 0!
    await act(async () => {
      fakeScrollRoot.scrollTop = 4800;
      for (const fn of [...scrollListeners]) fn();
    });
    const textAt4800 = collectText(renderer.toJSON());
    assert.match(
      textAt4800,
      /ROW_30_END/,
      `Expected ROW_30_END when inner scrollRoot.scrollTop=4800 (got: ${textAt4800})`,
    );

    // Scroll inner container deeper to 11200px while window.scrollY remains 0!
    await act(async () => {
      fakeScrollRoot.scrollTop = 11200;
      for (const fn of [...scrollListeners]) fn();
    });
    const textAt11200 = collectText(renderer.toJSON());
    assert.match(
      textAt11200,
      /ROW_70_END/,
      `Expected ROW_70_END when inner scrollRoot.scrollTop=11200 (got: ${textAt11200})`,
    );
  } finally {
    if (renderer) {
      await act(async () => renderer.unmount());
    }
    globalThis.window = prevWindow;
  }
});
