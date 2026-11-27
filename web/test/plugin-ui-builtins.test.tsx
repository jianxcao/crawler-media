import "./plugin-ui-env";

import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test, { after, before } from "node:test";
import { pathToFileURL } from "node:url";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import React from "react";
import { MemoryRouter } from "react-router-dom";

import { PluginSurfaceHost } from "../components/plugins/PluginSurfaceHost";
import type { SessionView } from "../lib/api/auth";
import type { PluginUiContract, UiDescriptorView } from "../lib/api/plugins";
import { clearFederationCache, loadFederatedSurface } from "../lib/plugins/federation-loader";
import { SessionProvider } from "../lib/session";

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

interface BuiltPluginUiRemote {
  distRoot: string;
  assetManifestPath: string;
}

function buildTestPluginUiRemote(
  workDir: string,
  remoteName: string,
  sources: Record<"Page.tsx" | "Config.tsx" | "App.tsx" | "Dashboard.tsx", string>,
): BuiltPluginUiRemote {
  const remoteDir = path.join(workDir, remoteName);
  const srcDir = path.join(remoteDir, "src");
  fs.mkdirSync(srcDir, { recursive: true });
  fs.writeFileSync(path.join(remoteDir, "package.json"), JSON.stringify({ name: remoteName, private: true, type: "module" }));
  fs.writeFileSync(path.join(srcDir, "virtual-entry.ts"), "export {};\n");
  for (const [name, code] of Object.entries(sources)) {
    fs.writeFileSync(path.join(srcDir, name), code);
  }

  const webRoot = path.resolve(import.meta.dirname, "..");
  const viteBin = path.join(webRoot, "node_modules/vite/bin/vite.js");
  const viteDist = pathToFileURL(path.join(webRoot, "node_modules/vite/dist/node/index.js")).href;
  const reactPluginPath = pathToFileURL(path.join(webRoot, "node_modules/@vitejs/plugin-react/dist/index.js")).href;
  const mfPluginPath = pathToFileURL(path.join(webRoot, "node_modules/@module-federation/vite/lib/index.js")).href;

  fs.writeFileSync(
    path.join(remoteDir, "vite.config.mjs"),
    `import { defineConfig } from ${JSON.stringify(viteDist)};
import react from ${JSON.stringify(reactPluginPath)};
import { federation } from ${JSON.stringify(mfPluginPath)};

export default defineConfig({
  root: ${JSON.stringify(remoteDir)},
  logLevel: "silent",
  resolve: {
    alias: {
      react: ${JSON.stringify(path.join(webRoot, "node_modules/react"))},
      "react-dom": ${JSON.stringify(path.join(webRoot, "node_modules/react-dom"))},
      "@crawler-media/plugin-ui-sdk": ${JSON.stringify(path.join(webRoot, "../sdk/plugin-ui/src/index.ts"))},
      "@crawler-media/plugin-ui-kit": ${JSON.stringify(path.join(webRoot, "../sdk/plugin-ui-kit/src/index.tsx"))},
    },
  },
  plugins: [
    react(),
    federation({
      name: ${JSON.stringify(remoteName)},
      filename: "assets/remoteEntry.js",
      exposes: {
        "./Page": "./src/Page.tsx",
        "./Config": "./src/Config.tsx",
        "./App": "./src/App.tsx",
        "./Dashboard": "./src/Dashboard.tsx",
      },
      dts: false,
      manifest: false,
      shared: {
        react: { singleton: true, requiredVersion: "19.3.0", import: false },
        "react-dom": { singleton: true, requiredVersion: "19.3.0", import: false },
        "react/jsx-runtime": { singleton: true, requiredVersion: "19.3.0", import: false },
        "@crawler-media/plugin-ui-sdk": { singleton: true, requiredVersion: "1.0.0", import: false },
        "@crawler-media/plugin-ui-kit": { singleton: true, requiredVersion: "1.0.0", import: false },
      },
    }),
  ],
  build: {
    outDir: "ui",
    emptyOutDir: true,
    target: "es2022",
    minify: false,
    sourcemap: false,
    rollupOptions: { input: "./src/virtual-entry.ts" },
  },
});
`,
  );

  execFileSync(process.execPath, [viteBin, "build", "--config", "vite.config.mjs"], {
    cwd: remoteDir,
    stdio: "pipe",
  });

  const uiOutDir = path.join(remoteDir, "ui");
  const relFiles: string[] = [];
  const walk = (dir: string) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) walk(full);
      else {
        const rel = `ui/${path.relative(uiOutDir, full).replaceAll("\\", "/")}`;
        if (!rel.endsWith(".html") && !rel.endsWith(".map")) relFiles.push(rel);
      }
    }
  };
  walk(uiOutDir);

  const manifestFiles = relFiles
    .filter((rel) => rel !== "ui/asset-manifest.json")
    .map((rel) => {
      const bytes = fs.readFileSync(path.join(remoteDir, rel));
      return {
        path: rel,
        sha256: createHash("sha256").update(bytes).digest("hex"),
        size: bytes.length,
        content_type: rel.endsWith(".css") ? "text/css" : "application/javascript",
      };
    });
  const assetManifestPath = path.join(uiOutDir, "asset-manifest.json");
  fs.writeFileSync(assetManifestPath, JSON.stringify({ version: 1, files: manifestFiles }, null, 2));
  return { distRoot: remoteDir, assetManifestPath };
}

