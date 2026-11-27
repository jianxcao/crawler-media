import "./plugin-ui-env";

import assert from "node:assert/strict";
import test from "node:test";

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import React from "react";
import { MemoryRouter } from "react-router-dom";

import { FeedbackProvider } from "../components/feedback";
import { PluginConfigHost } from "../components/plugins/PluginConfigHost";
import { PluginDetailView } from "../components/plugins/PluginDetailView";
import { PluginSchemaForm } from "../components/plugins/PluginSchemaForm";
import { PluginSurfaceHost } from "../components/plugins/PluginSurfaceHost";
import type { SessionView } from "../lib/api/auth";
import { buildInitialValues, validateAndNormalizeObject } from "../lib/plugins/schema-helpers";
import { SessionProvider } from "../lib/session";

test("R9-F2: closed objects strip and reject undeclared keys even when additionalProperties is omitted", () => {
  const schema = {
    type: "object",
    properties: {
      note: { type: "string" },
      rules: {
        type: "array",
        items: {
          type: "object",
          required: ["name"],
          properties: {
            name: { type: "string" },
          },
        },
      },
    },
  };

  const init = buildInitialValues(
    schema,
    {},
    { note: "ok", legacy_flag: true },
    new Map(),
    new Set(),
    false,
  );
  assert.deepEqual(init, { note: "ok" });

  const badArr = validateAndNormalizeObject(
    schema,
    { note: "ok", rules: [{ name: "r1", extra: 1 }] },
    undefined,
    [],
    {},
    new Set(),
    false,
  );
  assert.equal(badArr.ok, false);
});

test("R9-F3 & R9-F5: saved optional nested objects with default-bearing siblings clear to omitted and required empty-enum is accepted", async () => {
  const saved: Array<Record<string, unknown>> = [];
  try {
    render(
      <PluginSchemaForm
        schema={{
          type: "object",
          required: ["sep"],
          properties: {
            sep: {
              type: "string",
              title: "Separator",
              enum: ["", ",", ";"],
            },
            proxy: {
              type: "object",
              title: "Proxy",
              required: ["host", "protocol"],
              properties: {
                host: { type: "string", title: "Proxy Host" },
                protocol: {
                  type: "string",
                  title: "Proxy Protocol",
                  enum: ["http", "socks5"],
                  default: "http",
                },
                verify_tls: {
                  type: "boolean",
                  title: "Verify TLS",
                  default: true,
                },
              },
            },
          },
        }}
        values={{
          sep: "",
          proxy: { host: "10.0.0.1", protocol: "socks5", verify_tls: true },
        }}
        secrets={{}}
        editable
        sites={[]}
        downloaders={[]}
        onSave={async (vals) => {
          saved.push(vals);
        }}
      />,
    );

    const hostInput = screen.getByLabelText("Proxy Host") as HTMLInputElement;
    fireEvent.change(hostInput, { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));

    await waitFor(() => assert.equal(saved.length, 1));
    assert.deepEqual(saved[0], { sep: "" });
  } finally {
    cleanup();
  }
});

