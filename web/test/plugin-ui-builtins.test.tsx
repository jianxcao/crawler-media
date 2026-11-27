import "./plugin-ui-env";

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { pathToFileURL } from "node:url";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import React from "react";
import { MemoryRouter } from "react-router-dom";

import { PluginSurfaceHost } from "../components/plugins/PluginSurfaceHost";
import type { SessionView } from "../lib/api/auth";
import type { PluginUiContract, UiDescriptorView } from "../lib/api/plugins";
import { loadFederatedSurface } from "../lib/plugins/federation-loader";
import { SessionProvider } from "../lib/session";

const REPO_ROOT = path.resolve(import.meta.dirname, "../..");

const ADMIN_SESSION: SessionView = {
  id: "user-admin-1",
  username: "admin",
  nickname: "Admin",
  avatar_url: null,
  role: "admin",
  capabilities: { allow_subscribe: true, allow_search: true, allow_direct_download: true },
};

const CONTRACT: PluginUiContract = {
  contract: "1.0.0",
  react: "19.3.0",
  react_dom: "19.3.0",
  plugin_ui_sdk: "1.0.0",
  plugin_ui_kit: "1.0.0",
  federation_runtime_version: "0.10",
  sdk_methods: [],
};

function makeLocalLoader(uiDistDir: string) {
  return (descriptor: UiDescriptorView, expose: string) => {
    const entryUrl = pathToFileURL(path.join(uiDistDir, "ui/assets/remoteEntry.js")).href;
    return loadFederatedSurface(
      { ...descriptor, entry_url: entryUrl },
      expose,
      {
        fetchManifest: async () => [],
        importRemote: (url) => import(url),
      },
    );
  };
}

function makeCheckinDescriptor(): UiDescriptorView {
  return {
    plugin_id: "builtin.checkin",
    plugin_name: "Site Check-in Orchestrator",
    version: "1.0.0",
    generation: 1,
    lifecycle: "enabled",
    source: "builtin",
    package_hash: "builtin:checkin:1.0.0",
    package_sha256: "builtin:checkin:1.0.0",
    bundle_sha256: "builtin-checkin-sha",
    subject_user_id: "user-admin-1",
    subject_role: "admin",
    ui_mode: "federation-react",
    remote_name: "cmBuiltinCheckin",
    entry: "ui/assets/remoteEntry.js",
    entry_url: "/api/v1/plugin-ui-assets/builtin.checkin/builtin:checkin:1.0.0/ui/assets/remoteEntry.js",
    asset_base_url: "/api/v1/plugin-ui-assets/builtin.checkin/builtin:checkin:1.0.0",
    contract: "1.0.0",
    react: "19.3.0",
    react_dom: "19.3.0",
    ui_trusted: true,
    trust: { status: "trusted", revision: 1, granted_at: null, granted_by_user_id: null, revoked_at: null },
    capabilities: ["read:sites", "read:history", "command:site.maintenance", "task:run:check-in"],
    surfaces: [
      { id: "settings", kind: "config", expose: "./Config", title: "Check-in Settings", icon: "sliders", location: null, mountable: true, editable: true, required_role: "admin" },
      { id: "main", kind: "page", expose: "./Page", title: "Site Check-in", icon: "calendar-check", location: "/settings/plugins/builtin.checkin?tab=page&view=main", mountable: true, editable: false, required_role: "admin" },
      { id: "app", kind: "app", expose: "./App", title: "Check-in Studio", icon: "calendar-check", location: "/plugin-apps/builtin.checkin/app", mountable: true, editable: false, required_role: "admin" },
      { id: "summary", kind: "dashboard", expose: "./Dashboard", title: "Check-in Status", icon: "activity", location: "/dashboard", mountable: true, editable: false, required_role: "admin" },
    ],
  };
}

