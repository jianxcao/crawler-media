import "./plugin-ui-env";

import assert from "node:assert/strict";
import test from "node:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import React from "react";
import { MemoryRouter } from "react-router-dom";

import {
  usePluginCommand,
  type Capability,
  type OperationRef,
  type PluginHostSDK,
  type PluginSurfaceProps,
  type UiHandle,
} from "@crawler-media/plugin-ui-sdk";
import { FeedbackProvider } from "../components/feedback";
import { PluginDashboardSlot } from "../components/plugins/PluginDashboardSlot";
import { PluginDetailView } from "../components/plugins/PluginDetailView";
import { PluginSchemaForm, schemaHasSecrets } from "../components/plugins/PluginSchemaForm";
import { PluginSurfaceHost } from "../components/plugins/PluginSurfaceHost";
import type { SessionView } from "../lib/api/auth";
import type { PluginUiContract, UiDescriptorView } from "../lib/api/plugins";
import { HttpError } from "../lib/http";
import {
  clearFederationCache,
  getLoadedRemoteHash,
  loadFederatedSurface,
  type FederatedContainer,
} from "../lib/plugins/federation-loader";
import { createPluginHostSdk } from "../lib/plugins/sdk";
import { SessionProvider } from "../lib/session";

const GIB = 1024 ** 3;

