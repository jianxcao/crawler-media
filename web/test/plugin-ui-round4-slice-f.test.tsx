import "./plugin-ui-env";

import assert from "node:assert/strict";
import test from "node:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import React from "react";
import { MemoryRouter } from "react-router-dom";

import type { PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";
import { FeedbackProvider } from "../components/feedback";
import { PluginAppPage } from "../components/plugins/PluginAppPage";
import { PluginConfigHost } from "../components/plugins/PluginConfigHost";
import { PluginSchemaForm } from "../components/plugins/PluginSchemaForm";
import { PluginSurfaceHost } from "../components/plugins/PluginSurfaceHost";
import type { SessionView } from "../lib/api/auth";
import type { PluginUiContract, UiDescriptorView } from "../lib/api/plugins";
import { loadFederatedSurface, type FederatedContainer } from "../lib/plugins/federation-loader";
import { createPluginHostSdk } from "../lib/plugins/sdk";
import { SessionProvider } from "../lib/session";

const ADMIN_SESSION: SessionView = {
  id: "user-admin-r4",
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
    plugin_id: "org.example.r4",
    plugin_name: "R4 Plugin",
    version: "1.0.0",
    generation: 1,
    lifecycle: "enabled",
    source: "external",
    package_hash: "pkg-r4-1",
    package_sha256: "pkg-r4-1",
    bundle_sha256: "bundle-r4-1",
    subject_user_id: "user-admin-r4",
    subject_role: "admin",
    ui_mode: "federation-react",
    remote_name: "r4Remote",
    entry: "ui/assets/remoteEntry.js",
    entry_url: "/api/v1/plugin-ui-assets/org.example.r4/pkg-r4-1/ui/assets/remoteEntry.js",
    asset_base_url: "/api/v1/plugin-ui-assets/org.example.r4/pkg-r4-1",
    contract: "1.0.0",
    react: "19.3.0",
    react_dom: "19.3.0",
    ui_trusted: true,
    trust: {
      status: "trusted",
      revision: 1,
      granted_at: "2026-11-24T00:00:00Z",
      granted_by_user_id: "user-admin-r4",
      revoked_at: null,
    },
    capabilities: ["read:sites"],
    surfaces: [
      {
        id: "settings",
        kind: "config",
        expose: "./Config",
        title: "R4 Settings",
        icon: null,
        location: null,
        mountable: true,
        editable: true,
        required_role: "admin",
      },
      {
        id: "studio",
        kind: "app",
        expose: "./App",
        title: "R4 Studio",
        icon: null,
        location: null,
        mountable: true,
        editable: false,
        required_role: "admin",
      },
    ],
    ...overrides,
  };
}

