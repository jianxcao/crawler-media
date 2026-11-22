import assert from "node:assert/strict";
import { existsSync, readFileSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import React, { act } from "react";
import { create } from "react-test-renderer";
import ts from "typescript";

import {
  activityScopeFromQuery,
  TASK_CENTER_VIEWS,
  taskCenterViewFromQuery,
} from "../lib/task-center.ts";

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

const PAUSED_STATUS = {
  paused: true,
  remote_reads_paused: false,
  queued_metadata: 120,
  running_metadata: 0,
  queued_fingerprint: 0,
  running_fingerprint: 0,
  pending_obligations: 8110,
  total_pending: 8230,
  libraries: [
    {
      library_id: "lib-1",
      library_name: "国产剧",
      queued_count: 120,
      running_count: 0,
      obligation_count: 8110,
      total_pending: 8230,
    },
  ],
};

const RUNNING_STATUS = {
  ...PAUSED_STATUS,
  paused: false,
  running_metadata: 1,
  queued_metadata: 119,
};

test("/jobs redirects to /activity?view=scheduled and primary nav drops duplicate jobs item", () => {
  assert.deepEqual(TASK_CENTER_VIEWS, [
    "all",
    "active",
    "attention",
    "scheduled",
    "history",
  ]);
  assert.equal(activityScopeFromQuery("scheduled"), "tasks");
  assert.equal(taskCenterViewFromQuery("scheduled"), "scheduled");

  const mainSource = readFileSync(new URL("../src/main.tsx", import.meta.url), "utf8");
  assert.match(
    mainSource,
    /<Route\s+path="jobs"\s+element=\{<Navigate\s+to="\/activity\?view=scheduled"\s+replace\s*\/>\}\s*\/>/,
  );

  const sidebarSource = readFileSync(
    new URL("../components/sidebar.tsx", import.meta.url),
    "utf8",
  );
  assert.doesNotMatch(sidebarSource, /id:\s*"jobs"/);
  assert.doesNotMatch(sidebarSource, /定时任务/);

  const topNavSource = readFileSync(
    new URL("../components/netflix/top-nav.tsx", import.meta.url),
    "utf8",
  );
  assert.doesNotMatch(topNavSource, /label="定时任务"/);

  const myPageSource = readFileSync(
    new URL("../components/netflix/my-page.tsx", import.meta.url),
    "utf8",
  );
  assert.doesNotMatch(myPageSource, /label="定时任务"/);
});

test("probeQueueHasWork and probeQueueCountsAsActive distinguish paused backlog from active probing", () => {
  const { probeQueueHasWork, probeQueueCountsAsActive, taskActivityBadge } = loadTsModule(
    "../lib/task-activity.ts",
    {
      "@/lib/http": { request: async () => ({ data: PAUSED_STATUS }) },
      "@/lib/session": { useSession: () => ({ session: { role: "admin" } }) },
      "@/lib/use-visible-polling": { useVisiblePolling: () => undefined },
      "@/lib/jobs": { useJobs: () => ({ jobs: [] }) },
      "@/lib/download-tasks": { useDownloadTasks: () => ({ tasks: [] }) },
    },
  );

  assert.equal(probeQueueHasWork(null), false);
  assert.equal(probeQueueCountsAsActive(null), false);

  assert.equal(probeQueueHasWork(PAUSED_STATUS), true);
  assert.equal(probeQueueCountsAsActive(PAUSED_STATUS), false);

  assert.equal(probeQueueHasWork(RUNNING_STATUS), true);
  assert.equal(probeQueueCountsAsActive(RUNNING_STATUS), true);

  const badgeWhenActive = taskActivityBadge({
    ingestJobsByHash: new Map(),
    attentionDownloadGroups: [],
    activeDownloadGroups: [],
    boostTasks: [],
    standaloneAttentionJobs: [],
    standaloneActiveJobs: [],
    standaloneHistoricalJobs: [],
    probeStatus: RUNNING_STATUS,
    attentionTotal: 0,
    activeTotal: 1,
    historyTotal: 0,
  });
  assert.deepEqual(badgeWhenActive, {
    alert: false,
    count: 1,
    href: "/activity?view=active",
    hint: "1 个进行中",
  });
});

test("ProbeQueueCard renders paused and running states with resume, pause, cancel, and settings link", async () => {
  const { ProbeQueueCard } = loadTsModule("../components/task-center/probe-queue-card.tsx", {
    "react-router-dom": {
      Link: ({ to, children, ...rest }) => React.createElement("a", { href: to, ...rest }, children),
    },
  });

  let actions = [];
  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(ProbeQueueCard, {
        status: PAUSED_STATUS,
        busyAction: null,
        onPause: () => actions.push("pause"),
        onResume: () => actions.push("resume"),
        onCancel: () => actions.push("cancel"),
      }),
    );
  });

  const pausedText = collectText(renderer.toJSON());
  assert.match(pausedText, /已暂停 · 待手动继续/);
  assert.match(pausedText, /后台批量媒体探测已挂起，剩余 8,230 个文件待探测/);
  assert.match(pausedText, /国产剧：8,230 项/);

  const settingsLink = renderer.root.findByType("a");
  assert.equal(settingsLink.props.href, "/settings/scrape");

  const resumeBtn = renderer.root
    .findAllByType("button")
    .find((node) => collectText(node) === "继续执行");
  const cancelBtn = renderer.root
    .findAllByType("button")
    .find((node) => collectText(node) === "清空队列");
  await act(async () => {
    resumeBtn.props.onClick();
    cancelBtn.props.onClick();
  });
  assert.deepEqual(actions, ["resume", "cancel"]);

  actions = [];
  await act(async () => {
    renderer.update(
      React.createElement(ProbeQueueCard, {
        status: RUNNING_STATUS,
        busyAction: null,
        onPause: () => actions.push("pause"),
        onResume: () => actions.push("resume"),
        onCancel: () => actions.push("cancel"),
      }),
    );
  });
  const runningText = collectText(renderer.toJSON());
  assert.match(runningText, /正在探测/);
  assert.match(runningText, /正在执行 1 · 排队 8,229/);
  const pauseBtn = renderer.root
    .findAllByType("button")
    .find((node) => collectText(node) === "暂停");
  await act(async () => {
    pauseBtn.props.onClick();
  });
  assert.deepEqual(actions, ["pause"]);
  await act(async () => {
    renderer.unmount();
  });
});

