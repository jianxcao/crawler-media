import assert from "node:assert/strict";
import { existsSync, readFileSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import React, { act } from "react";
import { create } from "react-test-renderer";
import ts from "typescript";

import * as libraryFilePages from "../lib/library-file-pages.ts";
import * as chapterFrames from "../lib/chapter-frame-task.ts";
import * as itemProbe from "../lib/library-item-probe.ts";
import * as playLinks from "../lib/player/play-links.ts";
import * as activeTarget from "../lib/player/active-target.ts";
import * as format from "../lib/format.ts";
import * as timeline from "../lib/player/timeline.ts";

const require = createRequire(import.meta.url);
globalThis.IS_REACT_ACT_ENVIRONMENT = true;

function loadTsModule(relativePath, overrides = {}) {
  const cache = new Map();
  const mocks = {
    "@/lib/env": {
      publicEnv: { apiBaseUrl: "/api/v1", appName: "Crawler Media" },
    },
    "@/lib/http": {
      request: async () => ({ data: null }),
      resolveRequestUrl: (p) => p,
    },
    ...overrides,
  };

  function resolveLocal(specifier, parentUrl) {
    let targetUrl = null;
    if (specifier.startsWith("@/")) {
      targetUrl = new URL(`../${specifier.slice(2)}`, import.meta.url);
    } else if (specifier.startsWith("./") || specifier.startsWith("../")) {
      targetUrl = new URL(specifier, parentUrl);
    }
    if (!targetUrl) return null;
    for (const suffix of ["", ".tsx", ".ts", "/index.tsx", "/index.ts"]) {
      const candidate = new URL(`${targetUrl.href}${suffix}`);
      if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
    }
    return null;
  }

  function loadUrl(fileUrl) {
    if (cache.has(fileUrl.href)) return cache.get(fileUrl.href).exports;
    const mod = { exports: {} };
    cache.set(fileUrl.href, mod);
    const source = readFileSync(fileUrl, "utf8");
    const compiled = ts.transpileModule(source, {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        esModuleInterop: true,
        jsx: ts.JsxEmit.ReactJSX,
        target: ts.ScriptTarget.ES2022,
      },
    }).outputText;
    new Function("require", "module", "exports", compiled)(
      (name) => {
        if (name in mocks) return mocks[name];
        const local = resolveLocal(name, fileUrl);
        if (local) return loadUrl(local);
        return require(name);
      },
      mod,
      mod.exports,
    );
    return mod.exports;
  }

  return loadUrl(new URL(relativePath, import.meta.url));
}

function collectText(node) {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(collectText).join("");
  return collectText(node.children);
}

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

test("API client serializes Movie/TV markers and file_id fingerprint cancel per contract", async () => {
  const requests = [];
  const chaptersApi = loadTsModule("../lib/api/libraries/chapters.ts", {
    "@/lib/http": {
      request: async (path, options = {}) => {
        requests.push({
          path,
          method: options.method ?? "GET",
          body: options.body ? JSON.parse(options.body) : undefined,
        });
        return { ok: true, data: { deleted: true } };
      },
    },
  });
  const probeApi = loadTsModule("../lib/api/probe.ts", {
    "@/lib/http": {
      request: async (path, options = {}) => {
        requests.push({
          path,
          method: options.method ?? "GET",
          body: options.body ? JSON.parse(options.body) : undefined,
        });
        return { data: { cancelled_units: 1, aborted_running: 1 } };
      },
    },
  });

  await chaptersApi.fetchSeasonMarkers("lib-1", "movie-1", null);
  await chaptersApi.fetchSeasonMarkers("lib-1", "tv-1", 0);
  await chaptersApi.deleteManualMarker("lib-1", "movie-1", { file_id: "f-movie" });
  await chaptersApi.deleteManualMarker("lib-1", "tv-1", { file_id: "f-tv", season: 0, episode: 2 });
  await probeApi.cancelFingerprintQueue({ media_id: "tv-1", file_id: "f-tv", include_running: true });

  assert.equal(requests[0].path, "/libraries/lib-1/items/movie-1/markers");
  assert.equal(requests[1].path, "/libraries/lib-1/items/tv-1/markers?season=0");
  assert.equal(requests[2].path, "/libraries/lib-1/items/movie-1/markers?file_id=f-movie");
  assert.equal(requests[3].path, "/libraries/lib-1/items/tv-1/markers?file_id=f-tv&season=0&episode=2");
  assert.deepEqual(requests[4], {
    path: "/probe/fingerprint/cancel",
    method: "POST",
    body: { media_id: "tv-1", file_id: "f-tv", include_running: true },
  });
});

test("FingerprintSettingsCard controls standalone workers, shows honest pause badges, retries load failure, and polls active queue", async (t) => {
  t.mock.timers.enable({ apis: ["setInterval"] });
  const calls = [];
  let loadAttempt = 0;
  let statusIndex = 0;
  const statuses = [
    {
      paused: false,
      background_paused: true,
      remote_reads_paused: false,
      workers: 1,
      active_workers: 1,
      queued_fingerprint: 2,
      running_fingerprint: 1,
      queued_analysis: 0,
    },
    {
      paused: false,
      background_paused: false,
      remote_reads_paused: true,
      workers: 1,
      active_workers: 0,
      queued_fingerprint: 0,
      running_fingerprint: 0,
      queued_analysis: 0,
    },
  ];

  const { FingerprintSettingsCard } = loadTsModule(
    "../components/fingerprint-settings-card.tsx",
    {
      "@/lib/api/probe": {
        getFingerprintSettings: async () => {
          loadAttempt += 1;
          if (loadAttempt === 1) throw new Error("连接超时");
          return { workers: 1, paused: false };
        },
        saveFingerprintSettings: async (next) => {
          calls.push(["saveSettings", next]);
          return next;
        },
        getFingerprintQueueStatus: async () => {
          calls.push(["getStatus"]);
          const item = statuses[Math.min(statusIndex, statuses.length - 1)];
          statusIndex += 1;
          return item;
        },
        pauseFingerprintQueue: async () => {
          calls.push(["pause"]);
          return { ...statuses[0], paused: true, active_workers: 0, running_fingerprint: 0 };
        },
        resumeFingerprintQueue: async () => {
          calls.push(["resume"]);
          return statuses[0];
        },
        cancelFingerprintQueue: async (opts) => {
          calls.push(["cancel", opts]);
          return { cancelled_units: 2, aborted_running: 1 };
        },
      },
    },
  );

  let renderer;
  t.after(async () => {
    if (renderer) await act(async () => renderer.unmount());
  });
  await act(async () => {
    renderer = create(React.createElement(FingerprintSettingsCard));
  });

  assert.match(collectText(renderer.toJSON()), /连接超时/);
  const retryBtn = renderer.root.findAllByType("button").find((b) => collectText(b) === "重试");
  assert.ok(retryBtn, "must render 重试 button when initial load fails");

  statusIndex = 0;
  await act(async () => {
    await retryBtn.props.onClick();
  });

  const textAfterRetry = collectText(renderer.toJSON());
  assert.match(textAfterRetry, /受全局探测暂停限制/);
  assert.doesNotMatch(textAfterRetry, /声纹队列已启用/);

  await act(async () => {
    t.mock.timers.tick(3000);
    await Promise.resolve();
  });
  const textAfterPoll = collectText(renderer.toJSON());
  assert.match(textAfterPoll, /远程读取已暂停/);
  assert.doesNotMatch(textAfterPoll, /声纹队列已启用/);

  const workerInput = renderer.root.findByProps({ "aria-label": "声纹检测并发" });
  await act(async () => {
    workerInput.props.onChange({ target: { value: "2" } });
  });
  const saveBtn = renderer.root.findAllByType("button").find((b) => collectText(b) === "保存声纹设置");
  await act(async () => {
    await saveBtn.props.onClick();
  });
  assert.deepEqual(calls.find((c) => c[0] === "saveSettings"), [
    "saveSettings",
    { workers: 2, paused: false },
  ]);

  const pauseBtn = renderer.root.findAllByType("button").find((b) => collectText(b) === "暂停声纹队列");
  await act(async () => {
    await pauseBtn.props.onClick();
  });
  assert.ok(calls.some((c) => c[0] === "pause"));

  const cancelBtn = renderer.root.findAllByType("button").find((b) => collectText(b) === "终止声纹任务");
  await act(async () => {
    await cancelBtn.props.onClick();
  });
  assert.deepEqual(calls.find((c) => c[0] === "cancel"), ["cancel", { include_running: true }]);
});