test("R4-F1: boost Config preserves unset optional budget_bytes instead of coercing undefined to 0", async () => {
  const origFetch = globalThis.fetch;
  globalThis.fetch = (async () =>
    new Response(JSON.stringify({ ok: true, data: { items: [] } }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    })) as typeof fetch;

  try {
    const saved: Array<Record<string, unknown>> = [];
    const boostDesc = makeDescriptor({
      plugin_id: "cm.boost",
      source: "external",
      remote_name: "cmBoost",
      package_hash: "cm:boost:1.0.0",
      package_sha256: "cm:boost:1.0.0",
      capabilities: ["read:sites", "read:downloaders"],
    });
    const BoostConfigStub: React.FC<PluginSurfaceProps> = ({ config }) => {
      const vals = config?.values ?? {};
      const [budgetGiB, setBudgetGiB] = React.useState(
        typeof vals.budget_bytes === "number" ? String(vals.budget_bytes / 1024 ** 3) : "",
      );
      return (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            const out: Record<string, unknown> = { ...vals };
            if (budgetGiB.trim()) {
              out.budget_bytes = Math.round(Number(budgetGiB) * 1024 ** 3);
            } else {
              delete out.budget_bytes;
            }
            void config?.save(out);
          }}
        >
          <h2>自动刷流策略与配额设置</h2>
          <label>
            空间预算 (GiB)
            <input value={budgetGiB} onChange={(e) => setBudgetGiB(e.target.value)} />
          </label>
          <button type="submit">保存刷流配置</button>
        </form>
      );
    };
    const container: FederatedContainer = {
      init: () => {},
      get: async () => () => ({ default: BoostConfigStub }),
    };
    const boostLoader = (descriptor: UiDescriptorView, expose: string) =>
      loadFederatedSurface(descriptor, expose, {
        fetchManifest: async () => [],
        importRemote: async () => container,
      });

    render(
      <MemoryRouter>
        <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "r4-b1" }}>
          <PluginSurfaceHost
            pluginId="cm.boost"
            surfaceId="settings"
            expectedKind="config"
            config={{
              values: { max_active: 15 },
              schemaRevision: "1",
              revision: "1",
              editable: true,
              onDraftChange: () => {},
              save: async (vals) => {
                saved.push(vals);
              },
            }}
            deps={{
              fetchDescriptor: async () => boostDesc,
              fetchContract: async () => CONTRACT,
              loadSurface: boostLoader,
            }}
          />
        </SessionProvider>
      </MemoryRouter>,
    );
    await waitFor(() => assert.ok(screen.getByText("自动刷流策略与配额设置")));
    const budgetInput = screen.getByLabelText("空间预算 (GiB)") as HTMLInputElement;
    assert.equal(budgetInput.value, "");
    fireEvent.click(screen.getByRole("button", { name: "保存刷流配置" }));
    await waitFor(() => assert.equal(saved.length, 1));
    assert.equal("budget_bytes" in saved[0], false);
  } finally {
    cleanup();
    globalThis.fetch = origFetch;
  }
});