const ADMIN_SESSION: SessionView = {
  id: "user-admin-r6",
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
    plugin_id: "org.example.r6",
    plugin_name: "R6 Plugin",
    version: "1.0.0",
    generation: 1,
    lifecycle: "enabled",
    source: "external",
    package_hash: "pkg-r6-1",
    package_sha256: "pkg-r6-1",
    bundle_sha256: "bundle-r6-1",
    subject_user_id: ADMIN_SESSION.id,
    subject_role: "admin",
    ui_mode: "federation-react",
    remote_name: "r6Remote",
    entry: "ui/assets/remoteEntry.js",
    entry_url: "/api/v1/plugin-ui-assets/org.example.r6/pkg-r6-1/ui/assets/remoteEntry.js",
    asset_base_url: "/api/v1/plugin-ui-assets/org.example.r6/pkg-r6-1",
    styles: ["ui/assets/style-v1.css"],
    contract: "1.0.0",
    react: "19.3.0",
    react_dom: "19.3.0",
    ui_trusted: true,
    trust: {
      status: "trusted",
      revision: 1,
      granted_at: "2026-11-25T00:00:00Z",
      granted_by_user_id: ADMIN_SESSION.id,
      revoked_at: null,
    },
    capabilities: ["read:sites", "read:downloaders", "read:boost", "read:history"],
    surfaces: [
      {
        id: "main",
        kind: "page",
        expose: "./Page",
        title: "R6 Page",
        icon: null,
        location: null,
        mountable: true,
        editable: false,
        required_role: "admin",
      },
      {
        id: "card",
        kind: "dashboard",
        expose: "./Dashboard",
        title: "R6 Card",
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

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function jsonOk(data: unknown): Response {
  return new Response(JSON.stringify({ ok: true, data }), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

test("R6-F1: PluginSchemaForm & schemaHasSecrets merge #/$defs chains on root/properties and support nested object fields + dotted secrets", async () => {
  assert.equal(
    schemaHasSecrets({
      $defs: {
        root_obj: {
          type: "object",
          properties: { auth: { type: "object", properties: { api_token: { $ref: "#/$defs/sec" } } } },
        },
        sec: { type: "string", format: "password" },
      },
      $ref: "#/$defs/root_obj",
    }),
    true,
  );
  assert.equal(
    schemaHasSecrets({
      type: "object",
      $defs: { cyc: { type: "object", properties: { child: { $ref: "#/$defs/cyc" } } } },
      properties: { loop: { $ref: "#/$defs/cyc" } },
    }),
    false,
  );

  const schema = {
    $defs: {
      root_cfg: {
        type: "object",
        required: ["port", "auth"],
        properties: {
          port: { $ref: "#/$defs/port_alias", title: "Service Port" },
          ratio: { $ref: "#/$defs/ratio_num", title: "Share Ratio" },
          enabled_flag: { $ref: "#/$defs/bool_def", title: "Enable Auto" },
          mode: { $ref: "#/$defs/mode_enum", title: "Run Mode" },
          target_site: { $ref: "#/$defs/site_ref", title: "Target Site" },
          auth: { $ref: "#/$defs/auth_obj", title: "Auth Settings" },
        },
      },
      port_alias: { $ref: "#/$defs/port_int", description: "Port range 1..65535" },
      port_int: { type: "integer", minimum: 1, maximum: 65535, default: 8080 },
      ratio_num: { type: "number", minimum: 0.5, maximum: 10, default: 2.5 },
      bool_def: { type: "boolean", default: true },
      mode_enum: { type: "string", enum: ["fast", "safe"], default: "safe" },
      site_ref: { type: "string", "x-ui": { widget: "site" } },
      auth_obj: {
        type: "object",
        required: ["username"],
        properties: {
          username: { type: "string", title: "Auth Username" },
          retries: { $ref: "#/$defs/retries_int", title: "Auth Retries" },
          api_token: { type: "string", format: "password", title: "Auth Token" },
        },
      },
      retries_int: { type: "integer", minimum: 0, maximum: 10, default: 3 },
    },
    $ref: "#/$defs/root_cfg",
  };

  const savedCalls: Array<{ values: Record<string, unknown>; secrets: Record<string, unknown> }> = [];
  try {
    render(
      <PluginSchemaForm
        schema={schema}
        values={{
          ratio: 1.5,
          target_site: "mteam",
          auth: { username: "alice", api_token: "leaked-nested-plaintext" },
        }}
        secrets={{ "auth.api_token": { configured: true } }}
        editable
        sites={[{ id: "mteam", name: "M-Team" }]}
        downloaders={[]}
        onSave={async (values, secrets) => {
          savedCalls.push({ values, secrets });
        }}
      />,
    );

    for (const input of Array.from(document.querySelectorAll("input"))) {
      assert.notEqual(input.value, "[object Object]");
    }
    const portInput = screen.getByLabelText("Service Port") as HTMLInputElement;
    assert.equal(portInput.type, "number");
    assert.equal(portInput.value, "8080");
    assert.ok(screen.getByText("Port range 1..65535"));

    const ratioInput = screen.getByLabelText("Share Ratio") as HTMLInputElement;
    assert.equal(ratioInput.type, "number");
    assert.equal(ratioInput.value, "1.5");

    const enabledCheckbox = screen.getByLabelText("Enable Auto") as HTMLInputElement;
    assert.equal(enabledCheckbox.type, "checkbox");
    assert.equal(enabledCheckbox.checked, true);

    const modeSelect = screen.getByLabelText("Run Mode") as HTMLSelectElement;
    assert.equal(modeSelect.tagName, "SELECT");
    assert.equal(modeSelect.value, "safe");

    const siteSelect = screen.getByLabelText("Target Site") as HTMLSelectElement;
    assert.equal(siteSelect.tagName, "SELECT");
    assert.equal(siteSelect.value, "mteam");

    assert.equal((screen.getByLabelText("Auth Username") as HTMLInputElement).value, "alice");
    const authRetries = screen.getByLabelText("Auth Retries") as HTMLInputElement;
    assert.equal(authRetries.type, "number");
    assert.equal(authRetries.value, "3");
    assert.equal(screen.queryByLabelText("Auth Token"), null);

    const secretAct = screen.getByLabelText("auth.api_token 操作") as HTMLSelectElement;
    fireEvent.change(portInput, { target: { value: "9090" } });
    fireEvent.change(ratioInput, { target: { value: "" } });
    fireEvent.change(siteSelect, { target: { value: "" } });
    fireEvent.change(modeSelect, { target: { value: "fast" } });
    fireEvent.change(authRetries, { target: { value: "5" } });
    fireEvent.change(secretAct, { target: { value: "replace" } });
    fireEvent.change(screen.getByLabelText("auth.api_token 新密钥"), {
      target: { value: "rotated-secret-99" },
    });

    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => assert.equal(savedCalls.length, 1));
    assert.deepEqual(savedCalls[0].values, {
      port: 9090,
      enabled_flag: true,
      mode: "fast",
      auth: { username: "alice", retries: 5 },
    });
    assert.deepEqual(savedCalls[0].secrets, {
      "auth.api_token": { action: "replace", value: "rotated-secret-99" },
    });

    cleanup();

    const hiddenSaves: Array<Record<string, unknown>> = [];
    render(
      <PluginSchemaForm
        schema={schema}
        values={{ port: 8080, auth: { username: "bob", api_token: "must-strip-in-hidden-mode" } }}
        secrets={{ "auth.api_token": { configured: true } }}
        editable
        hideNonSecrets
        sites={[]}
        downloaders={[]}
        onSave={async (vals) => {
          hiddenSaves.push(vals);
        }}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => assert.equal(hiddenSaves.length, 1));
    assert.deepEqual(hiddenSaves[0], {
      port: 8080,
      ratio: 2.5,
      enabled_flag: true,
      mode: "safe",
      auth: { username: "bob", retries: 3 },
    });
  } finally {
    cleanup();
  }
});

test("R6-F2: federation-loader, PluginSurfaceHost, and PluginDashboardSlot evict caches cleanly and guard subject_user_id", async () => {
  clearFederationCache();
  const origFetch = globalThis.fetch;
  try {
    const v1 = makeDescriptor({ package_hash: "pkg-v1", bundle_sha256: "bundle-v1" });
    const CompV1 = () => <div>V1</div>;
    await loadFederatedSurface(v1, "./Page", {
      reloadPage: () => {},
      importRemote: async () => ({
        init: async () => {},
        get: async () => () => ({ default: CompV1 }),
      }),
    });
    assert.equal(getLoadedRemoteHash("r6Remote"), "pkg-v1:bundle-v1");
    assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 1);

    const v2 = makeDescriptor({ generation: 2, package_hash: "pkg-v2", bundle_sha256: "bundle-v2" });
    await assert.rejects(
      () =>
        loadFederatedSurface(v2, "./Page", {
          reloadPage: () => {},
          importRemote: async () => ({
            init: async () => {
              throw new Error("v2 init boom");
            },
            get: async () => () => ({ default: CompV1 }),
          }),
        }),
      /v2 init boom/,
    );
    assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 0);
    assert.notEqual(getLoadedRemoteHash("r6Remote"), "pkg-v2:bundle-v2");

    let hostMode: "ok" | "subject-mismatch" | "contract-mismatch" | "http-404" = "ok";
    const mountWithHost = (reloadKey: string) => (
      <MemoryRouter>
        <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "r6-h" }}>
          <PluginSurfaceHost
            pluginId="org.example.r6"
            surfaceId="main"
            expectedKind="page"
            reloadKey={reloadKey}
            deps={{
              fetchContract: async () => CONTRACT,
              fetchDescriptor: async () => {
                if (hostMode === "http-404") throw new HttpError("plugin not found", 404, "plugin.not_found");
                if (hostMode === "subject-mismatch") return makeDescriptor({ subject_user_id: "different-user" });
                if (hostMode === "contract-mismatch") return makeDescriptor({ contract: "9.0.0" });
                return makeDescriptor();
              },
              loadSurface: (desc, expose) =>
                loadFederatedSurface(desc, expose, {
                  reloadPage: () => {},
                  importRemote: async () => ({
                    init: async () => {},
                    get: async () => () => ({ default: () => <div>Surface Ready</div> }),
                  }),
                }),
            }}
          />
        </SessionProvider>
      </MemoryRouter>
    );

    const view = render(mountWithHost("k1"));
    await waitFor(() => assert.ok(screen.getByText("Surface Ready")));
    assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 1);

    for (const [key, mode, expectedText] of [
      ["k2", "subject-mismatch", "会话身份不匹配"],
      ["k4", "contract-mismatch", "UI 契约版本不兼容"],
      ["k6", "http-404", "无法加载插件界面"],
    ] as const) {
      hostMode = mode;
      view.rerender(mountWithHost(key));
      await waitFor(() => assert.ok(screen.getByText(expectedText)));
      assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 0);
      hostMode = "ok";
      view.rerender(mountWithHost(`${key}-ok`));
      await waitFor(() => assert.ok(screen.getByText("Surface Ready")));
      assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 1);
    }
    cleanup();
    clearFederationCache();

    const navSignals: AbortSignal[] = [];
    let navCall = 0;
    const inflightNav = deferred<Response>();
    const navData = [
      {
        plugin_id: "org.example.r6",
        plugin_name: "Match Plugin",
        generation: 1,
        remote_name: "r6Remote",
        entry_url: "/entry.js",
        id: "org.example.r6:card",
        surface_id: "card",
        kind: "dashboard",
        title: "Admin Owned Card",
        icon: null,
        expose: "./Dashboard",
        location: "/dashboard",
        path: "/dashboard",
        required_role: "admin",
        subject_user_id: "user-admin-r6",
      },
      {
        plugin_id: "org.example.other",
        plugin_name: "Foreign Plugin",
        generation: 1,
        remote_name: "otherRemote",
        entry_url: "/other.js",
        id: "org.example.other:card",
        surface_id: "card",
        kind: "dashboard",
        title: "Other User Card",
        icon: null,
        expose: "./Dashboard",
        location: "/dashboard",
        path: "/dashboard",
        required_role: "admin",
        subject_user_id: "user-admin-2",
      },
    ];
    globalThis.fetch = ((_input: string | URL | Request, init?: RequestInit) => {
      navCall += 1;
      if (init?.signal) navSignals.push(init.signal);
      return navCall === 2 ? inflightNav.promise : Promise.resolve(jsonOk(navData));
    }) as typeof fetch;

    const renderDashboardFor = (session: SessionView) => (
      <MemoryRouter>
        <FeedbackProvider>
          <SessionProvider value={{ session, setSession: () => {}, loginEpoch: `${session.id}:ep` }}>
            <PluginDashboardSlot />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>
    );

    const dashView = render(renderDashboardFor(ADMIN_SESSION));
    fireEvent.click(screen.getByRole("button", { name: "+ 添加看板卡片" }));
    await waitFor(() => assert.ok(screen.getByText(/Admin Owned Card/)));
    assert.equal(screen.queryByText(/Other User Card/), null);

    dashView.rerender(renderDashboardFor({ ...ADMIN_SESSION, id: "user-transient" }));
    assert.equal(navSignals[1]?.aborted, false);

    dashView.rerender(renderDashboardFor({ ...ADMIN_SESSION, id: "user-admin-2" }));
    assert.equal(navSignals[1]?.aborted, true);
    await waitFor(() => assert.ok(screen.getByText(/Other User Card/)));
    assert.equal(screen.queryByText(/Admin Owned Card/), null);
    inflightNav.resolve(jsonOk(navData));
  } finally {
    cleanup();
    clearFederationCache();
    globalThis.fetch = origFetch;
  }
});

