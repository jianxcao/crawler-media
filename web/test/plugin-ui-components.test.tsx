import "./plugin-ui-env";

import assert from "node:assert/strict";
import test from "node:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import React from "react";
import { MemoryRouter } from "react-router-dom";

import type { PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";
import { FeedbackProvider } from "../components/feedback";
import { PluginAppPage } from "../components/plugins/PluginAppPage";
import { PluginConfigHost } from "../components/plugins/PluginConfigHost";
import { PluginTasksTab } from "../components/plugins/PluginDetailTabs";
import { PluginDetailView } from "../components/plugins/PluginDetailView";
import { PluginSchemaForm } from "../components/plugins/PluginSchemaForm";
import { PluginSurfaceHost } from "../components/plugins/PluginSurfaceHost";
import type { SessionView } from "../lib/api/auth";
import type { PluginUiContract, UiDescriptorView } from "../lib/api/plugins";
import { accessiblePathFor } from "../lib/permissions";
import {
  clearFederationCache,
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
    plugin_id: "org.example.demo",
    plugin_name: "Demo Plugin",
    version: "1.0.0",
    generation: 1,
    lifecycle: "enabled",
    source: "external",
    package_hash: "pkg-1",
    package_sha256: "pkg-1",
    bundle_sha256: "bundle-1",
    subject_user_id: "user-admin-1",
    subject_role: "admin",
    ui_mode: "federation-react",
    remote_name: "demoRemote",
    entry: "ui/assets/remoteEntry.js",
    entry_url: "/api/v1/plugin-ui-assets/org.example.demo/pkg-1/ui/assets/remoteEntry.js",
    asset_base_url: "/api/v1/plugin-ui-assets/org.example.demo/pkg-1",
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
    surfaces: [
      {
        id: "main",
        kind: "page",
        expose: "./Page",
        title: "Demo Main",
        icon: "puzzle",
        location: "/settings/plugins/org.example.demo?tab=page&view=main",
        mountable: true,
        editable: false,
        required_role: "admin",
      },
    ],
    ...overrides,
  };
}

function wrapHost(ui: React.ReactElement, session: SessionView = ADMIN_SESSION) {
  return render(
    <MemoryRouter>
      <FeedbackProvider>
        <SessionProvider value={{ session, setSession: () => {}, loginEpoch: `${session.id}:ep1` }}>
          {ui}
        </SessionProvider>
      </FeedbackProvider>
    </MemoryRouter>,
  );
}

test("PluginSurfaceHost fails closed on untrusted UI, user mismatch, contract mismatch, and disabled surface without importing remote", async () => {
  try {
    let remoteLoaded = 0;
    const cases: Array<{ overrides: Partial<UiDescriptorView>; expected: RegExp | string }> = [
      { overrides: { ui_trusted: false }, expected: /未受信任|尚未信任/ },
      { overrides: { subject_user_id: "other-user" }, expected: "会话身份不匹配" },
      { overrides: { contract: "2.0.0" }, expected: /契约版本|不兼容/ },
      {
        overrides: {
          lifecycle: "disabled",
          surfaces: [
            {
              id: "main",
              kind: "page",
              expose: "./Page",
              title: "Demo Main",
              icon: null,
              location: null,
              mountable: false,
              editable: false,
              required_role: "admin",
            },
          ],
        },
        expected: /未启用|不可挂载/,
      },
    ];

    for (const c of cases) {
      wrapHost(
        <PluginSurfaceHost
          pluginId="org.example.demo"
          surfaceId="main"
          deps={{
            fetchDescriptor: async () => makeDescriptor(c.overrides),
            fetchContract: async () => CONTRACT,
            loadSurface: async () => {
              remoteLoaded += 1;
              return () => null;
            },
            theme: "silver",
            notifyToast: () => {},
            openConfirm: async () => true,
          }}
        />,
      );
      await waitFor(() => assert.ok(screen.getByText(c.expected)));
      assert.equal(remoteLoaded, 0);
      cleanup();
    }
  } finally {
    cleanup();
  }
});