let tmpRoot = "";
let checkinBuilt: BuiltPluginUiRemote;
let boostBuilt: BuiltPluginUiRemote;

before(() => {
  tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "cm-plugin-ui-firstparty-"));
  checkinBuilt = buildTestPluginUiRemote(tmpRoot, "cmCheckin", {
    "Page.tsx": `import * as React from "react";
import type { PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";

export default function CheckinPage({ host }: PluginSurfaceProps) {
  const [sites, setSites] = React.useState<Array<{ id: string; name: string }>>([]);
  const [error, setError] = React.useState<string | null>(null);
  React.useEffect(() => {
    void host.query("sites.list", {})
      .then((res) => setSites([...(res.items ?? [])]))
      .catch((err) => {
        const msg = err && typeof err === "object" && "message" in err ? String((err as { message: unknown }).message) : String(err);
        setError(msg);
      });
  }, [host]);
  const triggerCheckin = async () => {
    const siteId = sites[0]?.id ?? "site-1";
    const resolved = await host.objects.resolve("site", siteId);
    await host.commands.submit(
      "site.maintenance",
      { site_handle: resolved.handle, action: "check-in" },
      { expectedRevision: "1", idempotencyKey: "k-chk-1" },
    );
  };
  return (
    <div>
      <h2>站点签到控制台</h2>
      {error ? <div role="alert">{error}</div> : null}
      {!error && sites.length === 0 ? <p>尚未配置 PT 站点</p> : null}
      {sites.map((s) => <span key={s.id}>{s.name}</span>)}
      <button type="button" onClick={() => void triggerCheckin()}>立即签到</button>
    </div>
  );
}`,
    "Config.tsx": `import * as React from "react";
import type { PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";

export default function CheckinConfig({ config }: PluginSurfaceProps) {
  return (
    <div>
      <h2>站点自动签到设置</h2>
      <button type="button" onClick={() => void config?.save({ ...config.values })}>保存签到配置</button>
    </div>
  );
}`,
    "App.tsx": `import type { PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";
export default function CheckinApp(_props: PluginSurfaceProps) {
  return <div>Site Check-in Studio</div>;
}`,
    "Dashboard.tsx": `import * as React from "react";
import type { PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";

export default function CheckinDashboard({ host }: PluginSurfaceProps) {
  const [count, setCount] = React.useState(0);
  React.useEffect(() => {
    void host.query("sites.list", {}).then((res) => setCount((res.items ?? []).length));
  }, [host]);
  return (
    <div>
      <h3>站点签到状态</h3>
      <p>已接管 {count} 个站点</p>
    </div>
  );
}`,
  });

  boostBuilt = buildTestPluginUiRemote(tmpRoot, "cmBoost", {
    "Page.tsx": `import * as React from "react";
import type { PluginSurfaceProps, UiHandle } from "@crawler-media/plugin-ui-sdk";

interface BoostSnap {
  tasks: Array<{ handle: string; info_hash: string }>;
  disk: { status: string };
}

export default function BoostPage({ host }: PluginSurfaceProps) {
  const [snap, setSnap] = React.useState<BoostSnap | null>(null);
  React.useEffect(() => {
    void host.query("boost.snapshot", {}).then((res) => setSnap(res as unknown as BoostSnap));
  }, [host]);
  const handleCleanup = async () => {
    await host.commands.submit(
      "downloader.prepare-cleanup",
      { owned_task_handles: ["tf-1" as UiHandle], expected_revision: "4", reason: "cleanup" },
      { expectedRevision: "4", idempotencyKey: "k-prep-1" },
    );
    await host.commands.submit(
      "downloader.delete-files",
      { host_file_plan_handle: "plan-1" as UiHandle, revision: "4", reason: "cleanup", confirmation_token: "tok-confirm-1" },
      { expectedRevision: "4", idempotencyKey: "k-del-1" },
    );
  };
  const diskStatus = snap?.disk?.status ?? "unknown";
  return (
    <div>
      <h2>自动刷流控制台</h2>
      <span data-tone={diskStatus === "available" ? "ok" : "warn"}>磁盘: {diskStatus}</span>
      {snap?.tasks.map((t) => <span key={t.handle}>{t.info_hash}</span>)}
      <button type="button" onClick={() => void handleCleanup()}>清理文件</button>
    </div>
  );
}`,
    "Config.tsx": `import * as React from "react";
import type { PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";

export default function BoostConfig({ config }: PluginSurfaceProps) {
  return (
    <div>
      <h2>自动刷流策略与配额设置</h2>
      <button type="button" onClick={() => void config?.save({ ...(config?.values ?? {}) })}>保存刷流配置</button>
    </div>
  );
}`,
    "App.tsx": `import type { PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";
export default function BoostApp(_props: PluginSurfaceProps) {
  return <div>Boost Manager</div>;
}`,
    "Dashboard.tsx": `import * as React from "react";
import type { PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";

export default function BoostDashboard({ host }: PluginSurfaceProps) {
  const [snap, setSnap] = React.useState<{ tasks: unknown[]; disk: { status: string } } | null>(null);
  React.useEffect(() => {
    void host.query("boost.snapshot", {}).then((res) => setSnap(res as unknown as { tasks: unknown[]; disk: { status: string } }));
  }, [host]);
  const diskStatus = snap?.disk?.status ?? "unknown";
  return (
    <div>
      <h3>自动刷流遥测</h3>
      <p>托管任务 {snap?.tasks?.length ?? 0} 个</p>
      <span data-tone={diskStatus === "available" ? "ok" : "warn"}>磁盘 {diskStatus}</span>
    </div>
  );
}`,
  });
});