test("R6-F3: Capability union includes read:downloaders/read:libraries/site sub-commands and sdk.supports implies check-in & verify-session from command:site.maintenance", () => {
  const capsToCheck: Capability[] = [
    "read:downloaders",
    "read:libraries",
    "command:site.check-in",
    "command:site.verify-session",
  ];
  assert.equal(capsToCheck.length, 4);

  const makeSdk = (capabilities: string[]) =>
    createPluginHostSdk({
      descriptor: makeDescriptor({ capabilities }),
      surfaceId: "main",
      session: ADMIN_SESSION,
      loginEpoch: "r6-cap",
      theme: "silver",
      navigate: () => {},
      notifyToast: () => {},
      openConfirm: async () => false,
      openDialog: () => () => {},
    });

  const managedWithMaint = makeSdk(["read:downloaders", "read:libraries", "command:site.maintenance"]);
  try {
    assert.equal(managedWithMaint.sdk.supports("read:downloaders"), true);
    assert.equal(managedWithMaint.sdk.supports("read:libraries"), true);
    assert.equal(managedWithMaint.sdk.supports("command:site.maintenance"), true);
    assert.equal(managedWithMaint.sdk.supports("command:site.check-in"), true);
    assert.equal(managedWithMaint.sdk.supports("command:site.verify-session"), true);
    assert.equal(managedWithMaint.sdk.supports("command:site.delete"), false);
  } finally {
    managedWithMaint.dispose();
  }

  const managedNarrow = makeSdk(["command:site.check-in"]);
  try {
    assert.equal(managedNarrow.sdk.supports("command:site.check-in"), true);
    assert.equal(managedNarrow.sdk.supports("command:site.verify-session"), false);
  } finally {
    managedNarrow.dispose();
  }
});

