import "./plugin-ui-env";

import assert from "node:assert/strict";
import test from "node:test";

import type { PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import React from "react";
import { MemoryRouter } from "react-router-dom";

import { FeedbackProvider } from "../components/feedback";
import { PluginConfigHost } from "../components/plugins/PluginConfigHost";
import { PluginDetailView } from "../components/plugins/PluginDetailView";
import { PluginSchemaForm } from "../components/plugins/PluginSchemaForm";
import type { UiDescriptorView } from "../lib/api/plugins";
import {
  clearFederationCache,
  descriptorCacheKey,
  loadFederatedSurface,
  type FederatedContainer,
} from "../lib/plugins/federation-loader";
import { extractSchemaDefs, mergeSchemaNode } from "../lib/plugins/schema-helpers";
import { validateAndNormalizeObject } from "../lib/plugins/schema-validation";
import { SessionProvider } from "../lib/session";

const ADMIN_SESSION = {
  id: "user-admin",
  username: "admin",
  nickname: "Admin",
  avatar_url: null,
  role: "admin" as const,
  capabilities: {
    allow_subscribe: true,
    allow_search: true,
    allow_direct_download: true,
  },
};

function makeDescriptor(overrides: Partial<UiDescriptorView> = {}): UiDescriptorView {
  return {
    plugin_id: "org.example.r12-ui",
    plugin_name: "R12 UI",
    version: "1.0.0",
    generation: 1,
    lifecycle: "disabled",
    source: "external",
    package_hash: "hash-r12-v1",
    package_sha256: "hash-r12-v1",
    bundle_sha256: "bundle-r12-v1",
    subject_user_id: "user-admin",
    subject_role: "admin",
    ui_mode: "federation-react",
    remote_name: "cm_r12_ui",
    entry: "ui/remoteEntry.js",
    entry_url: "/api/v1/plugin-ui-assets/org.example.r12-ui/hash-r12-v1/ui/remoteEntry.js",
    asset_base_url: "/api/v1/plugin-ui-assets/org.example.r12-ui/hash-r12-v1",
    contract: "1.0.0",
    react: "19.3.0",
    react_dom: "19.3.0",
    ui_trusted: true,
    ui_assets_present: true,
    trust: {
      status: "trusted",
      revision: 1,
      granted_at: null,
      granted_by_user_id: null,
      revoked_at: null,
    },
    capabilities: [],
    styles: ["ui/assets/style.css"],
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
    ...overrides,
  };
}

test("R12-F1: disabled plugin retains federation cache when config surface is mountable and evicts when no surface is mountable", async () => {
  clearFederationCache();
  const origFetch = globalThis.fetch;
  try {
    const disabledWithConfig = makeDescriptor({ lifecycle: "disabled" });
    await loadFederatedSurface(disabledWithConfig, "./Config", {
      reloadPage: () => {},
      importRemote: async () => ({
        init: async () => {},
        get: async () => () => ({ default: () => <div>Federated Config</div> }),
      }),
    });
    const key1 = descriptorCacheKey(disabledWithConfig);
    assert.equal(document.head.querySelectorAll(`link[data-cm-plugin-ui="${key1}"]`).length, 1);

    let currentUi: UiDescriptorView = disabledWithConfig;
    globalThis.fetch = ((input: string | URL | Request) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
      if (url.endsWith("/config-schema")) {
        return Promise.resolve(
          new Response(
            JSON.stringify({
              ok: true,
              data: {
                plugin_id: disabledWithConfig.plugin_id,
                schema_revision: "r1",
                schema: { type: "object", properties: {} },
              },
            }),
            { status: 200, headers: { "Content-Type": "application/json" } },
          ),
        );
      }
      if (url.endsWith("/config")) {
        return Promise.resolve(
          new Response(
            JSON.stringify({
              ok: true,
              data: {
                plugin_id: disabledWithConfig.plugin_id,
                schema_revision: "r1",
                revision: 1,
                values: {},
                secrets: {},
              },
            }),
            { status: 200, headers: { "Content-Type": "application/json" } },
          ),
        );
      }
      if (url.endsWith("/ui")) {
        return Promise.resolve(
          new Response(JSON.stringify({ ok: true, data: currentUi }), {
            status: 200,
            headers: { "Content-Type": "application/json" },
          }),
        );
      }
      if (url.endsWith(`/plugins/${disabledWithConfig.plugin_id}`)) {
        return Promise.resolve(
          new Response(
            JSON.stringify({
              ok: true,
              data: {
                id: disabledWithConfig.plugin_id,
                name: "R12 UI",
                version: "1.0.0",
                source: "external",
                lifecycle: "disabled",
                enabled: false,
                desired_enabled: false,
                generation: 1,
                revision: 1,
                package_hash: disabledWithConfig.package_hash,
                signature_status: "trusted",
                credential_bound: false,
                description: "desc",
                authors: ["Test"],
                license: "MIT",
                api: "1.0.0",
                state_schema: 1,
                has_ui: true,
                ui_trusted: true,
                active_operation_id: null,
                failure_reason: null,
              },
            }),
            { status: 200, headers: { "Content-Type": "application/json" } },
          ),
        );
      }
      if (url.includes("/sites") || url.includes("/downloaders") || url.endsWith("/events")) {
        return Promise.resolve(
          new Response(JSON.stringify({ ok: true, data: [] }), {
            status: 200,
            headers: { "Content-Type": "application/json" },
          }),
        );
      }
      return Promise.resolve(
        new Response(JSON.stringify({ ok: true, data: { items: [], next_cursor: null } }), {
          status: 200,
          headers: { "Content-Type": "application/json" },
        }),
      );
    }) as typeof fetch;

    const cfgRender = render(
      <MemoryRouter>
        <FeedbackProvider>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "e1" }}>
            <PluginConfigHost
              pluginId={disabledWithConfig.plugin_id}
              pluginEditable
              credentialBound={false}
              surfaceDeps={{
                fetchDescriptor: async () => disabledWithConfig,
                fetchContract: async () => ({
                  contract: "1.0.0",
                  react: "19.3.0",
                  react_dom: "19.3.0",
                  plugin_ui_sdk: "1.0.0",
                  plugin_ui_kit: "1.0.0",
                  federation_runtime_version: "0.10",
                  sdk_methods: [],
                }),
                loadSurface: async () => () => <div>Federated Config Loaded</div>,
              }}
            />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );
    await waitFor(() => {
      assert.ok(screen.getByText("Federated Config Loaded"));
    });
    assert.equal(
      document.head.querySelectorAll(`link[data-cm-plugin-ui="${key1}"]`).length,
      1,
      "Disabled plugin with mountable config surface must retain federation stylesheet",
    );
    cfgRender.unmount();

    // Now no surface is mountable -> PluginDetailView MUST evict federation cache
    currentUi = makeDescriptor({
      lifecycle: "disabled",
      surfaces: [
        {
          id: "main",
          kind: "page",
          expose: "./Page",
          title: "Main",
          icon: null,
          location: null,
          mountable: false,
          editable: false,
          required_role: "admin",
        },
      ],
    });
    render(
      <MemoryRouter>
        <FeedbackProvider>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "e1" }}>
            <PluginDetailView pluginId={disabledWithConfig.plugin_id} />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );
    await waitFor(() => {
      assert.ok(screen.getByText("R12 UI"));
    });
    assert.equal(
      document.head.querySelectorAll(`link[data-cm-plugin-ui="${key1}"]`).length,
      0,
      "PluginDetailView must evict federation cache when zero surfaces are mountable",
    );
  } finally {
    globalThis.fetch = origFetch;
    cleanup();
    clearFederationCache();
  }
});

