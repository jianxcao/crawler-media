import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import React, { act } from "react";
import { create } from "react-test-renderer";
import ts from "typescript";

import { PREVIOUS_COLOR } from "../lib/watch-stats-helpers.ts";
import { wantedPresentation } from "../lib/subscription-inspector.ts";

const require = createRequire(import.meta.url);
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const webRoot = fileURLToPath(new URL("../", import.meta.url));

function transpileFile(relPath, mocks = {}) {
  const fullPath = path.join(webRoot, relPath);
  const baseDir = path.dirname(fullPath);
  const cache = new Map();

  const resolveSpec = (fromDir, spec) => {
    if (spec in mocks) return mocks[spec];
    if (spec === "@/lib/env") return { publicEnv: { apiBaseUrl: "/api/v1" } };
    if (spec === "nanoid") return { nanoid: () => "fixed123" };
    if (cache.has(`${fromDir}::${spec}`)) return cache.get(`${fromDir}::${spec}`);

    let targetPath = null;
    if (spec.startsWith("@/")) {
      const sub = spec.slice(2);
      for (const ext of ["", ".ts", ".tsx", "/index.ts", "/index.tsx"]) {
        const candidate = path.join(webRoot, `${sub}${ext}`);
        try {
          readFileSync(candidate);
          targetPath = candidate;
          break;
        } catch {
          // try next
        }
      }
    } else if (spec.startsWith("./") || spec.startsWith("../")) {
      for (const ext of ["", ".ts", ".tsx", "/index.ts", "/index.tsx"]) {
        const candidate = path.resolve(fromDir, `${spec}${ext}`);
        try {
          readFileSync(candidate);
          targetPath = candidate;
          break;
        } catch {
          // try next
        }
      }
    }

    if (targetPath) {
      const source = readFileSync(targetPath, "utf8").replaceAll("import.meta.env", "({})");
      const js = ts.transpileModule(source, {
        fileName: targetPath,
        compilerOptions: {
          allowJs: true,
          module: ts.ModuleKind.CommonJS,
          jsx: ts.JsxEmit.ReactJSX,
          target: ts.ScriptTarget.ES2022,
        },
      }).outputText;
      const mod = { exports: {} };
      cache.set(`${fromDir}::${spec}`, mod.exports);
      const nextDir = path.dirname(targetPath);
      new Function("require", "module", "exports", js)(
        (childSpec) => resolveSpec(nextDir, childSpec),
        mod,
        mod.exports,
      );
      return mod.exports;
    }

    return require(spec);
  };

  const source = readFileSync(fullPath, "utf8");
  const js = ts.transpileModule(source, {
    fileName: fullPath,
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      jsx: ts.JsxEmit.ReactJSX,
      target: ts.ScriptTarget.ES2022,
    },
  }).outputText;
  const mod = { exports: {} };
  new Function("require", "module", "exports", js)(
    (spec) => resolveSpec(baseDir, spec),
    mod,
    mod.exports,
  );
  return mod.exports;
}

function textOf(node) {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  return (node.children ?? []).map(textOf).join("");
}

const IconStub = new Proxy(
  {},
  {
    get: () => (props) => React.createElement("span", props),
  },
);

const ModalStub = ({ open, children, label }) =>
  open ? React.createElement("div", { role: "dialog", "aria-label": label }, children) : null;

const DropdownStub = {
  Root: ({ children }) => React.createElement("div", null, children),
  Trigger: ({ children }) => React.createElement("div", null, children),
  Portal: ({ children }) => React.createElement("div", null, children),
  Content: ({ children, className }) => React.createElement("div", { className }, children),
  Separator: ({ className }) => React.createElement("hr", { className }),
  Item: ({ children, onSelect, className, disabled }) =>
    React.createElement(
      "button",
      { type: "button", className, disabled, onClick: () => onSelect?.() },
      children,
    ),
};

test("Item 3: DownloadTargetDialog labels PathMap.from as downloader save path and PathMap.to as local directory, and DownloadButton guards empty downloader_name", async () => {
  const submittedPayloads = [];
  const { DownloadTargetDialog } = transpileFile("components/download-target-dialog.tsx", {
    "react-router-dom": {
      Link: ({ to, children, className }) =>
        React.createElement("a", { href: to, className }, children),
    },
    "@/components/icons": IconStub,
    "@/components/modal": { Modal: ModalStub },
    "@/lib/time": { formatRelativeTime: () => "刚刚" },
    "@/lib/api/downloaders": {
      listDownloaders: async () => [
        {
          id: "dl-1",
          name: "qBittorrent",
          kind: "qbittorrent",
          url: "http://qb:8080",
          path_maps: [{ from: "/downloads/tv", to: "/mnt/nas/media/downloads/tv" }],
          is_default: true,
          status: "active",
        },
      ],
      resolveManualDownloadTarget: async () => ({ status: "not_found", ok: false, candidates: [] }),
      submitTorrentDownload: async (payload) => {
        submittedPayloads.push(payload);
        return {
          info_hash: null,
          name: "Demo",
          already_exists: false,
          downloader_id: "",
          downloader_name: "",
          save_path: payload.save_path ?? null,
        };
      },
    },
  });

  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(DownloadTargetDialog, {
        request: {
          site_id: "mteam",
          download_url: "https://example.com/1.torrent",
          torrent_id: "t-1",
          identity: null,
          subtitle: null,
          category: "tv",
        },
        onClose() {},
        onSubmitted() {},
      }),
    );
  });

  const fullText = textOf(renderer.root);
  assert.match(
    fullText,
    /下载器保存路径：\/downloads\/tv/,
    "PathMap.from must be clearly labeled as the downloader save path",
  );
  assert.match(
    fullText,
    /本机对应目录：\/mnt\/nas\/media\/downloads\/tv（路径映射）/,
    "PathMap.to must be labeled as 本机对应目录：...（路径映射）",
  );
  assert.doesNotMatch(fullText, /下载器视角：\/mnt\/nas/, "must not invert host path as 下载器视角");
  assert.doesNotMatch(
    fullText,
    /自动翻译成下载器视角/,
    "must not claim manual dir submission translates host path to downloader path",
  );

  // Confirm submitting the mapped dir option sends m.from ("/downloads/tv")
  const confirmBtn = renderer.root
    .findAllByType("button")
    .find((b) => textOf(b).includes("确认下载"));
  await act(async () => {
    confirmBtn.props.onClick();
  });
  assert.equal(submittedPayloads.length, 1);
  assert.equal(submittedPayloads[0].save_path, "/downloads/tv");
  await act(async () => renderer.unmount());

  // Now test DownloadButton in torrent-actions.tsx with empty vs non-empty downloader_name
  const successToasts = [];
  let submitResult = {
    info_hash: null,
    name: "Demo",
    already_exists: false,
    downloader_id: "",
    downloader_name: "",
    save_path: "/downloads/tv",
  };
  const PrefCtx = React.createContext({
    byCategory: new Map([
      [
        "tv",
        {
          category: "tv",
          kind: "dir",
          save_path: "/downloads/tv",
          downloader_id: null,
          downloader_name: null,
          updated_at: "2026-01-01T00:00:00Z",
        },
      ],
    ]),
    isStaleDir: () => false,
    forget: async () => null,
    refresh: async () => {},
  });
  const GrabCtx = React.createContext(null);

  const { DownloadButton } = transpileFile("components/search/torrent-actions.tsx", {
    "@/components/download-target-dialog": {
      DownloadTargetConfirmBar: ({ onConfirm }) =>
        React.createElement("button", { type: "button", onClick: onConfirm }, "确认条提交"),
      DownloadTargetDialog: () => null,
      submitRememberedTarget: async () => submitResult,
    },
    "@/components/feedback": {
      useToast: () => ({
        success: (msg) => successToasts.push(msg),
        error() {},
        info() {},
      }),
    },
    "@/components/icons": IconStub,
    "@/components/modal": { Modal: ModalStub },
    "@/components/search/torrent-contexts": {
      DownloadTargetPrefContext: PrefCtx,
      GrabContext: GrabCtx,
    },
    "@/lib/api/subscriptions": { downloadSelectedTorrentForSubscription: async () => ({ units: [] }) },
    "@/lib/time": { formatRelativeTime: () => "刚刚" },
    "@/lib/permissions": { usePermissions: () => ({ canDirectDownload: true }) },
  });

  const hit = {
    site_id: "mteam",
    site_name: "M-Team",
    title: "Demo.S01E01.1080p",
    enclosure: "https://example.com/1.torrent",
    category: "tv",
    size_bytes: 1024,
    seeders: 10,
    leechers: 1,
    free: true,
    hr: false,
    upload_time: null,
    release: { title: "Demo", year: 2026, season: 1, episode: 1 },
  };

  let btnRenderer;
  await act(async () => {
    btnRenderer = create(React.createElement(DownloadButton, { hit, className: "btn" }));
  });
  await act(async () => {
    btnRenderer.root
      .findAllByType("button")
      .find((b) => textOf(b) === "下载")
      .props.onClick({ stopPropagation() {} });
  });
  await act(async () => {
    btnRenderer.root
      .findAllByType("button")
      .find((b) => textOf(b) === "确认条提交")
      .props.onClick();
  });
  assert.equal(successToasts[0], "已提交到下载器 · /downloads/tv");
  assert.doesNotMatch(successToasts[0], /「」/);
  await act(async () => btnRenderer.unmount());

  // Named downloader still includes 「name」
  submitResult = { ...submitResult, downloader_name: "qB-Main" };
  let btnRenderer2;
  await act(async () => {
    btnRenderer2 = create(React.createElement(DownloadButton, { hit, className: "btn" }));
  });
  await act(async () => {
    btnRenderer2.root
      .findAllByType("button")
      .find((b) => textOf(b) === "下载")
      .props.onClick({ stopPropagation() {} });
  });
  await act(async () => {
    btnRenderer2.root
      .findAllByType("button")
      .find((b) => textOf(b) === "确认条提交")
      .props.onClick();
  });
  assert.equal(successToasts[1], "已提交到「qB-Main」 · /downloads/tv");
  await act(async () => btnRenderer2.unmount());
});