test("R6-F4a: usePluginCommand tracks overlapping inflight calls and applies only latest lastOperation/error", async () => {
  const callQueue: Array<{ resolve: (op: OperationRef) => void; reject: (err: Error) => void }> = [];
  const fakeHost = {
    context: { plugin_id: "org.example.r6", package_hash: "pkg", generation: "1", surface_id: "main", login_epoch: "ep" },
    commands: {
      submit: () =>
        new Promise<OperationRef>((resolve, reject) => {
          callQueue.push({ resolve, reject });
        }),
    },
  } as unknown as PluginHostSDK;

  let triggerSubmit!: (key: string) => Promise<OperationRef>;
  function CommandHarness() {
    const cmd = usePluginCommand("site.maintenance", fakeHost);
    triggerSubmit = (key: string) =>
      cmd.submit(
        { site_handle: "h1" as UiHandle, action: "check-in" },
        { expectedRevision: "1", idempotencyKey: key },
      );
    return (
      <div>
        <span data-testid="cmd-pending">{String(cmd.pending)}</span>
        <span data-testid="cmd-op">{cmd.lastOperation?.operation_id ?? "none"}</span>
        <span data-testid="cmd-err">{cmd.error?.message ?? "none"}</span>
      </div>
    );
  }

  try {
    render(<CommandHarness />);
    assert.equal(screen.getByTestId("cmd-pending").textContent, "false");

    let p1!: Promise<OperationRef>;
    let p2!: Promise<OperationRef>;
    act(() => {
      p1 = triggerSubmit("k1").catch(() => ({ operation_id: "err", state: "failed" as const }));
      p2 = triggerSubmit("k2");
    });
    assert.equal(screen.getByTestId("cmd-pending").textContent, "true");

    await act(async () => {
      callQueue[1].resolve({ operation_id: "op-latest", state: "running" });
      await p2;
    });
    assert.equal(screen.getByTestId("cmd-op").textContent, "op-latest");
    assert.equal(screen.getByTestId("cmd-pending").textContent, "true");

    await act(async () => {
      callQueue[0].reject(new Error("stale failure"));
      await p1;
    });
    assert.equal(screen.getByTestId("cmd-pending").textContent, "false");
    assert.equal(screen.getByTestId("cmd-op").textContent, "op-latest");
    assert.equal(screen.getByTestId("cmd-err").textContent, "none");
  } finally {
    cleanup();
  }
});

