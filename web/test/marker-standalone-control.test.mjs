import assert from "node:assert/strict";
import { existsSync, readFileSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import React, { act } from "react";
import { create } from "react-test-renderer";
import ts from "typescript";

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

test("FingerprintSettingsCard controls standalone fingerprint workers, pause/resume, and cancel", async () => {
  const calls = [];
  const { FingerprintSettingsCard } = loadTsModule(
    "../components/fingerprint-settings-card.tsx",
    {
      "@/lib/api/probe": {
        getFingerprintSettings: async () => ({ workers: 1, paused: false }),
        saveFingerprintSettings: async (next) => {
          calls.push(["saveSettings", next]);
          return next;
        },
        getFingerprintQueueStatus: async () => ({
          paused: false,
          background_paused: false,
          remote_reads_paused: false,
          workers: 1,
          active_workers: 1,
          queued_fingerprint: 4,
          running_fingerprint: 1,
          queued_analysis: 0,
        }),
        pauseFingerprintQueue: async () => {
          calls.push(["pause"]);
          return {
            paused: true,
            background_paused: false,
            remote_reads_paused: false,
            workers: 1,
            active_workers: 0,
            queued_fingerprint: 4,
            running_fingerprint: 0,
            queued_analysis: 0,
          };
        },
        resumeFingerprintQueue: async () => {
          calls.push(["resume"]);
          return {
            paused: false,
            background_paused: false,
            remote_reads_paused: false,
            workers: 1,
            active_workers: 1,
            queued_fingerprint: 4,
            running_fingerprint: 1,
            queued_analysis: 0,
          };
        },
        cancelFingerprintQueue: async (opts) => {
          calls.push(["cancel", opts]);
          return { cancelled_units: 4, aborted_running: 1 };
        },
      },
    },
  );

  let renderer;
  await act(async () => {
    renderer = create(React.createElement(FingerprintSettingsCard));
  });

  const workerInput = renderer.root.findByProps({ "aria-label": "声纹检测并发" });
  assert.equal(workerInput.props.value, 1);
  await act(async () => {
    workerInput.props.onChange({ target: { value: "2" } });
  });
  const saveBtn = renderer.root
    .findAllByType("button")
    .find((b) => collectText(b) === "保存声纹设置");
  await act(async () => {
    await saveBtn.props.onClick();
  });
  assert.deepEqual(calls[0], ["saveSettings", { workers: 2, paused: false }]);

  const pauseBtn = renderer.root
    .findAllByType("button")
    .find((b) => collectText(b) === "暂停声纹队列");
  await act(async () => {
    await pauseBtn.props.onClick();
  });
  assert.deepEqual(calls[1], ["pause"]);

  const cancelBtn = renderer.root
    .findAllByType("button")
    .find((b) => collectText(b) === "终止声纹任务");
  await act(async () => {
    await cancelBtn.props.onClick();
  });
  assert.deepEqual(calls[2], ["cancel", { include_running: true }]);

  await act(async () => renderer.unmount());
});

test("MarkerControlPanel triggers season/episode detection, cancels running work, and saves manual locked markers", async () => {
  const calls = [];
  const sampleDiagnostics = {
    season: 1,
    active: false,
    job: {
      id: "job-1",
      kind: "marker_refresh",
      season: 1,
      status: "succeeded",
      total: 2,
      completed: 2,
      succeeded: 2,
      failed: 0,
      error: null,
      elapsed_ms: 1200,
      phase: "completed",
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
          intro_start_ms: 15000,
          intro_end_ms: 105000,
          outro_start_ms: null,
          outro_end_ms: null,
          source: "fingerprint",
          locked: false,
          updated_at: 1000,
        },
        detection: {
          job_id: "job-1",
          stage: "analysis",
          status: "succeeded",
          error: null,
          intro: {
            status: "verified",
            decision: "verified",
            reason: null,
            reason_label: "已验证",
          },
          outro: {
            status: "uncertain",
            decision: "uncertain",
            reason: "insufficient_reference_matches",
            reason_label: "参考集数不足",
          },
        },
      },
    ],
  };

  const { MarkerControlPanel } = loadTsModule(
    "../components/library-item/marker-control-panel.tsx",
    {
      "@/lib/api/libraries": {
        fetchSeasonMarkers: async () => sampleDiagnostics,
        detectItemMarkers: async (_lib, _media, body) => {
          calls.push(["detect", body]);
          return { queued: true, skipped_multi_unit: 0 };
        },
        saveManualMarker: async (_lib, _media, body) => {
          calls.push(["saveManual", body]);
          return { ...body, source: "manual" };
        },
        deleteManualMarker: async (_lib, _media, target) => {
          calls.push(["deleteManual", target]);
        },
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
  await act(async () => {
    renderer = create(
      React.createElement(MarkerControlPanel, {
        libraryId: "lib-1",
        mediaItemId: "media-1",
        seasons: [1, 2],
        selectedSeason: 1,
        selectedEpisode: 1,
        selectedFileId: "file-1",
      }),
    );
  });

  const text = collectText(renderer.toJSON());
  assert.match(text, /参考集数不足/);
  assert.match(text, /00:15/);

  const detectSeasonBtn = renderer.root
    .findAllByType("button")
    .find((b) => collectText(b) === "检测本季");
  await act(async () => {
    await detectSeasonBtn.props.onClick();
  });
  assert.deepEqual(calls[0], [
    "detect",
    { season: 1, mode: "reuse_valid" },
  ]);

  const editBtn = renderer.root
    .findAllByType("button")
    .find((b) => collectText(b) === "编辑/锁定");
  await act(async () => {
    editBtn.props.onClick();
  });
  const saveManualBtn = renderer.root
    .findAllByType("button")
    .find((b) => collectText(b) === "保存锁定");
  await act(async () => {
    await saveManualBtn.props.onClick();
  });
  assert.equal(calls[1][0], "saveManual");
  assert.equal(calls[1][1].locked, true);

  await act(async () => renderer.unmount());
});