test("R12-F2: schema-validation preserves optional nested defaults, validates active-secret parents strictly, and strips blank constrained strings under hideNonSecrets", () => {
  const schema = {
    type: "object",
    required: ["auth"],
    properties: {
      opts: {
        type: "object",
        properties: {
          verify_tls: { type: "boolean", default: true },
          timeout: { type: "integer", default: 30 },
          label: { type: "string", minLength: 1 },
        },
      },
      auth: {
        type: "object",
        properties: {
          token: { type: "string", "x-ui": { secret: true } },
        },
      },
      tag: { type: "string", minLength: 2, pattern: "^[a-z]+$", default: "v1" },
    },
  };
  const defs = extractSchemaDefs(schema);
  const merged = mergeSchemaNode(schema, defs);
  const secretSet = new Set(["auth.token"]);
  const activeSecrets = new Set(["auth.token"]);

  // 1) Activating optional nested object with default values when initialObj === undefined must preserve opts
  const resNewOpts = validateAndNormalizeObject(
    merged,
    { opts: { verify_tls: true, timeout: 30 }, auth: {} },
    {},
    [],
    defs,
    secretSet,
    false,
    0,
    false,
    activeSecrets,
  );
  assert.equal(resNewOpts.ok, true);
  if (resNewOpts.ok) {
    assert.deepEqual(resNewOpts.value.opts, { verify_tls: true, timeout: 30 });
  }

  // 2) Clearing optional label when timeout: 30 is still set must preserve saved opts
  const resSavedOpts = validateAndNormalizeObject(
    merged,
    { opts: { label: "", timeout: 30, verify_tls: true }, auth: {} },
    { opts: { label: "prod", timeout: 30, verify_tls: true }, auth: {} },
    [],
    defs,
    secretSet,
    false,
    0,
    false,
    activeSecrets,
  );
  assert.equal(resSavedOpts.ok, true);
  if (resSavedOpts.ok) {
    assert.deepEqual(resSavedOpts.value.opts, { timeout: 30, verify_tls: true });
  }

  // 3) fromJsonLiteral with missing required parent of active secret must fail 为必填项
  const resJsonMissing = validateAndNormalizeObject(
    merged,
    {},
    undefined,
    [],
    defs,
    secretSet,
    false,
    0,
    true,
    activeSecrets,
  );
  assert.equal(resJsonMissing.ok, false, "missing required auth in JSON literal must fail");

  // 4) Non-object value ("bad") on parent of active secret must fail 必须是对象 even in form mode
  const resBadAuth = validateAndNormalizeObject(
    merged,
    { auth: "bad" },
    undefined,
    [],
    defs,
    secretSet,
    false,
    0,
    false,
    activeSecrets,
  );
  assert.equal(resBadAuth.ok, false, "non-object auth must fail 必须是对象");

  // 5) hideNonSecrets save must strip blank optional constrained/default string `tag: ""`
  const resHidden = validateAndNormalizeObject(
    merged,
    { tag: "" },
    {},
    [],
    defs,
    secretSet,
    true,
    0,
    false,
    activeSecrets,
  );
  assert.equal(resHidden.ok, true);
  if (resHidden.ok) {
    assert.equal("tag" in resHidden.value, false, "blank optional constrained tag must be omitted");
  }
});