test("ScrapeSettingsSection associates scrape-fp-duration-secs label/input and validates integer 30..=3600", async (t) => {
  let savedSetting = null;
  const stableToast = { info: () => {}, success: () => {}, error: () => {} };
  const { ScrapeSettingsSection } = loadTsModule("../components/scrape-settings-section.tsx", {
    "@/components/brand-loader": {
      BrandLoader: () => React.createElement("span", null, "loading"),
    },
    "@/components/feedback": {
      useToast: () => stableToast,
    },
    "@/components/fingerprint-settings-card": {
      FingerprintSettingsCard: () => React.createElement("div", null, "fp-card"),
    },
    "@/components/probe-settings-card": {
      ProbeSettingsCard: () => React.createElement("div", null, "probe-card"),
    },
    "@/lib/api/scrape": {
      SCRAPE_DEFAULTS: {
        language_priority: [],
        cert_country_priority: [],
        poster_mode: "",
        poster_language_priority: [],
        backdrop_language_priority: [],
        poster_min_width: null,
        backdrop_min_width: null,
        poster_size: "",
        backdrop_size: "",
        still_size: "",
        naming_entry_dir: "",
        naming_movie_file: "",
        naming_season_dir: "",
        naming_episode_file: "",
        mirror_images: null,
        mirror_nfo: null,
        mirror_episode_thumbs: null,
        fanart_api_key: null,
        fanart_language: [],
        fingerprint_duration_secs: null,
        fingerprint_sampling_mode: null,
      },
      getScrapeConfig: async () => ({
        setting: {
          language_priority: [],
          cert_country_priority: [],
          poster_mode: "",
          poster_language_priority: [],
          backdrop_language_priority: [],
          poster_min_width: null,
          backdrop_min_width: null,
          poster_size: "",
          backdrop_size: "",
          still_size: "",
          naming_entry_dir: "",
          naming_movie_file: "",
          naming_season_dir: "",
          naming_episode_file: "",
          mirror_images: null,
          mirror_nfo: null,
          mirror_episode_thumbs: null,
          fanart_api_key: null,
          fanart_language: [],
          fingerprint_duration_secs: null,
          fingerprint_sampling_mode: null,
        },
        effective: {
          language_priority: ["zh-CN"],
          cert_country_priority: ["CN"],
          poster_mode: "default",
          poster_language_priority: [],
          backdrop_language_priority: [],
          poster_min_width: 500,
          backdrop_min_width: 1920,
          poster_size: "w780",
          backdrop_size: "original",
          still_size: "w300",
          naming_entry_dir: "",
          naming_movie_file: "",
          naming_season_dir: "",
          naming_episode_file: "",
          mirror_images: true,
          mirror_nfo: true,
          mirror_episode_thumbs: true,
          theintrodb_enabled: false,
          theintrodb_api_key: null,
          fanart_configured: false,
          fingerprint_duration_secs: 180,
          fingerprint_sampling_mode: "full_window",
        },
      }),
      previewScrapeNaming: async () => ({
        movie: "",
        tv: "",
        movie_media_title: "",
        tv_media_title: "",
      }),
      saveScrapeConfig: async (setting) => {
        savedSetting = setting;
        return { setting, effective: {} };
      },
      fetchCountryCandidates: async () => [],
      fetchLanguageCandidates: async () => [],
    },
  });

  let renderer;
  t.after(async () => {
    if (renderer) await act(async () => renderer.unmount());
  });
  await act(async () => {
    renderer = create(React.createElement(ScrapeSettingsSection));
  });

  const markersTab = renderer.root.findAllByType("button").find((b) => collectText(b) === "片头片尾");
  await act(async () => {
    markersTab.props.onClick();
  });

  const label = renderer.root.findByProps({ htmlFor: "scrape-fp-duration-secs" });
  assert.ok(label);
  const input = renderer.root.findByProps({ id: "scrape-fp-duration-secs" });
  const saveBtn = () => renderer.root.findAllByType("button").find((b) => collectText(b) === "保存设置");

  for (const invalid of ["29", "3601", "90.5"]) {
    await act(async () => {
      input.props.onChange({ target: { value: invalid } });
    });
    assert.equal(saveBtn().props.disabled, true, `expected disabled for ${invalid}`);
  }

  await act(async () => {
    input.props.onChange({ target: { value: "120" } });
  });
  assert.equal(saveBtn().props.disabled, false);
  await act(async () => {
    await saveBtn().props.onClick();
  });
  assert.equal(savedSetting.fingerprint_duration_secs, 120);
});