test("PluginSurfaceHost remounts with clean state on generation bump and isolates render crashes in ErrorBoundary", async () => {
  try {
    let gen = 1;
    let shouldThrow = false;
    const RemoteWidget: React.FC<PluginSurfaceProps> = ({ host }) => {
      const [count, setCount] = React.useState(0);
      if (shouldThrow) throw new Error("Remote crash boom");
      return (
        <div>
          <span data-testid="gen-label">gen:{host.context.generation}</span>
          <span data-testid="counter">{count}</span>
          <button type="button" onClick={() => setCount((c) => c + 1)}>
            bump
          </button>
        </div>
      );
    };

    const view = wrapHost(
      <PluginSurfaceHost
        pluginId="org.example.demo"
        surfaceId="main"
        reloadKey="v1"
        deps={{
          fetchDescriptor: async () => makeDescriptor({ generation: gen, bundle_sha256: `b-${gen}` }),
          fetchContract: async () => CONTRACT,
          loadSurface: async () => RemoteWidget,
          theme: "silver",
          notifyToast: () => {},
          openConfirm: async () => true,
        }}
      />,
    );

    await waitFor(() => assert.equal(screen.getByTestId("gen-label").textContent, "gen:1"));
    fireEvent.click(screen.getByRole("button", { name: "bump" }));
    assert.equal(screen.getByTestId("counter").textContent, "1");

    gen = 2;
    view.rerender(
      <MemoryRouter>
        <FeedbackProvider>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "user-admin-1:ep1" }}>
            <PluginSurfaceHost
              pluginId="org.example.demo"
              surfaceId="main"
              reloadKey="v2"
              deps={{
                fetchDescriptor: async () => makeDescriptor({ generation: gen, bundle_sha256: `b-${gen}` }),
                fetchContract: async () => CONTRACT,
                loadSurface: async () => RemoteWidget,
                theme: "silver",
                notifyToast: () => {},
                openConfirm: async () => true,
              }}
            />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );
    await waitFor(() => assert.equal(screen.getByTestId("gen-label").textContent, "gen:2"));
    assert.equal(screen.getByTestId("counter").textContent, "0");

    shouldThrow = true;
    const origError = console.error;
    console.error = () => {};
    try {
      gen = 3;
      view.rerender(
        <MemoryRouter>
          <FeedbackProvider>
            <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "user-admin-1:ep1" }}>
              <PluginSurfaceHost
                pluginId="org.example.demo"
                surfaceId="main"
                reloadKey="v3"
                deps={{
                  fetchDescriptor: async () => makeDescriptor({ generation: gen, bundle_sha256: `b-${gen}` }),
                  fetchContract: async () => CONTRACT,
                  loadSurface: async () => RemoteWidget,
                  theme: "silver",
                  notifyToast: () => {},
                  openConfirm: async () => true,
                }}
              />
            </SessionProvider>
          </FeedbackProvider>
        </MemoryRouter>,
      );
      await waitFor(() => assert.ok(screen.getByText("Remote crash boom")));
    } finally {
      console.error = origError;
    }
  } finally {
    cleanup();
  }
});