test("TaskCenterView shows five tabs, renders ProbeQueueCard in active/all, and embeds ScheduledJobsView in scheduled", async () => {
  const { TaskCenterView } = loadTsModule("../components/task-center-view.tsx", {
    "react-router-dom": {
      Link: ({ to, children, ...rest }) => React.createElement("a", { href: to, ...rest }, children),
    },
    "@/components/feedback": {
      useToast: () => ({ success() {}, error() {} }),
    },
    "@/components/job-center": { JobCard: () => null },
    "@/components/scheduled-jobs-view": {
      ScheduledJobsView: ({ embedded }) =>
        React.createElement("div", { "data-embedded": embedded }, "EMBEDDED_SCHEDULED_VIEW"),
    },
    "@/lib/download-tasks": {
      useDownloadTasks: () => ({
        tasks: [],
        loading: false,
        error: null,
        refreshedAt: null,
        refresh() {},
      }),
    },
    "@/lib/jobs": {
      useJobs: () => ({ jobs: [], activeJobs: [], refresh() {}, upsert() {} }),
    },
    "@/lib/task-activity": {
      useTaskActivity: () => ({
        ingestJobsByHash: new Map(),
        attentionDownloadGroups: [],
        activeDownloadGroups: [],
        boostTasks: [],
        standaloneAttentionJobs: [],
        standaloneActiveJobs: [],
        standaloneHistoricalJobs: [],
        probeStatus: PAUSED_STATUS,
        probeQueue: {
          status: PAUSED_STATUS,
          busyAction: null,
          pause: async () => PAUSED_STATUS,
          resume: async () => RUNNING_STATUS,
          cancel: async () => ({ cancelled_units: 120, cleared_obligations: 8110 }),
          refresh() {},
          retainFastPolling: () => () => undefined,
        },
        attentionTotal: 0,
        activeTotal: 0,
        historyTotal: 0,
      }),
    },
  });

  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(TaskCenterView, { view: "active", onViewChange() {} }),
    );
  });
  const activeText = collectText(renderer.toJSON());
  assert.match(activeText, /全部/);
  assert.match(activeText, /进行中/);
  assert.match(activeText, /需要处理/);
  assert.match(activeText, /定时计划/);
  assert.match(activeText, /已结束/);
  assert.match(activeText, /已暂停 · 待手动继续/);
  assert.doesNotMatch(activeText, /当前没有进行中的任务/);

  await act(async () => {
    renderer.update(
      React.createElement(TaskCenterView, { view: "scheduled", onViewChange() {} }),
    );
  });
  const scheduledText = collectText(renderer.toJSON());
  assert.match(scheduledText, /EMBEDDED_SCHEDULED_VIEW/);
  const embeddedNode = renderer.root.findByProps({ "data-embedded": true });
  assert.ok(embeddedNode);
  await act(async () => {
    renderer.unmount();
  });
});