test("MarkerControlPanel validates timestamps strictly, preserves sub-second precision, supports manual unlock, and handles delete while editing", async (t) => {
  const calls = [];
  let changedCount = 0;
  let deleteGate = null;
  const sampleDiagnostics = {
    media_id: "media-1",
    season: 1,
    active: false,
    paused: false,
    job: {
      id: "job-1",
      kind: "marker_refresh",
      season: 1,
      status: "succeeded",
      total: 1,
      completed: 1,
      succeeded: 1,
      failed: 0,
      error: null,
      elapsed_ms: 1200,
      phase: "succeeded",
      sampling_mode: "adaptive",
    },
    episodes: [
      {
        file_id: "file-1",
        season: 1,
        episode: 1,
        duration_ms: 1440000,
        multi_unit: false,
        marker: {
          intro_start_ms: 15250,
          intro_end_ms: 105125,
          outro_start_ms: null,
          outro_end_ms: null,
          source: "fingerprint",
          locked: true,
          updated_at: 1000,
        },
        detection: null,
      },
    ],
  };

  const { MarkerControlPanel } = loadTsModule(
    "../components/library-item/marker-control-panel.tsx",
    {
      "@/lib/api/libraries": {
        fetchSeasonMarkers: async () => sampleDiagnostics,
        detectItemMarkers: async () => ({
          queued: true,
          already_running: false,
          skipped_multi_unit: 0,
          job: null,
        }),
        saveManualMarker: async (_lib, _media, body) => {
          calls.push(["saveManual", body]);
          return { ...body, source: "manual", updated_at: 2000 };
        },
        deleteManualMarker: async (_lib, _media, target) => {
          calls.push(["deleteManual", target]);
          if (deleteGate) await deleteGate.promise;
          return { deleted: true };
        },
      },
      "@/lib/api/probe": {
        cancelFingerprintQueue: async () => ({ cancelled_units: 0, aborted_running: 0 }),
      },
    },
  );

  let renderer;
  t.after(async () => {
    deleteGate?.resolve();
    if (renderer) await act(async () => renderer.unmount());
  });
  await act(async () => {
    renderer = create(
      React.createElement(MarkerControlPanel, {
        libraryId: "lib-1",
        mediaItemId: "media-1",
        seasons: [1],
        selectedSeason: 1,
        selectedEpisode: 1,
        selectedFileId: "file-1",
        onChanged: () => {
          changedCount += 1;
        },
      }),
    );
  });

  const initialText = collectText(renderer.toJSON());
  assert.doesNotMatch(initialText, /阶段：/);
  assert.match(initialText, /模式：自适应采样/);

  const editBtn = () => renderer.root.findAllByType("button").find((b) => collectText(b) === "编辑/锁定");
  await act(async () => {
    editBtn().props.onClick();
  });

  const introStart = () => renderer.root.findByProps({ "aria-label": "片头起点" });
  const introEnd = () => renderer.root.findByProps({ "aria-label": "片头终点" });
  const outroStart = () => renderer.root.findByProps({ "aria-label": "片尾起点" });
  const outroEnd = () => renderer.root.findByProps({ "aria-label": "片尾终点" });
  assert.equal(introStart().props.value, "15.25");
  assert.equal(introEnd().props.value, "105.125");

  const saveBtn = () =>
    renderer.root
      .findAllByType("button")
      .find((b) => collectText(b) === "保存锁定" || collectText(b) === "保存并解锁");

  for (const bad of ["abc", "-5", "01:", ":30", ":", "01:60", "01:60:00", "01:00:60", "90000"]) {
    await act(async () => {
      outroStart().props.onChange({ target: { value: bad } });
      outroEnd().props.onChange({ target: { value: "1400" } });
    });
    await act(async () => {
      await saveBtn().props.onClick();
    });
    assert.equal(calls.length, 0, `malformed "${bad}" must not call saveManualMarker`);
    assert.ok(renderer.root.findAllByProps({ role: "alert" }).length > 0);
  }

  await act(async () => {
    outroStart().props.onChange({ target: { value: "100" } });
    outroEnd().props.onChange({ target: { value: "200" } });
  });
  await act(async () => {
    await saveBtn().props.onClick();
  });
  assert.equal(calls.length, 0, "introEnd > outroStart must be rejected");

  await act(async () => {
    outroStart().props.onChange({ target: { value: "1300" } });
    outroEnd().props.onChange({ target: { value: "1500" } });
  });
  await act(async () => {
    await saveBtn().props.onClick();
  });
  assert.equal(calls.length, 0, "outroEnd > duration_ms (1440s) must be rejected");

  await act(async () => {
    outroStart().props.onChange({ target: { value: "22:00" } });
    outroEnd().props.onChange({ target: { value: "23:30" } });
  });
  const lockCheckbox = renderer.root.findByProps({ "aria-label": "锁定人工标记" });
  assert.equal(lockCheckbox.props.checked, true);
  await act(async () => {
    lockCheckbox.props.onChange({ target: { checked: false } });
  });
  await act(async () => {
    await saveBtn().props.onClick();
  });
  assert.deepEqual(calls[0], [
    "saveManual",
    {
      file_id: "file-1",
      season: 1,
      episode: 1,
      intro_start_ms: 15250,
      intro_end_ms: 105125,
      outro_start_ms: 1320000,
      outro_end_ms: 1410000,
      locked: false,
    },
  ]);
  assert.equal(changedCount, 1);

  await act(async () => {
    editBtn().props.onClick();
  });
  assert.ok(renderer.root.findAllByProps({ "aria-label": "片头起点" }).length === 1);
  deleteGate = deferred();
  const clearBtn = () => renderer.root.findAllByType("button").find((b) => collectText(b) === "清除");
  let pendingDelete;
  await act(async () => {
    pendingDelete = clearBtn().props.onClick();
  });
  assert.equal(editBtn().props.disabled, true, "row edit button must be disabled while busy");
  assert.equal(clearBtn().props.disabled, true, "row clear button must be disabled while busy");
  await act(async () => {
    deleteGate.resolve();
    await pendingDelete;
  });
  assert.equal(
    renderer.root.findAllByProps({ "aria-label": "片头起点" }).length,
    0,
    "deleting edited row must close edit form",
  );
  assert.equal(changedCount, 2);
});

