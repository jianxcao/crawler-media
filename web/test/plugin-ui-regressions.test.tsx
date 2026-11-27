import "./plugin-ui-env";

import assert from "node:assert/strict";
import test from "node:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import React from "react";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";

import MediaDetailPage from "../app/(app)/media/[type]/[id]/page";
import { FeedbackProvider } from "../components/feedback";
import { PluginDetailView } from "../components/plugins/PluginDetailView";
import { PluginManagerView } from "../components/plugins/PluginManagerView";
import { PluginSurfaceHost } from "../components/plugins/PluginSurfaceHost";
import type { SessionView } from "../lib/api/auth";
import {
  ensureOperationSucceeded,
  type PluginUiContract,
  type UiDescriptorView,
} from "../lib/api/plugins";
import {
  clearFederationCache,
  isRemoteLoaded,
  loadFederatedSurface,
  setDocumentReloadHandlerForTest,
} from "../lib/plugins/federation-loader";
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

function makeDescriptor(overrides: Partial<UiDescriptorView> = {}): UiDescriptorView {
  return {
    plugin_id: "org.example.ext",
    plugin_name: "Ext Plugin",
    version: "1.0.0",
    generation: 1,
    lifecycle: "enabled",
    source: "external",
    package_hash: "pkg-ext-1",
    package_sha256: "pkg-ext-1",
    bundle_sha256: "bundle-ext-1",
    subject_user_id: "user-admin-1",
    subject_role: "admin",
    ui_mode: "federation-react",
    remote_name: "extRemote",
    entry: "ui/assets/remoteEntry.js",
    entry_url: "/api/v1/plugin-ui-assets/org.example.ext/pkg-ext-1/ui/assets/remoteEntry.js",
    asset_base_url: "/api/v1/plugin-ui-assets/org.example.ext/pkg-ext-1",
    contract: "1.0.0",
    react: "19.3.0",
    react_dom: "19.3.0",
    ui_trusted: true,
    trust: {
      status: "trusted",
      revision: 1,
      granted_at: "2026-11-23T00:00:00Z",
      granted_by_user_id: "user-admin-1",
      revoked_at: null,
    },
    capabilities: ["read:sites"],
    styles: ["ui/assets/ext.css"],
    surfaces: [
      {
        id: "main",
        kind: "page",
        expose: "./Page",
        title: "Ext Main",
        icon: null,
        location: "/settings/plugins/org.example.ext?tab=page&view=main",
        mountable: true,
        editable: false,
        required_role: "admin",
      },
    ],
    ...overrides,
  };
}

function LocationProbe() {
  const loc = useLocation();
  return <div data-testid="loc">{loc.pathname}</div>;
}