function makePluginDetailPayload(id: string, name: string, revision: number) {
  return {
    id,
    name,
    version: `${revision}.0.0`,
    package_hash: `pkg-${id}`,
    source: "external",
    revision,
    config_revision: 1,
    generation: revision,
    enabled: true,
    lifecycle: "enabled",
    health: "healthy",
    required_grants: [],
    effective_grants: [],
    active_operation_id: null,
    failure_reason: null,
    signing_key_id: null,
    state_schema: 1,
    credential_bound: false,
    task_count: 0,
    version_count: 1,
  };
}

function makeOutOfOrderQueryPageStub(opts: {
  selectLabel: string;
  refreshLabel: string;
  initialSelect: string;
  options: Array<{ value: string; label: string }>;
  fetchDisplay: (host: PluginHostSDK, siteId: string) => Promise<string>;
}): React.FC<PluginSurfaceProps> {
  return function OutOfOrderStub({ host }) {
    const [selected, setSelected] = React.useState(opts.initialSelect);
    const [display, setDisplay] = React.useState("");
    const reqSeqRef = React.useRef(0);
    const loadData = React.useCallback(
      (val: string) => {
        const seq = ++reqSeqRef.current;
        void opts.fetchDisplay(host, val).then((text) => {
          if (seq === reqSeqRef.current) setDisplay(text);
        });
      },
      [host],
    );
    React.useEffect(() => loadData(selected), [loadData, selected]);
    return (
      <div>
        <span>{display}</span>
        <label>
          {opts.selectLabel}
          <select value={selected} onChange={(e) => setSelected(e.target.value)}>
            {opts.options.map((o) => <option key={o.value} value={o.value}>{o.label}</option>)}
          </select>
        </label>
        <button type="button" onClick={() => loadData(selected)}>{opts.refreshLabel}</button>
      </div>
    );
  };
}