test("MarkerControlPanel defers onChanged for async detect until active->false, handles Season 0, cross-season guard, and out-of-order loads", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout", "setInterval"] });
  const calls = [];
  let changedCount = 0;
  let season0Gate = null;
  let activeState = false;
  const phaseState = "capturing";

  const { MarkerControlPanel } = loadTsModule(
    "../components/library-item/marker-control-panel.tsx",
    {
      "@/lib/api/libraries": {
        fetchSeasonMarkers: async (_lib, _media, season) => {
          calls.push(["fetch", season]);
          if (season === 0 && season0Gate) {
            await season0Gate.promise;
          }
          return {
            media_id: "media-1",
            season,
            active: activeState,
            paused: false,
            job: {
              id: "job-2",
              kind: "marker_refresh",
              season,
              status: activeState ? "running" : "succeeded",
              total: 3,
              completed: activeState ? 1 : 3,
              succeeded: activeState ? 1 : 3,
              failed: 0,
              error: null,
              elapsed_ms: 800,
              phase: activeState ? phaseState : "succeeded",
              sampling_mode: "full_window",
            },
            episodes: [
              {
                file_id: `file-s${season}`,
                season,
                episode: 1,
                duration_ms: 1200000,
                multi_unit: false,
                marker: null,
                detection: null,
              },
            ],
          };
        },
        detectItemMarkers: async (_lib, _media, body) => {
          calls.push(["detect", body]);
          if (body.mode === "theintrodb_only") {
            return {
              queued: false,
              already_running: false,
              skipped_multi_unit: 0,
              updated_episodes: 2,
              job: null,
            };
          }
          activeState = true;
          return { queued: true, already_running: false, skipped_multi_unit: 0, job: null };
        },
        saveManualMarker: async () => ({}),
        deleteManualMarker: async () => ({ deleted: true }),
      },
      "@/lib/api/probe": {
        cancelFingerprintQueue: async (opts) => {
          calls.push(["cancel", opts]);
          activeState = false;
          return { cancelled_units: 2, aborted_running: 1 };
        },
      },
    },
  );

  let renderer;
  t.after(async () => {
    season0Gate?.resolve();
    if (renderer) await act(async () => renderer.unmount());
  });
  await act(async () => {
    renderer = create(
      React.createElement(MarkerControlPanel, {
        libraryId: "lib-1",
        mediaItemId: "media-1",
        seasons: [0, 1],
        selectedSeason: undefined,
        selectedEpisode: 1,
        selectedFileId: "file-s0",
        onChanged: () => {
          changedCount += 1;
        },
      }),
    );
  });

  const seasonSelect = renderer.root.findByProps({ "aria-label": "检测季选择" });
  assert.equal(seasonSelect.props.value, 1);
  const optionLabels = seasonSelect.findAllByType("option").map((o) => collectText(o));
  assert.deepEqual(optionLabels, ["特别篇", "第 1 季"]);

  assert.equal(
    renderer.root.findAllByType("button").some((b) => collectText(b).includes("检测第 1 集")),
    false,
    "must hide single-episode detect when selectedSeason !== season",
  );

  const detectSeasonBtn = renderer.root
    .findAllByType("button")
    .find((b) => collectText(b) === "检测本季");
  await act(async () => {
    await detectSeasonBtn.props.onClick();
  });
  assert.equal(changedCount, 0, "async detect enqueue must not call onChanged before completion");
  const activeText = collectText(renderer.toJSON());
  assert.match(activeText, /阶段：采样声纹中/);
  assert.match(activeText, /模式：完整窗口/);

  activeState = false;
  await act(async () => {
    t.mock.timers.tick(2000);
    await Promise.resolve();
  });
  assert.equal(changedCount, 1, "active -> false transition during poll must call onChanged");

  season0Gate = deferred();
  await act(async () => {
    seasonSelect.props.onChange({ target: { value: "0" } });
  });
  await act(async () => {
    seasonSelect.props.onChange({ target: { value: "1" } });
  });
  await act(async () => {
    season0Gate.resolve();
    await Promise.resolve();
  });
  assert.equal(seasonSelect.props.value, 1);

  const modeSelect = renderer.root.findByProps({ "aria-label": "片头片尾检测策略" });
  await act(async () => {
    modeSelect.props.onChange({ target: { value: "theintrodb_only" } });
  });
  await act(async () => {
    await detectSeasonBtn.props.onClick();
  });
  assert.match(collectText(renderer.toJSON()), /2/);
  assert.equal(changedCount, 2);
});