test("R9-F4: secretSiteId preserves explicit blank on unchanged values re-render, strips site: prefix, and rejects missing secrets entry", async () => {
  const calls: Array<{ values: Record<string, unknown>; siteId?: string }> = [];
  try {
    const schema = {
      type: "object",
      required: ["api_token"],
      properties: {
        site_id: { type: "string", title: "Bound Site", "x-ui": { widget: "site" } },
        sites: { type: "array", items: { type: "string" } },
        api_token: { type: "string", "x-ui": { secret: true } },
      },
    };

    // 4.4: Missing entry in `secrets` (`secrets = {}`) for non-credential-bound required secret rejects `keep`
    const nonCb = render(
      <PluginSchemaForm
        schema={schema}
        values={{}}
        secrets={{}}
        editable
        sites={[]}
        downloaders={[]}
        onSave={async (v, _s, sid) => {
          calls.push({ values: v, siteId: sid });
        }}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    assert.ok(await screen.findByRole("alert"));
    assert.equal(calls.length, 0);
    nonCb.unmount();

    // 4.1 & 4.3: Explicit `""` in `#plugin-cfg-secret-site-id` survives re-render & `"site:<uuid>"` is stripped
    const uuidA = "11111111-2222-3333-4444-555555555555";
    const cb = render(
      <PluginSchemaForm
        schema={schema}
        values={{ site_id: "", sites: [`site:${uuidA}`] }}
        secrets={{ api_token: { configured: true } }}
        editable
        credentialBound
        sites={[{ id: `site:${uuidA}`, name: "Site A" }]}
        downloaders={[]}
        onSave={async (v, _s, sid) => {
          calls.push({ values: v, siteId: sid });
        }}
      />,
    );

    fireEvent.change(screen.getByLabelText("api_token 操作"), { target: { value: "replace" } });
    fireEvent.change(screen.getByLabelText("api_token 新密钥"), { target: { value: "tok-123" } });

    const secretSiteSel = screen.getByLabelText("凭证绑定站点 (site_id)") as HTMLSelectElement;
    assert.equal(secretSiteSel.value, `site:${uuidA}`);

    // User explicitly selects `""` ("请选择站点"), then parent re-renders with new `values` reference of same bound site
    fireEvent.change(secretSiteSel, { target: { value: "" } });
    cb.rerender(
      <PluginSchemaForm
        schema={schema}
        values={{ site_id: "", sites: [`site:${uuidA}`] }}
        secrets={{ api_token: { configured: true } }}
        editable
        credentialBound
        sites={[{ id: `site:${uuidA}`, name: "Site A" }]}
        downloaders={[]}
        onSave={async (v, _s, sid) => {
          calls.push({ values: v, siteId: sid });
        }}
      />,
    );
    assert.equal(secretSiteSel.value, "");

    // Now select `site:${uuidA}` and submit -> `siteId` passed to `onSave` must be bare `uuidA`!
    fireEvent.change(secretSiteSel, { target: { value: `site:${uuidA}` } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => assert.equal(calls.length, 1));
    assert.equal(calls[0].siteId, uuidA);
  } finally {
    cleanup();
  }
});

test("R9-F6: PluginDetailView evicts federation stylesheet/container cache when UI trust is false while PluginSurfaceHost is unmounted", async () => {
  const pid = "org.example.r9-evict";
  const origFetch = globalThis.fetch;
  const link = document.createElement("link");
  link.rel = "stylesheet";
  link.setAttribute("data-cm-plugin-ui", `${pid}:deadbeef`);
  document.head.appendChild(link);

  const adminSession: SessionView = {
    id: "user-admin-r9",
    username: "admin",
    nickname: "Admin",
    avatar_url: null,
    role: "admin",
    capabilities: { allow_subscribe: true, allow_search: true, allow_direct_download: true },
  };

  try {
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
      if (url.endsWith(`/api/v1/plugins/${pid}`)) {
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              id: pid,
              name: "R9 Evict",
              version: "1.0.0",
              package_hash: "deadbeef",
              lifecycle: "enabled",
              desired_enabled: true,
              revision: 2,
              config_revision: 1,
              generation: 1,
              credential_bound: false,
              signature_status: "unsigned_dev",
              description: "d",
              authors: ["t"],
              license: "MIT",
              api: "1.0.0",
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (url.endsWith(`/api/v1/plugins/${pid}/ui`)) {
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              plugin_id: pid,
              package_hash: "deadbeef",
              generation: 1,
              lifecycle: "enabled",
              contract: "1.0.0",
              ui_trusted: false,
              ui_assets_present: true,
              entry_url: `/api/v1/plugin-ui-assets/${pid}/deadbeef/remoteEntry.js`,
              capabilities: [],
              surfaces: [],
              navigation: [],
              trust: null,
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

    render(
      <MemoryRouter initialEntries={[`/settings/plugins/${pid}?tab=overview`]}>
        <FeedbackProvider>
          <SessionProvider
            value={{ session: adminSession, setSession: () => {}, loginEpoch: "r9-c" }}
          >
            <PluginDetailView pluginId={pid} />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );

    await waitFor(() => {
      assert.equal(document.head.querySelector(`link[data-cm-plugin-ui^="${pid}:"]`), null);
    });
  } finally {
    globalThis.fetch = origFetch;
    link.remove();
    cleanup();
  }
});

test("PluginPermissionsTab disables grant-all while enabled and only revokes granted capabilities without status field", async () => {
  const pid = "org.example.perm-guard";
  const origFetch = globalThis.fetch;
  const revokeBodies: Array<Record<string, unknown>> = [];
  const adminSession: SessionView = {
    id: "user-admin-perms",
    username: "admin",
    nickname: "Admin",
    avatar_url: null,
    role: "admin",
    capabilities: { allow_subscribe: true, allow_search: true, allow_direct_download: true },
  };

  try {
    globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
      if (url.endsWith(`/api/v1/plugins/${pid}`)) {
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              id: pid,
              name: "Perm Guard",
              version: "1.0.0",
              package_hash: "pkg-perm",
              source: "external",
              revision: 5,
              config_revision: 1,
              generation: 1,
              enabled: true,
              lifecycle: "enabled",
              health: "healthy",
              required_grants: ["kv", "http"],
              effective_grants: [],
              active_operation_id: null,
              failure_reason: null,
              signing_key_id: null,
              state_schema: 1,
              credential_bound: false,
              task_count: 0,
              version_count: 1,
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (url.endsWith(`/api/v1/plugins/${pid}/permissions`)) {
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              plugin_id: pid,
              package_hash: "pkg-perm",
              revision: 5,
              required_capabilities: ["kv", "http"],
              effective_grants: [
                { capability: "kv", scope: "Global", sensitive: false, status: "granted" },
                { capability: "http", scope: "Global", sensitive: false, status: "revoked" },
              ],
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (url.endsWith(`/api/v1/plugins/${pid}/permissions/revoke`)) {
        revokeBodies.push(JSON.parse(String(init?.body ?? "{}")));
        return new Response(
          JSON.stringify({
            ok: true,
            data: { operation_id: "op-rev-1", state: "succeeded", phase: "done" },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (url.endsWith(`/api/v1/plugins/${pid}/ui`)) {
        return new Response(
          JSON.stringify({ ok: false, error: { code: "plugin.no_ui", message: "headless" } }),
          { status: 404, headers: { "Content-Type": "application/json" } },
        );
      }
      return new Response(JSON.stringify({ ok: true, data: [] }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }) as typeof fetch;

    render(
      <MemoryRouter initialEntries={[`/settings/plugins/${pid}?tab=permissions`]}>
        <FeedbackProvider>
          <SessionProvider
            value={{ session: adminSession, setSession: () => {}, loginEpoch: "perm-1" }}
          >
            <PluginDetailView pluginId={pid} />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );

    const grantBtn = (await screen.findByRole("button", {
      name: "同步授予全部声明能力",
    })) as HTMLButtonElement;
    assert.equal(grantBtn.disabled, true, "enabled plugin cannot grant capabilities without draining first");
    assert.ok(screen.getByText(/请先停用插件/));

    const revokeBtns = screen.getAllByRole("button", { name: "撤销" });
    assert.equal(revokeBtns.length, 1, "revoked capability row must not render an active revoke button");
    assert.ok(screen.getByText("已撤销"));

    fireEvent.click(revokeBtns[0]!);
    await waitFor(() => assert.equal(revokeBodies.length, 1));
    assert.deepEqual(revokeBodies[0], {
      grants: [{ capability: "kv", scope: "Global", sensitive: false }],
      reason: "admin-ui-revoke",
    });
  } finally {
    globalThis.fetch = origFetch;
    cleanup();
  }
});

test("PluginConfigHost refuses federation remote and stays read-only while draining, and PluginAuditTab avoids duplicate event_domain prefix", async () => {
  const pid = "org.example.draining-audit";
  const origFetch = globalThis.fetch;
  let remoteImports = 0;
  const adminSession: SessionView = {
    id: "user-admin-drain",
    username: "admin",
    nickname: "Admin",
    avatar_url: null,
    role: "admin",
    capabilities: { allow_subscribe: true, allow_search: true, allow_direct_download: true },
  };

  try {
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
      if (url.endsWith(`/api/v1/plugins/${pid}`)) {
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              id: pid,
              name: "Draining Audit",
              version: "1.0.0",
              package_hash: "pkg-drain",
              source: "external",
              revision: 4,
              config_revision: 1,
              generation: 2,
              enabled: false,
              lifecycle: "draining",
              health: "healthy",
              required_grants: [],
              effective_grants: [],
              active_operation_id: "op-drain-1",
              failure_reason: null,
              signing_key_id: null,
              state_schema: 1,
              credential_bound: false,
              task_count: 0,
              version_count: 1,
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (url.endsWith(`/api/v1/plugins/${pid}/config-schema`)) {
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              plugin_id: pid,
              schema_revision: "s1",
              schema: {
                type: "object",
                additionalProperties: false,
                properties: { note: { type: "string", title: "备注" } },
              },
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (url.endsWith(`/api/v1/plugins/${pid}/config`)) {
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              plugin_id: pid,
              revision: 4,
              config_revision: 1,
              schema_revision: "s1",
              values: { note: "hello" },
              secrets: {},
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (url.endsWith(`/api/v1/plugins/${pid}/ui`)) {
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              plugin_id: pid,
              plugin_name: "Draining Audit",
              version: "1.0.0",
              package_hash: "pkg-drain",
              package_sha256: "pkg-drain",
              bundle_sha256: "b-drain",
              generation: 2,
              lifecycle: "draining",
              source: "external",
              subject_user_id: "user-admin-drain",
              subject_role: "admin",
              ui_mode: "federation-react",
              remote_name: "drainRemote",
              entry: "ui/assets/remoteEntry.js",
              entry_url: `/api/v1/plugin-ui-assets/${pid}/pkg-drain/ui/assets/remoteEntry.js`,
              asset_base_url: `/api/v1/plugin-ui-assets/${pid}/pkg-drain`,
              contract: "1.0.0",
              react: "19.3.0",
              react_dom: "19.3.0",
              ui_trusted: true,
              ui_assets_present: true,
              capabilities: [],
              surfaces: [
                {
                  id: "settings",
                  kind: "config",
                  expose: "./Config",
                  title: "Settings",
                  mountable: true,
                  editable: true,
                  required_role: "admin",
                },
              ],
              navigation: [],
              trust: { status: "trusted", revision: 1, granted_at: null, granted_by_user_id: "u", revoked_at: null },
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (url.includes(`/api/v1/plugins/${pid}/events`)) {
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              items: [
                {
                  event_id: "ev-1",
                  event_domain: "site",
                  event_kind: "site.checkin.after",
                  status: "delivered",
                  attempt_count: 1,
                  last_error: null,
                },
              ],
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

    render(
      <MemoryRouter>
        <FeedbackProvider>
          <SessionProvider
            value={{ session: adminSession, setSession: () => {}, loginEpoch: "drain-1" }}
          >
            <PluginConfigHost
              pluginId={pid}
              surfaceDeps={{
                loadSurface: async () => {
                  remoteImports += 1;
                  return () => null;
                },
              }}
            />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );

    const saveBtn = (await screen.findByRole("button", { name: "保存配置" })) as HTMLButtonElement;
    assert.equal(saveBtn.disabled, true);
    assert.equal(remoteImports, 0);
    cleanup();

    render(
      <MemoryRouter initialEntries={[`/settings/plugins/${pid}?tab=audit`]}>
        <FeedbackProvider>
          <SessionProvider
            value={{ session: adminSession, setSession: () => {}, loginEpoch: "drain-1" }}
          >
            <PluginDetailView pluginId={pid} />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );

    await screen.findByText("site.checkin.after");
    assert.equal(screen.queryByText("site.site.checkin.after"), null);
  } finally {
    globalThis.fetch = origFetch;
    cleanup();
  }
});

test("PluginConfigHost preserves federation stylesheet when Config unmounts due to active_operation_id while Page remains mountable", async () => {
  const pid = "org.example.op-active-ui";
  const origFetch = globalThis.fetch;
  let configMountable = true;
  let assetsPresent = true;
  const link = document.createElement("link");
  link.setAttribute("data-cm-plugin-ui", `${pid}:deadbeef:styles.css`);
  document.head.appendChild(link);
  const adminSession: SessionView = {
    id: "user-admin-op",
    username: "admin",
    nickname: "Admin",
    avatar_url: null,
    role: "admin",
    capabilities: { allow_subscribe: true, allow_search: true, allow_direct_download: true },
  };

  const jsonOk = (data: unknown) =>
    new Response(JSON.stringify({ ok: true, data }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });

  try {
    globalThis.fetch = (async (input: RequestInfo | URL) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
      if (url.endsWith("/api/v1/plugin-ui-contract")) {
        return jsonOk({
          contract: "1.0.0",
          react: "19.3.0",
          react_dom: "19.3.0",
          theme_schema_version: "1.0.0",
          sdk: "@crawler-media/plugin-ui@1.0.0",
          sdk_methods: [],
          shared: {
            react: { singleton: true, required_version: "19.3.0" },
            "react-dom": { singleton: true, required_version: "19.3.0" },
          },
        });
      }
      if (url.endsWith(`/api/v1/plugins/${pid}/config-schema`)) {
        return jsonOk({
          plugin_id: pid,
          schema: { type: "object", additionalProperties: false, properties: { title: { type: "string" } } },
          schema_revision: "s1",
        });
      }
      if (url.endsWith(`/api/v1/plugins/${pid}/config`)) {
        return jsonOk({
          plugin_id: pid,
          values: { title: "ok" },
          secrets: {},
          revision: 1,
          config_revision: 1,
          schema_revision: "s1",
        });
      }
      if (url.endsWith(`/api/v1/plugins/${pid}/ui`)) {
        return jsonOk({
          plugin_id: pid,
          plugin_name: "Op Active UI",
          version: "1.0.0",
          package_hash: "deadbeef",
          package_sha256: "deadbeef",
          bundle_sha256: "b-op",
          generation: 1,
          lifecycle: "enabled",
          source: "external",
          subject_user_id: adminSession.id,
          subject_role: "admin",
          ui_mode: "federation-react",
          remote_name: "opActiveRemote",
          entry: "ui/assets/remoteEntry.js",
          entry_url: `/api/v1/plugin-ui-assets/${pid}/deadbeef/ui/assets/remoteEntry.js`,
          asset_base_url: `/api/v1/plugin-ui-assets/${pid}/deadbeef`,
          contract: "1.0.0",
          react: "19.3.0",
          react_dom: "19.3.0",
          ui_trusted: true,
          ui_assets_present: assetsPresent,
          capabilities: [],
          surfaces: [
            { id: "cfg", kind: "config", expose: "./Config", title: "Cfg", mountable: configMountable, editable: false, location: null, required_role: "admin" },
            { id: "main", kind: "page", expose: "./Page", title: "Main", mountable: true, editable: false, location: `/settings/plugins/${pid}?tab=page&view=main`, required_role: "admin" },
          ],
          styles: [],
          trust: { status: "trusted", revision: 1, granted_at: null, granted_by_user_id: null, revoked_at: null },
        });
      }
      return jsonOk([]);
    }) as typeof fetch;

    const { rerender } = render(
      <MemoryRouter>
        <FeedbackProvider>
          <SessionProvider
            value={{ session: adminSession, setSession: () => {}, loginEpoch: "op-1" }}
          >
            <PluginConfigHost
              pluginId={pid}
              reloadKey="1:enabled:1::1"
              pluginEditable={false}
              credentialBound={false}
              surfaceDeps={{ loadSurface: async () => () => <div>FedCfg</div> }}
            />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );
    await screen.findByText("FedCfg");

    configMountable = false;
    rerender(
      <MemoryRouter>
        <FeedbackProvider>
          <SessionProvider
            value={{ session: adminSession, setSession: () => {}, loginEpoch: "op-1" }}
          >
            <PluginConfigHost
              pluginId={pid}
              reloadKey="1:enabled:1:op-99:1"
              pluginEditable={false}
              credentialBound={false}
              surfaceDeps={{ loadSurface: async () => () => <div>FedCfg</div> }}
            />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );
    await screen.findByRole("button", { name: "保存配置" });
    assert.notEqual(
      document.head.querySelector(`link[data-cm-plugin-ui="${pid}:deadbeef:styles.css"]`),
      null,
      "federation stylesheet must remain cached while Page surface is still mountable",
    );

    assetsPresent = false;
    rerender(
      <MemoryRouter>
        <FeedbackProvider>
          <SessionProvider
            value={{ session: adminSession, setSession: () => {}, loginEpoch: "op-1" }}
          >
            <PluginSurfaceHost pluginId={pid} surfaceId="main" expectedKind="page" />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );
    await screen.findByText("插件 UI 资产不可用");
    assert.equal(
      document.head.querySelector(`link[data-cm-plugin-ui="${pid}:deadbeef:styles.css"]`),
      null,
      "missing UI assets must evict cached federation stylesheet even if surfaces report mountable",
    );
  } finally {
    globalThis.fetch = origFetch;
    link.remove();
    cleanup();
  }
});
