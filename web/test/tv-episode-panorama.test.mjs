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

test("discover API preserves season counts and 5-state episode availability fields", async () => {
  const { load } = compileTsRegistry({
    "@/lib/http": {
      request: async (path) => {
        if (path === "/media/tv/9001") {
          return {
            ok: true,
            data: {
              item: { id: "9001", title: "繁花", kind: "tv", year: 2023 },
              info: { rating: "8.7", genres: ["剧情"] },
              seasons: [
                {
                  season_number: 1,
                  name: "第 1 季",
                  episode_count: 30,
                  owned_episode_count: 18,
                  downloading_episode_count: 2,
                },
                {
                  season_number: 2,
                  name: "第 2 季",
                  episode_count: 12,
                  owned_episode_count: 0,
                  downloading_episode_count: 0,
                },
              ],
            },
          };
        }
        if (path === "/media/tv/9001/season/1") {
          return {
            ok: true,
            data: {
              episodes: [
                {
                  episode_number: 1,
                  name: "第一集",
                  air_date: "2023-12-27",
                  status: "in_library",
                  owned: true,
                  library_id: "lib-tv",
                  media_item_id: "item-9001",
                  resolution: "2160p",
                },
                {
                  episode_number: 2,
                  name: "第二集",
                  air_date: "2023-12-28",
                  status: "downloading",
                  owned: false,
                  library_id: null,
                  media_item_id: null,
                  resolution: null,
                },
              ],
            },
          };
        }
        throw new Error(`Unexpected path ${path}`);
      },
      resolveRequestUrl: (p) => `/api/v1/${p.replace(/^\//, "")}`,
    },
  });

  const discover = load("@/lib/api/discover");
  const detail = await discover.fetchDiscoveredTitleDetails("tmdb:tv:9001");
  assert.equal(detail.seasons?.[0]?.owned_episode_count, 18);
  assert.equal(detail.seasons?.[0]?.downloading_episode_count, 2);

  const episodes = await discover.fetchTvSeasonEpisodes("9001", 1);
  assert.equal(episodes.length, 2);
  assert.equal(episodes[0].status, "in_library");
  assert.equal(episodes[0].owned, true);
  assert.equal(episodes[0].library_id, "lib-tv");
  assert.equal(episodes[0].media_item_id, "item-9001");
  assert.equal(episodes[0].resolution, "2160p");
  assert.equal(episodes[1].status, "downloading");
  assert.equal(episodes[1].owned, false);
});

test("libraries API parses season_summaries and episode status/resolution", async () => {
  const detailPayload = {
    media_item_id: "m-1",
    kind: "tv",
    title: "庆余年",
    year: 2019,
    seasons: [1],
    season_summaries: [
      { season_number: 1, owned_count: 46, total_count: 46, downloading_count: 0 },
      { season_number: 2, owned_count: 0, total_count: 36, downloading_count: 3 },
    ],
    files: [],
    next_file_cursor: null,
  };
  const episodesPayload = [
    {
      episode_number: 1,
      name: "第 1 集",
      overview: null,
      air_date: "2024-05-16",
      still_url: null,
      owned: true,
      status: "in_library",
      resolution: "2160p",
      file_ids: ["f-1"],
      position_ms: 0,
      played: false,
      progress_percent: null,
    },
    {
      episode_number: 2,
      name: "第 2 集",
      overview: null,
      air_date: "2024-05-16",
      still_url: null,
      owned: false,
      status: "downloading",
      resolution: null,
      file_ids: [],
      position_ms: 0,
      played: false,
      progress_percent: null,
    },
  ];

  const api = loadLibrariesApi(
    {
      "@/lib/http": {
        request: async (path) => {
          if (path.includes("/episodes")) return { ok: true, data: episodesPayload };
          return { ok: true, data: detailPayload };
        },
        resolveRequestUrl: (p) => p,
      },
      "@/lib/library-file-pages": { collectAllFilePages: async (first) => first.files },
    },
    { fallbackEmpty: true },
  );

  const detail = await api.getLibraryItemDetail("lib-1", "m-1");
  assert.deepEqual(detail.seasons, [1]);
  assert.deepEqual(detail.season_summaries, [
    { season_number: 1, owned_count: 46, total_count: 46, downloading_count: 0 },
    { season_number: 2, owned_count: 0, total_count: 36, downloading_count: 3 },
  ]);

  const seasonData = await api.getItemEpisodes("lib-1", "m-1", 1);
  assert.equal(seasonData.episodes[0].status, "in_library");
  assert.equal(seasonData.episodes[0].resolution, "2160p");
  assert.equal(seasonData.episodes[1].status, "downloading");
});