test("R12-F3: PluginSchemaForm, BoostConfig, and CheckinConfig render stale saved site/downloader IDs so admins can clear them", async () => {
  try {
    // 1) PluginSchemaForm with empty sites/downloaders catalogs but stale saved IDs
    const schemaSaves: Array<Record<string, unknown>> = [];
    const schema = {
      type: "object",
      properties: {
        sites: {
          type: "array",
          title: "参与站点",
          items: { type: "string", "x-ui": { widget: "site" } },
        },
        downloader_id: {
          type: "string",
          title: "下载器",
          "x-ui": { widget: "downloader" },
        },
      },
    };
    const r1 = render(
      <PluginSchemaForm
        schema={schema}
        values={{ sites: ["deleted-site-1"], downloader_id: "deleted-dl-1" }}
        secrets={{}}
        editable
        sites={[]}
        downloaders={[]}
        onSave={async (vals) => {
          schemaSaves.push(vals);
        }}
      />,
    );
    const staleSiteCheckbox = screen.getByLabelText(/deleted-site-1/);
    assert.equal((staleSiteCheckbox as HTMLInputElement).checked, true);
    const dlSelect = screen.getByLabelText("下载器") as HTMLSelectElement;
    assert.equal(dlSelect.value, "deleted-dl-1");
    fireEvent.click(staleSiteCheckbox);
    fireEvent.change(dlSelect, { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => assert.equal(schemaSaves.length, 1));
    assert.deepEqual(schemaSaves[0], { sites: [] });
    r1.unmount();

    // 2) cm.boost Config federated surface with empty catalogs and stale sites + downloader_id
    clearFederationCache();
    const BoostStaleStub: React.FC<PluginSurfaceProps> = ({ host, config }) => {
      const vals = config?.values ?? {};
      const initialSites = Array.isArray(vals.sites) ? vals.sites.map(String) : [];
      const [sites, setSites] = React.useState<string[]>(initialSites);
      const [dlId, setDlId] = React.useState(typeof vals.downloader_id === "string" ? vals.downloader_id : "");
      const [catalogSites, setCatalogSites] = React.useState<string[]>([]);
      const [catalogDls, setCatalogDls] = React.useState<string[]>([]);
      React.useEffect(() => {
        const cats = (host as unknown as { catalogs?: { list: (k: string) => Promise<{ items: Array<{ id: string }> }> } }).catalogs;
        void cats?.list("sites").then((r) => setCatalogSites(r.items.map((x) => x.id)));
        void cats?.list("downloaders").then((r) => setCatalogDls(r.items.map((x) => x.id)));
      }, [host]);
      const allSites = Array.from(new Set([...catalogSites, ...initialSites]));
      const allDls = Array.from(new Set([...catalogDls, ...(dlId ? [dlId] : [])]));
      return (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            const out: Record<string, unknown> = { ...vals, sites };
            if (dlId) out.downloader_id = dlId;
            else delete out.downloader_id;
            void config?.save(out);
          }}
        >
          {allSites.map((sid) => (
            <label key={sid}>
              <input
                type="checkbox"
                checked={sites.includes(sid)}
                onChange={(e) =>
                  setSites((prev) => (e.target.checked ? [...prev, sid] : prev.filter((x) => x !== sid)))
                }
              />
              {sid}
            </label>
          ))}
          <label>
            目标下载器
            <select value={dlId} onChange={(e) => setDlId(e.target.value)}>
              <option value="">默认</option>
              {allDls.map((id) => (
                <option key={id} value={id}>
                  {id}
                </option>
              ))}
            </select>
          </label>
          <button type="submit">保存刷流配置</button>
        </form>
      );
    };
    const boostContainer: FederatedContainer = {
      init: () => {},
      get: async () => () => ({ default: BoostStaleStub }),
    };
    const BoostConfig = await loadFederatedSurface(
      makeDescriptor({
        plugin_id: "cm.boost",
        remote_name: "cmBoost",
        package_hash: "cm:boost:1.0.0",
        entry_url: "/api/v1/plugin-ui-assets/cm.boost/cm:boost:1.0.0/ui/assets/remoteEntry.js",
        styles: [],
      }),
      "./Config",
      { fetchManifest: async () => [], importRemote: async () => boostContainer },
    );
    const stubHost = {
      catalogs: {
        list: async () => ({ items: [] }),
      },
    } as never;
    const boostSaves: Array<Record<string, unknown>> = [];
    const r2 = render(
      <BoostConfig
        host={stubHost}
        surfaceId="settings"
        config={{
          schemaRevision: "1",
          revision: "1",
          values: { sites: ["stale-boost-site"], downloader_id: "stale-boost-dl" },
          editable: true,
          onDraftChange: () => {},
          save: async (vals) => {
            boostSaves.push(vals);
          },
        }}
      />,
    );
    const boostSiteCb = await screen.findByLabelText(/stale-boost-site/);
    assert.equal((boostSiteCb as HTMLInputElement).checked, true);
    const boostDlSelect = screen.getByLabelText("目标下载器") as HTMLSelectElement;
    assert.equal(boostDlSelect.value, "stale-boost-dl");
    fireEvent.click(boostSiteCb);
    fireEvent.change(boostDlSelect, { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: "保存刷流配置" }));
    await waitFor(() => assert.equal(boostSaves.length, 1));
    assert.deepEqual(boostSaves[0].sites, []);
    assert.equal("downloader_id" in boostSaves[0], false);
    r2.unmount();

    // 3) cm.checkin Config federated surface with empty catalogs and stale site_id
    const CheckinStaleStub: React.FC<PluginSurfaceProps> = ({ host, config }) => {
      const vals = config?.values ?? {};
      const [siteId, setSiteId] = React.useState(typeof vals.site_id === "string" ? vals.site_id : "");
      const [catalogSites, setCatalogSites] = React.useState<string[]>([]);
      React.useEffect(() => {
        const cats = (host as unknown as { catalogs?: { list: (k: string) => Promise<{ items: Array<{ id: string }> }> } }).catalogs;
        void cats?.list("sites").then((r) => setCatalogSites(r.items.map((x) => x.id)));
      }, [host]);
      const allSites = Array.from(new Set([...catalogSites, ...(siteId ? [siteId] : [])]));
      return (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            const out: Record<string, unknown> = { ...vals };
            if (siteId) out.site_id = siteId;
            else delete out.site_id;
            void config?.save(out);
          }}
        >
          <label>
            默认站点
            <select value={siteId} onChange={(e) => setSiteId(e.target.value)}>
              <option value="">全部站点</option>
              {allSites.map((id) => (
                <option key={id} value={id}>
                  {id}
                </option>
              ))}
            </select>
          </label>
          <button type="submit">保存签到设置</button>
        </form>
      );
    };
    const checkinContainer: FederatedContainer = {
      init: () => {},
      get: async () => () => ({ default: CheckinStaleStub }),
    };
    const CheckinConfig = await loadFederatedSurface(
      makeDescriptor({
        plugin_id: "cm.checkin",
        remote_name: "cmCheckin",
        package_hash: "cm:checkin:1.0.0",
        entry_url: "/api/v1/plugin-ui-assets/cm.checkin/cm:checkin:1.0.0/ui/assets/remoteEntry.js",
        styles: [],
      }),
      "./Config",
      { fetchManifest: async () => [], importRemote: async () => checkinContainer },
    );
    const checkinSaves: Array<Record<string, unknown>> = [];
    render(
      <CheckinConfig
        host={stubHost}
        surfaceId="settings"
        config={{
          schemaRevision: "1",
          revision: "1",
          values: { interval_secs: 86400, site_id: "stale-checkin-site" },
          editable: true,
          onDraftChange: () => {},
          save: async (vals) => {
            checkinSaves.push(vals);
          },
        }}
      />,
    );
    const checkinSelect = (await screen.findByLabelText(/默认站点/)) as HTMLSelectElement;
    assert.equal(checkinSelect.value, "stale-checkin-site");
    fireEvent.change(checkinSelect, { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: "保存签到设置" }));
    await waitFor(() => assert.equal(checkinSaves.length, 1));
    assert.equal("site_id" in checkinSaves[0], false);
  } finally {
    cleanup();
    clearFederationCache();
  }
});