test("Item 4: LibraryCustomizeView and NetflixLibraryHero exclude exclude_from_home libraries and fall back to libraryItem.backdrop_url", async () => {
  const libs = [
    { id: "lib-movies", name: "电影", kind: "movie", exclude_from_home: false, is_default: true },
    { id: "lib-private", name: "私密归档", kind: "video", exclude_from_home: true, is_default: false },
  ];

  const { LibraryCustomizeView } = transpileFile("components/library-customize-view.tsx", {
    "@/components/icons": IconStub,
    "@/components/library-filter-bar": { WallSortControl: () => null },
    "@/components/page-nav": { PageNav: () => null },
    "@/lib/api/libraries": { listLibraries: async () => libs },
    "@/lib/ui-prefs": {
      useUiPrefs: () => ({
        prefs: { home: { rows: [] } },
        savePrefs: async () => {},
      }),
    },
  });

  let custRenderer;
  await act(async () => {
    custRenderer = create(React.createElement(LibraryCustomizeView));
  });
  const custText = textOf(custRenderer.root);
  assert.match(custText, /电影/, "non-excluded library must appear in customize view");
  assert.doesNotMatch(
    custText,
    /私密归档/,
    "exclude_from_home library must be excluded from default rows AND add-row picker",
  );
  await act(async () => custRenderer.unmount());

  // NetflixLibraryHero
  const queriedLibIds = [];
  const { NetflixLibraryHero } = transpileFile("components/netflix/library-hero.tsx", {
    "react-router-dom": { useNavigate: () => () => {} },
    "@/components/icons": IconStub,
    "@/components/artwork-image": {
      ArtworkImage: ({ src }) => React.createElement("img", { "data-testid": "hero-art", src }),
    },
    "@/components/poster-image": {
      PosterImage: ({ src }) => React.createElement("img", { "data-testid": "poster-art", src }),
    },
    "@/lib/artwork-loading": {
      createArtworkImage: () => {
        const image = { onload: null, onerror: null, decode: async () => {} };
        return {
          image,
          load: () => {
            queueMicrotask(() => image.onload?.());
          },
        };
      },
    },
    "@/lib/api/playback": { listUpNext: async () => [] },
    "@/lib/api/libraries": {
      listLibraries: async () => libs,
      listLibraryItems: async (libId) => {
        queriedLibIds.push(libId);
        return {
          items: [
            {
              media_item_id: "item-1",
              library_id: libId,
              kind: "movie",
              title: "星际穿越",
              year: 2014,
              poster_url: "/posters/item-1",
              backdrop_url: "/fanart/item-1",
              file_count: 1,
              seasons: [],
              episode_count: 0,
              missing: false,
            },
          ],
          next_cursor: null,
          total: 1,
        };
      },
    },
    "@/lib/image-proxy": {
      imageUrl: (u) => u,
      upgradedTmdbOriginalUrl: (u) => u,
      cardVariantFor: () => "poster-card",
    },
  });

  let heroRenderer;
  await act(async () => {
    heroRenderer = create(React.createElement(NetflixLibraryHero));
    await Promise.resolve();
    await Promise.resolve();
  });
  assert.deepEqual(
    queriedLibIds,
    ["lib-movies"],
    "NetflixLibraryHero must not fetch items from exclude_from_home libraries",
  );
  const heroArt = heroRenderer.root.findAll(
    (n) => n.type === "img" && n.props["data-testid"] === "hero-art",
  );
  assert.equal(heroArt.length, 1, "NetflixBillboard must render libraryItem.backdrop_url artwork");
  assert.equal(heroArt[0].props.src, "/fanart/item-1");
  await act(async () => heroRenderer.unmount());
});