test("Movie ItemChaptersSection + LibraryItemDetailView support Movie markers and refresh ChapterStrip on marker changes", async (t) => {
  const movieCalls = [];
  const { ItemChaptersSection } = loadTsModule(
    "../components/library-item/item-chapters-section.tsx",
    {
      "@/components/chapter-strip": {
        ChapterStrip: ({ chapters }) =>
          React.createElement("div", { "data-chapters": chapters.length }),
      },
      "@/lib/api/libraries": {
        fetchSeasonMarkers: async (_lib, _media, season) => {
          movieCalls.push(["fetch", season]);
          return {
            media_id: "movie-1",
            season: null,
            active: false,
            paused: false,
            job: null,
            episodes: [
              {
                file_id: "movie-file-1",
                season: null,
                episode: null,
                duration_ms: 7200000,
                multi_unit: false,
                marker: {
                  intro_start_ms: 0,
                  intro_end_ms: 60000,
                  outro_start_ms: 6900000,
                  outro_end_ms: 7200000,
                  source: "theintrodb",
                  locked: false,
                  updated_at: 1000,
                },
                detection: null,
              },
            ],
          };
        },
        detectItemMarkers: async (_lib, _media, body) => {
          movieCalls.push(["detect", body]);
          return {
            queued: false,
            already_running: false,
            skipped_multi_unit: 0,
            updated_episodes: 1,
            job: null,
          };
        },
        saveManualMarker: async (_lib, _media, body) => {
          movieCalls.push(["save", body]);
          return { ...body, source: "manual", updated_at: 2000 };
        },
        deleteManualMarker: async (_lib, _media, target) => {
          movieCalls.push(["delete", target]);
          return { deleted: true };
        },
      },
      "@/lib/api/probe": {
        cancelFingerprintQueue: async () => ({ cancelled_units: 0, aborted_running: 0 }),
      },
    },
  );

  let changed = 0;
  let sectionRenderer;
  t.after(async () => {
    if (sectionRenderer) await act(async () => sectionRenderer.unmount());
  });
  await act(async () => {
    sectionRenderer = create(
      React.createElement(ItemChaptersSection, {
        libraryId: "lib-movie",
        mediaItemId: "movie-1",
        seasons: [],
        isTv: false,
        canManageLibraries: true,
        chapterTarget: { file_id: "movie-file-1" },
        chapters: [],
        frameTask: null,
        chapterError: null,
        chaptersBusy: false,
        probing: false,
        resumeMs: null,
        onRefreshChapters: () => {},
        onRegenerateChapters: () => {},
        onProbeMedia: () => {},
        onPlayChapter: () => {},
        onMarkersChanged: () => {
          changed += 1;
        },
      }),
    );
  });

  const toggleBtn = sectionRenderer.root
    .findAllByType("button")
    .find((b) => collectText(b).includes("片头片尾控制"));
  assert.ok(toggleBtn, "Movie item must render 片头片尾控制 toggle button");
  assert.equal(toggleBtn.props["aria-expanded"], false);

  await act(async () => {
    toggleBtn.props.onClick();
  });
  assert.equal(toggleBtn.props["aria-expanded"], true);

  const movieText = collectText(sectionRenderer.toJSON());
  assert.match(movieText, /正片/);
  assert.doesNotMatch(movieText, /Enull|E00/);
  assert.equal(sectionRenderer.root.findAllByProps({ "aria-label": "检测季选择" }).length, 0);

  const modeOptions = sectionRenderer.root
    .findByProps({ "aria-label": "片头片尾检测策略" })
    .findAllByType("option")
    .map((o) => o.props.value);
  assert.deepEqual(modeOptions, ["theintrodb_only"]);

  const detectMovieBtn = sectionRenderer.root
    .findAllByType("button")
    .find((b) => collectText(b) === "同步云端标记");
  await act(async () => {
    await detectMovieBtn.props.onClick();
  });
  assert.deepEqual(movieCalls.find((c) => c[0] === "detect"), [
    "detect",
    { mode: "theintrodb_only" },
  ]);
  assert.equal(changed, 1);

  const editMovieBtn = sectionRenderer.root
    .findAllByType("button")
    .find((b) => collectText(b) === "编辑/锁定");
  await act(async () => {
    editMovieBtn.props.onClick();
  });
  const saveMovieBtn = sectionRenderer.root
    .findAllByType("button")
    .find((b) => collectText(b) === "保存锁定");
  await act(async () => {
    await saveMovieBtn.props.onClick();
  });
  const savedPayload = movieCalls.find((c) => c[0] === "save")[1];
  assert.equal("season" in savedPayload, false);
  assert.equal("episode" in savedPayload, false);

  const deleteMovieBtn = sectionRenderer.root
    .findAllByType("button")
    .find((b) => collectText(b) === "清除");
  await act(async () => {
    await deleteMovieBtn.props.onClick();
  });
  assert.deepEqual(movieCalls.find((c) => c[0] === "delete"), [
    "delete",
    { file_id: "movie-file-1" },
  ]);

  await act(async () => {
    sectionRenderer.unmount();
    sectionRenderer = null;
  });

  // Full LibraryItemDetailView -> ItemChaptersSection -> MarkerControlPanel -> ChapterStrip reload wiring
  let chapterLoads = 0;
  let frameLoads = 0;
  let chapterRows = [];
  const leaf = () => null;
  const passthrough = ({ children: c }) => c;
  const fallback = new Proxy({}, { get: (_, k) => (k === "__esModule" ? false : leaf) });
  globalThis.window = {
    location: { pathname: "/library/lib-1/item/movie-1", search: "" },
    sessionStorage: { setItem: () => {} },
  };
  const stableDetailToast = { info: () => {}, success: () => {}, error: () => {} };
  const stableConfirm = async () => true;
  const stableBackdrop = { setOverrideBackdrop: () => {} };
  const stableTheme = { id: "default" };
  const stablePerms = { canManageLibraries: true, isAdmin: true };
  const stableSubEntry = { canSubscribe: false };
  const noopNav = () => {};
  const { LibraryItemDetailView } = loadTsModule("../components/library-item-detail-view.tsx", {
    "react-router-dom": { Link: "a", useNavigate: () => noopNav },
    "@radix-ui/react-dropdown-menu": {
      Root: passthrough,
      Trigger: passthrough,
      Portal: passthrough,
      Content: passthrough,
      Separator: leaf,
      Item: ({ children: c, onSelect }) =>
        React.createElement("button", { type: "button", onClick: onSelect }, c),
    },
    "@/components/page-nav": {
      PAGE_NAV_BUTTON_CLASS: "nav-btn",
      PageNav: ({ actions }) => React.createElement("nav", null, actions),
    },
    "@/components/chapter-strip": {
      ChapterStrip: ({ chapters }) =>
        React.createElement(
          "div",
          { "data-chapter-strip": "true" },
          chapters.map((ch) => ch.title).join(","),
        ),
    },
    "@/components/artwork-image": { ArtworkImage: leaf },
    "@/components/cast-row": { CastRow: leaf },
    "@/components/collections-add-button": { CollectionsAddButton: leaf },
    "@/components/media-row": { MediaRow: leaf },
    "@/components/media-track-rows": { MediaTrackRows: leaf },
    "@/components/modal": { Modal: leaf },
    "@/components/tooltip": { Tooltip: passthrough },
    "@/components/h-scroller": { HScroller: passthrough },
    "@/components/feedback": {
      useConfirm: () => stableConfirm,
      useToast: () => stableDetailToast,
    },
    "@/components/subscribe-entry": { useSubscribeEntry: () => stableSubEntry },
    "@/lib/permissions": { usePermissions: () => stablePerms },
    "@/lib/backdrop": { useBackdrop: () => stableBackdrop },
    "@/lib/ui-prefs": { useTheme: () => stableTheme },
    "@/lib/use-media-query": { useIsMobile: () => false },
    "@/lib/use-page-title": { usePageTitle: noopNav },
    "@/lib/back-navigation": { useBackNavigation: () => noopNav },
    "@/lib/library-file-pages": libraryFilePages,
    "@/lib/chapter-frame-task": chapterFrames,
    "@/lib/library-item-probe": itemProbe,
    "@/lib/player/play-links": playLinks,
    "@/lib/player/active-target": activeTarget,
    "@/lib/format": format,
    "@/lib/player/timeline": timeline,
    "@/lib/api/playback": {
      fetchResumeState: async () => null,
      fetchPlaybackMarks: async () => ({ played: false, is_favorite: false, unplayed_count: null }),
      setPlaybackMarks: async () => ({ played: false, is_favorite: false, unplayed_count: null }),
      clearPlaybackHistory: async () => ({ deleted_states: 0 }),
    },
    "@/lib/api/probe": {
      cancelFingerprintQueue: async () => ({ cancelled_units: 0, aborted_running: 0 }),
    },
    "@/lib/api/libraries": {
      ...fallback,
      getLibraryItemDetail: async () => ({
        media_item_id: "movie-1",
        kind: "movie",
        title: "Test Movie",
        year: 2025,
        original_title: null,
        tmdb_id: null,
        poster_url: null,
        backdrop_url: null,
        overview: null,
        rating: null,
        runtime_minutes: 120,
        brands: [],
        genres: [],
        cast: [],
        seasons: [],
        files: [
          {
            file_id: "movie-file-1",
            path: "/movies/test.mkv",
            presence: "present",
            publication: "published",
            units: [{ season: 0, episode: 0 }],
            season: null,
            episode: null,
            resolution: "1080p",
            codec: "hevc",
            hdr: null,
            quality_source: "probe",
            audio_tracks: [],
            subtitle_tracks: [],
            video_track: null,
            probed: true,
            probe_queued: false,
          },
        ],
      }),
      getLibrary: async () => ({ id: "lib-1", name: "Movies", kind: "movie" }),
      fetchItemChapters: async () => {
        chapterLoads += 1;
        return chapterRows;
      },
      fetchChapterFrameStatus: async () => {
        frameLoads += 1;
        return { task: null, empty: chapterRows.length === 0 };
      },
      fetchSeasonMarkers: async () => ({
        media_id: "movie-1",
        season: null,
        active: false,
        paused: false,
        job: null,
        episodes: [
          {
            file_id: "movie-file-1",
            season: null,
            episode: null,
            duration_ms: 7200000,
            multi_unit: false,
            marker: null,
            detection: null,
          },
        ],
      }),
      saveManualMarker: async () => {
        chapterRows = [{ start_ms: 0, end_ms: 60000, title: "片头", image_url: null }];
        return {
          intro_start_ms: 0,
          intro_end_ms: 60000,
          outro_start_ms: null,
          outro_end_ms: null,
          source: "manual",
          locked: true,
          updated_at: 1000,
        };
      },
    },
  });

  let detailRenderer;
  t.after(async () => {
    if (detailRenderer) await act(async () => detailRenderer.unmount());
  });
  await act(async () => {
    detailRenderer = create(
      React.createElement(LibraryItemDetailView, {
        libraryId: "lib-1",
        mediaItemId: "movie-1",
      }),
    );
  });

  const beforeChapterLoads = chapterLoads;
  const beforeFrameLoads = frameLoads;
  const openPanelBtn = detailRenderer.root
    .findAllByType("button")
    .find((b) => collectText(b).includes("片头片尾控制"));
  await act(async () => {
    openPanelBtn.props.onClick();
  });
  const rowEditBtn = detailRenderer.root
    .findAllByType("button")
    .find((b) => collectText(b) === "编辑/锁定");
  await act(async () => {
    rowEditBtn.props.onClick();
  });
  await act(async () => {
    detailRenderer.root
      .findByProps({ "aria-label": "片头起点" })
      .props.onChange({ target: { value: "0" } });
    detailRenderer.root
      .findByProps({ "aria-label": "片头终点" })
      .props.onChange({ target: { value: "60" } });
  });
  const saveDetailMarkerBtn = detailRenderer.root
    .findAllByType("button")
    .find((b) => collectText(b) === "保存锁定");
  await act(async () => {
    await saveDetailMarkerBtn.props.onClick();
  });

  assert.ok(chapterLoads > beforeChapterLoads, "saving manual marker must reload item chapters");
  assert.ok(frameLoads > beforeFrameLoads, "saving manual marker must rediscover chapter frames");
  assert.match(
    collectText(detailRenderer.root.findByProps({ "data-chapter-strip": "true" })),
    /片头/,
  );
});