test("R4-F2 & R4-F4: PluginSurfaceHost enforces exact surfaceId + expectedKind, clears dialog on reload, and PluginAppPage resets route state", async () => {
  const desc = makeDescriptor();
  let remoteCalls = 0;

  // 1. Unknown surfaceId with expectedKind="app" must NOT silently fall back to "studio"
  render(
    <MemoryRouter>
      <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "r4-1" }}>
        <PluginSurfaceHost
          pluginId="org.example.r4"
          surfaceId="nonexistent-app"
          expectedKind="app"
          deps={{
            fetchDescriptor: async () => desc,
            fetchContract: async () => CONTRACT,
            loadSurface: async () => {
              remoteCalls += 1;
              return () => <div>Should Not Mount</div>;
            },
          }}
        />
      </SessionProvider>
    </MemoryRouter>,
  );
  await waitFor(() => assert.ok(screen.getByText("未找到目标界面入口")));
  assert.equal(remoteCalls, 0);
  cleanup();

  // 2. Requesting a "config" surface ("settings") with expectedKind="app" must fail closed
  render(
    <MemoryRouter>
      <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "r4-2" }}>
        <PluginSurfaceHost
          pluginId="org.example.r4"
          surfaceId="settings"
          expectedKind="app"
          deps={{
            fetchDescriptor: async () => desc,
            fetchContract: async () => CONTRACT,
            loadSurface: async () => {
              remoteCalls += 1;
              return () => <div>Wrong Kind Mounted</div>;
            },
          }}
        />
      </SessionProvider>
    </MemoryRouter>,
  );
  await waitFor(() => assert.ok(screen.getByText("未找到目标界面入口")));
  assert.equal(remoteCalls, 0);
  cleanup();

  // 3. Dialog opened by generation 1 is cleared when reloadKey bumps AND stale close() does not clobber newer dialog
  let closeFirstDialog: (() => void) | null = null;
  function makeOpener(label: string) {
    return function DialogOpener({ host }: PluginSurfaceProps) {
      return (
        <button
          type="button"
          onClick={() => {
            const close = host.ui.dialog(() => <div>{label}</div>);
            if (!closeFirstDialog) closeFirstDialog = close;
          }}
        >
          打开弹窗
        </button>
      );
    };
  }
  const view = render(
    <MemoryRouter>
      <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "r4-3" }}>
        <PluginSurfaceHost
          pluginId="org.example.r4"
          surfaceId="studio"
          expectedKind="app"
          reloadKey={1}
          deps={{
            fetchDescriptor: async () => makeDescriptor({ generation: 1 }),
            fetchContract: async () => CONTRACT,
            loadSurface: async () => makeOpener("Modal Gen 1"),
          }}
        />
      </SessionProvider>
    </MemoryRouter>,
  );
  await waitFor(() => assert.ok(screen.getByRole("button", { name: "打开弹窗" })));
  fireEvent.click(screen.getByRole("button", { name: "打开弹窗" }));
  await waitFor(() => assert.ok(screen.getByText("Modal Gen 1")));

  view.rerender(
    <MemoryRouter>
      <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "r4-3" }}>
        <PluginSurfaceHost
          pluginId="org.example.r4"
          surfaceId="studio"
          expectedKind="app"
          reloadKey={2}
          deps={{
            fetchDescriptor: async () => makeDescriptor({ generation: 2 }),
            fetchContract: async () => CONTRACT,
            loadSurface: async () => makeOpener("Modal Gen 2"),
          }}
        />
      </SessionProvider>
    </MemoryRouter>,
  );
  await waitFor(() => assert.equal(screen.queryByText("Modal Gen 1"), null));
  await waitFor(() => assert.ok(screen.getByRole("button", { name: "打开弹窗" })));
  fireEvent.click(screen.getByRole("button", { name: "打开弹窗" }));
  await waitFor(() => assert.ok(screen.getByText("Modal Gen 2")));
  act(() => {
    closeFirstDialog?.();
  });
  assert.ok(screen.getByText("Modal Gen 2"), "stale close() must not close newer dialog");
  cleanup();

  // 4. PluginAppPage resets title when switching [pluginId, surfaceId] to a blocked surface
  const appView = render(
    <MemoryRouter>
      <FeedbackProvider>
        <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "r4-4" }}>
          <PluginAppPage
            pluginId="org.example.r4"
            surfaceId="studio"
            deps={{
              fetchDescriptor: async () => desc,
              fetchContract: async () => CONTRACT,
              loadSurface: async () => () => <div>Studio Body</div>,
            }}
          />
        </SessionProvider>
      </FeedbackProvider>
    </MemoryRouter>,
  );
  await waitFor(() => assert.ok(screen.getByRole("heading", { name: "R4 Studio" })));
  appView.rerender(
    <MemoryRouter>
      <FeedbackProvider>
        <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "r4-4" }}>
          <PluginAppPage
            pluginId="org.example.other"
            surfaceId="missing"
            deps={{
              fetchDescriptor: async () => makeDescriptor({ plugin_id: "org.example.other", surfaces: [] }),
              fetchContract: async () => CONTRACT,
              loadSurface: async () => () => <div>Other</div>,
            }}
          />
        </SessionProvider>
      </FeedbackProvider>
    </MemoryRouter>,
  );
  await waitFor(() =>
    assert.ok(screen.getByRole("heading", { name: "org.example.other · missing" })),
  );
  cleanup();
});