test("MediaDetailPage resolves /media/item/:id across libraries and falls back to NotFound when missing", async () => {
  const origFetch = globalThis.fetch;
  globalThis.fetch = (async (input: string | URL | Request) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    if (url.endsWith("/libraries")) {
      return new Response(
        JSON.stringify({
          ok: true,
          data: [
            { id: "lib-1", name: "Movies", kind: "movie", root_paths: ["/m"], is_default: true },
            { id: "lib-2", name: "Shows", kind: "tv", root_paths: ["/tv"], is_default: true },
          ],
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.endsWith("/libraries/lib-2/items/item-found")) {
      return new Response(
        JSON.stringify({
          ok: true,
          data: { media_item_id: "item-found", kind: "tv", title: "Andor", files: [] },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    return new Response(
      JSON.stringify({ ok: false, error: { code: "not_found", message: "missing" } }),
      { status: 404, headers: { "Content-Type": "application/json" } },
    );
  }) as typeof fetch;

  try {
    render(
      <MemoryRouter initialEntries={["/media/item/item-found"]}>
        <Routes>
          <Route path="/media/:type/:id" element={<MediaDetailPage />} />
          <Route path="/library/:id/item/:mediaItemId" element={<LocationProbe />} />
        </Routes>
      </MemoryRouter>,
    );
    await waitFor(() =>
      assert.equal(screen.getByTestId("loc").textContent, "/library/lib-2/item/item-found"),
    );
    cleanup();

    render(
      <MemoryRouter initialEntries={["/media/item/item-missing"]}>
        <Routes>
          <Route path="/media/:type/:id" element={<MediaDetailPage />} />
          <Route path="/library/:id/item/:mediaItemId" element={<LocationProbe />} />
        </Routes>
      </MemoryRouter>,
    );
    await waitFor(() => assert.ok(screen.getByText("页面不存在或已被移动")));
  } finally {
    globalThis.fetch = origFetch;
    cleanup();
  }
});

test("ensureOperationSucceeded, PluginManagerView, and PluginDetailView surface failed/rejected operations instead of false success", async () => {
  assert.throws(
    () =>
      ensureOperationSucceeded({
        operation_id: "op-f",
        plugin_id: "p1",
        kind: "enable",
        state: "rejected",
        phase: "preflight",
        revision: 1,
        generation: 1,
        job_id: null,
        progress: 0,
        blocked_reason: "缺少必需权限",
        error: null,
        result: null,
        next_run_at: null,
        retry_after_ms: null,
        created_at: "",
        updated_at: "",
      }),
    /缺少必需权限/,
  );

  const origFetch = globalThis.fetch;
  const seenCalls: Array<{ url: string; method: string; ifMatch: string | null }> = [];
  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    const method = init?.method ?? "GET";
    const headers = new Headers(init?.headers);
    seenCalls.push({ url, method, ifMatch: headers.get("If-Match") });

    if (url.includes("/plugins?limit=100")) {
      return new Response(
        JSON.stringify({
          ok: true,
          data: [
            {
              id: "org.example.ext",
              name: "Ext Plugin",
              version: "1.0.0",
              package_hash: "pkg-ext-1",
              source: "external",
              revision: 1,
              config_revision: 1,
              generation: 1,
              enabled: false,
              lifecycle: "disabled",
              health: "healthy",
              required_grants: [],
              effective_grants: [],
              active_operation_id: null,
              failure_reason: null,
            },
          ],
          next_cursor: null,
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.endsWith("/plugins/org.example.ext") && method === "GET") {
      return new Response(
        JSON.stringify({
          ok: true,
          data: {
            id: "org.example.ext",
            name: "Ext Plugin",
            version: "1.0.0",
            package_hash: "pkg-ext-1",
            source: "external",
            signing_key_id: null,
            revision: 1,
            config_revision: 1,
            generation: 1,
            enabled: false,
            lifecycle: "disabled",
            health: "healthy",
            required_grants: [],
            effective_grants: [],
            active_operation_id: null,
            failure_reason: null,
            state_schema: 1,
            credential_bound: false,
            task_count: 1,
            version_count: 1,
          },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.includes("/plugins/org.example.ext/tasks?limit=50")) {
      return new Response(
        JSON.stringify({
          ok: true,
          data: [
            {
              id: "sync-task",
              binding_id: "b-1",
              plugin_id: "org.example.ext",
              definition_revision: 1,
              enabled: false,
              schedule: { interval_secs: 3600 },
              config: {},
              last_job: null,
              next_run_after: null,
              ownership_summary: "plugin",
            },
          ],
          next_cursor: null,
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.endsWith("/enable") || url.endsWith("/uninstall") || url.endsWith("/tasks/sync-task/update")) {
      return new Response(
        JSON.stringify({
          ok: true,
          data: {
            operation_id: "op-fail-1",
            plugin_id: "org.example.ext",
            kind: "lifecycle",
            state: "failed",
            phase: "drain",
            revision: 1,
            generation: 1,
            job_id: null,
            progress: 0,
            blocked_reason: null,
            error: "后台操作执行失败",
            result: null,
            next_run_at: null,
            retry_after_ms: null,
            created_at: "",
            updated_at: "",
          },
        }),
        { status: 202, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.includes("/plugin-operations/op-fail-1")) {
      return new Response(
        JSON.stringify({
          ok: true,
          data: {
            operation_id: "op-fail-1",
            plugin_id: "org.example.ext",
            kind: "lifecycle",
            state: "failed",
            phase: "drain",
            revision: 1,
            generation: 1,
            job_id: null,
            progress: 0,
            blocked_reason: null,
            error: "后台操作执行失败",
            result: null,
            next_run_at: null,
            retry_after_ms: null,
            created_at: "",
            updated_at: "",
          },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    return new Response(JSON.stringify({ ok: true, data: [], next_cursor: null }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  }) as typeof fetch;

  try {
    // 1. PluginManagerView enable failure -> error toast, no success toast
    render(
      <MemoryRouter>
        <FeedbackProvider>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
            <PluginManagerView />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );
    const enableBtn = await waitFor(() => screen.getByRole("button", { name: "启用" }));
    fireEvent.click(enableBtn);
    await waitFor(() => assert.ok(screen.getByText("后台操作执行失败")));
    assert.equal(screen.queryByText(/已启用 Ext Plugin/), null);
    cleanup();

    // 2. PluginDetailView failed uninstall must NOT navigate away, failed task auto-enable must NOT call /run
    render(
      <MemoryRouter initialEntries={["/settings/plugins/org.example.ext?tab=tasks"]}>
        <FeedbackProvider>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
            <Routes>
              <Route
                path="/settings/plugins/:pluginId"
                element={<PluginDetailView pluginId="org.example.ext" />}
              />
              <Route path="/settings/plugins" element={<div>PLUGIN_LIST_ROUTE</div>} />
            </Routes>
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );

    const runBtn = await waitFor(() => screen.getByRole("button", { name: "立即运行" }));
    fireEvent.click(runBtn);
    await waitFor(() => assert.ok(screen.getByText("后台操作执行失败")));
    assert.equal(
      seenCalls.some((c) => c.url.endsWith("/tasks/sync-task/run")),
      false,
      "Failed task auto-enable must not proceed to /run with bumped defRev",
    );

    fireEvent.click(screen.getByRole("button", { name: "卸载" }));
    const confirmUninstall = await waitFor(() =>
      screen.getAllByRole("button", { name: "卸载" }).at(-1)!,
    );
    fireEvent.click(confirmUninstall);
    await waitFor(() => assert.equal(screen.queryByText("PLUGIN_LIST_ROUTE"), null));
    assert.equal(screen.queryByText("插件状态已更新"), null);
  } finally {
    globalThis.fetch = origFetch;
    cleanup();
  }
});

test("PluginSurfaceHost evicts stylesheet link when ui_trusted becomes false and PluginDetailView evicts & reloads on uninstall", async () => {
  clearFederationCache();
  const origFetch = globalThis.fetch;
  let reloadPath: string | undefined;
  let reloadCount = 0;
  setDocumentReloadHandlerForTest((target) => {
    reloadCount += 1;
    reloadPath = target;
  });

  try {
    const desc = makeDescriptor();
    await loadFederatedSurface(desc, "./Page", {
      importRemote: async () => ({
        init: async () => {},
        get: async () => () => ({ default: () => <div>Loaded Ext</div> }),
      }),
    });
    assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 1);

    // 1. PluginSurfaceHost seeing ui_trusted: false evicts <link data-cm-plugin-ui>
    render(
      <MemoryRouter>
        <FeedbackProvider>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
            <PluginSurfaceHost
              pluginId="org.example.ext"
              surfaceId="main"
              deps={{
                fetchDescriptor: async () => makeDescriptor({ ui_trusted: false }),
                fetchContract: async () => CONTRACT,
              }}
            />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );
    await waitFor(() => assert.ok(screen.getByText(/尚未信任/)));
    assert.equal(
      document.head.querySelectorAll("link[data-cm-plugin-ui]").length,
      0,
      "Stylesheet must be evicted when ui_trusted is false",
    );
    cleanup();

    // Re-mount stylesheet & mark remote loaded, then uninstall via PluginDetailView
    await loadFederatedSurface(desc, "./Page", {
      importRemote: async () => ({
        init: async () => {},
        get: async () => () => ({ default: () => null }),
      }),
    });
    assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 1);
    assert.equal(isRemoteLoaded("extRemote"), true);

    globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
      const method = init?.method ?? "GET";
      if (url.endsWith("/plugins/org.example.ext") && method === "GET") {
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              id: "org.example.ext",
              name: "Ext Plugin",
              version: "1.0.0",
              package_hash: "pkg-ext-1",
              source: "external",
              signing_key_id: null,
              revision: 1,
              config_revision: 1,
              generation: 1,
              enabled: false,
              lifecycle: "disabled",
              health: "healthy",
              required_grants: [],
              effective_grants: [],
              active_operation_id: null,
              failure_reason: null,
              state_schema: 1,
              credential_bound: false,
              task_count: 0,
              version_count: 1,
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (url.endsWith("/plugins/org.example.ext/ui")) {
        return new Response(JSON.stringify({ ok: true, data: desc }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        });
      }
      if (url.endsWith("/uninstall") || url.includes("/plugin-operations/")) {
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              operation_id: "op-un-1",
              plugin_id: "org.example.ext",
              kind: "uninstall",
              state: "succeeded",
              phase: "done",
              revision: 2,
              generation: 1,
              job_id: null,
              progress: 100,
              blocked_reason: null,
              error: null,
              result: null,
              next_run_at: null,
              retry_after_ms: null,
              created_at: "",
              updated_at: "",
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      return new Response(JSON.stringify({ ok: true, data: [], next_cursor: null }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }) as typeof fetch;

    render(
      <MemoryRouter initialEntries={["/settings/plugins/org.example.ext"]}>
        <FeedbackProvider>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
            <Routes>
              <Route
                path="/settings/plugins/:pluginId"
                element={<PluginDetailView pluginId="org.example.ext" />}
              />
              <Route path="/settings/plugins" element={<LocationProbe />} />
            </Routes>
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );

    const uninstallBtn = await waitFor(() => screen.getByRole("button", { name: "卸载" }));
    fireEvent.click(uninstallBtn);
    const confirmBtn = await waitFor(() => screen.getAllByRole("button", { name: "卸载" }).at(-1)!);
    fireEvent.click(confirmBtn);

    await waitFor(() => assert.equal(reloadCount, 1));
    assert.equal(reloadPath, "/settings/plugins");
    assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 0);
    assert.equal(isRemoteLoaded("extRemote"), false);
  } finally {
    setDocumentReloadHandlerForTest(null);
    clearFederationCache();
    globalThis.fetch = origFetch;
    cleanup();
  }
});

test("PluginDetailView refreshes PluginConfigHost editable & revision when disabling plugin on config tab", async () => {
  const origFetch = globalThis.fetch;
  let enabled = true;
  let lifecycle = "enabled";
  let rev = 4;

  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    const method = init?.method ?? "GET";
    if (url.endsWith("/plugins/org.example.ext") && method === "GET") {
      return new Response(
        JSON.stringify({
          ok: true,
          data: {
            id: "org.example.ext",
            name: "Ext Plugin",
            version: "1.0.0",
            package_hash: "pkg-ext-1",
            source: "external",
            signing_key_id: null,
            revision: rev,
            config_revision: 1,
            generation: 1,
            enabled,
            lifecycle,
            health: "healthy",
            required_grants: [],
            effective_grants: [],
            active_operation_id: null,
            failure_reason: null,
            state_schema: 1,
            credential_bound: false,
            task_count: 0,
            version_count: 1,
          },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.endsWith("/plugins/org.example.ext/ui")) {
      return new Response(
        JSON.stringify({ ok: false, error: { code: "plugin.no_ui", message: "headless" } }),
        { status: 404, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.endsWith("/config-schema")) {
      return new Response(
        JSON.stringify({
          ok: true,
          data: {
            plugin_id: "org.example.ext",
            schema_revision: "s1",
            schema: { type: "object", properties: { endpoint: { type: "string", default: "" } } },
          },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.endsWith("/config")) {
      return new Response(
        JSON.stringify({
          ok: true,
          data: {
            plugin_id: "org.example.ext",
            revision: rev,
            config_revision: 1,
            schema_revision: "s1",
            values: { endpoint: "https://example.test" },
            secrets: {},
          },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.endsWith("/disable")) {
      enabled = false;
      lifecycle = "disabled";
      rev = 5;
      return new Response(
        JSON.stringify({
          ok: true,
          data: {
            operation_id: "op-dis-1",
            plugin_id: "org.example.ext",
            kind: "disable",
            state: "succeeded",
            phase: "done",
            revision: 5,
            generation: 1,
            job_id: null,
            progress: 100,
            blocked_reason: null,
            error: null,
            result: null,
            next_run_at: null,
            retry_after_ms: null,
            created_at: "",
            updated_at: "",
          },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    return new Response(JSON.stringify({ ok: true, data: [], next_cursor: null }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  }) as typeof fetch;

  try {
    render(
      <MemoryRouter initialEntries={["/settings/plugins/org.example.ext?tab=config"]}>
        <FeedbackProvider>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
            <PluginDetailView pluginId="org.example.ext" />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );

    await waitFor(() => assert.ok(screen.getByText(/配置版本 #4/)));
    assert.ok(screen.getByText("当前状态只读"));
    assert.equal((screen.getByLabelText(/endpoint/) as HTMLInputElement).disabled, true);

    fireEvent.click(screen.getByRole("button", { name: "停用插件" }));
    await waitFor(() => assert.ok(screen.getByText(/配置版本 #5/)));
    assert.equal(screen.queryByText("当前状态只读"), null);
    assert.equal((screen.getByLabelText(/endpoint/) as HTMLInputElement).disabled, false);
  } finally {
    globalThis.fetch = origFetch;
    cleanup();
  }
});