function makeBoostDescriptor(): UiDescriptorView {
  return {
    plugin_id: "builtin.boost",
    plugin_name: "Automated Ratio Boost",
    version: "1.0.0",
    generation: 1,
    lifecycle: "enabled",
    source: "builtin",
    package_hash: "builtin:boost:1.0.0",
    package_sha256: "builtin:boost:1.0.0",
    bundle_sha256: "builtin-boost-sha",
    subject_user_id: "user-admin-1",
    subject_role: "admin",
    ui_mode: "federation-react",
    remote_name: "cmBuiltinBoost",
    entry: "ui/assets/remoteEntry.js",
    entry_url: "/api/v1/plugin-ui-assets/builtin.boost/builtin:boost:1.0.0/ui/assets/remoteEntry.js",
    asset_base_url: "/api/v1/plugin-ui-assets/builtin.boost/builtin:boost:1.0.0",
    contract: "1.0.0",
    react: "19.3.0",
    react_dom: "19.3.0",
    ui_trusted: true,
    trust: { status: "trusted", revision: 1, granted_at: null, granted_by_user_id: null, revoked_at: null },
    capabilities: [
      "read:sites",
      "read:downloads",
      "read:boost",
      "command:downloader.delete-task",
      "command:downloader.prepare-cleanup",
      "command:downloader.delete-files",
      "task:run:traffic-cycle",
    ],
    surfaces: [
      { id: "settings", kind: "config", expose: "./Config", title: "Boost Settings", icon: "sliders", location: null, mountable: true, editable: true, required_role: "admin" },
      { id: "main", kind: "page", expose: "./Page", title: "Boost Console", icon: "gauge", location: "/settings/plugins/builtin.boost?tab=page&view=main", mountable: true, editable: false, required_role: "admin" },
      { id: "app", kind: "app", expose: "./App", title: "Boost Manager", icon: "gauge", location: "/plugin-apps/builtin.boost/app", mountable: true, editable: false, required_role: "admin" },
      { id: "summary", kind: "dashboard", expose: "./Dashboard", title: "Boost Telemetry", icon: "activity", location: "/dashboard", mountable: true, editable: false, required_role: "admin" },
    ],
  };
}