test("MarkerControlPanel preserves manual season switch when selectedSeason is set and sends library_id on cancel", async (t) => {
  const calls = [];
  let season2Gate = null;
  const { MarkerControlPanel } = loadTsModule(
    "../components/library-item/marker-control-panel.tsx",
    {
      "@/lib/api/libraries": {
        fetchSeasonMarkers: async (_lib, _media, season) => {
          calls.push(["fetch", _lib, season]);
          if (season === 2 && season2Gate) {
            await season2Gate.promise;
          }
          return {
            media_id: "media-shared",
            season,
            active: true,
            paused: false,
            job: {
              id: `job-s${season}`,
              kind: "marker_refresh",
              season,
              status: "running",
              total: season === 2 ? 9 : 2,
              completed: 0,
              succeeded: 0,
              failed: 0,
              error: null,
              elapsed_ms: 100,
              phase: "capturing",
              sampling_mode: "full_window",
            },
            episodes: [
              {
                file_id: `file-s${season}-e1`,
                season,
                episode: 1,
                duration_ms: 1200000,
                multi_unit: false,
                marker: null,
                detection: {
                  job_id: `job-s${season}`,
                  stage: "fingerprint",
                  status: "running",
                  error: null,
                  intro: null,
                  outro: null,
                },
              },
            ],
          };
        },
        detectItemMarkers: async (_lib, _media, body) => {
          calls.push(["detect", _lib, body]);
          return { queued: true, already_running: false, skipped_multi_unit: 0, job: null };
        },
        saveManualMarker: async () => ({}),
        deleteManualMarker: async () => ({ deleted: true }),
      },
      "@/lib/api/probe": {
        cancelFingerprintQueue: async (opts) => {
          calls.push(["cancel", opts]);
          return { cancelled_units: 1, aborted_running: 1 };
        },
      },
    },
  );

  let renderer;
  t.after(async () => {
    season2Gate?.resolve();
    if (renderer) await act(async () => renderer.unmount());
  });

  const renderPanel = (props) =>
    React.createElement(MarkerControlPanel, {
      libraryId: "lib-a",
      mediaItemId: "media-shared",
      seasons: [0, 1, 2],
      selectedSeason: 1,
      selectedEpisode: 1,
      selectedFileId: "file-s1-e1",
      ...props,
    });

  await act(async () => {
    renderer = create(renderPanel({}));
  });

  const selector = () => renderer.root.findByProps({ "aria-label": "检测季选择" });
  assert.equal(selector().props.value, 1);

  // Item 5: manual season selection must NOT snap back to selectedSeason=1.
  await act(async () => {
    selector().props.onChange({ target: { value: "2" } });
  });
  assert.equal(selector().props.value, 2);

  // Re-rendering with a new array reference having the same seasons must preserve manual selection.
  await act(async () => {
    renderer.update(renderPanel({ seasons: [0, 1, 2] }));
  });
  assert.equal(selector().props.value, 2);
  assert.equal(
    renderer.root.findAllByType("button").some((b) => collectText(b).includes("检测第 1 集")),
    false,
    "single-episode button from season 1 must hide while viewing season 2",
  );

  // Season detect + season cancel + row cancel must carry season=2 and library_id="lib-a".
  const detectSeasonBtn = renderer.root
    .findAllByType("button")
    .find((b) => collectText(b) === "检测本季");
  await act(async () => {
    await detectSeasonBtn.props.onClick();
  });
  assert.deepEqual(
    calls.find((c) => c[0] === "detect"),
    ["detect", "lib-a", { season: 2, mode: "reuse_valid" }],
  );

  const stopSeasonBtn = renderer.root
    .findAllByType("button")
    .find((b) => collectText(b) === "取消当前检测");
  await act(async () => {
    await stopSeasonBtn.props.onClick();
  });
  const rowStopBtn = renderer.root
    .findAllByType("button")
    .find((b) => collectText(b) === "停止");
  await act(async () => {
    await rowStopBtn.props.onClick();
  });
  const cancelCalls = calls.filter((c) => c[0] === "cancel").map((c) => c[1]);
  assert.deepEqual(cancelCalls[0], {
    library_id: "lib-a",
    media_id: "media-shared",
    season: 2,
    include_running: true,
  });
  assert.deepEqual(cancelCalls[1], {
    library_id: "lib-a",
    media_id: "media-shared",
    season: 2,
    file_id: "file-s2-e1",
    include_running: true,
  });

  // External chapter switch to season 0 (特别篇) must sync panel to 0.
  await act(async () => {
    renderer.update(renderPanel({ selectedSeason: 0, selectedEpisode: 1 }));
  });
  assert.equal(selector().props.value, 0);

  // Out-of-order GET: slow season 2 response resolving after switching back to 0 must not overwrite 0.
  season2Gate = deferred();
  await act(async () => {
    selector().props.onChange({ target: { value: "2" } });
  });
  await act(async () => {
    selector().props.onChange({ target: { value: "0" } });
  });
  await act(async () => {
    season2Gate.resolve();
    await Promise.resolve();
  });
  season2Gate = null;
  assert.equal(selector().props.value, 0);
  const afterStaleResolve = collectText(renderer.toJSON());
  assert.match(afterStaleResolve, /进度：0\/2/);
  assert.doesNotMatch(afterStaleResolve, /进度：0\/9/);

  // Shrinking seasons list to exclude manual choice must fall back cleanly; empty list keeps current.
  await act(async () => {
    selector().props.onChange({ target: { value: "2" } });
  });
  assert.equal(selector().props.value, 2);
  await act(async () => {
    renderer.update(renderPanel({ selectedSeason: 0, seasons: [0, 1] }));
  });
  assert.equal(selector().props.value, 1);
  await act(async () => {
    renderer.update(renderPanel({ selectedSeason: 0, seasons: [] }));
  });
  assert.equal(
    renderer.root.findAllByProps({ "aria-label": "检测季选择" }).length,
    0,
    "empty seasons list hides season dropdown",
  );
  const detectCountBefore = calls.filter((c) => c[0] === "detect").length;
  await act(async () => {
    await renderer.root
      .findAllByType("button")
      .find((b) => collectText(b) === "检测本季")
      .props.onClick();
  });
  const detectCalls = calls.filter((c) => c[0] === "detect");
  assert.equal(detectCalls.length, detectCountBefore + 1);
  assert.deepEqual(detectCalls.at(-1), ["detect", "lib-a", { season: 1, mode: "reuse_valid" }]);
});