function renderOutOfOrderSurface(
  pluginId: string,
  loginEpoch: string,
  desc: ReturnType<typeof makeDescriptor>,
  PageStub: React.FC<PluginSurfaceProps>,
) {
  const container: FederatedContainer = { init: () => {}, get: async () => () => ({ default: PageStub }) };
  return render(
    <MemoryRouter>
      <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch }}>
        <PluginSurfaceHost
          pluginId={pluginId}
          surfaceId="main"
          expectedKind="page"
          deps={{
            fetchDescriptor: async () => desc,
            fetchContract: async () => CONTRACT,
            loadSurface: (d, expose) =>
              loadFederatedSurface(d, expose, { fetchManifest: async () => [], importRemote: async () => container }),
          }}
        />
      </SessionProvider>
    </MemoryRouter>,
  );
}

test("R6-F4b: PluginDetailView, BoostPage, and CheckinPage ignore out-of-order earlier async responses", async () => {
  const origFetch = globalThis.fetch;
  try {
    // 1. PluginDetailView out-of-order pluginId switch
    const slowDetail = deferred<Response>();
    globalThis.fetch = ((input: string | URL | Request) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
      if (url.endsWith("/plugins/plugin-slow")) return slowDetail.promise;
      if (url.endsWith("/plugins/plugin-fast")) return Promise.resolve(jsonOk(makePluginDetailPayload("plugin-fast", "Fast Plugin Title", 2)));
      if (url.includes("/ui")) return Promise.resolve(new Response(JSON.stringify({ ok: false, error: { message: "no ui" } }), { status: 404 }));
      if (url.includes("/tasks") || url.includes("/audit")) return Promise.resolve(jsonOk([]));
      return Promise.resolve(jsonOk({ items: [], required_capabilities: [], effective_grants: [] }));
    }) as typeof fetch;

    const wrapDetail = (pid: string) => (
      <MemoryRouter initialEntries={[`/settings/plugins/${pid}`]}>
        <FeedbackProvider>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "r6-d" }}>
            <PluginDetailView pluginId={pid} />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>
    );
    const detailRender = render(wrapDetail("plugin-slow"));
    detailRender.rerender(wrapDetail("plugin-fast"));
    await waitFor(() => assert.ok(screen.getByRole("heading", { name: "Fast Plugin Title" })));
    await act(async () => {
      slowDetail.resolve(jsonOk(makePluginDetailPayload("plugin-slow", "Stale Slow Title", 1)));
      await slowDetail.promise;
    });
    assert.ok(screen.getByRole("heading", { name: "Fast Plugin Title" }));
    assert.equal(screen.queryByText("Stale Slow Title"), null);
    cleanup();

    // 2. BoostPage (cm.boost) out-of-order refresh race
    const slowBoostSecond = deferred<Response>();
    let boostSnapCalls = 0;
    const makeBoostSnap = (rev: string, usedGib: number) =>
      jsonOk({
        revision: rev, used_bytes: usedGib * GIB, reserved_bytes: 0, retained_file_bytes: 0,
        disk: { available_bytes: 50 * GIB, status: "ok" }, tasks: [], reservations: [],
      });
    globalThis.fetch = ((input: string | URL | Request, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
      const body = typeof init?.body === "string" ? JSON.parse(init.body) : null;
      if (url.endsWith("/ui/query") && body?.name === "boost.snapshot") {
        boostSnapCalls += 1;
        return boostSnapCalls === 2 ? slowBoostSecond.promise : Promise.resolve(makeBoostSnap(String(boostSnapCalls), boostSnapCalls === 1 ? 1 : 2));
      }
      if (url.endsWith("/ui/query") && body?.name === "sites.list") {
        return Promise.resolve(jsonOk({ items: [{ id: "site-a", name: "Site A", capabilities: [] }, { id: "site-b", name: "Site B", capabilities: [] }] }));
      }
      return Promise.resolve(jsonOk({ items: [] }));
    }) as typeof fetch;

    renderOutOfOrderSurface(
      "cm.boost",
      "r6-boost",
      makeDescriptor({ plugin_id: "cm.boost", source: "external", remote_name: "cmBoost", package_hash: "cm:boost:1.0.0", package_sha256: "cm:boost:1.0.0" }),
      makeOutOfOrderQueryPageStub({
        selectLabel: "按站点筛选", refreshLabel: "刷新快照", initialSelect: "",
        options: [{ value: "", label: "全部" }, { value: "site-b", label: "Site B" }],
        fetchDisplay: async (h, sid) => {
          const res = await h.query("boost.snapshot", sid ? { site_id: sid } : {});
          return `${(Number(res?.used_bytes ?? 0) / GIB).toFixed(2)} GiB`;
        },
      }),
    );
    await waitFor(() => assert.ok(screen.getByText("1.00 GiB")));
    fireEvent.click(screen.getByRole("button", { name: "刷新快照" }));
    fireEvent.change(screen.getByLabelText("按站点筛选"), { target: { value: "site-b" } });
    await waitFor(() => assert.ok(screen.getByText("2.00 GiB")));
    await act(async () => {
      slowBoostSecond.resolve(makeBoostSnap("2", 9));
      await slowBoostSecond.promise;
    });
    assert.ok(screen.getByText("2.00 GiB"));
    assert.equal(screen.queryByText("9.00 GiB"), null);
    cleanup();
    clearFederationCache();

    // 3. CheckinPage (cm.checkin) out-of-order site switch must not snap siteId back
    const slowSite1Receipts = deferred<Response>();
    let site1Calls = 0;
    globalThis.fetch = ((input: string | URL | Request, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
      const body = typeof init?.body === "string" ? JSON.parse(init.body) : null;
      if (url.endsWith("/ui/query") && body?.name === "sites.list") {
        return Promise.resolve(jsonOk({ items: [{ id: "site-1", name: "Site One", capabilities: [] }, { id: "site-2", name: "Site Two", capabilities: [] }] }));
      }
      if (url.endsWith("/ui/query") && body?.name === "checkin.receipts") {
        if (body?.input?.site_id === "site-1") {
          site1Calls += 1;
          if (site1Calls === 2) return slowSite1Receipts.promise;
          return Promise.resolve(jsonOk({ items: [{ site_id: "site-1", state: "completed", observed_at: "2026-11-25T01:00:00Z", detail: "Site1 initial receipt" }] }));
        }
        return Promise.resolve(jsonOk({ items: [{ site_id: "site-2", state: "completed", observed_at: "2026-11-25T02:00:00Z", detail: "Site2 fresh receipt" }] }));
      }
      if (url.includes("/browser-availability")) return Promise.resolve(jsonOk({ site_id: "site-2", status: "ready", missing_requirements: [] }));
      return Promise.resolve(jsonOk({ items: [] }));
    }) as typeof fetch;

    renderOutOfOrderSurface(
      "cm.checkin",
      "r6-chk",
      makeDescriptor({ plugin_id: "cm.checkin", source: "external", remote_name: "cmCheckin", package_hash: "cm:checkin:1.0.0", package_sha256: "cm:checkin:1.0.0" }),
      makeOutOfOrderQueryPageStub({
        selectLabel: "选择签到站点", refreshLabel: "刷新", initialSelect: "site-1",
        options: [{ value: "site-1", label: "Site One" }, { value: "site-2", label: "Site Two" }],
        fetchDisplay: async (h, sid) => {
          const res = await h.query("checkin.receipts", { site_id: sid });
          return (res?.items ?? []).map((r) => r.detail ?? "").join(", ");
        },
      }),
    );
    await waitFor(() => assert.ok(screen.getByText("Site1 initial receipt")));
    fireEvent.click(screen.getByRole("button", { name: "刷新" }));
    const siteSelect = screen.getByLabelText("选择签到站点") as HTMLSelectElement;
    fireEvent.change(siteSelect, { target: { value: "site-2" } });
    await waitFor(() => assert.ok(screen.getByText("Site2 fresh receipt")));
    assert.equal(siteSelect.value, "site-2");
    await act(async () => {
      slowSite1Receipts.resolve(jsonOk({ items: [{ site_id: "site-1", state: "failed", observed_at: "2026-11-25T01:30:00Z", detail: "Stale Site1 slow receipt" }] }));
      await slowSite1Receipts.promise;
    });
    assert.equal((screen.getByLabelText("选择签到站点") as HTMLSelectElement).value, "site-2");
    assert.ok(screen.getByText("Site2 fresh receipt"));
    assert.equal(screen.queryByText("Stale Site1 slow receipt"), null);
  } finally {
    cleanup();
    clearFederationCache();
    globalThis.fetch = origFetch;
  }
});