test("ScheduledJobsView embedded mode omits standalone h1 and uses useToast instead of native alert", async () => {
  const scheduledSource = readFileSync(
    new URL("../components/scheduled-jobs-view.tsx", import.meta.url),
    "utf8",
  );
  assert.doesNotMatch(scheduledSource, /\balert\s*\(/);

  const toastErrors = [];
  const { ScheduledJobsView } = loadTsModule("../components/scheduled-jobs-view.tsx", {
    "@/components/feedback": {
      useToast: () => ({
        success() {},
        error: (msg) => toastErrors.push(msg),
      }),
    },
    "@/components/maintenance-job-settings": { MaintenanceJobSettings: () => null },
    "@/lib/api/jobs": {
      isMaintenanceJob: () => false,
      parseMaintenanceReport: () => null,
      listJobs: async () => [
        {
          id: "job-rss",
          kind: "subscribe_rss",
          name: "RSS 轮询",
          friendly_name: "RSS 订阅巡检",
          enabled: true,
          schedule: { interval_secs: 300 },
          next_run_after: Math.floor(Date.now() / 1000) + 120,
          last_status: "succeeded",
          last_finished_at: null,
          last_error: null,
          last_report: null,
          payload: null,
        },
      ],
      runJob: async () => {
        throw new Error("服务维护中");
      },
      toggleJob: async () => ({ id: "job-rss", enabled: false }),
      updateJobSchedule: async () => ({}),
      updateMaintenanceConfig: async () => ({}),
    },
  });

  let renderer;
  await act(async () => {
    renderer = create(React.createElement(ScheduledJobsView, { embedded: true }));
  });
  assert.equal(renderer.root.findAllByType("h1").length, 0);
  const content = collectText(renderer.toJSON());
  assert.match(content, /RSS 订阅巡检/);

  const runBtn = renderer.root
    .findAllByType("button")
    .find((node) => collectText(node) === "立即执行");
  await act(async () => {
    await runBtn.props.onClick();
  });
  assert.equal(toastErrors.length, 1);
  assert.match(toastErrors[0], /服务维护中/);
  await act(async () => {
    renderer.unmount();
  });
});

test("ProbeSettingsCard surfaces a shortcut link to /activity?view=active", async () => {
  const { ProbeSettingsCard } = loadTsModule("../components/probe-settings-card.tsx", {
    "@/lib/api/probe-settings": {
      getProbeSettings: async () => ({
        metadata_workers: 1,
        remote_concurrency: 1,
        remote_cooldown_ms: 500,
      }),
      saveProbeSettings: async (x) => x,
      getRemoteReadsSettings: async () => ({ paused: false }),
      saveRemoteReadsSettings: async (paused) => ({ paused }),
    },
  });
  let renderer;
  await act(async () => {
    renderer = create(React.createElement(ProbeSettingsCard));
  });
  const link = renderer.root.findByType("a");
  assert.equal(link.props.href, "/activity?view=active");
  await act(async () => {
    renderer.unmount();
  });
});

test("ProbeQueueProvider ignores stale in-flight refresh started before pause and queues fresh status read", async () => {
  let resolveFirstStatus;
  let statusCalls = 0;
  const { ProbeQueueProvider, useProbeQueue } = loadTsModule("../lib/task-activity.ts", {
    "@/lib/api/probe": {
      getProbeStatus: () => {
        statusCalls += 1;
        if (statusCalls === 1) {
          return new Promise((resolve) => {
            resolveFirstStatus = resolve;
          });
        }
        return Promise.resolve(PAUSED_STATUS);
      },
      pauseProbeQueue: async () => PAUSED_STATUS,
      resumeProbeQueue: async () => RUNNING_STATUS,
      cancelProbeQueue: async () => ({ cancelled_units: 0, cleared_obligations: 0 }),
    },
    "@/lib/session": { useSession: () => ({ session: { role: "admin" } }) },
    "@/lib/use-visible-polling": {
      useVisiblePolling: (cb, _ms, opts) => {
        React.useEffect(() => {
          if (opts?.leading) cb();
        }, [cb, opts?.leading]);
      },
    },
    "@/lib/jobs": { useJobs: () => ({ jobs: [] }) },
    "@/lib/download-tasks": { useDownloadTasks: () => ({ tasks: [] }) },
  });

  let latestQueue = null;
  function Inspector() {
    latestQueue = useProbeQueue();
    return null;
  }

  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(ProbeQueueProvider, null, React.createElement(Inspector)),
    );
  });

  assert.equal(statusCalls, 1, "Initial leading refresh is in-flight");

  // User pauses queue BEFORE the initial refresh resolves
  await act(async () => {
    await latestQueue.pause();
  });
  assert.equal(latestQueue.status?.paused, true);

  // Now the stale pre-pause refresh resolves with RUNNING_STATUS (paused: false)!
  await act(async () => {
    resolveFirstStatus(RUNNING_STATUS);
  });

  assert.equal(
    latestQueue.status?.paused,
    true,
    "Stale refresh started before pause() must NOT overwrite post-pause state with paused=false",
  );
  assert.equal(
    statusCalls,
    2,
    "Stale refresh must queue a fresh post-mutation getProbeStatus() read",
  );

  await act(async () => {
    renderer.unmount();
  });
});

