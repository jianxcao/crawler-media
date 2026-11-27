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
import type { UiDescriptorView } from "../lib/api/plugins";
import {
  clearFederationCache,
  descriptorCacheKey,
  evictPluginFederationCache,
  type FederatedContainer,
  loadFederatedSurface,
} from "../lib/plugins/federation-loader";
import { extractSchemaDefs, mergeSchemaNode } from "../lib/plugins/schema-helpers";
import { validateAndNormalizeObject } from "../lib/plugins/schema-validation";
import { SessionProvider } from "../lib/session";

function makeDescriptor(overrides: Partial<UiDescriptorView> = {}): UiDescriptorView {
  return {
    plugin_id: "org.example.r11-ui",
    plugin_name: "R11 UI",
    version: "1.0.0",
    generation: 1,
    lifecycle: "enabled",
    source: "external",
    package_hash: "hash-r11-v1",
    package_sha256: "hash-r11-v1",
    bundle_sha256: "bundle-r11-v1",
    subject_user_id: "user-admin",
    subject_role: "admin",
    ui_mode: "federation-react",
    remote_name: "cm_r11_ui",
    entry: "ui/remoteEntry.js",
    entry_url: "/api/v1/plugin-ui-assets/org.example.r11-ui/hash-r11-v1/ui/remoteEntry.js",
    asset_base_url: "/api/v1/plugin-ui-assets/org.example.r11-ui/hash-r11-v1",
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
        id: "main",
        kind: "page",
        expose: "./Page",
        title: "Main Console",
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

test("R11-F1: active descendant secret inside optional nested object enforces required sibling fields", async () => {
  const saves: Array<Record<string, unknown>> = [];
  const schema = {
    type: "object",
    properties: {
      proxy: {
        type: "object",
        title: "Proxy",
        required: ["host"],
        properties: {
          host: { type: "string", title: "Proxy Host" },
          password: {
            type: "string",
            title: "Proxy Password",
            "x-ui": { secret: true },
          },
        },
      },
    },
  };

  try {
    render(
      <PluginSchemaForm
        schema={schema}
        values={{}}
        secrets={{}}
        editable
        sites={[]}
        downloaders={[]}
        onSave={async (vals) => {
          saves.push(vals);
        }}
      />,
    );

    // Replace proxy.password while leaving required sibling proxy.host blank
    fireEvent.change(screen.getByLabelText("proxy.password 操作"), {
      target: { value: "replace" },
    });
    fireEvent.change(screen.getByLabelText("proxy.password 新密钥"), {
      target: { value: "s3cr3t" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));

    assert.match((await screen.findByRole("alert")).textContent ?? "", /proxy\.host/);
    assert.equal(saves.length, 0);
  } finally {
    cleanup();
  }
});

test("R11-F2: clearing an optional child field inside a saved optional nested object preserves the object", async () => {
  const saves: Array<Record<string, unknown>> = [];
  const schema = {
    type: "object",
    properties: {
      proxy: {
        type: "object",
        title: "Proxy",
        required: ["protocol"],
        properties: {
          protocol: {
            type: "string",
            title: "Proxy Protocol",
            enum: ["http", "socks5"],
          },
          username: { type: "string", minLength: 1, title: "Proxy Username" },
          verify_tls: { type: "boolean", title: "Verify TLS" },
        },
      },
    },
  };

  try {
    render(
      <PluginSchemaForm
        schema={schema}
        values={{
          proxy: { protocol: "socks5", username: "alice", verify_tls: true },
        }}
        secrets={{}}
        editable
        sites={[]}
        downloaders={[]}
        onSave={async (vals) => {
          saves.push(vals);
        }}
      />,
    );

    fireEvent.change(screen.getByLabelText("Proxy Username"), { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));

    await waitFor(() => {
      assert.equal(saves.length, 1);
    });
    assert.deepEqual(saves[0], {
      proxy: { protocol: "socks5", verify_tls: true },
    });
  } finally {
    cleanup();
  }
});

test("R11-F3: mergeSchemaNode preserves accumulated $ref patterns on array items across re-merge", () => {
  const rootSchema = {
    $defs: {
      LowerAlpha: { type: "string", pattern: "^[a-z]+$" },
    },
    type: "object",
    properties: {
      tags: {
        type: "array",
        items: {
          $ref: "#/$defs/LowerAlpha",
          pattern: "^a",
        },
      },
    },
  };
  const defs = extractSchemaDefs(rootSchema);
  const mergedRoot = mergeSchemaNode(rootSchema, defs);
  // Validate array item "a123" (matches ^a, violates ^[a-z]+$ from $defs/LowerAlpha)
  const badRes = validateAndNormalizeObject(
    mergedRoot,
    { tags: ["a123"] },
    undefined,
    [],
    defs,
    new Set(),
    false,
    0,
    true,
  );
  assert.equal(badRes.ok, false, "expected ^[a-z]+$ from $ref to reject 'a123'");
});

test("R11-F4: fromJsonLiteral validates null/empty literals strictly and unset enum-only fields omit undefined", () => {
  const schema = {
    type: "object",
    properties: {
      count: { type: "integer" },
      prefix: { type: "string", default: "pre-" },
      mode: { type: "string", enum: ["fast", "slow"] },
      sep: { enum: ["", ",", ";"] },
    },
  };
  const defs = extractSchemaDefs(schema);
  const merged = mergeSchemaNode(schema, defs);

  const nullInt = validateAndNormalizeObject(
    merged,
    { count: null },
    undefined,
    [],
    defs,
    new Set(),
    false,
    0,
    true,
  );
  assert.equal(nullInt.ok, false, "explicit null on integer in JSON literal must fail");

  const emptyEnum = validateAndNormalizeObject(
    merged,
    { mode: "" },
    undefined,
    [],
    defs,
    new Set(),
    false,
    0,
    true,
  );
  assert.equal(emptyEnum.ok, false, "explicit empty string on non-empty enum in JSON literal must fail");

  const emptyDefaultStr = validateAndNormalizeObject(
    merged,
    { prefix: "" },
    undefined,
    [],
    defs,
    new Set(),
    false,
    0,
    true,
  );
  assert.equal(emptyDefaultStr.ok, true);
  if (emptyDefaultStr.ok) {
    assert.deepEqual(emptyDefaultStr.value, { prefix: "" });
  }

  // Form mode (!fromJsonLiteral): unset enum-only field (`sep` omitted) must not leave `{ sep: undefined }`
  const formRes = validateAndNormalizeObject(
    merged,
    {},
    undefined,
    [],
    defs,
    new Set(),
    false,
    0,
    false,
  );
  assert.equal(formRes.ok, true);
  if (formRes.ok) {
    assert.equal("sep" in formRes.value, false);
  }
});

test("R11-F5: federation loader aborts before init on eviction, PluginConfigHost keeps page-only federation cache, and PluginDetailView enforces subject_user_id", async () => {
  clearFederationCache();
  const origFetch = globalThis.fetch;
  try {
    // 1) Eviction while importRemote is in flight must abort BEFORE container.init() runs
    const desc = makeDescriptor();
    let initCalled = false;
    let resolveImport!: (c: FederatedContainer) => void;
    const loadPromise = loadFederatedSurface(desc, "./Page", {
      reloadPage: () => {},
      importRemote: () =>
        new Promise<FederatedContainer>((resolve) => {
          resolveImport = resolve;
        }),
    });
    evictPluginFederationCache(desc.plugin_id);
    resolveImport({
      init: async () => {
        initCalled = true;
      },
      get: async () => () => ({ default: () => <div>Page</div> }),
    });
    await assert.rejects(loadPromise, /evicted while loading/);
    assert.equal(initCalled, false, "evicted container must not call container.init()");

    // 2) Load a valid page-only federated plugin stylesheet, then mount PluginConfigHost:
    //    because uiDesc IS enabled & trusted & subject-matched (just no config surface), stylesheet must NOT be evicted!
    await loadFederatedSurface(desc, "./Page", {
      reloadPage: () => {},
      importRemote: async () => ({
        init: async () => {},
        get: async () => () => ({ default: () => <div>Page</div> }),
      }),
    });
    const expectedKey = descriptorCacheKey(desc);
    assert.equal(
      document.head.querySelectorAll(`link[data-cm-plugin-ui="${expectedKey}"]`).length,
      1,
    );

    globalThis.fetch = ((input: string | URL | Request) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
      if (url.endsWith("/config-schema")) {
        return Promise.resolve(
          new Response(
            JSON.stringify({
              ok: true,
              data: {
                plugin_id: desc.plugin_id,
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
                plugin_id: desc.plugin_id,
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
          new Response(
            JSON.stringify({
              ok: true,
              data: makeDescriptor({ subject_user_id: "other-user" }),
            }),
            { status: 200, headers: { "Content-Type": "application/json" } },
          ),
        );
      }
      if (url.endsWith(`/plugins/${desc.plugin_id}`)) {
        return Promise.resolve(
          new Response(
            JSON.stringify({
              ok: true,
              data: {
                id: desc.plugin_id,
                name: "R11 UI",
                version: "1.0.0",
                source: "external",
                lifecycle: "enabled",
                enabled: true,
                desired_enabled: true,
                generation: 1,
                revision: 1,
                package_hash: desc.package_hash,
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

    const cfgView = render(
      <MemoryRouter>
        <FeedbackProvider>
          <SessionProvider
            value={{
              session: {
                id: "user-admin",
                username: "admin",
                nickname: "Admin",
                avatar_url: null,
                role: "admin",
                capabilities: {
                  allow_subscribe: true,
                  allow_search: true,
                  allow_direct_download: true,
                },
              },
              setSession: () => {},
              loginEpoch: "e1",
            }}
          >
            <PluginConfigHost
              pluginId={desc.plugin_id}
              pluginEditable
              surfaceDeps={{
                fetchDescriptor: async () => desc,
              }}
            />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );
    await waitFor(() => {
      assert.ok(screen.getByRole("button", { name: "保存配置" }));
    });
    assert.equal(
      document.head.querySelectorAll(`link[data-cm-plugin-ui="${expectedKey}"]`).length,
      1,
      "PluginConfigHost must NOT evict active page/app stylesheet when plugin is enabled, trusted, and subject-matched",
    );
    cfgView.unmount();

    // 3) PluginDetailView with mismatched subject_user_id must evict cache and hide console tab
    render(
      <MemoryRouter>
        <FeedbackProvider>
          <SessionProvider
            value={{
              session: {
                id: "user-admin",
                username: "admin",
                nickname: "Admin",
                avatar_url: null,
                role: "admin",
                capabilities: {
                  allow_subscribe: true,
                  allow_search: true,
                  allow_direct_download: true,
                },
              },
              setSession: () => {},
              loginEpoch: "e1",
            }}
          >
            <PluginDetailView pluginId={desc.plugin_id} />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );
    await waitFor(() => {
      assert.ok(screen.getByText("R11 UI"));
    });
    assert.equal(
      screen.queryByRole("button", { name: "控制台" }),
      null,
      "mismatched subject_user_id must not expose console tab",
    );
    assert.equal(
      document.head.querySelectorAll(`link[data-cm-plugin-ui="${expectedKey}"]`).length,
      0,
      "mismatched subject_user_id must evict federation stylesheet",
    );
  } finally {
    globalThis.fetch = origFetch;
    cleanup();
    clearFederationCache();
  }
});
