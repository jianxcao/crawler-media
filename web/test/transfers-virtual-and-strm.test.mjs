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
const toastStub = { success() {}, error() {} };

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

test("transfer-helpers drops dead computeVirtualRange and VirtualTransferList uses stable measureRow + ResizeObserver scrollMargin", async () => {
  const iconStub = () => React.createElement("span");
  const measureCalls = [];
  let latestVirtualizerOpts = null;

  const loadWithSpy = createModuleLoader({
    "@/components/icons": new Proxy({}, { get: () => iconStub }),
    "@tanstack/react-virtual": {
      useVirtualizer: (opts) => {
        latestVirtualizerOpts = opts;
        const [instance] = React.useState(() => ({
          options: { scrollMargin: 0 },
          getTotalSize: () => latestVirtualizerOpts.count * 148,
          getVirtualItems: () =>
            latestVirtualizerOpts.count > 0
              ? [
                  {
                    index: 0,
                    key: latestVirtualizerOpts.getItemKey(0),
                    start: latestVirtualizerOpts.scrollMargin ?? 0,
                    size: 148,
                  },
                ]
              : [],
          measureElement(el) {
            measureCalls.push(el);
          },
        }));
        instance.options.scrollMargin = opts.scrollMargin ?? 0;
        return instance;
      },
    },
  });

  const helpers = loadWithSpy("components/transfers/transfer-helpers.tsx");
  assert.equal(
    "computeVirtualRange" in helpers,
    false,
    "Dead legacy computeVirtualRange helper must be deleted",
  );

  const { VirtualTransferList } = loadWithSpy("components/transfers/virtual-transfer-list.tsx");

  const resizeObservers = [];
  const prevWindow = globalThis.window;
  const prevResizeObserver = globalThis.ResizeObserver;
  globalThis.window = {
    addEventListener() {},
    removeEventListener() {},
  };
  globalThis.ResizeObserver = class {
    constructor(cb) {
      this.cb = cb;
      this.targets = [];
      resizeObservers.push(this);
    }
    observe(target) {
      this.targets.push(target);
    }
    unobserve() {}
    disconnect() {
      this.disconnected = true;
    }
  };

  let containerTop = 180;
  const parentEl = { nodeName: "DIV" };
  const fakeScroller = {
    scrollTop: 0,
    getBoundingClientRect: () => ({ top: 40, bottom: 840, left: 0, right: 1000, width: 1000, height: 800 }),
  };
  const rowDomNode = {
    getAttribute: (name) => (name === "data-index" ? "0" : null),
  };

  let renderer;
  try {
    await act(async () => {
      renderer = create(
        React.createElement(VirtualTransferList, {
          items: [{ id: "r1", label: "Row 1" }],
          getKey: (it) => it.id,
          defaultItemHeight: 148,
          scrollElement: fakeScroller,
          hasMore: true,
          loadingMore: false,
          onLoadMore() {},
          renderItem: (it) => React.createElement("span", null, it.label),
        }),
        {
          createNodeMock(element) {
            if (element.props?.["data-transfer-row"] !== undefined) {
              return rowDomNode;
            }
            if (element.type === "div") {
              return {
                parentElement: parentEl,
                getBoundingClientRect: () => ({
                  top: containerTop,
                  bottom: containerTop + 500,
                  left: 0,
                  right: 1000,
                  width: 1000,
                  height: 500,
                }),
              };
            }
            return null;
          },
        },
      );
    });

    assert.equal(latestVirtualizerOpts.scrollMargin, 140);
    const initialMeasureCount = measureCalls.length;
    assert.ok(initialMeasureCount >= 1);

    // Re-rendering with updated prop (loadingMore=true) must NOT re-attach inline callback ref!
    await act(async () => {
      renderer.update(
        React.createElement(VirtualTransferList, {
          items: [{ id: "r1", label: "Row 1" }],
          getKey: (it) => it.id,
          defaultItemHeight: 148,
          scrollElement: fakeScroller,
          hasMore: true,
          loadingMore: true,
          onLoadMore() {},
          renderItem: (it) => React.createElement("span", null, it.label),
        }),
      );
    });
    assert.equal(
      measureCalls.length,
      initialMeasureCount,
      "Stable useCallback ref must not re-fire measureElement on parent re-renders",
    );

    // Layout shift above list (e.g. error banner mounted or Sidebar toggled) reported via ResizeObserver updates scrollMargin!
    assert.ok(
      resizeObservers.some((ro) => ro.targets.includes(fakeScroller)),
      "ResizeObserver must observe activeScrollEl",
    );
    containerTop = 260;
    await act(async () => {
      for (const ro of resizeObservers) ro.cb([]);
    });
    assert.equal(
      latestVirtualizerOpts.scrollMargin,
      220,
      "ResizeObserver callback must recalculate scrollMargin when layout shifts",
    );

    // Unmounting row must forward null to measureElement so @tanstack/virtual-core purges detached nodes!
    await act(async () => {
      renderer.update(
        React.createElement(VirtualTransferList, {
          items: [],
          getKey: (it) => it.id,
          defaultItemHeight: 148,
          scrollElement: fakeScroller,
          hasMore: false,
          loadingMore: false,
          onLoadMore() {},
          renderItem: (it) => React.createElement("span", null, it.label),
        }),
      );
    });
    assert.equal(
      measureCalls.at(-1),
      null,
      "Unmounting a virtual row must invoke measureElement(null) to purge elementsCache",
    );
  } finally {
    if (renderer) await act(async () => renderer.unmount());
    if (prevWindow === undefined) delete globalThis.window;
    else globalThis.window = prevWindow;
    if (prevResizeObserver === undefined) delete globalThis.ResizeObserver;
    else globalThis.ResizeObserver = prevResizeObserver;
  }
});