after(() => {
  clearFederationCache();
  if (tmpRoot && fs.existsSync(tmpRoot)) {
    fs.rmSync(tmpRoot, { recursive: true, force: true });
  }
});

function makeLocalLoader(distRoot: string) {
  return (descriptor: UiDescriptorView, expose: string) => {
    const entryUrl = pathToFileURL(path.join(distRoot, "ui/assets/remoteEntry.js")).href;
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
    plugin_id: "cm.checkin",
    plugin_name: "站点自动签到",
    version: "1.0.0",
    generation: 1,
    lifecycle: "enabled",
    source: "external",
    package_hash: "cm:checkin:1.0.0",
    package_sha256: "cm:checkin:1.0.0",
    bundle_sha256: "cm-checkin-sha",
    subject_user_id: "user-admin-1",
    subject_role: "admin",
    ui_mode: "federation-react",
    remote_name: "cmCheckin",
    entry: "ui/assets/remoteEntry.js",
    entry_url: "/api/v1/plugin-ui-assets/cm.checkin/cm:checkin:1.0.0/ui/assets/remoteEntry.js",
    asset_base_url: "/api/v1/plugin-ui-assets/cm.checkin/cm:checkin:1.0.0",
    contract: "1.0.0",
    react: "19.3.0",
    react_dom: "19.3.0",
    ui_trusted: true,
    trust: { status: "trusted", revision: 1, granted_at: null, granted_by_user_id: null, revoked_at: null },
    capabilities: ["read:sites", "read:history", "command:site.maintenance", "task:run:check-in"],
    surfaces: [
      { id: "settings", kind: "config", expose: "./Config", title: "签到设置", icon: "sliders", location: null, mountable: true, editable: true, required_role: "admin" },
      { id: "main", kind: "page", expose: "./Page", title: "站点签到", icon: "calendar-check", location: "/settings/plugins/cm.checkin?tab=page&view=main", mountable: true, editable: false, required_role: "admin" },
      { id: "app", kind: "app", expose: "./App", title: "签到工作台", icon: "calendar-check", location: "/plugin-apps/cm.checkin/app", mountable: true, editable: false, required_role: "admin" },
      { id: "summary", kind: "dashboard", expose: "./Dashboard", title: "签到概览", icon: "activity", location: "/dashboard", mountable: true, editable: false, required_role: "admin" },
    ],
  };
}