test("ScheduledJobsView uses useVisiblePolling and silent refresh without flashing '正在刷新…'", async () => {
  let pollCallback = null;
  let pollInterval = null;
  let resolveSecondFetch = null;
  let fetchCount = 0;

  const { ScheduledJobsView } = loadTsModule("../components/scheduled-jobs-view.tsx", {
    "@/components/feedback": {
      useToast: () => ({ success() {}, error() {} }),
    },
    "@/components/maintenance-job-settings": { MaintenanceJobSettings: () => null },
    "@/lib/use-visible-polling": {
      useVisiblePolling: (cb, ms) => {
        pollCallback = cb;
        pollInterval = ms;
      },
    },
    "@/lib/api/jobs": {
      isMaintenanceJob: () => false,
      parseMaintenanceReport: () => null,
      listJobs: () => {
        fetchCount += 1;
        const job = {
          id: "job-rss",
          kind: "subscribe_rss",
          name: "RSS 轮询",
          friendly_name: fetchCount === 1 ? "RSS 巡检 v1" : "RSS 巡检 v2",
          enabled: true,
          schedule: { interval_secs: 300 },
          next_run_after: Math.floor(Date.now() / 1000) + 120,
          last_status: "succeeded",
          last_finished_at: null,
          last_error: null,
          last_report: null,
          payload: null,
        };
        if (fetchCount === 2) {
          return new Promise((resolve) => {
            resolveSecondFetch = () => resolve([job]);
          });
        }
        return Promise.resolve([job]);
      },
      runJob: async () => ({}),
      toggleJob: async () => ({}),
      updateJobSchedule: async () => ({}),
      updateMaintenanceConfig: async () => ({}),
    },
  });

  let renderer;
  await act(async () => {
    renderer = create(React.createElement(ScheduledJobsView, { embedded: true }));
  });
  try {
    assert.equal(pollInterval, 10_000, "ScheduledJobsView must poll via useVisiblePolling(10_000)");
    assert.equal(typeof pollCallback, "function");
    assert.match(collectText(renderer.toJSON()), /RSS 巡检 v1/);

    // Trigger background 10s poll and hold response in-flight: button must NOT flash "正在刷新…" or disable!
    await act(async () => {
      pollCallback();
    });
    assert.equal(fetchCount, 2);
    const refreshBtnDuringSilentPoll = renderer.root
      .findAllByType("button")
      .find((node) => collectText(node).includes("刷新"));
    assert.equal(collectText(refreshBtnDuringSilentPoll), "刷新");
    assert.equal(refreshBtnDuringSilentPoll.props.disabled, false);

    await act(async () => {
      resolveSecondFetch();
    });
    assert.match(collectText(renderer.toJSON()), /RSS 巡检 v2/);
  } finally {
    await act(async () => {
      renderer.unmount();
    });
  }
});

test("NetflixSubscriptionsPage ComingSoonCard key includes statusLabel and episodeLabel to avoid duplicate keys", () => {
  const source = readFileSync(
    new URL("../components/netflix/subscriptions-page.tsx", import.meta.url),
    "utf8",
  );
  assert.match(
    source,
    /key=\{`\$\{group\.subscriptionId\}-\$\{group\.presentation\.statusLabel\}-\$\{group\.episodeLabel\}`\}/,
  );
});