test("MediaTransferView avoids empty-state and stale-list flashes across viewMode & filter transitions", async () => {
  const iconStub = () => React.createElement("span");
  const pendingResolvers = [];

  const summary = {
    total_files: 10,
    media_count: 3,
    movie_files: 0,
    tv_files: 10,
    strm_count: 10,
    hardlink_count: 0,
    copy_count: 0,
    move_count: 0,
    local_size_bytes: 0,
    probed_duration_ms: 600_000,
    strm_probed_count: 10,
  };

  const load = createModuleLoader({
    "@/components/icons": new Proxy({}, { get: () => iconStub }),
    "@/components/copy-button": {
      CopyButton: () => React.createElement("button", { type: "button" }, "copy"),
    },
    "@/components/feedback": {
      useToast: () => toastStub,
    },
    "@/components/modal": {
      Modal: ({ open, children }) => (open ? React.createElement("div", null, children) : null),
    },
    "@/lib/api/transfers": {
      listTransfers: (params) =>
        new Promise((resolve) => {
          pendingResolvers.push({ params, resolve });
        }),
      deleteLedgerRow: async () => ({ deleted: true, path: "", file_deleted: false }),
      retransferLedgerRow: async () => ({ retransferred: true, source: "", destination: "" }),
      refreshItemMetadata: async () => ({ ok: true }),
    },
  });

  const { MediaTransferView } = load("components/media-transfer-view.tsx");

  let renderer;
  await act(async () => {
    renderer = create(React.createElement(MediaTransferView));
  });

  try {
    // 1. Resolve initial files load
    assert.equal(pendingResolvers.length, 1);
    await act(async () => {
      pendingResolvers.shift().resolve({
        summary,
        view: "files",
        filtered_total: 1,
        filtered_file_total: 1,
        offset: 0,
        limit: 50,
        has_more: false,
        items: [
          {
            id: "f-1",
            media_id: "m-1",
            library_id: "lib-1",
            media_title: "初始文件作品",
            media_kind: "tv",
            path: "/tv/S01E01.strm",
            source_path: null,
            units: [{ season: 1, episode: 1 }],
            resolution: "1080p",
            codec: "hevc",
            hdr: null,
            quality_source: "probe",
            confidence: "high",
            filter_score: null,
            is_strm: true,
            transfer_mode: "strm",
            file_size: null,
            duration_ms: 60_000,
          },
        ],
        groups: [],
      });
    });

    // 2. Switch to "groups" BEFORE resolving the groups fetch -> must show loading indicator, NOT "未找到匹配的入库记录"!
    const groupToggle = renderer.root
      .findAllByType("button")
      .find((btn) => collectText(btn).includes("按作品聚合"));
    await act(async () => {
      groupToggle.props.onClick();
    });

    const midSwitchText = collectText(renderer.toJSON());
    assert.doesNotMatch(
      midSwitchText,
      /未找到匹配的入库记录|暂无整理入库记录/,
      "Switching viewMode must not flash empty state while new mode is fetching",
    );
    assert.ok(
      renderer.root.findAll((n) => n.props?.role === "status").length > 0,
      "MediaTransferView must render a loading skeleton/status when activeListCount === 0 && ctl.loading",
    );

    // Resolve groups with unfiltered item "未过滤旧分组"
    await act(async () => {
      pendingResolvers.shift().resolve({
        summary,
        view: "groups",
        filtered_total: 1,
        filtered_file_total: 1,
        offset: 0,
        limit: 50,
        has_more: false,
        items: [],
        groups: [
          {
            media_id: "g-old",
            media_title: "未过滤旧分组",
            media_kind: "tv",
            library_id: "lib-1",
            file_count: 1,
            strm_count: 1,
            hardlink_count: 0,
            local_size_bytes: 0,
            total_duration_ms: 60_000,
            primary_resolution: "1080p",
            primary_mode: "strm",
            sample_dir: "/tv",
          },
        ],
      });
    });
    assert.match(collectText(renderer.toJSON()), /未过滤旧分组/);

    // 3. Switch back to "files", resolve it, then change kindFilter to "movie" (which invalidates inactive groupItems!)
    const filesToggle = renderer.root
      .findAllByType("button")
      .find((btn) => collectText(btn).includes("逐文件列表"));
    await act(async () => {
      filesToggle.props.onClick();
    });
    await act(async () => {
      pendingResolvers.shift().resolve({
        summary,
        view: "files",
        filtered_total: 0,
        filtered_file_total: 0,
        offset: 0,
        limit: 50,
        has_more: false,
        items: [],
        groups: [],
      });
    });

    const movieFilterBtn = renderer.root
      .findAllByType("button")
      .find((btn) => collectText(btn).trim() === "电影");
    await act(async () => {
      movieFilterBtn.props.onClick();
    });
    await act(async () => {
      pendingResolvers.shift().resolve({
        summary,
        view: "files",
        filtered_total: 0,
        filtered_file_total: 0,
        offset: 0,
        limit: 50,
        has_more: false,
        items: [],
        groups: [],
      });
    });

    // 4. Now switch back to "groups" while "movie" filter is active -> must NOT flash stale "未过滤旧分组"!
    await act(async () => {
      groupToggle.props.onClick();
    });
    assert.doesNotMatch(
      collectText(renderer.toJSON()),
      /未过滤旧分组/,
      "Changing filters in files view must clear stale groupItems so switching back never flashes unfiltered groups",
    );
    assert.doesNotMatch(
      collectText(renderer.toJSON()),
      /未找到匹配的入库记录/,
      "Switching back to cleared groups view must show loading state until fetch completes",
    );
  } finally {
    await act(async () => renderer.unmount());
  }
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
      useToast: () => toastStub,
    },
    "@/components/modal": {
      Modal: ({ open, children }) => (open ? React.createElement("div", null, children) : null),
    },
    "@/lib/api/transfers": {
      listTransfers: async (params) => {
        apiCalls.push(params);
        if (apiCalls.length > 10) {
          throw new Error(`Pagination loop detected after ${apiCalls.length} calls`);
        }
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

test("VirtualTransferList & useTransferController block auto-load on error, allow manual retry, and stop on empty tail page", async () => {
  const iconStub = () => React.createElement("span");
  const apiCalls = [];
  let failOffset50Once = true;

  const makeFileItem = (i) => ({
    id: `file-${i}`,
    media_id: `media-${i}`,
    library_id: "lib-1",
    media_title: `作品 ${i}`,
    media_kind: "movie",
    path: `/media/movies/work-${i}.mkv`,
    source_path: `/downloads/work-${i}.mkv`,
    units: [],
    resolution: "1080p",
    codec: "hevc",
    hdr: null,
    quality_source: "probe",
    confidence: "high",
    filter_score: null,
    is_strm: false,
    transfer_mode: "hardlink",
    file_size: 1_000_000,
    duration_ms: 60_000,
  });

  const summary = {
    total_files: 60,
    media_count: 60,
    movie_files: 60,
    tv_files: 0,
    strm_count: 0,
    hardlink_count: 60,
    copy_count: 0,
    move_count: 0,
    local_size_bytes: 60_000_000,
    probed_duration_ms: 3_600_000,
    strm_probed_count: 0,
  };

  const load = createModuleLoader({
    "@/components/icons": new Proxy({}, { get: () => iconStub }),
    "@/components/copy-button": {
      CopyButton: () => React.createElement("button", { type: "button" }, "copy"),
    },
    "@/components/feedback": {
      useToast: () => toastStub,
    },
    "@/components/modal": {
      Modal: ({ open, children }) => (open ? React.createElement("div", null, children) : null),
    },
    "@/lib/api/transfers": {
      listTransfers: async (params) => {
        apiCalls.push(params);
        if (apiCalls.length > 10) {
          return {
            summary,
            view: "files",
            filtered_total: 60,
            filtered_file_total: 60,
            offset: params.offset ?? 0,
            limit: 50,
            has_more: false,
            items: [],
            groups: [],
          };
        }
        if (params.offset === 0) {
          return {
            summary,
            view: "files",
            filtered_total: 60,
            filtered_file_total: 60,
            offset: 0,
            limit: 50,
            has_more: true,
            items: Array.from({ length: 12 }, (_, i) => makeFileItem(i)),
            groups: [],
          };
        }
        if (params.offset === 12) {
          if (failOffset50Once) {
            failOffset50Once = false;
            throw new Error("模拟网络超时");
          }
          return {
            summary,
            view: "files",
            filtered_total: 60,
            filtered_file_total: 60,
            offset: 12,
            limit: 50,
            has_more: true,
            items: Array.from({ length: 5 }, (_, i) => makeFileItem(12 + i)),
            groups: [],
          };
        }
        // Empty tail page with stale has_more: true from server must force hasMore=false!
        return {
          summary,
          view: "files",
          filtered_total: 60,
          filtered_file_total: 60,
          offset: params.offset,
          limit: 50,
          has_more: true,
          items: [],
          groups: [],
        };
      },
      deleteLedgerRow: async () => ({ deleted: true, path: "", file_deleted: false }),
      retransferLedgerRow: async () => ({ retransferred: true, source: "", destination: "" }),
      refreshItemMetadata: async () => ({ ok: true }),
    },
  });

  const { MediaTransferView } = load("components/media-transfer-view.tsx");

  let renderer;
  await act(async () => {
    renderer = create(React.createElement(MediaTransferView));
  });

  // Initial head load (offset=0 -> 12 items, lastVirtualIndex near end) + 1 auto-load attempt (offset=12) that failed
  assert.deepEqual(
    apiCalls.map((c) => c.offset),
    [0, 12],
    "Failed auto-load must set loadMoreError and pause further automatic retries",
  );

  // Typing in search without debouncing yet (inline callback identity changes on re-render) must NOT re-trigger auto-load while loadMoreError=true!
  const searchInput = renderer.root.findByType("input");
  await act(async () => {
    searchInput.props.onChange({ target: { value: " " } });
  });
  assert.deepEqual(apiCalls.map((c) => c.offset), [0, 12]);

  // Manual "加载更多" button calls retryLoadMore -> clears error, fetches offset=12 (5 items), then auto-loads offset=17 (0 items -> forces hasMore=false and stops!)
  const loadMoreBtn = renderer.root
    .findAllByType("button")
    .find((btn) => collectText(btn).includes("加载更多"));
  assert.ok(loadMoreBtn, "Manual 加载更多 button must remain available after error");

  await act(async () => {
    loadMoreBtn.props.onClick();
  });

  assert.deepEqual(
    apiCalls.map((c) => c.offset),
    [0, 12, 12, 17],
    "Manual retry must succeed and empty tail page at offset=17 must stop further pagination",
  );
  const remainingLoadMoreBtn = renderer.root
    .findAllByType("button")
    .find((btn) => collectText(btn).includes("加载更多"));
  assert.equal(remainingLoadMoreBtn, undefined, "Empty tail page must set hasMore=false");

  await act(async () => {
    renderer.unmount();
  });
});

test("confirmDelete preserves loaded >50 list without resetting scroll, and useGroupExpansion aborts stale filter requests & uses functional catch", async () => {
  const iconStub = () => React.createElement("span");
  const scrollToCalls = [];
  let pendingGroupExpand = null;
  let pendingGroupLoadMore = null;

  const makeFileItem = (i, mediaId = `media-${i}`) => ({
    id: `file-${i}`,
    media_id: mediaId,
    library_id: "lib-1",
    media_title: `剧集 ${i}`,
    media_kind: "tv",
    path: `/media/tv/ep-${i}.mkv`,
    source_path: `/downloads/ep-${i}.mkv`,
    units: [{ season: 1, episode: i + 1 }],
    resolution: "1080p",
    codec: "hevc",
    hdr: null,
    quality_source: "probe",
    confidence: "high",
    filter_score: null,
    is_strm: false,
    transfer_mode: "hardlink",
    file_size: 1_000_000,
    duration_ms: 60_000,
  });

  let totalFiles = 75;
  const makeSummary = () => ({
    total_files: totalFiles,
    media_count: 10,
    movie_files: 0,
    tv_files: totalFiles,
    strm_count: 0,
    hardlink_count: totalFiles,
    copy_count: 0,
    move_count: 0,
    local_size_bytes: totalFiles * 1_000_000,
    probed_duration_ms: totalFiles * 60_000,
    strm_probed_count: 0,
  });

  const groupSignals = [];

  const load = createModuleLoader({
    "@/components/icons": new Proxy({}, { get: () => iconStub }),
    "@/components/copy-button": {
      CopyButton: () => React.createElement("button", { type: "button" }, "copy"),
    },
    "@/components/feedback": {
      useToast: () => toastStub,
    },
    "@/components/modal": {
      Modal: ({ open, children }) => (open ? React.createElement("div", null, children) : null),
    },
    "@/lib/api/transfers": {
      listTransfers: async (params, init) => {
        if (params.media_id) {
          groupSignals.push({ params, signal: init?.signal });
          if (params.offset > 0) {
            return new Promise((resolve, reject) => {
              pendingGroupLoadMore = { resolve, reject, signal: init?.signal };
            });
          }
          return new Promise((resolve, reject) => {
            pendingGroupExpand = { resolve, reject, signal: init?.signal };
          });
        }
        if (params.view === "groups") {
          return {
            summary: makeSummary(),
            view: "groups",
            filtered_total: 1,
            filtered_file_total: 3,
            offset: 0,
            limit: 50,
            has_more: false,
            items: [],
            groups: [
              {
                media_id: "tv-group-1",
                media_title: "漫长的季节",
                media_kind: "tv",
                library_id: "lib-1",
                file_count: 3,
                strm_count: 0,
                hardlink_count: 3,
                local_size_bytes: 3_000_000,
                total_duration_ms: 180_000,
                primary_resolution: "1080p",
                primary_mode: "hardlink",
                sample_dir: "/media/tv/漫长的季节",
              },
            ],
          };
        }
        if (params.limit === 1) {
          return {
            summary: makeSummary(),
            view: "files",
            filtered_total: totalFiles,
            filtered_file_total: totalFiles,
            offset: 0,
            limit: 1,
            has_more: totalFiles > 1,
            items: [makeFileItem(1)],
            groups: [],
          };
        }
        if (params.offset === 0) {
          return {
            summary: makeSummary(),
            view: "files",
            filtered_total: totalFiles,
            filtered_file_total: totalFiles,
            offset: 0,
            limit: 50,
            has_more: true,
            items: Array.from({ length: 50 }, (_, i) => makeFileItem(i)),
            groups: [],
          };
        }
        return {
          summary: makeSummary(),
          view: "files",
          filtered_total: totalFiles,
          filtered_file_total: totalFiles,
          offset: 50,
          limit: 50,
          has_more: false,
          items: Array.from({ length: 25 }, (_, i) => makeFileItem(50 + i)),
          groups: [],
        };
      },
      deleteLedgerRow: async () => {
        totalFiles -= 1;
        return { deleted: true, path: "", file_deleted: false };
      },
      retransferLedgerRow: async () => ({ retransferred: true, source: "", destination: "" }),
      refreshItemMetadata: async () => ({ ok: true }),
    },
  });

  const { MediaTransferView } = load("components/media-transfer-view.tsx");

  let renderer;
  await act(async () => {
    renderer = create(React.createElement(MediaTransferView), {
      createNodeMock(element) {
        if (element.props?.["data-scroll-root"]) {
          return {
            scrollTop: 0,
            scrollTo(opts) {
              scrollToCalls.push(opts);
            },
            getBoundingClientRect: () => ({ top: 0, bottom: 800, left: 0, right: 1000, width: 1000, height: 800 }),
            addEventListener() {},
            removeEventListener() {},
          };
        }
        return null;
      },
    });
  });

  // Load second page via manual button so 75 items are in memory
  const loadMoreBtn = renderer.root
    .findAllByType("button")
    .find((btn) => collectText(btn).includes("加载更多"));
  assert.ok(loadMoreBtn);
  await act(async () => {
    loadMoreBtn.props.onClick();
  });

  const scrollCallsBeforeDelete = scrollToCalls.length;

  // Delete file-0 and verify 74 items remain (NOT truncated back to 50) and scroll is NOT reset!
  const firstDeleteBtn = renderer.root
    .findAllByType("button")
    .find((btn) => collectText(btn).includes("删除管理"));
  assert.ok(firstDeleteBtn);
  await act(async () => {
    firstDeleteBtn.props.onClick();
  });

  const confirmBtn = renderer.root
    .findAllByType("button")
    .find((btn) => collectText(btn).includes("确认删除"));
  assert.ok(confirmBtn);
  await act(async () => {
    confirmBtn.props.onClick();
  });

  assert.equal(
    scrollToCalls.length,
    scrollCallsBeforeDelete,
    "Deleting an item must not reset scroll position to top",
  );
  // Total height of VirtualTransferList reflects 74 items (74 * 148 + 73 * 12 = 11828px), NOT 50 items (7988px)!
  const listTrack = renderer.root.findAllByType("div").find((d) => d.props.style?.position === "relative" && String(d.props.style?.height).endsWith("px"));
  assert.equal(listTrack.props.style.height, `${74 * 148 + 73 * 12}px`);

  // Switching viewMode DOES reset scroll to top: 0
  const groupToggle = renderer.root
    .findAllByType("button")
    .find((btn) => collectText(btn).includes("按作品聚合"));
  await act(async () => {
    groupToggle.props.onClick();
  });
  assert.ok(scrollToCalls.length > scrollCallsBeforeDelete, "Changing viewMode must reset scroll to top");

  // Start expanding group -> in-flight request gets AbortSignal
  const expandBtn = renderer.root
    .findAllByType("button")
    .find((btn) => collectText(btn).includes("展开文件"));
  await act(async () => {
    expandBtn.props.onClick();
  });
  assert.ok(pendingGroupExpand?.signal, "listTransfers in toggleGroupExpand must receive AbortSignal");
  const firstExpandReq = pendingGroupExpand;

  // Change kindFilter while expand is in-flight -> aborts in-flight controller and ignores stale resolution!
  const tvKindBtn = renderer.root
    .findAllByType("button")
    .find((btn) => collectText(btn).trim() === "剧集");
  await act(async () => {
    tvKindBtn.props.onClick();
  });
  assert.equal(firstExpandReq.signal.aborted, true, "Changing kindFilter must abort in-flight group expand");

  await act(async () => {
    firstExpandReq.resolve({
      summary: makeSummary(),
      view: "files",
      filtered_total: 1,
      filtered_file_total: 1,
      offset: 0,
      limit: 200,
      has_more: false,
      items: [makeFileItem(999, "tv-group-1")],
      groups: [],
    });
  });
  assert.doesNotMatch(collectText(renderer.toJSON()), /ep-999\.mkv/, "Stale aborted group response must be ignored");

  // Now expand cleanly with 2 episodes and has_more: true, start loadMoreGroupFiles, delete ep-0 while pending, then fail loadMoreGroupFiles
  const expandBtn2 = renderer.root
    .findAllByType("button")
    .find((btn) => collectText(btn).includes("展开文件"));
  await act(async () => {
    expandBtn2.props.onClick();
  });
  await act(async () => {
    pendingGroupExpand.resolve({
      summary: makeSummary(),
      view: "files",
      filtered_total: 3,
      filtered_file_total: 3,
      offset: 0,
      limit: 200,
      has_more: true,
      items: [makeFileItem(0, "tv-group-1"), makeFileItem(1, "tv-group-1")],
      groups: [],
    });
  });
  assert.match(collectText(renderer.toJSON()), /ep-0\.mkv/);

  const loadMoreGroupBtn = renderer.root
    .findAllByType("button")
    .find((btn) => collectText(btn).includes("加载剩余剧集文件"));
  await act(async () => {
    loadMoreGroupBtn.props.onClick();
  });
  assert.ok(pendingGroupLoadMore?.signal, "loadMoreGroupFiles must pass AbortSignal");

  // Delete ep-0 while loadMoreGroupFiles is still in flight
  const deleteGroupFileBtn = renderer.root
    .findAllByType("button")
    .find((btn) => btn.props.title === "删除此文件记录");
  await act(async () => {
    deleteGroupFileBtn.props.onClick();
  });
  const confirmBtn2 = renderer.root
    .findAllByType("button")
    .find((btn) => collectText(btn).includes("确认删除"));
  await act(async () => {
    confirmBtn2.props.onClick();
  });
  assert.doesNotMatch(collectText(renderer.toJSON()), /ep-0\.mkv/);

  // Reject loadMoreGroupFiles -> functional catch must NOT resurrect deleted ep-0 from stale closure!
  await act(async () => {
    pendingGroupLoadMore.reject(new Error("分页超时"));
  });
  assert.doesNotMatch(
    collectText(renderer.toJSON()),
    /ep-0\.mkv/,
    "loadMoreGroupFiles catch must functionally update prev state without resurrecting deleted files",
  );
  assert.match(collectText(renderer.toJSON()), /ep-1\.mkv/);

  await act(async () => {
    renderer.unmount();
  });
});

test("useTransferController isolates per-mode pagination, resets expanded groups on refresh, updates expanded path on retransfer, and keeps StrictMode search updaters pure", async () => {
  const iconStub = () => React.createElement("span");
  const apiCalls = [];
  let failFilesRefetch = false;
  let groupEpisodeGeneration = 1;

  const summary = {
    total_files: 120,
    media_count: 5,
    movie_files: 0,
    tv_files: 120,
    strm_count: 0,
    hardlink_count: 120,
    copy_count: 0,
    move_count: 0,
    local_size_bytes: 120_000_000,
    probed_duration_ms: 7_200_000,
    strm_probed_count: 0,
  };

  const makeFileItem = (i, mediaId = "tv-1") => ({
    id: `file-${i}`,
    media_id: mediaId,
    library_id: "lib-1",
    media_title: `剧集作品 ${mediaId}`,
    media_kind: "tv",
    path: `/media/tv/${mediaId}/ep-${i}-v${groupEpisodeGeneration}.mkv`,
    source_path: `/downloads/${mediaId}/raw-${i}.mkv`,
    units: [{ season: 1, episode: i + 1 }],
    resolution: "1080p",
    codec: "hevc",
    hdr: null,
    quality_source: "probe",
    confidence: "high",
    filter_score: null,
    is_strm: false,
    transfer_mode: "hardlink",
    file_size: 1_000_000,
    duration_ms: 60_000,
  });

  const makeGroupItem = (i) => ({
    media_id: `tv-${i}`,
    media_title: `聚合作品 ${i}`,
    media_kind: "tv",
    library_id: "lib-1",
    file_count: 24,
    strm_count: 0,
    hardlink_count: 24,
    local_size_bytes: 24_000_000,
    total_duration_ms: 1_440_000,
    primary_resolution: "1080p",
    primary_mode: "hardlink",
    sample_dir: `/media/tv/tv-${i}`,
  });

  const load = createModuleLoader({
    "@/components/icons": new Proxy({}, { get: () => iconStub }),
    "@/components/copy-button": {
      CopyButton: ({ text }) => React.createElement("button", { type: "button", "data-copy": text }, "copy"),
    },
    "@/components/feedback": {
      useToast: () => toastStub,
    },
    "@/components/modal": {
      Modal: ({ open, children }) => (open ? React.createElement("div", null, children) : null),
    },
    "@/lib/api/transfers": {
      listTransfers: async (params) => {
        apiCalls.push(params);
        if (params.media_id) {
          return {
            summary,
            view: "files",
            filtered_total: 1,
            filtered_file_total: 1,
            offset: 0,
            limit: 200,
            has_more: false,
            items: [makeFileItem(0, params.media_id)],
            groups: [],
          };
        }
        if (params.view === "groups") {
          return {
            summary,
            view: "groups",
            filtered_total: 5,
            filtered_file_total: 120,
            offset: 0,
            limit: 50,
            has_more: false,
            items: [],
            groups: Array.from({ length: 5 }, (_, i) => makeGroupItem(i + 1)),
          };
        }
        if (params.limit === 1) {
          return {
            summary,
            view: "files",
            filtered_total: 120,
            filtered_file_total: 120,
            offset: 0,
            limit: 1,
            has_more: true,
            items: [makeFileItem(0)],
            groups: [],
          };
        }
        if (params.offset === 0 && failFilesRefetch) {
          throw new Error("切换回逐文件时网络抖动");
        }
        if (params.offset === 0) {
          return {
            summary,
            view: "files",
            filtered_total: 120,
            filtered_file_total: 120,
            offset: 0,
            limit: 50,
            has_more: true,
            items: Array.from({ length: 50 }, (_, i) => makeFileItem(i)),
            groups: [],
          };
        }
        return {
          summary,
          view: "files",
          filtered_total: 120,
          filtered_file_total: 120,
          offset: params.offset,
          limit: 50,
          has_more: false,
          items: Array.from({ length: 10 }, (_, i) => makeFileItem(params.offset + i)),
          groups: [],
        };
      },
      deleteLedgerRow: async () => ({ deleted: true, path: "", file_deleted: false }),
      retransferLedgerRow: async (id) => ({
        retransferred: true,
        source: "/downloads/tv-1/raw-0.mkv",
        destination: `/media/tv/tv-1/rebuilt-${id}.mkv`,
      }),
      refreshItemMetadata: async () => ({ ok: true }),
    },
  });

  const { MediaTransferView } = load("components/media-transfer-view.tsx");

  const consoleErrors = [];
  const origConsoleError = console.error;
  console.error = (...args) => {
    const msg = args.map(String).join(" ");
    if (msg.includes("react-test-renderer is deprecated")) return;
    consoleErrors.push(msg);
  };

  let renderer;
  try {
    await act(async () => {
      renderer = create(
        React.createElement(React.StrictMode, null, React.createElement(MediaTransferView)),
      );
    });

    // 1. Initial "files" mode has 50 items, filteredTotal=120, hasMore=true, nextOffset=50
    assert.match(collectText(renderer.toJSON()), /共找到\s*120\s*条入库记录/);

    // 2. Switch to "groups" -> filteredTotal=5, hasMore=false, nextOffset=5
    const groupToggle = renderer.root
      .findAllByType("button")
      .find((btn) => collectText(btn).includes("按作品聚合"));
    await act(async () => {
      groupToggle.props.onClick();
    });
    assert.match(collectText(renderer.toJSON()), /共匹配\s*5\s*部作品/);

    // 3. Expand tv-1 -> shows ep-0-v1.mkv; retransfer file-0 inside expanded group -> path updates immediately!
    const expandFirstGroup = () =>
      renderer.root
        .findAllByType("button")
        .find((btn) => collectText(btn).includes("展开文件"));
    await act(async () => {
      expandFirstGroup().props.onClick();
    });
    assert.match(collectText(renderer.toJSON()), /ep-0-v1\.mkv/);

    const rebuildBtn = renderer.root
      .findAllByType("button")
      .find((btn) => collectText(btn).trim() === "重建");
    assert.ok(rebuildBtn, "Expanded hardlink episode must render 重建 button");
    await act(async () => {
      await rebuildBtn.props.onClick();
    });
    assert.match(
      collectText(renderer.toJSON()),
      /rebuilt-file-0\.mkv/,
      "Retransferring an episode inside an expanded group must immediately update its displayed path",
    );

    // 4. Collapse tv-1 (which caches items), bump server episode generation to v2, click "刷新记录", then re-expand tv-1 -> must fetch fresh v2 instead of stale cache!
    const collapseBtn = renderer.root
      .findAllByType("button")
      .find((btn) => collectText(btn).includes("收起文件"));
    await act(async () => {
      collapseBtn.props.onClick();
    });
    groupEpisodeGeneration = 2;

    const refreshBtn = renderer.root
      .findAllByType("button")
      .find((btn) => collectText(btn).includes("刷新记录"));
    await act(async () => {
      await refreshBtn.props.onClick();
    });
    await act(async () => {
      expandFirstGroup().props.onClick();
    });
    assert.match(
      collectText(renderer.toJSON()),
      /ep-0-v2\.mkv/,
      "Manual 刷新记录 must clear cached expandedGroups so re-expanding fetches fresh episodes",
    );

    // 5. Switch back to "files" when head refetch fails -> cached 50 fileItems MUST keep files slice (filteredTotal=120, hasMore=true, nextOffset=50), NOT groups slice (5 / false / 5)!
    failFilesRefetch = true;
    const filesToggle = renderer.root
      .findAllByType("button")
      .find((btn) => collectText(btn).includes("逐文件列表"));
    await act(async () => {
      filesToggle.props.onClick();
    });

    const textAfterFailedSwitch = collectText(renderer.toJSON());
    assert.match(
      textAfterFailedSwitch,
      /共找到\s*120\s*条入库记录/,
      "Switching back to files must read files pagination slice (120), not groups filteredTotal (5)",
    );
    const loadMoreBtn = renderer.root
      .findAllByType("button")
      .find((btn) => collectText(btn).includes("加载更多"));
    assert.ok(
      loadMoreBtn,
      "Switching back to files must preserve files hasMore=true even when head refresh fails",
    );
    failFilesRefetch = false;
    await act(async () => {
      loadMoreBtn.props.onClick();
    });
    assert.equal(
      apiCalls.at(-1).offset,
      50,
      "loadMore in files mode must use files nextOffset (50), not groups nextOffset (5)",
    );

    // 6. Debounced search under <React.StrictMode> must keep state updaters pure (zero React render-phase update console errors)
    const searchInput = renderer.root.findByType("input");
    await act(async () => {
      searchInput.props.onChange({ target: { value: "漫长" } });
      await new Promise((r) => setTimeout(r, 240));
    });
    await act(async () => {
      searchInput.props.onChange({ target: { value: "" } });
    });
    assert.deepEqual(
      consoleErrors,
      [],
      `StrictMode search debounce/clear must not trigger impure state updater warnings: ${consoleErrors.join(" | ")}`,
    );
  } finally {
    console.error = origConsoleError;
    if (renderer) {
      await act(async () => renderer.unmount());
    }
  }
});

test("useTransferController tracks filterKey across failed filter fetches & rapid mode switches, and useGroupExpansion epoch blocks stale resolves", async () => {
  const iconStub = () => React.createElement("span");
  const apiCalls = [];
  let failStrmHeadOnce = true;
  let holdMovieHead = false;
  const heldMovieResolvers = [];
  const pendingGroupPromises = [];

  const summary = {
    total_files: 120,
    media_count: 5,
    movie_files: 20,
    tv_files: 100,
    strm_count: 40,
    hardlink_count: 80,
    copy_count: 0,
    move_count: 0,
    local_size_bytes: 80_000_000,
    probed_duration_ms: 3_600_000,
    strm_probed_count: 40,
  };

  const makeFile = (id, mode = "hardlink") => ({
    id,
    media_id: `m-${id}`,
    library_id: "lib-1",
    media_title: `记录_${id}`,
    media_kind: "tv",
    path: `/media/tv/${id}.${mode === "strm" ? "strm" : "mkv"}`,
    source_path: mode === "strm" ? null : `/downloads/${id}.mkv`,
    units: [{ season: 1, episode: 1 }],
    resolution: "1080p",
    codec: "hevc",
    hdr: null,
    quality_source: "probe",
    confidence: "high",
    filter_score: null,
    is_strm: mode === "strm",
    transfer_mode: mode,
    file_size: mode === "strm" ? null : 1_000_000,
    duration_ms: 60_000,
  });

  const load = createModuleLoader({
    "@/components/icons": new Proxy({}, { get: () => iconStub }),
    "@/components/copy-button": {
      CopyButton: () => React.createElement("button", { type: "button" }, "copy"),
    },
    "@/components/feedback": {
      useToast: () => toastStub,
    },
    "@/components/modal": {
      Modal: ({ open, children }) => (open ? React.createElement("div", null, children) : null),
    },
    "@/lib/api/transfers": {
      listTransfers: (params) => {
        apiCalls.push(params);
        if (params.media_id) {
          // Intentionally ignore init.signal to prove generation epoch alone blocks stale resolves!
          return new Promise((resolve) => {
            pendingGroupPromises.push({ params, resolve });
          });
        }
        if (params.mode === "strm" && params.offset === 0 && failStrmHeadOnce) {
          failStrmHeadOnce = false;
          return Promise.reject(new Error("STRM 筛选请求失败"));
        }
        if (params.mode === "strm" && (params.offset ?? 0) > 0) {
          return Promise.resolve({
            summary,
            view: "files",
            filtered_total: 80,
            filtered_file_total: 80,
            offset: params.offset,
            limit: 50,
            has_more: false,
            items: [makeFile("STRM_PAGE2_CORRUPTION", "strm")],
            groups: [],
          });
        }
        if (holdMovieHead && params.kind === "movie") {
          return new Promise((resolve) => {
            heldMovieResolvers.push({ params, resolve });
          });
        }
        if (params.view === "groups") {
          return Promise.resolve({
            summary,
            view: "groups",
            filtered_total: 1,
            filtered_file_total: 3,
            offset: 0,
            limit: 50,
            has_more: false,
            items: [],
            groups: [
              {
                media_id: "grp-epoch-1",
                media_title: "纪元分组作品",
                media_kind: "tv",
                library_id: "lib-1",
                file_count: 3,
                strm_count: 0,
                hardlink_count: 3,
                local_size_bytes: 3_000_000,
                total_duration_ms: 180_000,
                primary_resolution: "1080p",
                primary_mode: "hardlink",
                sample_dir: "/media/tv/grp-epoch-1",
              },
            ],
          });
        }
        return Promise.resolve({
          summary,
          view: "files",
          filtered_total: 120,
          filtered_file_total: 120,
          offset: params.offset ?? 0,
          limit: 50,
          has_more: (params.offset ?? 0) === 0,
          items: Array.from({ length: 50 }, (_, i) =>
            makeFile(`ALL_OLD_${(params.offset ?? 0) + i}`, "hardlink"),
          ),
          groups: [],
        });
      },
      deleteLedgerRow: async () => ({ deleted: true, path: "", file_deleted: false }),
      retransferLedgerRow: async () => ({ retransferred: true, source: "", destination: "" }),
      refreshItemMetadata: async () => ({ ok: true }),
    },
  });

  const { MediaTransferView } = load("components/media-transfer-view.tsx");

  let renderer;
  await act(async () => {
    renderer = create(React.createElement(MediaTransferView));
  });

  try {
    // Initial head load in "files" (mode="all") succeeded
    assert.match(collectText(renderer.toJSON()), /ALL_OLD_0/);

    // 1. Switch modeFilter to "strm" while head fetch fails -> must clear stale ALL_OLD_* items AND never load offset>0 for "strm"!
    const strmBtn = renderer.root
      .findAllByType("button")
      .find((btn) => collectText(btn).trim() === "STRM 直链");
    await act(async () => {
      strmBtn.props.onClick();
    });

    const afterFailedFilterText = collectText(renderer.toJSON());
    assert.doesNotMatch(
      afterFailedFilterText,
      /ALL_OLD_0/,
      "Failed head fetch after changing filter must clear stale items from previous filter",
    );
    assert.doesNotMatch(
      afterFailedFilterText,
      /STRM_PAGE2_CORRUPTION/,
      "loadMoreInternal must never append page 2 of new filter onto page 1 of old filter",
    );
    assert.equal(
      apiCalls.some((c) => c.mode === "strm" && (c.offset ?? 0) > 0),
      false,
      "No offset > 0 request should fire when activePagination.filterKey !== currentFilterKey",
    );

    // Switch back to "all" so files load cleanly again
    const allModeBtn = renderer.root
      .findAllByType("button")
      .find((btn) => collectText(btn).trim() === "全部流转");
    await act(async () => {
      allModeBtn.props.onClick();
    });
    assert.match(collectText(renderer.toJSON()), /ALL_OLD_0/);

    // 2. Rapid filter ("movie") -> switch to "groups" -> switch back to "files" while "movie" fetches are still pending
    holdMovieHead = true;
    const movieBtn = renderer.root
      .findAllByType("button")
      .find((btn) => collectText(btn).trim() === "电影");
    await act(async () => {
      movieBtn.props.onClick();
    });

    const groupToggle = renderer.root
      .findAllByType("button")
      .find((btn) => collectText(btn).includes("按作品聚合"));
    await act(async () => {
      groupToggle.props.onClick();
    });

    const filesToggle = renderer.root
      .findAllByType("button")
      .find((btn) => collectText(btn).includes("逐文件列表"));
    await act(async () => {
      filesToggle.props.onClick();
    });

    const rapidSwitchBackText = collectText(renderer.toJSON());
    assert.doesNotMatch(
      rapidSwitchBackText,
      /ALL_OLD_0/,
      "Rapid filter -> mode switch -> switch back must clear stale unfiltered items",
    );
    assert.ok(
      renderer.root.findAll((n) => n.props?.role === "status").length > 0,
      "Rapid filter -> mode switch -> switch back must show loading skeleton",
    );

    // Reset filters and switch to "groups" to test useGroupExpansion reset epoch against non-aborting promises
    holdMovieHead = false;
    const resetBtn = renderer.root
      .findAllByType("button")
      .find((btn) => collectText(btn).includes("重置所有筛选"));
    await act(async () => {
      resetBtn.props.onClick();
    });
    await act(async () => {
      groupToggle.props.onClick();
    });
    assert.match(collectText(renderer.toJSON()), /纪元分组作品/);

    // 3a. Start toggleGroupExpand -> click manual "刷新记录" (calls resetExpanded) -> resolve stale expand promise (ignoring signal)
    const expandBtn = () =>
      renderer.root
        .findAllByType("button")
        .find((btn) => collectText(btn).includes("展开文件"));
    await act(async () => {
      expandBtn().props.onClick();
    });
    assert.equal(pendingGroupPromises.length, 1);
    const staleExpandPromise = pendingGroupPromises.shift();

    const refreshBtn = renderer.root
      .findAllByType("button")
      .find((btn) => collectText(btn).includes("刷新记录"));
    await act(async () => {
      await refreshBtn.props.onClick();
    });

    await act(async () => {
      staleExpandPromise.resolve({
        summary,
        view: "files",
        filtered_total: 1,
        filtered_file_total: 1,
        offset: 0,
        limit: 200,
        has_more: false,
        items: [makeFile("STALE_AFTER_RESET_EXPAND")],
        groups: [],
      });
    });
    assert.doesNotMatch(
      collectText(renderer.toJSON()),
      /STALE_AFTER_RESET_EXPAND/,
      "useGroupExpansion generation ref must ignore toggleGroupExpand resolving after resetExpanded()",
    );

    // 3b. Expand cleanly with has_more=true, start loadMoreGroupFiles -> click "刷新记录" -> resolve stale loadMoreGroupFiles
    await act(async () => {
      expandBtn().props.onClick();
    });
    await act(async () => {
      pendingGroupPromises.shift().resolve({
        summary,
        view: "files",
        filtered_total: 2,
        filtered_file_total: 2,
        offset: 0,
        limit: 200,
        has_more: true,
        items: [makeFile("FRESH_EP_1")],
        groups: [],
      });
    });
    assert.match(collectText(renderer.toJSON()), /FRESH_EP_1/);

    const loadMoreGroupBtn = renderer.root
      .findAllByType("button")
      .find((btn) => collectText(btn).includes("加载剩余剧集文件"));
    await act(async () => {
      loadMoreGroupBtn.props.onClick();
    });
    assert.equal(pendingGroupPromises.length, 1);
    const staleLoadMorePromise = pendingGroupPromises.shift();

    await act(async () => {
      await refreshBtn.props.onClick();
    });
    await act(async () => {
      staleLoadMorePromise.resolve({
        summary,
        view: "files",
        filtered_total: 2,
        filtered_file_total: 2,
        offset: 1,
        limit: 200,
        has_more: false,
        items: [makeFile("STALE_AFTER_RESET_LOADMORE")],
        groups: [],
      });
    });
    assert.doesNotMatch(
      collectText(renderer.toJSON()),
      /STALE_AFTER_RESET_LOADMORE/,
      "useGroupExpansion generation ref must ignore loadMoreGroupFiles resolving after resetExpanded()",
    );
  } finally {
    await act(async () => renderer.unmount());
  }
});

test("handleItemDeleted decrements copy_count/move_count and recomputes group primary_mode in strm > hardlink > move > copy order", async () => {
  const iconStub = () => React.createElement("span");

  const summary = {
    total_files: 4,
    media_count: 1,
    movie_files: 0,
    tv_files: 4,
    strm_count: 1,
    hardlink_count: 1,
    copy_count: 1,
    move_count: 1,
    local_size_bytes: 3_000_000,
    probed_duration_ms: 240_000,
    strm_probed_count: 1,
  };

  const groupFiles = [
    {
      id: "ep-strm",
      media_id: "m-mixed",
      library_id: "lib-1",
      media_title: "多模式合集",
      media_kind: "tv",
      path: "/tv/S01E01.strm",
      source_path: null,
      units: [{ season: 1, episode: 1 }],
      resolution: "1080p",
      codec: "hevc",
      hdr: null,
      quality_source: "probe",
      confidence: "high",
      filter_score: null,
      is_strm: true,
      transfer_mode: "strm",
      file_size: null,
      duration_ms: 60_000,
    },
    {
      id: "ep-hl",
      media_id: "m-mixed",
      library_id: "lib-1",
      media_title: "多模式合集",
      media_kind: "tv",
      path: "/tv/S01E02.mkv",
      source_path: "/dl/S01E02.mkv",
      units: [{ season: 1, episode: 2 }],
      resolution: "1080p",
      codec: "hevc",
      hdr: null,
      quality_source: "probe",
      confidence: "high",
      filter_score: null,
      is_strm: false,
      transfer_mode: "hardlink",
      file_size: 1_000_000,
      duration_ms: 60_000,
    },
    {
      id: "ep-mv",
      media_id: "m-mixed",
      library_id: "lib-1",
      media_title: "多模式合集",
      media_kind: "tv",
      path: "/tv/S01E03.mkv",
      source_path: null,
      units: [{ season: 1, episode: 3 }],
      resolution: "1080p",
      codec: "hevc",
      hdr: null,
      quality_source: "probe",
      confidence: "high",
      filter_score: null,
      is_strm: false,
      transfer_mode: "move",
      file_size: 1_000_000,
      duration_ms: 60_000,
    },
    {
      id: "ep-cp",
      media_id: "m-mixed",
      library_id: "lib-1",
      media_title: "多模式合集",
      media_kind: "tv",
      path: "/tv/S01E04.mkv",
      source_path: "/dl/S01E04.mkv",
      units: [{ season: 1, episode: 4 }],
      resolution: "1080p",
      codec: "hevc",
      hdr: null,
      quality_source: "probe",
      confidence: "high",
      filter_score: null,
      is_strm: false,
      transfer_mode: "copy",
      file_size: 1_000_000,
      duration_ms: 60_000,
    },
  ];

  const load = createModuleLoader({
    "@/components/icons": new Proxy({}, { get: () => iconStub }),
    "@/components/copy-button": {
      CopyButton: () => React.createElement("button", { type: "button" }, "copy"),
    },
    "@/components/feedback": {
      useToast: () => toastStub,
    },
    "@/components/modal": {
      Modal: ({ open, children }) => (open ? React.createElement("div", null, children) : null),
    },
    "@/lib/api/transfers": {
      listTransfers: async (params) => {
        if (params.media_id === "m-mixed") {
          return {
            summary,
            view: "files",
            filtered_total: groupFiles.length,
            filtered_file_total: groupFiles.length,
            offset: 0,
            limit: 200,
            has_more: false,
            items: groupFiles,
            groups: [],
          };
        }
        if (params.limit === 1) {
          return {
            summary,
            view: params.view ?? "groups",
            filtered_total: 1,
            filtered_file_total: 3,
            offset: 0,
            limit: 1,
            has_more: false,
            items: [],
            groups: [],
          };
        }
        if (params.view === "groups") {
          return {
            summary,
            view: "groups",
            filtered_total: 1,
            filtered_file_total: 4,
            offset: 0,
            limit: 50,
            has_more: false,
            items: [],
            groups: [
              {
                media_id: "m-mixed",
                media_title: "多模式合集",
                media_kind: "tv",
                library_id: "lib-1",
                file_count: 4,
                strm_count: 1,
                hardlink_count: 1,
                move_count: 1,
                copy_count: 1,
                local_size_bytes: 3_000_000,
                total_duration_ms: 240_000,
                primary_resolution: "1080p",
                primary_mode: "strm",
                sample_dir: "/tv",
              },
            ],
          };
        }
        return {
          summary,
          view: "files",
          filtered_total: 4,
          filtered_file_total: 4,
          offset: 0,
          limit: 50,
          has_more: false,
          items: groupFiles,
          groups: [],
        };
      },
      deleteLedgerRow: async () => ({ deleted: true, path: "", file_deleted: false }),
      retransferLedgerRow: async () => ({ retransferred: true, source: "", destination: "" }),
      refreshItemMetadata: async () => ({ ok: true }),
    },
  });

  const { MediaTransferView } = load("components/media-transfer-view.tsx");

  let renderer;
  await act(async () => {
    renderer = create(React.createElement(MediaTransferView));
  });

  try {
    const groupToggle = renderer.root
      .findAllByType("button")
      .find((btn) => collectText(btn).includes("按作品聚合"));
    await act(async () => {
      groupToggle.props.onClick();
    });

    const expandBtn = renderer.root
      .findAllByType("button")
      .find((btn) => collectText(btn).includes("展开文件"));
    await act(async () => {
      expandBtn.props.onClick();
    });

    const findModeBadgeText = () => {
      const spans = renderer.root.findAllByType("span");
      const badge = spans.find((s) =>
        /^(混合 \(1 STRM\)|STRM|硬链接|移动|复制)$/.test(collectText(s).trim()),
      );
      return badge ? collectText(badge).trim() : "";
    };

    assert.equal(findModeBadgeText(), "混合 (1 STRM)");

    const deleteFirstRemainingRow = async () => {
      const delBtn = renderer.root
        .findAllByType("button")
        .find((btn) => btn.props.title === "删除此文件记录");
      await act(async () => {
        delBtn.props.onClick();
      });
      const confirmBtn = renderer.root
        .findAllByType("button")
        .find((btn) => collectText(btn).includes("确认删除"));
      await act(async () => {
        confirmBtn.props.onClick();
      });
    };

    // Delete ep-strm -> tie [hardlink:1, move:1, copy:1] -> "硬链接"
    await deleteFirstRemainingRow();
    assert.equal(findModeBadgeText(), "硬链接");

    // Delete ep-hl -> tie [move:1, copy:1] -> "移动"
    await deleteFirstRemainingRow();
    assert.equal(findModeBadgeText(), "移动");

    // Delete ep-mv -> remaining [copy:1] -> "复制"
    await deleteFirstRemainingRow();
    assert.equal(findModeBadgeText(), "复制");
  } finally {
    await act(async () => renderer.unmount());
  }
});