function createBuiltinMockFetch(
  submittedCommands: Array<{ kind: string; payload: unknown; ifMatch: string | null }>,
): typeof fetch {
  return (async (input: string | URL | Request, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    const headers = new Headers(init?.headers);
    const body = typeof init?.body === "string" ? JSON.parse(init.body) : null;

    if (url.includes("/ui/query")) {
      if (body?.name === "sites.list") {
        return new Response(
          JSON.stringify({ ok: true, data: { items: [{ id: "site-1", name: "PT Alpha", capabilities: ["maintenance"] }] } }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (body?.name === "downloaders.list") {
        return new Response(
          JSON.stringify({ ok: true, data: { items: [{ id: "dl-1", name: "qBit Main" }] } }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (body?.name === "checkin.receipts") {
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              items: [{ receipt_id: "r-1", site_id: "site-1", state: "completed", outcome: "completed", evidence: "ok", updated_at: "2026-11-23T01:00:00Z" }],
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (body?.name === "boost.snapshot") {
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              revision: 4,
              used_bytes: 2048,
              reserved_bytes: 1024,
              retained_file_bytes: 512,
              reservations: [],
              tasks: [
                {
                  handle: "tf-1",
                  task_id: "tf-1",
                  quota_key: "q1",
                  downloader_id: "dl-1",
                  site_id: "site-1",
                  info_hash: "abcdef1234567890",
                  state: "seeding",
                  task_bytes: 2048,
                  retained_bytes: 512,
                  uploaded_bytes: 4096,
                  protection_state: "eligible",
                  transfer_active: false,
                  library_referenced: false,
                  observed_at: 1000,
                  revision: 2,
                },
              ],
              disk: { available_bytes: 100000, observed_at: 1000, status: "available" },
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (body?.name === "handles.resolve") {
        // Return entity revision "1" while plugin.revision on backend is 4
        return new Response(
          JSON.stringify({ ok: true, data: { handle: "uh1.tok.sig", revision: "1", expires_at: "2026-11-23T02:00:00Z" } }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
    }
    if (url.includes("/browser-availability")) {
      return new Response(
        JSON.stringify({ ok: true, data: { site_id: "site-1", purpose: "maintenance", status: "ready", missing_requirements: [] } }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.includes("/brush-cleanup-preview")) {
      return new Response(
        JSON.stringify({
          ok: true,
          data: {
            plan_handle: "plan-1",
            revision: 2,
            confirmation_token: "tok-confirm-1",
            total_retained_bytes: 512,
            tasks: [{ paths: ["/downloads/file.mkv"] }],
          },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.includes("/commands")) {
      submittedCommands.push({
        kind: body?.kind,
        payload: body?.payload,
        ifMatch: headers.get("If-Match"),
      });
      return new Response(
        JSON.stringify({
          ok: true,
          data: { operation_id: "op-live-1", state: "succeeded", result: { plan_handle: "plan-1" } },
        }),
        { status: 202, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.includes("/plugin-operations/")) {
      return new Response(
        JSON.stringify({
          ok: true,
          data: {
            operation_id: "op-live-1",
            state: "succeeded",
            revision: 1,
            phase: "done",
            result: { plan_handle: "plan-1" },
          },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    return new Response(JSON.stringify({ ok: true, data: {} }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  }) as typeof fetch;
}

test("built-in checkin federation bundle mounts all 4 surfaces and omits entity If-Match on site.maintenance", async () => {
  const checkinUiRoot = path.join(REPO_ROOT, "examples/plugins/checkin/ui");
  const manifestPath = path.join(checkinUiRoot, "asset-manifest.json");
  assert.ok(fs.existsSync(manifestPath), `missing ${manifestPath}`);
  const parsed = JSON.parse(fs.readFileSync(manifestPath, "utf8")) as {
    version: number;
    files: Array<{ path: string }>;
  };
  assert.equal(parsed.version, 1);
  assert.ok(parsed.files.some((f) => f.path === "ui/assets/remoteEntry.js"));
  for (const f of parsed.files) {
    assert.equal(f.path.endsWith(".html") || f.path.endsWith(".map"), false);
  }

  const submittedCommands: Array<{ kind: string; payload: unknown; ifMatch: string | null }> = [];
  const origFetch = globalThis.fetch;
  globalThis.fetch = createBuiltinMockFetch(submittedCommands);

  try {
    const checkinLoader = makeLocalLoader(path.join(checkinUiRoot, "ui-dist"));
    const checkinDesc = makeCheckinDescriptor();

    render(
      <MemoryRouter>
        <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
          <PluginSurfaceHost
            pluginId="builtin.checkin"
            surfaceId="main"
            deps={{
              fetchDescriptor: async () => checkinDesc,
              fetchContract: async () => CONTRACT,
              loadSurface: checkinLoader,
              theme: "silver",
              notifyToast: () => {},
              openConfirm: async () => true,
            }}
          />
        </SessionProvider>
      </MemoryRouter>,
    );

    await waitFor(() => assert.ok(screen.getByText("站点签到控制台")), { timeout: 5000 });
    await waitFor(() => assert.ok(screen.getByText("PT Alpha")), { timeout: 5000 });
    assert.equal(submittedCommands.length, 0, "Check-in must NOT auto-run on mount");
    fireEvent.click(screen.getByRole("button", { name: "立即签到" }));
    await waitFor(() => assert.equal(submittedCommands.length, 1));
    assert.equal(submittedCommands[0].kind, "site.maintenance");
    // site.revision is "1" while plugin.revision is 4 -> must NOT send If-Match: "1"
    assert.equal(submittedCommands[0].ifMatch, null);
    cleanup();

    const checkinExpected: Record<string, string> = {
      settings: "站点自动签到设置",
      app: "Site Check-in Studio",
      summary: "站点签到状态",
    };
    for (const sid of ["settings", "app", "summary"]) {
      render(
        <MemoryRouter>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
            <PluginSurfaceHost
              pluginId="builtin.checkin"
              surfaceId={sid}
              config={
                sid === "settings"
                  ? {
                      values: { interval_secs: 86400 },
                      schemaRevision: "1",
                      revision: "1",
                      editable: true,
                      onDraftChange: () => {},
                      save: async () => {},
                    }
                  : undefined
              }
              deps={{
                fetchDescriptor: async () => checkinDesc,
                fetchContract: async () => CONTRACT,
                loadSurface: checkinLoader,
                theme: "paper",
                notifyToast: () => {},
                openConfirm: async () => true,
              }}
            />
          </SessionProvider>
        </MemoryRouter>,
      );
      await waitFor(() => assert.ok(screen.getByText(checkinExpected[sid])));
      if (sid === "summary") {
        await waitFor(() => assert.ok(screen.getByText(/已接管 1 个站点/)));
      }
      cleanup();
    }
  } finally {
    cleanup();
    globalThis.fetch = origFetch;
  }
});

test("built-in boost federation bundle mounts all 4 surfaces and preserves config defaults & disk available tone", async () => {
  const boostUiRoot = path.join(REPO_ROOT, "examples/plugins/boost/ui");
  const submittedCommands: Array<{ kind: string; payload: unknown; ifMatch: string | null }> = [];
  const savedConfigs: Array<Record<string, unknown>> = [];
  const origFetch = globalThis.fetch;
  globalThis.fetch = createBuiltinMockFetch(submittedCommands);

  try {
    const boostDesc = makeBoostDescriptor();
    const boostLoader = makeLocalLoader(path.join(boostUiRoot, "ui-dist"));
    const boostExpected: Record<string, string> = {
      main: "自动刷流控制台",
      settings: "自动刷流策略与配额设置",
      app: "Boost Manager",
      summary: "自动刷流遥测",
    };
    for (const sid of ["main", "settings", "app", "summary"]) {
      render(
        <MemoryRouter>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
            <PluginSurfaceHost
              pluginId="builtin.boost"
              surfaceId={sid}
              config={
                sid === "settings"
                  ? {
                      values: {
                        budget_bytes: 1024 ** 3,
                        hold_days: 7,
                        downloader_id: "dl-1",
                        max_adds_per_round: 85,
                        min_seeders: 7,
                        enforce_budget_on_all: true,
                      },
                      schemaRevision: "1",
                      revision: "1",
                      editable: true,
                      onDraftChange: () => {},
                      save: async (vals) => {
                        savedConfigs.push(vals);
                      },
                    }
                  : undefined
              }
              deps={{
                fetchDescriptor: async () => boostDesc,
                fetchContract: async () => CONTRACT,
                loadSurface: boostLoader,
                theme: "netflix",
                notifyToast: () => {},
                openConfirm: async () => true,
              }}
            />
          </SessionProvider>
        </MemoryRouter>,
      );
      await waitFor(() => assert.ok(screen.getByText(boostExpected[sid])), { timeout: 5000 });
      if (sid === "settings") {
        fireEvent.click(screen.getByRole("button", { name: "保存刷流配置" }));
        await waitFor(() => assert.equal(savedConfigs.length, 1));
        assert.equal(savedConfigs[0].downloader_id, "dl-1");
        assert.equal(savedConfigs[0].max_adds_per_round, 85);
        assert.equal(savedConfigs[0].min_seeders, 7);
        assert.equal(savedConfigs[0].enforce_budget_on_all, true);
      } else if (sid === "main") {
        await waitFor(() => assert.ok(screen.getByText(/abcdef123456/)));
        const diskBadge = screen.getByText("磁盘: available");
        assert.equal(diskBadge.getAttribute("data-tone"), "ok");
        const before = submittedCommands.length;
        fireEvent.click(screen.getByRole("button", { name: "清理文件" }));
        await waitFor(() => assert.equal(submittedCommands.length, before + 2));
        assert.equal(submittedCommands[before].kind, "downloader.prepare-cleanup");
        assert.equal(submittedCommands[before + 1].kind, "downloader.delete-files");
      } else if (sid === "summary") {
        await waitFor(() => assert.ok(screen.getByText(/托管任务 1 个/)));
        const summaryDisk = screen.getByText("磁盘 available");
        assert.equal(summaryDisk.getAttribute("data-tone"), "ok");
      }
      cleanup();
    }
  } finally {
    cleanup();
    globalThis.fetch = origFetch;
  }
});

test("built-in checkin Page renders role=alert error instead of empty-sites placeholder when initial sites.list fails", async () => {
  const checkinUiRoot = path.join(REPO_ROOT, "examples/plugins/checkin/ui");
  const origFetch = globalThis.fetch;
  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    const body = typeof init?.body === "string" ? JSON.parse(init.body) : null;
    if (url.includes("/ui/query") && body?.name === "sites.list") {
      return new Response(
        JSON.stringify({
          ok: false,
          error: { code: "plugin.capability_denied", message: "缺少 read:sites 权限" },
        }),
        { status: 403, headers: { "Content-Type": "application/json" } },
      );
    }
    return new Response(JSON.stringify({ ok: true, data: {} }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  }) as typeof fetch;

  try {
    const checkinLoader = makeLocalLoader(path.join(checkinUiRoot, "ui-dist"));
    const checkinDesc = makeCheckinDescriptor();

    render(
      <MemoryRouter>
        <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-err-1" }}>
          <PluginSurfaceHost
            pluginId="builtin.checkin"
            surfaceId="main"
            deps={{
              fetchDescriptor: async () => checkinDesc,
              fetchContract: async () => CONTRACT,
              loadSurface: checkinLoader,
              theme: "silver",
              notifyToast: () => {},
              openConfirm: async () => true,
            }}
          />
        </SessionProvider>
      </MemoryRouter>,
    );

    const alertEl = await waitFor(() => screen.getByRole("alert"), { timeout: 5000 });
    assert.match(alertEl.textContent ?? "", /缺少 read:sites 权限/);
    assert.equal(screen.queryByText("尚未配置 PT 站点"), null);
  } finally {
    cleanup();
    globalThis.fetch = origFetch;
  }
});