test("PluginConfigHost offers fallback button to PluginSchemaForm when Federated Config crashes or fails to load", async () => {
  const origFetch = globalThis.fetch;
  const origError = console.error;
  console.error = () => {};
  globalThis.fetch = (async (input: string | URL | Request) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    if (url.endsWith("/config-schema")) {
      return new Response(
        JSON.stringify({
          ok: true,
          data: {
            plugin_id: "org.example.demo",
            schema_revision: "s1",
            schema: {
              type: "object",
              properties: { greeting: { type: "string", default: "hello" } },
            },
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
            plugin_id: "org.example.demo",
            revision: 1,
            config_revision: 1,
            schema_revision: "s1",
            values: { greeting: "fallback-ok" },
            secrets: {},
          },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    return new Response(JSON.stringify({ ok: true, data: [] }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  }) as typeof fetch;

  try {
    const CrashingConfig: React.FC<PluginSurfaceProps> = () => {
      throw new Error("Federated config exploded");
    };
    const cfgDesc = makeDescriptor({
      lifecycle: "disabled",
      surfaces: [
        {
          id: "settings",
          kind: "config",
          expose: "./Config",
          title: "Settings",
          icon: null,
          location: null,
          mountable: true,
          editable: true,
          required_role: "admin",
        },
      ],
    });
    wrapHost(
      <PluginConfigHost
        pluginId="org.example.demo"
        surfaceDeps={{
          fetchDescriptor: async () => cfgDesc,
          fetchContract: async () => CONTRACT,
          loadSurface: async () => CrashingConfig,
        }}
      />,
    );

    const switchBtn = await waitFor(() =>
      screen.getByRole("button", { name: "切换为基础表单配置" }),
    );
    fireEvent.click(switchBtn);
    const input = (await waitFor(() => screen.getByLabelText(/greeting/))) as HTMLInputElement;
    assert.equal(input.value, "fallback-ok");
  } finally {
    console.error = origError;
    globalThis.fetch = origFetch;
    cleanup();
  }
});

test("PluginDetailView prompts Cookie re-login on 401 and reloads document when revoking trust of loaded remote", async () => {
  const origFetch = globalThis.fetch;
  let uiStatus = 401;
  let reloadTriggered = 0;
  setDocumentReloadHandlerForTest(() => {
    reloadTriggered += 1;
  });

  const desc = makeDescriptor({ plugin_id: "org.example.demo", remote_name: "demoRevokeRemote" });
  await loadFederatedSurface(desc, "./Page", {
    importRemote: async () => ({
      init: async () => {},
      get: async () => () => ({ default: () => null }),
    }),
  });

  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    if (url.endsWith("/plugins/org.example.demo") && (!init?.method || init.method === "GET")) {
      return new Response(
        JSON.stringify({
          ok: true,
          data: {
            id: "org.example.demo",
            name: "Demo Plugin",
            version: "1.0.0",
            package_hash: "pkg-1",
            source: "external",
            signing_key_id: null,
            revision: 2,
            config_revision: 1,
            generation: 1,
            enabled: true,
            lifecycle: "enabled",
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
    if (url.endsWith("/plugins/org.example.demo/ui")) {
      if (uiStatus === 401) {
        return new Response(
          JSON.stringify({ ok: false, error: { code: "auth.unauthorized", message: "missing cookie" } }),
          { status: 401, headers: { "Content-Type": "application/json" } },
        );
      }
      return new Response(JSON.stringify({ ok: true, data: desc }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }
    if (url.endsWith("/ui-trust/revoke")) {
      return new Response(
        JSON.stringify({
          ok: true,
          data: {
            plugin_id: "org.example.demo",
            package_hash: "pkg-1",
            package_sha256: "pkg-1",
            bundle_sha256: "bundle-1",
            status: "revoked",
            ui_trusted: false,
            revision: 2,
            granted_by_user_id: "u1",
            granted_at: "2026-11-24T00:00:00Z",
            revoked_at: "2026-11-24T00:01:00Z",
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
    wrapHost(<PluginDetailView pluginId="org.example.demo" />);
    await waitFor(() => assert.ok(screen.getByText(/Cookie 会话已失效|HTTP 401/)));
    assert.equal(screen.queryByText(/纯 schema 模式插件/), null);
    cleanup();

    uiStatus = 200;
    wrapHost(<PluginDetailView pluginId="org.example.demo" />);
    const revokeBtn = await waitFor(() => screen.getByRole("button", { name: "撤销 UI 信任" }));
    fireEvent.click(revokeBtn);
    const confirmBtn = await waitFor(() => screen.getByRole("button", { name: "撤销信任" }));
    fireEvent.click(confirmBtn);
    await waitFor(() => assert.equal(reloadTriggered, 1));
  } finally {
    setDocumentReloadHandlerForTest(null);
    clearFederationCache();
    globalThis.fetch = origFetch;
    cleanup();
  }
});

test("PluginTasksTab toggles task enable/run and PluginAppPage renders shell actions & tabs", async () => {
  const taskUrls: string[] = [];
  const origFetch = globalThis.fetch;
  globalThis.fetch = (async (input: string | URL | Request) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    taskUrls.push(url);
    return new Response(
      JSON.stringify({ ok: true, data: { operation_id: "op-t1", state: "succeeded" } }),
      { status: 202, headers: { "Content-Type": "application/json" } },
    );
  }) as typeof fetch;

  try {
    render(
      <PluginTasksTab
        pluginId="builtin.checkin"
        tasks={[
          {
            id: "check-in",
            binding_id: "b1",
            plugin_id: "builtin.checkin",
            definition_revision: 1,
            enabled: false,
            schedule: { interval_secs: 86400 },
            config: {},
            last_job: null,
            next_run_after: null,
            ownership_summary: "plugin",
          },
        ]}
        receiptsByTask={{}}
        onReceiptsLoaded={() => {}}
        busyAction={null}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "启用" }));
    await waitFor(() => assert.ok(taskUrls.some((u) => u.endsWith("/tasks/check-in/update"))));
    fireEvent.click(screen.getByRole("button", { name: "立即运行" }));
    await waitFor(() => assert.ok(taskUrls.some((u) => u.endsWith("/tasks/check-in/run"))));
    cleanup();

    let actionClicks = 0;
    let tabClicks = 0;
    const AppWidget: React.FC<PluginSurfaceProps> = ({ host }) => {
      React.useEffect(() => {
        const d1 = host.shell.registerAction({
          id: "sync-now",
          label: "壳层同步",
          onSelect: () => {
            actionClicks += 1;
          },
        });
        const d2 = host.shell.registerTabs([
          {
            id: "t-overview",
            label: "子视图A",
            onSelect: () => {
              tabClicks += 1;
            },
          },
        ]);
        return () => {
          d1();
          d2();
        };
      }, [host]);
      return <div>App Loaded</div>;
    };

    wrapHost(
      <PluginAppPage
        pluginId="org.example.demo"
        surfaceId="app"
        deps={{
          fetchDescriptor: async () =>
            makeDescriptor({
              surfaces: [
                {
                  id: "app",
                  kind: "app",
                  expose: "./App",
                  title: "Demo App",
                  icon: null,
                  location: "/plugin-apps/org.example.demo/app",
                  mountable: true,
                  editable: false,
                  required_role: "admin",
                },
              ],
            }),
          fetchContract: async () => CONTRACT,
          loadSurface: async () => AppWidget,
        }}
      />,
    );

    const actBtn = await waitFor(() => screen.getByRole("button", { name: "壳层同步" }));
    fireEvent.click(actBtn);
    assert.equal(actionClicks, 1);
    const tabBtn = screen.getByRole("tab", { name: "子视图A" });
    fireEvent.click(tabBtn);
    assert.equal(tabClicks, 1);
  } finally {
    globalThis.fetch = origFetch;
    cleanup();
  }
});

test("PluginSchemaForm renders declarative JSON schema + secret keep/replace/clear and accessiblePathFor guards admin routes", async () => {
  try {
    const savedCalls: Array<{ values: Record<string, unknown>; secrets: Record<string, unknown> }> = [];
    render(
      <PluginSchemaForm
        schema={{
          type: "object",
          required: ["interval_secs"],
          properties: {
            interval_secs: { type: "integer", minimum: 3600, maximum: 86400, default: 86400 },
            dry_run: { type: "boolean", default: false },
            api_token: { type: "string", "x-ui": { widget: "secret" } },
          },
        }}
        values={{ interval_secs: 7200, dry_run: true }}
        secrets={{ api_token: { configured: true } }}
        editable={true}
        sites={[]}
        downloaders={[]}
        onSave={async (values, secrets) => {
          savedCalls.push({ values, secrets });
        }}
      />,
    );

    const intervalInput = screen.getByLabelText(/interval_secs/) as HTMLInputElement;
    assert.equal(intervalInput.value, "7200");
    fireEvent.change(intervalInput, { target: { value: "10800" } });

    const select = screen.getByLabelText(/api_token 操作/) as HTMLSelectElement;
    fireEvent.change(select, { target: { value: "replace" } });
    const secretInput = screen.getByLabelText(/api_token 新密钥/) as HTMLInputElement;
    fireEvent.change(secretInput, { target: { value: "new-sec-123" } });

    fireEvent.click(screen.getByRole("button", { name: /保存配置/ }));
    await waitFor(() => assert.equal(savedCalls.length, 1));
    assert.deepEqual(savedCalls[0].values, { interval_secs: 10800, dry_run: true });
    assert.deepEqual(savedCalls[0].secrets, { api_token: { action: "replace", value: "new-sec-123" } });

    const member: SessionView = { ...ADMIN_SESSION, id: "user-member-1", role: "member" };
    assert.equal(accessiblePathFor(member, "/settings/plugins"), "/library");
    assert.equal(accessiblePathFor(member, "/plugin-apps/builtin.checkin/app"), "/library");
    assert.equal(accessiblePathFor(ADMIN_SESSION, "/plugin-apps/builtin.checkin/app"), "/plugin-apps/builtin.checkin/app");
  } finally {
    cleanup();
  }
});

test("PluginConfigHost refreshes editable/revision on reloadKey flip and marks enabled headless plugins read-only", async () => {
  const origFetch = globalThis.fetch;
  let pluginEnabled = true;
  let pluginLifecycle = "enabled";
  let configRev = 2;

  globalThis.fetch = (async (input: string | URL | Request) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    if (url.endsWith("/plugins/org.example.headless")) {
      return new Response(
        JSON.stringify({
          ok: true,
          data: {
            id: "org.example.headless",
            name: "Headless Plugin",
            version: "1.0.0",
            package_hash: "pkg-h",
            source: "external",
            signing_key_id: null,
            revision: configRev,
            config_revision: 1,
            generation: 1,
            enabled: pluginEnabled,
            lifecycle: pluginLifecycle,
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
    if (url.endsWith("/ui")) {
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
            plugin_id: "org.example.headless",
            schema_revision: "s1",
            schema: {
              type: "object",
              properties: { label: { type: "string", default: "init" } },
            },
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
            plugin_id: "org.example.headless",
            revision: configRev,
            config_revision: 1,
            schema_revision: "s1",
            values: { label: "v1" },
            secrets: {},
          },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    return new Response(JSON.stringify({ ok: true, data: [] }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  }) as typeof fetch;

  try {
    // 1. Standalone headless plugin while enabled -> MUST be read-only (editable === false)
    const view = render(
      <MemoryRouter>
        <FeedbackProvider>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
            <PluginConfigHost pluginId="org.example.headless" />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );

    await waitFor(() => assert.ok(screen.getByText(/配置版本 #2/)));
    assert.ok(screen.getByText("当前状态只读"), "Enabled headless plugin must be read-only");
    const input1 = screen.getByLabelText(/label/) as HTMLInputElement;
    assert.equal(input1.disabled, true);

    // 2. Flip from enabled -> disabled via reloadKey + pluginEditable props
    pluginEnabled = false;
    pluginLifecycle = "disabled";
    configRev = 3;
    const hostProps = {
      pluginId: "org.example.headless",
      reloadKey: "3:disabled:0",
      pluginEditable: true,
    };
    view.rerender(
      <MemoryRouter>
        <FeedbackProvider>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
            <PluginConfigHost {...hostProps} />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );

    await waitFor(() => assert.ok(screen.getByText(/配置版本 #3/)));
    assert.equal(screen.queryByText("当前状态只读"), null);
    const input2 = screen.getByLabelText(/label/) as HTMLInputElement;
    assert.equal(input2.disabled, false);

    // 3. Draining headless plugin without explicit pluginEditable prop -> read-only & inline surfaceDeps does not loop
    pluginEnabled = false;
    pluginLifecycle = "draining";
    let fetchDescCount = 0;
    view.rerender(
      <MemoryRouter>
        <FeedbackProvider>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
            <PluginConfigHost
              pluginId="org.example.headless"
              reloadKey="4:draining:0"
              surfaceDeps={{
                fetchDescriptor: async () => {
                  fetchDescCount += 1;
                  throw new Error("headless");
                },
              }}
            />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );
    await waitFor(() => assert.ok(screen.getByText("当前状态只读")));
    assert.equal(fetchDescCount, 1, "Inline surfaceDeps must not trigger re-fetch loops");
  } finally {
    globalThis.fetch = origFetch;
    cleanup();
  }
});