function makeBoostDescriptor(): UiDescriptorView {
  return {
    plugin_id: "cm.boost",
    plugin_name: "自动刷流",
    version: "1.0.0",
    generation: 1,
    lifecycle: "enabled",
    source: "external",
    package_hash: "cm:boost:1.0.0",
    package_sha256: "cm:boost:1.0.0",
    bundle_sha256: "cm-boost-sha",
    subject_user_id: "user-admin-1",
    subject_role: "admin",
    ui_mode: "federation-react",
    remote_name: "cmBoost",
    entry: "ui/assets/remoteEntry.js",
    entry_url: "/api/v1/plugin-ui-assets/cm.boost/cm:boost:1.0.0/ui/assets/remoteEntry.js",
    asset_base_url: "/api/v1/plugin-ui-assets/cm.boost/cm:boost:1.0.0",
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
      { id: "settings", kind: "config", expose: "./Config", title: "刷流设置", icon: "sliders", location: null, mountable: true, editable: true, required_role: "admin" },
      { id: "main", kind: "page", expose: "./Page", title: "自动刷流", icon: "gauge", location: "/settings/plugins/cm.boost?tab=page&view=main", mountable: true, editable: false, required_role: "admin" },
      { id: "app", kind: "app", expose: "./App", title: "刷流工作台", icon: "gauge", location: "/plugin-apps/cm.boost/app", mountable: true, editable: false, required_role: "admin" },
      { id: "summary", kind: "dashboard", expose: "./Dashboard", title: "刷流概览", icon: "activity", location: "/dashboard", mountable: true, editable: false, required_role: "admin" },
    ],
  };
}

function createFirstPartyMockFetch(
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
        return new Response(
          JSON.stringify({ ok: true, data: { handle: "uh1.tok.sig", revision: "1", expires_at: "2026-11-23T02:00:00Z" } }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
    }
    if (url.includes("/commands")) {
      submittedCommands.push({ kind: body?.kind, payload: body?.payload, ifMatch: headers.get("If-Match") });
      return new Response(
        JSON.stringify({ ok: true, data: { operation_id: "op-live-1", state: "succeeded", result: { plan_handle: "plan-1" } } }),
        { status: 202, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.includes("/plugin-operations/")) {
      return new Response(
        JSON.stringify({ ok: true, data: { operation_id: "op-live-1", state: "succeeded", revision: 1, phase: "done", result: { plan_handle: "plan-1" } } }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    return new Response(JSON.stringify({ ok: true, data: {} }), { status: 200, headers: { "Content-Type": "application/json" } });
  }) as typeof fetch;
}

test("cm.checkin federation bundle mounts all 4 surfaces and omits entity If-Match on site.maintenance", async () => {
  const parsed = JSON.parse(fs.readFileSync(checkinBuilt.assetManifestPath, "utf8")) as {
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
  globalThis.fetch = createFirstPartyMockFetch(submittedCommands);

  try {
    const checkinLoader = makeLocalLoader(checkinBuilt.distRoot);
    const checkinDesc = makeCheckinDescriptor();

    render(
      <MemoryRouter>
        <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
          <PluginSurfaceHost
            pluginId="cm.checkin"
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
              pluginId="cm.checkin"
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

test("cm.boost federation bundle mounts all 4 surfaces and preserves config defaults & disk available tone", async () => {
  const submittedCommands: Array<{ kind: string; payload: unknown; ifMatch: string | null }> = [];
  const savedConfigs: Array<Record<string, unknown>> = [];
  const origFetch = globalThis.fetch;
  globalThis.fetch = createFirstPartyMockFetch(submittedCommands);

  try {
    const boostDesc = makeBoostDescriptor();
    const boostLoader = makeLocalLoader(boostBuilt.distRoot);
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
              pluginId="cm.boost"
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

test("cm.checkin Page renders role=alert error instead of empty-sites placeholder when initial sites.list fails", async () => {
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
    return new Response(JSON.stringify({ ok: true, data: {} }), { status: 200, headers: { "Content-Type": "application/json" } });
  }) as typeof fetch;

  try {
    const checkinLoader = makeLocalLoader(checkinBuilt.distRoot);
    const checkinDesc = makeCheckinDescriptor();

    render(
      <MemoryRouter>
        <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-err-1" }}>
          <PluginSurfaceHost
            pluginId="cm.checkin"
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