test("SeasonsSection renders completion pills, summary bar, 5-state badges, and clickable play link for owned episodes", async () => {
  const { load } = compileTsRegistry({
    "react-router-dom": {
      Link: ({ to, children, ...rest }) => React.createElement("a", { href: to, ...rest }, children),
    },
    "@/components/artwork-image": {
      ArtworkImage: (props) => React.createElement("img", props),
    },
    "@/components/brand-loader": {
      BrandLoader: () => React.createElement("span", null, "loading"),
    },
    "@/lib/env": { publicEnv: { apiBaseUrl: "/api/v1" } },
    "@/lib/http": {
      request: async (path) => {
        if (path === "/media/tv/9001/season/2") {
          return {
            ok: true,
            data: {
              episodes: [
                {
                  episode_number: 1,
                  name: "首播",
                  still_url: "/stills/f-1",
                  air_date: "2024-01-01",
                  status: "in_library",
                  owned: true,
                  library_id: "lib-tv",
                  media_item_id: "item-9001",
                  resolution: "2160p",
                },
                {
                  episode_number: 2,
                  name: "抓取中",
                  still_url: null,
                  air_date: "2024-01-08",
                  status: "downloading",
                  owned: false,
                },
                {
                  episode_number: 3,
                  name: "掉盘集",
                  still_url: null,
                  air_date: "2024-01-15",
                  status: "file_missing",
                  owned: false,
                },
                {
                  episode_number: 4,
                  name: "漏抓集",
                  still_url: null,
                  air_date: "2024-01-22",
                  status: "missing",
                  owned: false,
                },
                {
                  episode_number: 5,
                  name: "未来集",
                  still_url: null,
                  air_date: "2099-12-31",
                  status: "unaired",
                  owned: false,
                },
              ],
            },
          };
        }
        return { ok: true, data: { episodes: [] } };
      },
      resolveRequestUrl: (p) => `/api/v1/${p.replace(/^\//, "")}`,
    },
  });

  const { SeasonsSection } = load("@/components/media-detail/seasons-section");
  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(SeasonsSection, {
        mediaId: "9001",
        seasons: [
          {
            season_number: 1,
            name: "第 1 季",
            episode_count: 24,
            owned_episode_count: 24,
            downloading_episode_count: 0,
          },
          {
            season_number: 2,
            name: "第 2 季",
            episode_count: 5,
            owned_episode_count: 1,
            downloading_episode_count: 1,
          },
          {
            season_number: 3,
            name: "第 3 季",
            episode_count: 10,
            owned_episode_count: 0,
            downloading_episode_count: 0,
          },
        ],
      }),
    );
  });

  const pills = renderer.root.findAllByType("button");
  const pillTexts = pills.map((btn) => JSON.stringify(btn.children));
  assert.ok(pillTexts.some((t) => t.includes("24/24")), `expected 24/24 pill, got ${pillTexts}`);
  assert.ok(pillTexts.some((t) => t.includes("1/5")), `expected 1/5 pill, got ${pillTexts}`);
  assert.ok(
    String(pills[0].props.className).includes("emerald"),
    "completed season pill should use emerald green accent",
  );

  // Switch to Season 2 to verify 5-state badges and click-to-play link
  await act(async () => {
    pills[1].props.onClick();
  });

  const html = JSON.stringify(renderer.toJSON());
  assert.ok(html.includes("共 5 集") || (html.includes("共 ") && html.includes("5")));
  assert.ok(html.includes("已入库"), "must render 已入库 status");
  assert.ok(html.includes("4K"), "must format 2160p resolution as 4K on owned badge");
  assert.ok(html.includes("下载中"), "must render 下载中 status");
  assert.ok(html.includes("文件缺失"), "must render 文件缺失 status");
  assert.ok(html.includes("未入库"), "must render 未入库 status");
  assert.ok(html.includes("未开播"), "must render 未开播 status");

  const links = renderer.root.findAllByType("a");
  assert.equal(links.length, 1, "only in_library episode card should be a Link");
  assert.equal(links[0].props.href, "/library/lib-tv/item/item-9001?season=2&episode=1");

  await act(async () => renderer.unmount());
});