test("R4-F3 & R4-F4b: PluginSchemaForm omits empty optional site_id/number in hideNonSecrets mode and PluginConfigHost ignores stale load responses", async () => {
  const savedCaptures: Array<Record<string, unknown>> = [];
  render(
    <PluginSchemaForm
      schema={{
        type: "object",
        properties: {
          site_id: { type: "string", "x-ui": { widget: "site" } },
          downloader_id: { type: "string", "x-ui": { widget: "downloader" } },
          max_retries: { type: "integer" },
          api_token: { type: "string", format: "secret", title: "API Token" },
        },
      }}
      values={{ site_id: "", downloader_id: "", max_retries: "", keep_flag: true }}
      secrets={{ api_token: { configured: false } }}
      editable
      hideNonSecrets
      sites={[{ id: "site-1", name: "Site 1" }]}
      downloaders={[]}
      onSave={async (vals) => {
        savedCaptures.push(vals);
      }}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
  await waitFor(() => assert.equal(savedCaptures.length, 1));
  assert.equal("site_id" in savedCaptures[0], false);
  assert.equal("downloader_id" in savedCaptures[0], false);
  assert.equal("max_retries" in savedCaptures[0], false);
  assert.equal(savedCaptures[0].keep_flag, true);
  cleanup();

  // Out-of-order PluginConfigHost load race: slow plugin-A response resolves AFTER fast plugin-B response
  const origFetch = globalThis.fetch;
  const slowResolvers: Array<(res: Response) => void> = [];
  try {
    globalThis.fetch = ((input: string | URL | Request) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
      if (url.includes("/plugins/plugin-a/config") && !url.endsWith("/config-schema")) {
        return new Promise<Response>((resolve) => {
          slowResolvers.push(resolve);
        });
      }
      if (url.endsWith("/config-schema")) {
        return Promise.resolve(
          new Response(
            JSON.stringify({
              ok: true,
              data: {
                plugin_id: url.includes("plugin-a") ? "plugin-a" : "plugin-b",
                schema_revision: "1",
                schema: { type: "object", properties: { label: { type: "string", title: "标签" } } },
              },
            }),
            { status: 200, headers: { "Content-Type": "application/json" } },
          ),
        );
      }
      if (url.includes("/plugins/plugin-b/config")) {
        return Promise.resolve(
          new Response(
            JSON.stringify({
              ok: true,
              data: {
                plugin_id: "plugin-b",
                schema_revision: "1",
                revision: 2,
                values: { label: "VALUE-FROM-B" },
                secrets: {},
              },
            }),
            { status: 200, headers: { "Content-Type": "application/json" } },
          ),
        );
      }
      return Promise.resolve(
        new Response(JSON.stringify({ ok: false, error: { code: "not_found", message: "none" } }), {
          status: 404,
          headers: { "Content-Type": "application/json" },
        }),
      );
    }) as typeof fetch;

    const hostView = render(
      <MemoryRouter>
        <FeedbackProvider>
          <PluginConfigHost pluginId="plugin-a" pluginEditable />
        </FeedbackProvider>
      </MemoryRouter>,
    );
    hostView.rerender(
      <MemoryRouter>
        <FeedbackProvider>
          <PluginConfigHost pluginId="plugin-b" pluginEditable />
        </FeedbackProvider>
      </MemoryRouter>,
    );
    await waitFor(() => {
      const inp = screen.getByLabelText("标签") as HTMLInputElement;
      assert.equal(inp.value, "VALUE-FROM-B");
    });
    for (const resolve of slowResolvers) {
      resolve(
        new Response(
          JSON.stringify({
            ok: true,
            data: {
              plugin_id: "plugin-a",
              schema_revision: "1",
              revision: 1,
              values: { label: "STALE-FROM-A" },
              secrets: {},
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        ),
      );
    }
    await new Promise((r) => setTimeout(r, 40));
    assert.equal((screen.getByLabelText("标签") as HTMLInputElement).value, "VALUE-FROM-B");
  } finally {
    cleanup();
    globalThis.fetch = origFetch;
  }
});

test("R4-F5: sdk.operations.watch enforces single-flight polling when get() exceeds 500ms", async () => {
  let activeInFlight = 0;
  let maxConcurrent = 0;
  let totalCalls = 0;

  const managed = createPluginHostSdk({
    descriptor: makeDescriptor(),
    surfaceId: "studio",
    session: ADMIN_SESSION,
    loginEpoch: "r4-watch",
    theme: "silver",
    navigate: () => {},
    notifyToast: () => {},
    openConfirm: async () => false,
    openDialog: () => () => {},
    fetchImpl: (async () => {
      activeInFlight += 1;
      totalCalls += 1;
      maxConcurrent = Math.max(maxConcurrent, activeInFlight);
      await new Promise((r) => setTimeout(r, 680));
      activeInFlight -= 1;
      return new Response(
        JSON.stringify({
          ok: true,
          data: {
            operation_id: "op-slow-1",
            state: totalCalls >= 2 ? "succeeded" : "running",
            revision: String(totalCalls),
          },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }) as typeof fetch,
  });

  const states: string[] = [];
  const unsub = managed.sdk.operations.watch("op-slow-1", (op) => {
    states.push(op.state);
  });
  try {
    await new Promise((r) => setTimeout(r, 1250));
    assert.equal(maxConcurrent, 1, "operations.watch must not overlap concurrent polls");
    assert.ok(states.length >= 1);
  } finally {
    unsub();
    managed.dispose();
  }
});