test("MarkerControlPanel isolates async ops across season switch and unmount, and validates detect receipt message", async (t) => {
  const calls = [];
  let s1DetectGate = null;
  let s2FetchGate = null;
  let s1SaveGate = null;
  let detectReceipt = { queued: true, already_running: false, skipped_multi_unit: 0, job: null };
  let onChangedCount = 0;

  const makeDiag = (season) => ({
    media_id: "media-race",
    season,
    active: false,
    paused: false,
    job: null,
    episodes: [
      {
        file_id: `file-s${season}-e1`,
        season,
        episode: 1,
        duration_ms: 1200000,
        multi_unit: false,
        marker: {
          intro_start_ms: season * 10000,
          intro_end_ms: season * 10000 + 30000,
          outro_start_ms: null,
          outro_end_ms: null,
          source: "manual",
          locked: true,
        },
        detection: null,
      },
    ],
  });

  const { MarkerControlPanel } = loadTsModule(
    "../components/library-item/marker-control-panel.tsx",
    {
      "@/lib/api/libraries": {
        fetchSeasonMarkers: async (_lib, _media, season) => {
          calls.push(["fetch", season]);
          if (season === 2 && s2FetchGate) {
            await s2FetchGate.promise;
          }
          return makeDiag(season);
        },
        detectItemMarkers: async (_lib, _media, body) => {
          calls.push(["detect", body]);
          if (body.season === 1 && s1DetectGate) {
            await s1DetectGate.promise;
          }
          return detectReceipt;
        },
        saveManualMarker: async (_lib, _media, body) => {
          calls.push(["save", body]);
          if (s1SaveGate) {
            await s1SaveGate.promise;
          }
          return {};
        },
        deleteManualMarker: async (_lib, _media, body) => {
          calls.push(["delete", body]);
          return { deleted: true };
        },
      },
      "@/lib/api/probe": {
        cancelFingerprintQueue: async () => ({ cancelled_units: 0, aborted_running: 0 }),
      },
    },
  );

  let renderer;
  t.after(async () => {
    s1DetectGate?.resolve();
    s2FetchGate?.resolve();
    s1SaveGate?.resolve();
    if (renderer) await act(async () => renderer.unmount());
  });

  await act(async () => {
    renderer = create(
      React.createElement(MarkerControlPanel, {
        libraryId: "lib-race",
        mediaItemId: "media-race",
        seasons: [1, 2],
        selectedSeason: 1,
        onChanged: () => {
          onChangedCount += 1;
        },
      }),
    );
  });

  const selector = () => renderer.root.findByProps({ "aria-label": "检测季选择" });
  assert.match(collectText(renderer.toJSON()), /00:10–00:40/);

  // 1. Start deferred S1 detect, switch to S2 (with S2 fetch pending), verify S1 rows vanish immediately.
  s1DetectGate = deferred();
  s2FetchGate = deferred();
  let s1DetectPromise;
  await act(async () => {
    const detectBtn = renderer.root
      .findAllByType("button")
      .find((b) => collectText(b) === "检测本季");
    s1DetectPromise = detectBtn.props.onClick();
    await Promise.resolve();
  });
  await act(async () => {
    selector().props.onChange({ target: { value: "2" } });
    await Promise.resolve();
  });
  assert.equal(selector().props.value, 2);
  assert.doesNotMatch(
    collectText(renderer.toJSON()),
    /00:10–00:40/,
    "switching to season 2 must clear season 1 episode rows while season 2 loads",
  );

  // Resolve S2 fetch, then let late S1 detect complete -> S2 must keep file-s2-e1 & not fire stale onChanged.
  await act(async () => {
    s2FetchGate.resolve();
    await Promise.resolve();
  });
  s2FetchGate = null;
  assert.match(collectText(renderer.toJSON()), /00:20–00:50/);

  const changedBeforeLateResolve = onChangedCount;
  await act(async () => {
    s1DetectGate.resolve();
    await s1DetectPromise;
    await Promise.resolve();
  });
  s1DetectGate = null;
  assert.equal(selector().props.value, 2);
  assert.match(collectText(renderer.toJSON()), /00:20–00:50/);
  assert.doesNotMatch(collectText(renderer.toJSON()), /00:10–00:40/);
  assert.equal(
    onChangedCount,
    changedBeforeLateResolve,
    "late season 1 detect completion must not trigger onChanged after switching to season 2",
  );

  // Clicking 清除 while viewing S2 must target file-s2-e1 (season 2), never S1.
  const delBtn = renderer.root
    .findAllByType("button")
    .find((b) => collectText(b) === "清除");
  await act(async () => {
    await delBtn.props.onClick();
  });
  assert.deepEqual(calls.filter((c) => c[0] === "delete").at(-1), [
    "delete",
    { season: 2, episode: 1, file_id: "file-s2-e1" },
  ]);

  // 2. Non-queued / non-running receipt must NOT claim "已提交".
  detectReceipt = { queued: false, already_running: false, skipped_multi_unit: 0, job: null };
  await act(async () => {
    const detectBtn = renderer.root
      .findAllByType("button")
      .find((b) => collectText(b) === "检测本季");
    await detectBtn.props.onClick();
  });
  assert.doesNotMatch(
    collectText(renderer.toJSON()),
    /已提交/,
    "queued=false, already_running=false must not claim task was submitted",
  );

  // 3. Unmounting during in-flight save must not invoke onChanged.
  s1SaveGate = deferred();
  const editBtn = renderer.root
    .findAllByType("button")
    .find((b) => collectText(b) === "编辑/锁定");
  await act(async () => {
    editBtn.props.onClick();
  });
  const saveBtn = renderer.root
    .findAllByType("button")
    .find((b) => collectText(b) === "保存锁定");
  let savePromise;
  await act(async () => {
    savePromise = saveBtn.props.onClick();
    await Promise.resolve();
  });
  const changedBeforeUnmount = onChangedCount;
  await act(async () => {
    renderer.unmount();
    renderer = null;
  });
  await act(async () => {
    s1SaveGate.resolve();
    await savePromise;
    await Promise.resolve();
  });
  assert.equal(onChangedCount, changedBeforeUnmount, "unmounted save must not fire onChanged");
});