test("Items 5 & 6: cross-theme contrast surfaces, CSS variables, segmented-glass, and unlayered .glass-row !text-[var(--danger)]", async () => {
  // 5a. PREVIOUS_COLOR uses var(--text-faint)
  assert.equal(PREVIOUS_COLOR, "var(--text-faint)");

  // 5b. subscription-inspector wantedPresentation & SubscriptionHeroCard avoid pastel hex literals
  const now = new Date("2026-05-01T12:00:00Z");
  const upgradingPres = wantedPresentation(
    {
      id: "w1",
      season_number: 1,
      episode_number: 1,
      title: null,
      air_date: "2025-01-01",
      status: "imported",
      last_search_at: null,
      next_search_at: null,
      search_attempts: 0,
      last_reject_reason: null,
      grab_title: null,
      grabbed_at: null,
      downloaded_at: null,
      imported_at: "2026-04-01T00:00:00Z",
      info_hash: null,
      resource_timing: null,
      release_forecast: null,
      upgrade: { active: true, current_label: "1080p", target_label: "4K" },
    },
    now,
  );
  const pendingSearchPres = wantedPresentation(
    {
      id: "w2",
      season_number: 1,
      episode_number: 2,
      title: null,
      air_date: "2025-01-01",
      status: "wanted",
      last_search_at: null,
      next_search_at: "2026-05-01T15:00:00Z",
      search_attempts: 0,
      last_reject_reason: null,
      grab_title: null,
      grabbed_at: null,
      downloaded_at: null,
      imported_at: null,
      info_hash: null,
      resource_timing: null,
      release_forecast: null,
      upgrade: null,
    },
    now,
  );
  assert.match(upgradingPres.color, /^var\(--/);
  assert.match(pendingSearchPres.color, /^var\(--/);

  const heroCardSrc = readFileSync(
    path.join(webRoot, "components/subscription-inspector/subscription-hero-card.tsx"),
    "utf8",
  );
  assert.doesNotMatch(heroCardSrc, /#2dd4bf|#f5c451/i);

  // 5c. TrailerRow blocked player box includes dark-media-surface
  const prevWindow = globalThis.window;
  const prevImage = globalThis.Image;
  globalThis.window = {
    setTimeout: () => 1,
    clearTimeout: () => {},
  };
  globalThis.Image = class {
    set src(_v) {
      if (this.onerror) this.onerror();
    }
  };
  try {
    const { TrailerRow } = transpileFile("components/media-detail/trailer-row.tsx", {
      "@/components/h-scroller": { HScroller: ({ children }) => React.createElement("div", null, children) },
      "@/components/icons": IconStub,
      "@/components/modal": { Modal: ModalStub },
      "@/components/poster-image": { PosterImage: () => null },
      "@/lib/use-tap-guard": { useTapGuard: (fn) => ({ onClick: fn }) },
    });

    let trRenderer;
    await act(async () => {
      trRenderer = create(
        React.createElement(TrailerRow, {
          title: "沙丘",
          videos: [
            {
              key: "yt1",
              name: "正式预告",
              kind: "预告片",
              thumbnailUrl: "/thumb.jpg",
              embedUrl: "https://www.youtube.com/embed/yt1",
              watchUrl: "https://www.youtube.com/watch?v=yt1",
            },
          ],
        }),
      );
    });
    await act(async () => {
      trRenderer.root.findByType("button").props.onClick();
    });
    const playerBox = trRenderer.root
      .findAllByType("div")
      .find((d) => String(d.props.className ?? "").includes("aspect-video w-full bg-black"));
    assert.ok(playerBox, "TrailerPlayer must render aspect-video w-full bg-black container");
    assert.match(playerBox.props.className, /\bdark-media-surface\b/);
    await act(async () => trRenderer.unmount());
  } finally {
    if (prevWindow === undefined) delete globalThis.window;
    else globalThis.window = prevWindow;
    if (prevImage === undefined) delete globalThis.Image;
    else globalThis.Image = prevImage;
  }

  // 5d. TorrentPosterGrid & CatalogCacheView opaque bg-[#141824] on .dark-media-surface poster boxes
  const posterGridSrc = readFileSync(
    path.join(webRoot, "components/search/torrent-poster-grid.tsx"),
    "utf8",
  );
  assert.match(posterGridSrc, /dark-media-surface[^\n]*bg-\[#141824\]/);

  const catalogSrc = readFileSync(path.join(webRoot, "components/catalog-cache-view.tsx"), "utf8");
  assert.match(catalogSrc, /dark-media-surface[^\n]*bg-\[#141824\]/);
  assert.doesNotMatch(catalogSrc, /--poster-placeholder/);

  // 5e. FilterToolbar uses bg-[var(--surface-inset)] (or css-glass) and segmented-glass / segmented-glass__item
  const { FilterToolbar } = transpileFile("components/search/torrent-filter-toolbar.tsx", {
    "@/components/icons": IconStub,
  });
  let ftRenderer;
  await act(async () => {
    ftRenderer = create(
      React.createElement(FilterToolbar, {
        sites: [],
        facets: {
          sites: new Map(),
          resolution: [],
          source: [],
          codec: [],
          hdr: [],
          years: [],
          seasons: [],
          episodes: [],
          groups: [],
        },
        filters: {
          site: new Set(),
          resolution: new Set(),
          source: new Set(),
          codec: new Set(),
          hdr: new Set(),
          year: new Set(),
          season: new Set(),
          episode: new Set(),
          group: new Set(),
        },
        onChange() {},
        sort: { key: "seeders", dir: "desc" },
        smartSortKeys: [],
        onSortChange() {},
        resultCount: 5,
        view: "group",
        onViewChange() {},
      }),
    );
  });
  const ftDivs = ftRenderer.root.findAllByType("div").map((d) => String(d.props.className ?? ""));
  assert.equal(
    ftDivs.some((cls) => cls.includes("bg-[var(--surface-inset)]") || cls.includes("css-glass")),
    true,
    "FilterToolbar root must use bg-[var(--surface-inset)] or css-glass",
  );
  assert.equal(
    ftDivs.some((cls) => cls.includes("bg-black/[0.14]") || cls.includes("bg-black/[0.16]")),
    false,
    "FilterToolbar must not hardcode dark-only bg-black/[0.14] or bg-black/[0.16]",
  );
  assert.equal(
    ftDivs.some((cls) => cls.includes("segmented-glass")),
    true,
    "FilterToolbar view switcher must use segmented-glass",
  );
  const groupViewBtn = ftRenderer.root
    .findAllByType("button")
    .find((b) => b.props["aria-label"] === "分组");
  assert.match(groupViewBtn.props.className, /\bsegmented-glass__item\b/);
  await act(async () => ftRenderer.unmount());

  // 6. Unlayered .glass-row destructive items use !text-[var(--danger)] & data-[highlighted]:!bg-[var(--danger)]/10 & data-[highlighted]:!text-[var(--danger)]
  const { LibraryActionsMenu } = transpileFile("components/library-detail/library-actions-menu.tsx", {
    "@radix-ui/react-dropdown-menu": DropdownStub,
    "@/components/icons": IconStub,
    "@/components/page-nav": { PAGE_NAV_BUTTON_CLASS: "page-nav-btn" },
    "@/components/video-gallery": { WallPrefItems: () => null },
    "@/lib/api/libraries": { setDefaultLibrary: async () => {} },
  });
  let lamRenderer;
  await act(async () => {
    lamRenderer = create(
      React.createElement(LibraryActionsMenu, {
        library: { id: "lib-1", name: "电影", kind: "movie", is_default: true },
        canManage: true,
        toast: { success() {}, error() {} },
        reload() {},
        onToggleScan() {},
        pendingCount: 0,
        onOpenPending() {},
        onToggleMetaRefresh() {},
        onMediaInfo() {},
        onEdit() {},
        onDelete() {},
      }),
    );
  });
  const deleteMenuBtn1 = lamRenderer.root
    .findAllByType("button")
    .find((b) => textOf(b).includes("删除媒体库"));
  for (const token of [
    "!text-[var(--danger)]",
    "data-[highlighted]:!bg-[var(--danger)]/10",
    "data-[highlighted]:!text-[var(--danger)]",
  ]) {
    assert.ok(
      deleteMenuBtn1.props.className.includes(token),
      `LibraryActionsMenu delete item must include ${token}`,
    );
  }
  await act(async () => lamRenderer.unmount());

  const { LibraryManageRow } = transpileFile("components/library-manage-row.tsx", {
    "react-router-dom": { Link: ({ to, children, className }) => React.createElement("a", { href: to, className }, children) },
    "@radix-ui/react-dropdown-menu": DropdownStub,
    "@/components/artwork-image": { ArtworkImage: () => null },
    "@/components/icons": IconStub,
    "@/components/library-kind-meta": {
      LIBRARY_KIND_META: { movie: { Icon: () => null } },
    },
    "@/components/library-cover-art": { LibraryKindCoverArt: () => null },
    "@/lib/env": { publicEnv: { apiBaseUrl: "/api/v1" } },
  });
  let lmrRenderer;
  await act(async () => {
    lmrRenderer = create(
      React.createElement(LibraryManageRow, {
        library: {
          id: "lib-1",
          name: "电影",
          kind: "movie",
          root_paths: ["/movies"],
          is_default: true,
          access_mode: "everyone",
          root_missing: false,
          admin_visible: true,
          member_ids: [],
          stats: { item_count: 10, file_count: 10, total_size_bytes: 1000 },
        },
        actions: {
          onToggleScan() {},
          onOpenPending() {},
          onToggleRefresh() {},
          onMediaInfo() {},
          onOrganize() {},
          onReconcile() {},
          onConsolidate() {},
          onEdit() {},
          onSetDefault() {},
          onDelete() {},
        },
        drag: null,
      }),
    );
  });
  const deleteMenuBtn2 = lmrRenderer.root
    .findAllByType("button")
    .find((b) => textOf(b).includes("删除媒体库"));
  for (const token of [
    "!text-[var(--danger)]",
    "data-[highlighted]:!bg-[var(--danger)]/10",
    "data-[highlighted]:!text-[var(--danger)]",
  ]) {
    assert.ok(
      deleteMenuBtn2.props.className.includes(token),
      `LibraryManageRow delete item must include ${token}`,
    );
  }
  await act(async () => lmrRenderer.unmount());

  const { UpNextRow } = transpileFile("components/up-next-row.tsx", {
    "react-router-dom": { Link: ({ to, children, className }) => React.createElement("a", { href: to, className }, children) },
    "@radix-ui/react-dropdown-menu": DropdownStub,
    "@/components/h-scroller": { HScroller: ({ children }) => React.createElement("div", null, children) },
    "@/components/icons": IconStub,
    "@/components/poster-image": { PosterImage: () => null },
    "@/components/watch-history-menu": { WatchHistoryMenu: () => null },
    "@/components/feedback": { useToast: () => ({ success() {}, error() {}, info() {} }) },
    "@/lib/api/playback": { clearPlaybackHistory: async () => {} },
    "@/lib/image-proxy": { cardVariantFor: () => "poster-card", imageUrl: (u) => u },
    "@/lib/time": { formatRelativeTime: () => "刚刚" },
    "@/lib/use-tap-guard": { useTapGuard: () => ({}) },
  });
  let unRenderer;
  await act(async () => {
    unRenderer = create(
      React.createElement(UpNextRow, {
        items: [
          {
            media_item_id: "m-1",
            library_id: "lib-1",
            kind: "movie",
            title: "盗梦空间",
            year: 2010,
            poster_url: null,
            poster_aspect: 2 / 3,
            backdrop_url: null,
            episode_still_url: null,
            season_number: 0,
            episode_number: 0,
            episode_title: null,
            position_ms: 1000,
            duration_ms: 5000,
            progress_percent: 20,
            last_played_at: "2026-01-01T00:00:00Z",
            advanced: false,
            unwatched_ahead_count: 0,
          },
        ],
        libraries: [],
        onCleared() {},
      }),
    );
  });
  const removeBtn = unRenderer.root
    .findAllByType("button")
    .find((b) => textOf(b).includes("从继续观看中移出"));
  for (const token of [
    "!text-[var(--danger)]",
    "data-[highlighted]:!bg-[var(--danger)]/10",
    "data-[highlighted]:!text-[var(--danger)]",
  ]) {
    assert.ok(
      removeBtn.props.className.includes(token),
      `UpNextRow remove item must include ${token}`,
    );
  }
  await act(async () => unRenderer.unmount());
});

test("Item 7: AccessScopeEditor stops Escape propagation when open, and SearchCommand handles window Escape & restores input focus on history removal", async () => {
  const prevDoc = globalThis.document;
  const prevWin = globalThis.window;
  const winListeners = new Map();
  globalThis.document = {
    addEventListener() {},
    removeEventListener() {},
    body: {
      classList: { toggle() {}, remove() {} },
    },
  };
  globalThis.window = {
    addEventListener(type, fn) {
      if (!winListeners.has(type)) winListeners.set(type, []);
      winListeners.get(type).push(fn);
    },
    removeEventListener(type, fn) {
      const arr = winListeners.get(type) ?? [];
      const idx = arr.indexOf(fn);
      if (idx >= 0) arr.splice(idx, 1);
    },
    localStorage: {
      getItem: () => null,
      setItem: () => {},
    },
  };

  try {
    // 7a. AccessScopeEditor
    const { AccessScopeEditor } = transpileFile("components/library-form/access-scope-editor.tsx", {
      "@/components/icons": IconStub,
      "@/lib/api/libraries": { listLibraryRoutingOptions: async () => ({}) },
      "@/lib/api/members": {
        listMembers: async () => [{ id: "u1", login: "alice" }],
      },
    });
    let aseRenderer;
    await act(async () => {
      aseRenderer = create(
        React.createElement(AccessScopeEditor, {
          mode: "selected",
          adminVisible: true,
          memberIds: [],
          onMode() {},
          onAdminVisible() {},
          onMemberIds() {},
        }),
      );
    });
    const comboInput = aseRenderer.root.find((n) => n.type === "input" && n.props.role === "combobox");
    await act(async () => {
      comboInput.props.onFocus();
    });
    assert.equal(comboInput.props["aria-expanded"], true);

    let prevented = false;
    let stopped = false;
    await act(async () => {
      comboInput.props.onKeyDown({
        key: "Escape",
        nativeEvent: { isComposing: false },
        preventDefault() {
          prevented = true;
        },
        stopPropagation() {
          stopped = true;
        },
      });
    });
    assert.equal(prevented, true, "Escape while combobox open must preventDefault");
    assert.equal(stopped, true, "Escape while combobox open must stopPropagation");
    assert.equal(comboInput.props["aria-expanded"], false);

    // Second Escape while already closed must NOT stopPropagation so parent Modal can close
    let stoppedWhenClosed = false;
    await act(async () => {
      comboInput.props.onKeyDown({
        key: "Escape",
        nativeEvent: { isComposing: false },
        preventDefault() {},
        stopPropagation() {
          stoppedWhenClosed = true;
        },
      });
    });
    assert.equal(stoppedWhenClosed, false, "Escape when combobox is closed must bubble to Modal");
    await act(async () => aseRenderer.unmount());

    // 7b. SearchCommand: window Escape listener & inputRef.current.focus() after removeOne / removeGroup / removeAll
    let focusCount = 0;
    const { SearchCommand } = transpileFile("components/search-command.tsx", {
      "react-dom": { createPortal: (node) => node },
      "@/components/feedback": { useConfirm: () => async () => true },
      "@/components/icons": IconStub,
      "@/components/search/search-command-rows": {
        CategoryChip: ({ label, onClick }) =>
          React.createElement("button", { type: "button", onClick }, label),
        Kbd: ({ children }) => React.createElement("kbd", null, children),
        ModeSwitch: () => null,
        HistoryGroupRow: ({ group, onRemoveItem, onRemoveGroup }) =>
          React.createElement(
            "li",
            null,
            React.createElement(
              "button",
              { type: "button", onClick: () => onRemoveItem(group.items[0].id) },
              `删单项:${group.keyword}`,
            ),
            React.createElement(
              "button",
              { type: "button", onClick: onRemoveGroup },
              `删整组:${group.keyword}`,
            ),
          ),
      },
      "@/lib/api/search": {
        listSearchHistory: async () => [
          { id: "h1", query: "沙丘", provider: "titles", category: null, searched_at: "2026-01-01T00:00:00Z" },
          { id: "h2", query: "奥本海默", provider: "titles", category: null, searched_at: "2026-01-01T00:00:00Z" },
          { id: "h3", query: "奥本海默", provider: "torrents", category: "movie", searched_at: "2026-01-01T00:00:00Z" },
          { id: "h4", query: "星际穿越", provider: "titles", category: null, searched_at: "2026-01-01T00:00:00Z" },
        ],
        deleteSearchHistoryEntry: async () => {},
        clearSearchHistory: async () => {},
      },
      "@/lib/search-access": {
        useSearchAccess: () => ({
          ready: true,
          available: ["media", "torrent", "library"],
          firstAvailable: "media",
          canMedia: true,
          canTorrent: true,
          canLibrary: true,
        }),
      },
      "@/lib/search-prefs": {
        useSearchPrefs: () => ({ visibleTabs: [], loading: false }),
      },
    });

    let scRenderer;
    await act(async () => {
      scRenderer = create(
        React.createElement(SearchCommand, { onSearch() {} }),
        {
          createNodeMock: (element) => {
            if (element.type === "input") {
              return {
                focus() {
                  focusCount += 1;
                },
              };
            }
            return {};
          },
        },
      );
    });

    // Open SearchPalette
    const triggerBtn = scRenderer.root.findByType("button");
    await act(async () => {
      triggerBtn.props.onClick();
    });
    assert.ok(
      scRenderer.root.findAll((n) => n.props?.role === "dialog").length === 1,
      "SearchPalette must open",
    );

    const focusBeforeRemovals = focusCount;

    // removeOne restores focus
    await act(async () => {
      scRenderer.root
        .findAllByType("button")
        .find((b) => textOf(b) === "删单项:沙丘")
        .props.onClick();
    });
    assert.equal(focusCount, focusBeforeRemovals + 1, "removeOne must restore input focus");

    // removeGroup restores focus
    await act(async () => {
      await scRenderer.root
        .findAllByType("button")
        .find((b) => textOf(b) === "删整组:奥本海默")
        .props.onClick();
    });
    assert.equal(focusCount, focusBeforeRemovals + 2, "removeGroup must restore input focus");

    // removeAll restores focus
    await act(async () => {
      scRenderer.root
        .findAllByType("button")
        .find((b) => textOf(b) === "清空")
        .props.onClick();
    });
    assert.equal(focusCount, focusBeforeRemovals + 3, "removeAll must restore input focus");

    // Window Escape listener closes SearchPalette even when focus is outside dialog
    await act(async () => {
      for (const fn of [...(winListeners.get("keydown") ?? [])]) {
        fn({ key: "Escape", defaultPrevented: false, preventDefault() {} });
      }
    });
    assert.equal(
      scRenderer.root.findAll((n) => n.props?.role === "dialog").length,
      0,
      "Window Escape must close SearchPalette",
    );

    await act(async () => scRenderer.unmount());
  } finally {
    if (prevDoc === undefined) delete globalThis.document;
    else globalThis.document = prevDoc;
    if (prevWin === undefined) delete globalThis.window;
    else globalThis.window = prevWin;
  }
});

test("Item 8: MaintenanceJobSettings preserves operations_days and uses toast.error; ScheduledJobsView keeps string draft for scheduleSecs and passes parsed payload_token as expectedPayload", async () => {
  const maintSrc = readFileSync(
    path.join(webRoot, "components/maintenance-job-settings.tsx"),
    "utf8",
  );
  assert.doesNotMatch(maintSrc, /\balert\s*\(/, "MaintenanceJobSettings must not call native alert()");

  const savedConfigs = [];
  const toastErrors = [];
  const { MaintenanceJobSettings } = transpileFile("components/maintenance-job-settings.tsx", {
    "@/components/feedback": {
      useToast: () => ({
        success() {},
        info() {},
        error: (msg) => toastErrors.push(msg),
      }),
    },
    "@/lib/api/jobs": {
      updateMaintenanceConfig: async (id, payload, expectedPayload, intervalSecs) => {
        if (id === "job-fail") throw new Error("CAS conflict");
        savedConfigs.push({ id, payload, expectedPayload, intervalSecs });
        return {};
      },
      previewMaintenance: async () => ({
        kind: "data_retention",
        scanned: 0,
        candidates: 0,
        protected: 0,
        truncated: false,
      }),
    },
  });

  // 8a. data_retention with custom operations_days: 45
  let mRenderer;
  await act(async () => {
    mRenderer = create(
      React.createElement(MaintenanceJobSettings, {
        job: {
          id: "job-dr",
          kind: "data_retention",
          name: "数据保留",
          enabled: false,
          schedule: { interval_secs: 86400 },
          payload: { version: 1 },
          payload_token: '{"version":1,"operations_days":45}',
          concurrency_key: null,
          last_status: null,
          last_finished_at: null,
          next_run_after: null,
          maintenance: {
            kind: "data_retention",
            version: 1,
            dry_run: true,
            qoe_days: 90,
            transient_cache_days: 90,
            probe_days: 90,
            operations_days: 45,
          },
        },
        onSaved() {},
      }),
    );
  });
  const saveMaintBtn = mRenderer.root
    .findAllByType("button")
    .find((b) => textOf(b).includes("保存维护配置"));
  assert.match(saveMaintBtn.props.className, /\bbg-sky-700\b/);
  assert.match(saveMaintBtn.props.className, /\bhover:bg-sky-600\b/);
  await act(async () => {
    await saveMaintBtn.props.onClick();
  });
  assert.equal(savedConfigs.length, 1);
  assert.equal(
    savedConfigs[0].payload.operations_days,
    45,
    "draftPayload must preserve summary.operations_days",
  );
  await act(async () => mRenderer.unmount());

  // Fallback operations_days: 30 + toast.error when onError is omitted and save fails
  let mFailRenderer;
  await act(async () => {
    mFailRenderer = create(
      React.createElement(MaintenanceJobSettings, {
        job: {
          id: "job-fail",
          kind: "data_retention",
          name: "数据保留",
          enabled: false,
          schedule: { interval_secs: 86400 },
          payload: { version: 1 },
          concurrency_key: null,
          last_status: null,
          last_finished_at: null,
          next_run_after: null,
          maintenance: {
            kind: "data_retention",
            version: 1,
            dry_run: true,
            qoe_days: 90,
            transient_cache_days: 90,
            probe_days: 90,
          },
        },
        onSaved() {},
      }),
    );
  });
  await act(async () => {
    await mFailRenderer.root
      .findAllByType("button")
      .find((b) => textOf(b).includes("保存维护配置"))
      .props.onClick();
  });
  assert.equal(toastErrors.length, 1, "MaintenanceJobSettings must report errors via toast.error when onError omitted");
  assert.match(toastErrors[0], /CAS conflict/);
  await act(async () => mFailRenderer.unmount());

  // 8b. ScheduledJobsView string draft for scheduleSecs & payload_token CAS
  const scheduleUpdates = [];
  const { ScheduledJobsView } = transpileFile("components/scheduled-jobs-view.tsx", {
    "@/components/feedback": {
      useToast: () => ({ success() {}, error() {}, info() {} }),
    },
    "@/components/maintenance-job-settings": { MaintenanceJobSettings: () => null },
    "@/lib/use-visible-polling": { useVisiblePolling: () => {} },
    "@/lib/api/jobs": {
      isMaintenanceJob: (j) => j.kind === "jobs_retention",
      parseMaintenanceReport: () => null,
      listJobs: async () => [
        {
          id: "job-maint",
          kind: "jobs_retention",
          name: "任务历史保留",
          friendly_name: "任务历史保留",
          enabled: true,
          schedule: { interval_secs: 86400 },
          payload: { version: 1, retention_days: 90, dry_run: false },
          payload_token: '{"version":1,"retention_days":90,"dry_run":false,"cas_seq":7}',
          concurrency_key: null,
          last_status: "succeeded",
          last_finished_at: null,
          next_run_after: null,
        },
      ],
      runJob: async () => ({ queued: true }),
      toggleJob: async () => ({ id: "job-maint", enabled: false }),
      updateJobSchedule: async () => ({}),
      updateMaintenanceConfig: async (id, payload, expectedPayload, secs) => {
        scheduleUpdates.push({ id, payload, expectedPayload, secs });
        return {};
      },
    },
  });

  let sjRenderer;
  await act(async () => {
    sjRenderer = create(React.createElement(ScheduledJobsView, { embedded: true }));
  });

  // Open schedule editor
  const editScheduleBtn = sjRenderer.root
    .findAllByType("button")
    .find((b) => String(b.props.title ?? "").includes("调整调度周期"));
  await act(async () => {
    editScheduleBtn.props.onClick();
  });

  const activePresetBtn = sjRenderer.root
    .findAllByType("button")
    .find((b) => textOf(b) === "1天");
  assert.match(activePresetBtn.props.className, /\bbg-sky-700\b/);
  assert.match(activePresetBtn.props.className, /\bhover:bg-sky-600\b/);

  const saveScheduleBtnInitial = sjRenderer.root
    .findAllByType("button")
    .find((b) => textOf(b).includes("保存周期"));
  assert.match(saveScheduleBtnInitial.props.className, /\bbg-sky-700\b/);
  assert.match(saveScheduleBtnInitial.props.className, /\bhover:bg-sky-600\b/);

  const getScheduleInput = () =>
    sjRenderer.root.find((n) => n.type === "input" && n.props.type === "number");

  // Typing "7" must NOT snap to 3600 per-keystroke!
  await act(async () => {
    getScheduleInput().props.onChange({ target: { value: "7" } });
  });
  assert.equal(
    String(getScheduleInput().props.value),
    "7",
    "typing '7' must keep draft '7' without snapping to 3600 mid-typing",
  );

  // Blurring "7" clamps to floor 3600
  await act(async () => {
    getScheduleInput().props.onBlur?.();
  });
  assert.equal(String(getScheduleInput().props.value), "3600", "blurring below floor must clamp to 3600");

  // Typing "7", "72", "720", "7200" then saving submits 7200 and parsed payload_token as expectedPayload
  for (const step of ["7", "72", "720", "7200"]) {
    await act(async () => {
      getScheduleInput().props.onChange({ target: { value: step } });
    });
    assert.equal(String(getScheduleInput().props.value), step);
  }

  const saveScheduleBtn = sjRenderer.root
    .findAllByType("button")
    .find((b) => textOf(b).includes("保存周期"));
  await act(async () => {
    await saveScheduleBtn.props.onClick();
  });

  assert.equal(scheduleUpdates.length, 1);
  assert.equal(scheduleUpdates[0].secs, 7200);
  assert.deepEqual(scheduleUpdates[0].expectedPayload, {
    version: 1,
    retention_days: 90,
    dry_run: false,
    cas_seq: 7,
  });
  await act(async () => sjRenderer.unmount());
});

test("Item 9: PlayerPage guards getPlaybackItem .catch with !cancelled when switching mediaItemId or unmounting", async () => {
  let rejectFirst;
  const { PlayerPage } = transpileFile("components/player/player-page.tsx", {
    "react-router-dom": { useNavigate: () => () => {} },
    "@/components/player/video-player": {
      VideoPlayer: ({ unit, title }) =>
        React.createElement("div", { "data-testid": "video-player" }, `${unit.media_item_id}:${title ?? ""}`),
    },
    "@/lib/api/playback": {
      DEFAULT_PLAYBACK_SCOPE: "member",
      getPlaybackItem: (id) => {
        if (id === "item-old") {
          return new Promise((_res, rej) => {
            rejectFirst = rej;
          });
        }
        return Promise.resolve({
          media_item_id: "item-new",
          library_id: "lib-1",
          kind: "movie",
          title: "新影片",
          poster_url: null,
        });
      },
      getPlaybackItemEpisodes: async () => ({ season_number: 1, episodes: [] }),
    },
    "@/lib/image-proxy": { imageUrl: (u) => u },
    "@/lib/player/play-links": { playerReturnPath: () => null },
  });

  let ppRenderer;
  await act(async () => {
    ppRenderer = create(React.createElement(PlayerPage, { mediaItemId: "item-old" }));
  });
  // Switch to item-new BEFORE item-old's getPlaybackItem rejects
  await act(async () => {
    ppRenderer.update(React.createElement(PlayerPage, { mediaItemId: "item-new" }));
  });
  await act(async () => {
    rejectFirst(new Error("旧条目已不可见"));
  });

  // Must still render VideoPlayer for item-new, NOT the failed screen from item-old!
  assert.doesNotMatch(textOf(ppRenderer.root), /旧条目已不可见/);
  const vp = ppRenderer.root.findAll((n) => n.props?.["data-testid"] === "video-player");
  assert.equal(vp.length, 1);
  assert.match(textOf(vp[0]), /item-new:新影片/);
  await act(async () => ppRenderer.unmount());
});

test("Item 10: CatalogCacheView replaces native window.confirm and alert with useConfirm and useToast", async () => {
  const catalogSrc = readFileSync(path.join(webRoot, "components/catalog-cache-view.tsx"), "utf8");
  assert.doesNotMatch(catalogSrc, /window\.confirm|\balert\s*\(/);

  const confirmCalls = [];
  const toastErrors = [];
  const deletedCalls = [];
  let confirmDecision = false;
  let shouldFailDelete = false;

  const { CatalogCacheView } = transpileFile("components/catalog-cache-view.tsx", {
    "@/components/poster-image": { PosterImage: () => null },
    "@/components/feedback": {
      useConfirm: () => async (opts) => {
        confirmCalls.push(opts);
        return confirmDecision;
      },
      useToast: () => ({
        success() {},
        info() {},
        error: (msg) => toastErrors.push(msg),
      }),
    },
    "@/lib/api/catalog-cache": {
      listCatalogCache: async () => [
        {
          id: "tmdb:movie:1",
          source: "tmdb",
          kind: "movie",
          title: "沙丘2",
          original_title: "Dune: Part Two",
          year: 2024,
          poster_url: "/p.jpg",
          overview: "简介",
          rating: 8.5,
          genres: ["科幻"],
          cached_keys: ["k1", "k2"],
        },
      ],
      deleteMediaCache: async (source, keys) => {
        if (shouldFailDelete) throw new Error("SQLite busy");
        deletedCalls.push({ source, keys });
      },
    },
    "@/lib/image-proxy": { imageUrl: (u) => u },
  });

  let ccRenderer;
  await act(async () => {
    ccRenderer = create(React.createElement(CatalogCacheView));
  });

  const getClearBtn = () =>
    ccRenderer.root.findAllByType("button").find((b) => textOf(b).trim() === "清除");

  // 1. User cancels confirm -> does not delete
  confirmDecision = false;
  await act(async () => {
    await getClearBtn().props.onClick();
  });
  assert.equal(confirmCalls.length, 1);
  assert.match(confirmCalls[0].title, /沙丘2/);
  assert.equal(deletedCalls.length, 0);

  // 2. User confirms, delete rejects -> toast.error called (no alert)
  confirmDecision = true;
  shouldFailDelete = true;
  await act(async () => {
    await getClearBtn().props.onClick();
  });
  assert.equal(toastErrors.length, 1);
  assert.match(toastErrors[0], /SQLite busy/);

  // 3. User confirms, delete succeeds
  shouldFailDelete = false;
  await act(async () => {
    await getClearBtn().props.onClick();
  });
  assert.equal(deletedCalls.length, 1);
  assert.deepEqual(deletedCalls[0], { source: "tmdb", keys: ["k1", "k2"] });

  await act(async () => ccRenderer.unmount());
});

test("Round 3: standalone non-AppShell views render relative z-10 roots and modal/12px buttons render contrast-safe classes", async () => {
  const prevDoc = globalThis.document;
  globalThis.document = { querySelector: () => null };
  try {
    // 1. PlayLayout root wrapper
    const { default: PlayLayout } = transpileFile("app/play/layout.tsx", {
      "react-router-dom": { Outlet: () => React.createElement("div", { "data-testid": "outlet" }) },
      "@/components/auth-gate": { AuthGate: ({ children }) => React.createElement(React.Fragment, null, children) },
      "@/components/route-fallback": { RouteFallback: () => null },
    });
    let playRenderer;
    await act(async () => {
      playRenderer = create(React.createElement(PlayLayout));
    });
    const playRoot = playRenderer.root.findByType("div");
    assert.match(playRoot.props.className, /\brelative\b/);
    assert.match(playRoot.props.className, /\bz-10\b/);
    await act(async () => playRenderer.unmount());

    // 2. PlayerPage failed state root wrapper
    const { PlayerPage } = transpileFile("components/player/player-page.tsx", {
      "react-router-dom": { useNavigate: () => () => {} },
      "@/components/player/video-player": { VideoPlayer: () => null },
      "@/lib/api/playback": {
        DEFAULT_PLAYBACK_SCOPE: "member",
        getPlaybackItem: async () => {
          throw new Error("条目不可见");
        },
        getPlaybackItemEpisodes: async () => ({ season_number: 1, episodes: [] }),
      },
      "@/lib/image-proxy": { imageUrl: (u) => u },
      "@/lib/player/play-links": { playerReturnPath: () => null },
    });
    let failedPlayerRenderer;
    await act(async () => {
      failedPlayerRenderer = create(React.createElement(PlayerPage, { mediaItemId: "item-err" }));
      await Promise.resolve();
    });
    const failedRoot = failedPlayerRenderer.root
      .findAllByType("div")
      .find((d) => String(d.props.className ?? "").includes("dark-media-surface"));
    assert.ok(failedRoot, "PlayerPage failed screen must render dark-media-surface wrapper");
    assert.match(failedRoot.props.className, /\brelative\b/);
    assert.match(failedRoot.props.className, /\bz-10\b/);
    await act(async () => failedPlayerRenderer.unmount());

    // 3. NotFound & RouteFallback root wrappers
    const { NotFound } = transpileFile("src/not-found.tsx");
    let nfRenderer;
    await act(async () => {
      nfRenderer = create(React.createElement(NotFound));
    });
    const nfRoot = nfRenderer.root.findByType("div");
    assert.match(nfRoot.props.className, /\brelative\b/);
    assert.match(nfRoot.props.className, /\bz-10\b/);
    await act(async () => nfRenderer.unmount());

    const { RouteFallback } = transpileFile("components/route-fallback.tsx", {
      "@/components/brand-loader": { BrandLoader: () => React.createElement("span") },
    });
    let rfRenderer;
    await act(async () => {
      rfRenderer = create(React.createElement(RouteFallback));
    });
    const rfRoot = rfRenderer.root.findByType("div");
    assert.match(rfRoot.props.className, /\brelative\b/);
    assert.match(rfRoot.props.className, /\bz-10\b/);
    await act(async () => rfRenderer.unmount());

    // 4. RevokeDeviceDialog, DeleteDownloadTaskDialog, and ClearLibraryHistoryDialog use .btn-danger
    const { RevokeDeviceDialog } = transpileFile("components/media-activity/device-action-dialogs.tsx", {
      "@/components/modal": { Modal: ModalStub },
    });
    let revokeRenderer;
    await act(async () => {
      revokeRenderer = create(
        React.createElement(RevokeDeviceDialog, {
          target: { userId: "u1", deviceId: "d1", label: "Apple TV" },
          busy: false,
          onClose() {},
          onConfirm() {},
        }),
      );
    });
    const revokeBtn = revokeRenderer.root
      .findAllByType("button")
      .find((b) => textOf(b).includes("注销设备"));
    assert.match(revokeBtn.props.className, /\bbtn-danger\b/);
    assert.doesNotMatch(revokeBtn.props.className, /bg-red-500\/85/);
    await act(async () => revokeRenderer.unmount());

    const { DeleteDownloadTaskDialog } = transpileFile("components/task-center/task-layout.tsx", {
      "react-router-dom": { Link: ({ to, children }) => React.createElement("a", { href: to }, children) },
      "@/components/icons": IconStub,
      "@/components/modal": { Modal: ModalStub },
      "@/lib/format": { formatBytes: (v) => `${v} B` },
    });
    let delTaskRenderer;
    await act(async () => {
      delTaskRenderer = create(
        React.createElement(DeleteDownloadTaskDialog, {
          task: {
            downloader_id: "dl-1",
            downloader_name: "qB",
            info_hash: "abc",
            name: "Demo.mkv",
            subscriptions: [],
          },
          busy: false,
          onClose() {},
          onConfirm() {},
        }),
      );
    });
    const delTaskBtn = delTaskRenderer.root
      .findAllByType("button")
      .find((b) => textOf(b).includes("仅删除任务"));
    assert.match(delTaskBtn.props.className, /\bbtn-danger\b/);
    assert.doesNotMatch(delTaskBtn.props.className, /bg-red-500\/85/);
    await act(async () => delTaskRenderer.unmount());

    const { WatchHistoryMenu } = transpileFile("components/watch-history-menu.tsx", {
      "@radix-ui/react-dropdown-menu": DropdownStub,
      "@/components/feedback": {
        useConfirm: () => async () => false,
        useToast: () => ({ success() {}, error() {}, info() {} }),
      },
      "@/components/icons": IconStub,
      "@/components/modal": { Modal: ModalStub },
      "@/lib/api/playback": { clearPlaybackHistory: async () => ({ deleted_states: 1 }) },
    });
    let whRenderer;
    await act(async () => {
      whRenderer = create(
        React.createElement(WatchHistoryMenu, {
          libraries: [{ id: "lib-1", name: "电影", kind: "movie" }],
          onCleared() {},
        }),
      );
    });
    await act(async () => {
      whRenderer.root
        .findAllByType("button")
        .find((b) => textOf(b).includes("清空某个媒体库的观看记录"))
        .props.onClick();
    });
    const clearLibBtn = whRenderer.root
      .findAllByType("button")
      .find((b) => textOf(b).includes("清空「电影」"));
    assert.match(clearLibBtn.props.className, /\bbtn-danger\b/);
    assert.doesNotMatch(clearLibBtn.props.className, /bg-red-500\/85/);
    await act(async () => whRenderer.unmount());
  } finally {
    if (prevDoc === undefined) delete globalThis.document;
    else globalThis.document = prevDoc;
  }
});

test("Round 3: UiPrefsProvider.savePrefs writes server-committed prefs to localStorage even when superseded by a concurrent save that fails", async () => {
  const prevWin = globalThis.window;
  const prevDoc = globalThis.document;
  const prevStorage = globalThis.localStorage;
  const prevMO = globalThis.MutationObserver;

  const store = new Map();
  globalThis.localStorage = {
    getItem: (k) => (store.has(k) ? store.get(k) : null),
    setItem: (k, v) => store.set(k, String(v)),
    removeItem: (k) => store.delete(k),
  };
  globalThis.window = { localStorage: globalThis.localStorage };
  globalThis.MutationObserver = class {
    observe() {}
    disconnect() {}
  };
  globalThis.document = {
    documentElement: {
      style: { setProperty() {} },
      setAttribute() {},
      removeAttribute() {},
      getAttribute: () => null,
    },
    querySelector: () => null,
  };

  try {
    const basePrefs = {
      theme: "silver",
      sidebar: { transparency: 0.49, brightness: -0.36, depth: 28 },
      scrim: { blur: 13, dark: 0.69 },
      nav: { order: [] },
      home: { rows: [] },
    };
    store.set("crawler-media.ui-prefs", JSON.stringify(basePrefs));

    const pendingUpdates = [];
    const { UiPrefsProvider, useUiPrefs } = transpileFile("lib/ui-prefs.tsx", {
      "@/lib/api/ui": {
        DEFAULT_UI_PREFS: basePrefs,
        fetchUiPreferences: async () => basePrefs,
        updateUiPreferences: (next) =>
          new Promise((resolve, reject) => {
            pendingUpdates.push({ next, resolve, reject });
          }),
      },
    });

    let latestCtx = null;
    function Probe() {
      latestCtx = useUiPrefs();
      return null;
    }

    let renderer;
    await act(async () => {
      renderer = create(
        React.createElement(UiPrefsProvider, null, React.createElement(Probe)),
      );
      await Promise.resolve();
    });
    assert.equal(latestCtx.prefs.theme, "silver");

    // Trigger save #1 (paper), then immediately save #2 (liquid) while #1 is in flight
    let save1Promise;
    let save2Promise;
    await act(async () => {
      save1Promise = latestCtx.savePrefs({ ...basePrefs, theme: "paper" });
      save2Promise = latestCtx.savePrefs({ ...basePrefs, theme: "liquid" });
    });
    assert.equal(pendingUpdates.length, 2);
    assert.equal(latestCtx.prefs.theme, "liquid");

    // Save #1 succeeds on server while save #2 is still in flight
    await act(async () => {
      pendingUpdates[0].resolve({ ...basePrefs, theme: "paper" });
      await save1Promise;
    });
    // localStorage FOUC cache must already record the newly server-committed "paper"
    assert.equal(
      JSON.parse(store.get("crawler-media.ui-prefs")).theme,
      "paper",
      "superseded save #1 that succeeded on server must still update localStorage cache",
    );

    // Save #2 fails -> React state rolls back to "paper" AND localStorage matches "paper"
    let save2Err = null;
    await act(async () => {
      pendingUpdates[1].reject(new Error("500 write timeout"));
      try {
        await save2Promise;
      } catch (err) {
        save2Err = err;
      }
    });
    assert.ok(save2Err instanceof Error);
    assert.equal(latestCtx.prefs.theme, "paper", "React prefs must roll back to save #1 ('paper')");
    assert.equal(
      JSON.parse(store.get("crawler-media.ui-prefs")).theme,
      "paper",
      "localStorage cache must match rolled-back server state ('paper'), not stale 'silver'",
    );

    // Also verify out-of-order resolution: save #3 (liquid) vs save #4 (netflix) where #4 resolves first
    let save3Promise;
    let save4Promise;
    await act(async () => {
      save3Promise = latestCtx.savePrefs({ ...basePrefs, theme: "liquid" });
      save4Promise = latestCtx.savePrefs({ ...basePrefs, theme: "netflix" });
    });
    await act(async () => {
      pendingUpdates[3].resolve({ ...basePrefs, theme: "netflix" });
      await save4Promise;
    });
    await act(async () => {
      pendingUpdates[2].resolve({ ...basePrefs, theme: "liquid" });
      await save3Promise;
    });
    assert.equal(latestCtx.prefs.theme, "netflix");
    assert.equal(
      JSON.parse(store.get("crawler-media.ui-prefs")).theme,
      "netflix",
      "older out-of-order save #3 must not overwrite newer committed cache ('netflix')",
    );

    await act(async () => renderer.unmount());
  } finally {
    if (prevWin === undefined) delete globalThis.window;
    else globalThis.window = prevWin;
    if (prevDoc === undefined) delete globalThis.document;
    else globalThis.document = prevDoc;
    if (prevStorage === undefined) delete globalThis.localStorage;
    else globalThis.localStorage = prevStorage;
    if (prevMO === undefined) delete globalThis.MutationObserver;
    else globalThis.MutationObserver = prevMO;
  }
});