test("SeasonEpisodesSection supports unowned seasons from season_summaries, 5-state badges, and stills/grid view toggle", async () => {
  globalThis.window = {
    requestAnimationFrame: (cb) => {
      cb(0);
      return 1;
    },
    cancelAnimationFrame: () => {},
  };

  const { load } = compileTsRegistry({
    "@/components/brand-loader": {
      BrandLoader: () => React.createElement("span", null, "loading"),
    },
    "@/components/h-scroller": {
      HScroller: ({ children, className }) => React.createElement("div", { className, "data-view": "stills" }, children),
    },
    "@/components/poster-image": {
      PosterImage: ({ fallback, src, alt }) =>
        src ? React.createElement("img", { src, alt }) : fallback,
    },
    "@/lib/env": { publicEnv: { apiBaseUrl: "/api/v1" } },
    "@/lib/http": {
      request: async () => ({ ok: true, data: [] }),
      resolveRequestUrl: (p) => `/api/v1/${p.replace(/^\//, "")}`,
    },
  });

  const { SeasonEpisodesSection } = load("@/components/library-item/season-episodes-section");
  const selections = [];
  const fetchEpisodes = async (_mediaId, season) => {
    if (season === 2) {
      return {
        season_number: 2,
        episodes: [
          {
            episode_number: 1,
            name: "第二季首集",
            overview: null,
            air_date: "2025-01-01",
            still_url: null,
            owned: false,
            status: "downloading",
            resolution: null,
            file_ids: [],
            position_ms: 0,
            played: false,
            progress_percent: null,
          },
          {
            episode_number: 2,
            name: "第二季待播",
            overview: null,
            air_date: "2099-01-01",
            still_url: null,
            owned: false,
            status: "unaired",
            resolution: null,
            file_ids: [],
            position_ms: 0,
            played: false,
            progress_percent: null,
          },
        ],
      };
    }
    return {
      season_number: 1,
      episodes: [
        {
          episode_number: 1,
          name: "入库已看",
          overview: null,
          air_date: "2024-01-01",
          still_url: "/stills/f-1",
          owned: true,
          status: "in_library",
          resolution: "2160p",
          file_ids: ["f-1"],
          position_ms: 0,
          played: true,
          progress_percent: null,
        },
        {
          episode_number: 2,
          name: "正在下载",
          overview: null,
          air_date: "2024-01-08",
          still_url: null,
          owned: false,
          status: "downloading",
          resolution: null,
          file_ids: [],
          position_ms: 0,
          played: false,
          progress_percent: null,
        },
        {
          episode_number: 3,
          name: "磁盘丢失",
          overview: null,
          air_date: "2024-01-15",
          still_url: null,
          owned: false,
          status: "file_missing",
          resolution: null,
          file_ids: [],
          position_ms: 0,
          played: false,
          progress_percent: null,
        },
        {
          episode_number: 4,
          name: "已播缺失",
          overview: null,
          air_date: "2024-01-22",
          still_url: null,
          owned: false,
          status: "missing",
          resolution: null,
          file_ids: [],
          position_ms: 0,
          played: false,
          progress_percent: null,
        },
        {
          episode_number: 5,
          name: "尚未播出",
          overview: null,
          air_date: "2099-02-01",
          still_url: null,
          owned: false,
          status: "unaired",
          resolution: null,
          file_ids: [],
          position_ms: 0,
          played: false,
          progress_percent: null,
        },
      ],
    };
  };

  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(SeasonEpisodesSection, {
        libraryId: "lib-1",
        detail: {
          media_item_id: "m-1",
          seasons: [1],
          season_summaries: [
            { season_number: 1, owned_count: 1, total_count: 5, downloading_count: 1 },
            { season_number: 2, owned_count: 0, total_count: 24, downloading_count: 1 },
          ],
          files: [{ file_id: "f-1", season: 1 }],
        },
        fetchEpisodes,
        onEpisodeChange: (sel) => selections.push(sel),
      }),
    );
  });

  // Season dropdown must include unowned Season 2 with (0/24)
  const options = renderer.root.findAllByType("option");
  assert.equal(options.length, 2);
  assert.ok(JSON.stringify(options[0].children).includes("第 1 季 (1/5)"));
  assert.ok(JSON.stringify(options[1].children).includes("第 2 季 (0/24)"));

  // Stills view header and distinct 5-state badges
  const stillsJson = JSON.stringify(renderer.toJSON());
  assert.ok(stillsJson.includes("下载中"), "stills view must show 下载中");
  assert.ok(stillsJson.includes("文件缺失"), "stills view must show 文件缺失");
  assert.ok(stillsJson.includes("缺失"), "stills view must show 缺失");
  assert.ok(stillsJson.includes("待播"), "stills view must show 待播");

  // Toggle to MoviePilot-style 全季网格 view
  const gridToggle = renderer.root
    .findAllByType("button")
    .find((btn) => JSON.stringify(btn.children).includes("全季网格"));
  assert.ok(gridToggle, "must render 全季网格 view toggle button");
  await act(async () => {
    gridToggle.props.onClick();
  });

  const gridJson = JSON.stringify(renderer.toJSON());
  assert.ok(gridJson.includes("在库"), "grid view must render 在库 badge");
  assert.ok(gridJson.includes("4K"), "grid view must render 4K resolution on 在库 badge");
  assert.ok(gridJson.includes("下载中"), "grid view must render 下载中 badge");
  assert.ok(gridJson.includes("文件缺失"), "grid view must render 文件缺失 badge");
  assert.ok(gridJson.includes("缺失"), "grid view must render 缺失 badge");
  assert.ok(gridJson.includes("待播"), "grid view must render 待播 badge");

  // Clicking episode 3 in grid view selects episode 3
  const ep3Btn = renderer.root.find(
    (node) => node.type === "button" && node.props["data-episode-number"] === 3,
  );
  await act(async () => {
    ep3Btn.props.onClick();
  });
  assert.equal(selections.at(-1)?.episode.episode_number, 3);

  // Switch to unowned Season 2 via dropdown
  const selectEl = renderer.root.findByType("select");
  await act(async () => {
    selectEl.props.onChange({ target: { value: "2" } });
  });
  assert.equal(selections.at(-1)?.seasonNumber, 2);
  assert.equal(selections.at(-1)?.episode.status, "downloading");

  await act(async () => renderer.unmount());
});