test("MarkerControlPanel waits for slow diagnostics response before next poll and drops old season response", async (t) => {
  t.mock.timers.enable({ apis: ["setTimeout", "setInterval"] });
  let slowPollGate = null;
  let s1FetchCount = 0;
  let changedCount = 0;

  const makeDiag = (season, active) => ({
    media_id: "media-slow",
    season,
    active,
    paused: false,
    job: {
      id: `job-s${season}`,
      kind: "marker_refresh",
      season,
      status: active ? "running" : "succeeded",
      total: 2,
      completed: active ? 0 : 2,
      succeeded: active ? 0 : 2,
      failed: 0,
      error: null,
      elapsed_ms: 500,
      phase: active ? "capturing" : null,
      sampling_mode: "full_window",
    },
    episodes: [],
  });

  const { MarkerControlPanel } = loadTsModule(
    "../components/library-item/marker-control-panel.tsx",
    {
      "@/lib/api/libraries": {
        fetchSeasonMarkers: async (_lib, _media, season, signal) => {
          if (season === 1) {
            s1FetchCount += 1;
            if (s1FetchCount === 1) return makeDiag(1, true);
            slowPollGate = deferred();
            if (signal) {
              signal.addEventListener("abort", () => {
                slowPollGate?.reject(new Error("aborted"));
              });
            }
            return slowPollGate.promise;
          }
          return makeDiag(season, false);
        },
        detectItemMarkers: async () => ({
          queued: true,
          already_running: false,
          skipped_multi_unit: 0,
          job: null,
        }),
        saveManualMarker: async () => ({}),
        deleteManualMarker: async () => ({ deleted: true }),
      },
      "@/lib/api/probe": {
        cancelFingerprintQueue: async () => ({ cancelled_units: 0, aborted_running: 0 }),
      },
    },
  );

  let renderer;
  t.after(async () => {
    slowPollGate?.resolve(makeDiag(1, false));
    if (renderer) await act(async () => renderer.unmount());
  });

  await act(async () => {
    renderer = create(
      React.createElement(MarkerControlPanel, {
        libraryId: "lib-slow",
        mediaItemId: "media-slow",
        seasons: [1, 2],
        selectedSeason: 1,
        onChanged: () => {
          changedCount += 1;
        },
      }),
    );
  });
  assert.match(collectText(renderer.toJSON()), /进行中/);
  assert.equal(s1FetchCount, 1);

  // Advance past 3 polling intervals (6000ms) while the 2nd GET is still in flight:
  // single-flight polling must NOT abort and re-issue a 3rd GET before the 2nd settles.
  await act(async () => {
    t.mock.timers.tick(2000);
    await Promise.resolve();
  });
  assert.equal(s1FetchCount, 2);
  await act(async () => {
    t.mock.timers.tick(2000);
    t.mock.timers.tick(2000);
    await Promise.resolve();
  });
  assert.equal(
    s1FetchCount,
    2,
    "slow in-flight diagnostics poll must not be aborted and re-fired before settling",
  );

  // Now resolve the slow poll with active=false -> panel must leave 进行中 and notify onChanged once.
  await act(async () => {
    slowPollGate.resolve(makeDiag(1, false));
    await Promise.resolve();
  });
  assert.equal(changedCount, 1);
  assert.doesNotMatch(collectText(renderer.toJSON()), /进行中/);

  // Control: switching season during a slow poll drops old season result cleanly.
  s1FetchCount = 0;
  await act(async () => {
    renderer.root
      .findByProps({ "aria-label": "检测季选择" })
      .props.onChange({ target: { value: "2" } });
    await Promise.resolve();
  });
  await act(async () => {
    renderer.root
      .findByProps({ "aria-label": "检测季选择" })
      .props.onChange({ target: { value: "1" } });
    await Promise.resolve();
  });
  await act(async () => {
    t.mock.timers.tick(2000);
    await Promise.resolve();
  });
  const staleGate = slowPollGate;
  await act(async () => {
    renderer.root
      .findByProps({ "aria-label": "检测季选择" })
      .props.onChange({ target: { value: "2" } });
    await Promise.resolve();
  });
  await act(async () => {
    staleGate?.resolve(makeDiag(1, false));
    await Promise.resolve();
  });
  assert.equal(renderer.root.findByProps({ "aria-label": "检测季选择" }).props.value, 2);
  assert.equal(changedCount, 1, "stale season 1 response after switch must not call onChanged");
});
