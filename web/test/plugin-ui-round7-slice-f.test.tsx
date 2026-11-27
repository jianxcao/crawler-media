import "./plugin-ui-env";

import assert from "node:assert/strict";
import test from "node:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import React, { useState } from "react";
import { MemoryRouter } from "react-router-dom";

import { FeedbackProvider } from "../components/feedback";
import { PluginConfigHost } from "../components/plugins/PluginConfigHost";
import { PluginSchemaForm } from "../components/plugins/PluginSchemaForm";
import { PluginSurfaceHost } from "../components/plugins/PluginSurfaceHost";
import type { SessionView } from "../lib/api/auth";
import type { PluginUiContract, SecretWriteAction, UiDescriptorView } from "../lib/api/plugins";
import { clearFederationCache, loadFederatedSurface } from "../lib/plugins/federation-loader";
import {
  buildInitialValues,
  computeSecretPaths,
  extractSchemaDefs,
  type JsonSchemaProperty,
  mergeSchemaNode,
  validateAndNormalizeObject,
} from "../lib/plugins/schema-helpers";
import { SessionProvider } from "../lib/session";

const ADMIN_SESSION: SessionView = {
  id: "user-admin-r7",
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
    plugin_id: "org.example.r7",
    plugin_name: "R7 Plugin",
    version: "1.0.0",
    generation: 1,
    lifecycle: "enabled",
    source: "external",
    package_hash: "pkg-r7-1",
    package_sha256: "pkg-r7-1",
    bundle_sha256: "bundle-r7-1",
    subject_user_id: ADMIN_SESSION.id,
    subject_role: "admin",
    ui_mode: "federation-react",
    remote_name: "r7Remote",
    entry: "ui/assets/remoteEntry.js",
    entry_url: "/api/v1/plugin-ui-assets/org.example.r7/pkg-r7-1/ui/assets/remoteEntry.js",
    asset_base_url: "/api/v1/plugin-ui-assets/org.example.r7/pkg-r7-1",
    styles: ["ui/assets/style-r7.css"],
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
    capabilities: ["read:sites"],
    surfaces: [
      {
        id: "settings",
        kind: "config",
        expose: "./Config",
        title: "R7 Settings",
        icon: null,
        location: null,
        mountable: true,
        editable: true,
        required_role: "admin",
      },
      {
        id: "main",
        kind: "page",
        expose: "./Page",
        title: "R7 Page",
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

function jsonOk(data: unknown): Response {
  return new Response(JSON.stringify({ ok: true, data }), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

test("R7-F1: optional nested objects do not eagerly materialize from child defaults, can be cleared after save, and multi-level secret-only objects render no empty fieldset", async () => {
  const schema = {
    type: "object",
    properties: {
      label: { type: "string", title: "Label", default: "main" },
      proxy: {
        type: "object",
        title: "Optional Proxy",
        required: ["host", "port"],
        properties: {
          host: { type: "string", title: "Proxy Host" },
          port: { type: "integer", title: "Proxy Port", default: 8080 },
        },
      },
      preset_box: {
        type: "object",
        title: "Defaulted Box",
        default: { mode: "auto" },
        properties: {
          mode: { type: "string", title: "Box Mode" },
          retries: { type: "integer", title: "Box Retries", default: 2 },
        },
      },
      auth: {
        type: "object",
        title: "Auth Root",
        properties: {
          oauth: {
            type: "object",
            title: "OAuth Nested",
            properties: {
              client_secret: { type: "string", writeOnly: true, title: "Client Secret" },
            },
          },
        },
      },
    },
  };

  const defs = extractSchemaDefs(schema);
  const mergedRoot = mergeSchemaNode(schema as JsonSchemaProperty, defs);
  const secretPaths = computeSecretPaths(mergedRoot, defs, {});
  const secretKeySet = new Set(secretPaths.keys());

  // 1a: Absent or null optional nested object without object-level default must NOT materialize
  const initEmpty = buildInitialValues(mergedRoot, defs, {}, secretPaths, secretKeySet);
  assert.equal("proxy" in initEmpty, false);
  assert.deepEqual(initEmpty.preset_box, { mode: "auto", retries: 2 });

  const initNull = buildInitialValues(mergedRoot, defs, { proxy: null }, secretPaths, secretKeySet);
  assert.equal("proxy" in initNull, false);

  const savedCalls: Array<Record<string, unknown>> = [];
  try {
    const view = render(
      <PluginSchemaForm
        schema={schema}
        values={{}}
        secrets={{ "auth.oauth.client_secret": { configured: false } }}
        editable
        sites={[]}
        downloaders={[]}
        onSave={async (vals) => {
          savedCalls.push(vals);
        }}
      />,
    );

    // 1c: Multi-level secret-only object `auth` -> `oauth` -> `client_secret` must NOT render empty fieldset/legend
    assert.equal(screen.queryByText("Auth Root"), null);
    assert.equal(screen.queryByText("OAuth Nested"), null);
    assert.ok(screen.getByLabelText("auth.oauth.client_secret 操作"));

    // 1a: Saving without touching unset optional `proxy` omits `proxy` and does not fail `proxy.host` required
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => assert.equal(savedCalls.length, 1));
    assert.equal("proxy" in savedCalls[0], false);
    assert.deepEqual(savedCalls[0].preset_box, { mode: "auto", retries: 2 });

    // Editing `Proxy Host` inside unset optional `proxy` merges child defaults (`port: 8080`)
    fireEvent.change(screen.getByLabelText("Proxy Host"), { target: { value: "10.0.0.8" } });
    assert.equal((screen.getByLabelText("Proxy Port") as HTMLInputElement).value, "8080");
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => assert.equal(savedCalls.length, 2));
    assert.deepEqual(savedCalls[1].proxy, { host: "10.0.0.8", port: 8080 });

    // 1b: Previously-saved optional nested object can be cleared back to omitted
    view.rerender(
      <PluginSchemaForm
        schema={schema}
        values={{ label: "main", proxy: { host: "10.0.0.8", port: 8080 } }}
        secrets={{ "auth.oauth.client_secret": { configured: false } }}
        editable
        sites={[]}
        downloaders={[]}
        onSave={async (vals) => {
          savedCalls.push(vals);
        }}
      />,
    );
    fireEvent.change(screen.getByLabelText("Proxy Host"), { target: { value: "" } });
    fireEvent.change(screen.getByLabelText("Proxy Port"), { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => assert.equal(savedCalls.length, 3));
    assert.equal("proxy" in savedCalls[2], false);
  } finally {
    cleanup();
  }
});

test("R7-F2: PluginSchemaForm & schema-helpers support non-site arrays, fractional numbers, required booleans without default, and $ref enum intersection", async () => {
  const schema = {
    $defs: {
      base_mode: {
        type: "string",
        enum: ["fast", "balanced", "safe"],
      },
    },
    type: "object",
    required: ["strict_tls", "tags", "ratio", "mode"],
    properties: {
      strict_tls: { type: "boolean", title: "Strict TLS" },
      ratio: { type: "number", title: "Target Ratio", minimum: 0.5, maximum: 10 },
      max_workers: { type: "integer", title: "Max Workers", default: 2 },
      mode: {
        $ref: "#/$defs/base_mode",
        enum: ["balanced", "safe", "turbo"],
        default: "balanced",
        title: "Mode",
      },
      tags: {
        type: "array",
        title: "Release Tags",
        minItems: 1,
        maxItems: 3,
        uniqueItems: true,
        items: { type: "string", minLength: 2, maxLength: 8 },
      },
      ports: {
        type: "array",
        title: "Listen Ports",
        items: { type: "integer", minimum: 1, maximum: 65535 },
      },
      weights: {
        type: "array",
        title: "Weights",
        items: { type: "number", minimum: 0, maximum: 5 },
      },
      flags: {
        type: "array",
        title: "Bool Flags",
        items: { type: "boolean" },
      },
      codecs: {
        type: "array",
        title: "Allowed Codecs",
        items: { type: "string", enum: ["hevc", "av1"] },
      },
      rules: {
        type: "array",
        title: "Object Rules",
        items: {
          type: "object",
          required: ["name"],
          properties: {
            name: { type: "string" },
            prio: { type: "integer", minimum: 1 },
          },
        },
      },
    },
  };

  // 2d: $ref enum intersection in mergeSchemaNode + validation
  const defs = extractSchemaDefs(schema);
  const mergedRoot = mergeSchemaNode(schema as JsonSchemaProperty, defs);
  const mergedMode = mergeSchemaNode(
    (mergedRoot.properties as Record<string, JsonSchemaProperty>).mode,
    defs,
  );
  assert.deepEqual(mergedMode.enum, ["balanced", "safe"]);
  const badEnumCheck = validateAndNormalizeObject(
    mergedRoot,
    { strict_tls: false, ratio: 1.5, mode: "turbo", tags: ["hdr"] },
    undefined,
    [],
    defs,
    new Set(),
    false,
  );
  assert.equal(badEnumCheck.ok, false);

  const savedCalls: Array<Record<string, unknown>> = [];
  try {
    render(
      <PluginSchemaForm
        schema={schema}
        values={{
          ratio: 1.0,
          tags: ["hdr", "4k"],
          ports: [8080, 9090],
          weights: [0.5, 1.25],
          flags: [true, false],
          codecs: ["hevc"],
          rules: [{ name: "r1", prio: 2 }],
        }}
        secrets={{}}
        editable
        sites={[]}
        downloaders={[]}
        onSave={async (vals) => {
          savedCalls.push(vals);
        }}
      />,
    );

    // 2b: <form noValidate> and step="any" for number vs step="1" for integer
    const formEl = document.querySelector("form");
    assert.equal(formEl?.noValidate, true);
    const ratioInput = screen.getByLabelText("Target Ratio") as HTMLInputElement;
    assert.equal(ratioInput.step, "any");
    const workersInput = screen.getByLabelText("Max Workers") as HTMLInputElement;
    assert.equal(workersInput.step, "1");

    // 2d: Select options only contain intersected enum values ["balanced", "safe"]
    const modeSelect = screen.getByLabelText("Mode") as HTMLSelectElement;
    const modeValues = Array.from(modeSelect.options).map((o) => o.value);
    assert.deepEqual(modeValues, ["balanced", "safe"]);

    // 2c: Required boolean without default initializes to false & saves without toggling
    // 2a: Untouched `tags: ["hdr", "4k"]` stays a real JS Array (not "hdr,4k")
    // 2b: Fractional number 1.25 with min=0.5 submits cleanly via submit button
    fireEvent.change(ratioInput, { target: { value: "1.25" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => assert.equal(savedCalls.length, 1));
    assert.equal(savedCalls[0].strict_tls, false);
    assert.equal(savedCalls[0].ratio, 1.25);
    assert.deepEqual(savedCalls[0].tags, ["hdr", "4k"]);
    assert.deepEqual(savedCalls[0].ports, [8080, 9090]);
    assert.deepEqual(savedCalls[0].weights, [0.5, 1.25]);
    assert.deepEqual(savedCalls[0].flags, [true, false]);
    assert.deepEqual(savedCalls[0].codecs, ["hevc"]);
    assert.deepEqual(savedCalls[0].rules, [{ name: "r1", prio: 2 }]);

    // 2a validation: uniqueItems violation blocks save
    const tagsInput = screen.getByLabelText("Release Tags") as HTMLTextAreaElement;
    fireEvent.change(tagsInput, { target: { value: "hdr, hdr" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    assert.ok(await screen.findByRole("alert"));
    assert.equal(savedCalls.length, 1);

    // 2a validation: minLength violation blocks save
    fireEvent.change(tagsInput, { target: { value: "x" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    assert.ok(await screen.findByRole("alert"));
    assert.equal(savedCalls.length, 1);

    // 2a validation: maxItems violation blocks save
    fireEvent.change(tagsInput, { target: { value: "aa\nbb\ncc\ndd" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    assert.ok(await screen.findByRole("alert"));
    assert.equal(savedCalls.length, 1);

    // Fix tags with comma + newline separated items, test invalid item enum on codecs
    fireEvent.change(tagsInput, { target: { value: "dolby, 4k\nremux" } });
    const codecsInput = screen.getByLabelText("Allowed Codecs") as HTMLTextAreaElement;
    fireEvent.change(codecsInput, { target: { value: "hevc, xvid" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    assert.ok(await screen.findByRole("alert"));
    assert.equal(savedCalls.length, 1);

    // Fix codecs, test out-of-range port (> 65535)
    fireEvent.change(codecsInput, { target: { value: "hevc, av1" } });
    const portsInput = screen.getByLabelText("Listen Ports") as HTMLTextAreaElement;
    fireEvent.change(portsInput, { target: { value: "443, 70000" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    assert.ok(await screen.findByRole("alert"));
    assert.equal(savedCalls.length, 1);

    // Fix ports, test invalid JSON / missing required field in object array
    fireEvent.change(portsInput, { target: { value: "443\n8443" } });
    const rulesInput = screen.getByLabelText("Object Rules") as HTMLTextAreaElement;
    fireEvent.change(rulesInput, { target: { value: '[{"prio": 0}]' } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    assert.ok(await screen.findByRole("alert"));
    assert.equal(savedCalls.length, 1);

    // Valid edits across all array types save as real JS Arrays
    fireEvent.change(rulesInput, { target: { value: '[{"name":"hd","prio":3}]' } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => assert.equal(savedCalls.length, 2));
    assert.deepEqual(savedCalls[1].tags, ["dolby", "4k", "remux"]);
    assert.deepEqual(savedCalls[1].codecs, ["hevc", "av1"]);
    assert.deepEqual(savedCalls[1].ports, [443, 8443]);
    assert.deepEqual(savedCalls[1].rules, [{ name: "hd", prio: 3 }]);
  } finally {
    cleanup();
  }
});

test("R7-F3: credential-bound secretSiteId stays synchronized with draft.site_id in full mode and values.site_id in hideNonSecrets mode", async () => {
  const schema = {
    type: "object",
    properties: {
      site_id: { type: "string", title: "目标站点", "x-ui": { widget: "site" } },
      api_key: { type: "string", writeOnly: true, title: "API Key" },
    },
  };
  const sites = [
    { id: "site-old", name: "Old Site" },
    { id: "site-new", name: "New Site" },
  ];

  const saves: Array<{
    values: Record<string, unknown>;
    secrets: Record<string, SecretWriteAction>;
    siteId?: string;
  }> = [];

  try {
    // 1. Full schema mode: changing draft.site_id from "site-old" to "site-new" updates secretSiteId & submit siteId
    render(
      <PluginSchemaForm
        schema={schema}
        values={{ site_id: "site-old" }}
        secrets={{ api_key: { configured: true } }}
        editable
        credentialBound
        sites={sites}
        downloaders={[]}
        onSave={async (values, secrets, siteId) => {
          saves.push({ values, secrets, siteId });
        }}
      />,
    );

    fireEvent.change(screen.getByLabelText("目标站点"), { target: { value: "site-new" } });
    fireEvent.change(screen.getByLabelText("api_key 操作"), { target: { value: "replace" } });
    fireEvent.change(screen.getByLabelText("api_key 新密钥"), { target: { value: "sec-new-1" } });

    const boundSelect = screen.getByLabelText("凭证绑定站点 (site_id)") as HTMLSelectElement;
    assert.equal(boundSelect.value, "site-new");

    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => assert.equal(saves.length, 1));
    assert.equal(saves[0].values.site_id, "site-new");
    assert.equal(saves[0].siteId, "site-new");
    cleanup();

    // 2. Federated hideNonSecrets mode: updating values.site_id from "site-old" to "site-new" updates secretSiteId & submit siteId
    let updateFedSite!: (nextSite: string) => void;
    function FederatedSecretHarness() {
      const [fedSite, setFedSite] = useState("site-old");
      updateFedSite = setFedSite;
      return (
        <PluginSchemaForm
          schema={schema}
          values={{ site_id: fedSite }}
          secrets={{ api_key: { configured: true } }}
          editable
          hideNonSecrets
          credentialBound
          sites={sites}
          downloaders={[]}
          onSave={async (values, secrets, siteId) => {
            saves.push({ values, secrets, siteId });
          }}
        />
      );
    }

    render(<FederatedSecretHarness />);
    fireEvent.change(screen.getByLabelText("api_key 操作"), { target: { value: "replace" } });
    fireEvent.change(screen.getByLabelText("api_key 新密钥"), { target: { value: "sec-new-2" } });
    assert.equal(
      (screen.getByLabelText("凭证绑定站点 (site_id)") as HTMLSelectElement).value,
      "site-old",
    );

    fireEvent.click(document.body);
    React.act(() => {
      updateFedSite("site-new");
    });
    await waitFor(() =>
      assert.equal(
        (screen.getByLabelText("凭证绑定站点 (site_id)") as HTMLSelectElement).value,
        "site-new",
      ),
    );

    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => assert.equal(saves.length, 2));
    assert.equal(saves[1].siteId, "site-new");
  } finally {
    cleanup();
  }
});

test("R7-F4a: PluginSurfaceHost evicts federation cache when fetchContract rejects after fetchDescriptor resolves or when missing-surface blocks, and disposes SDK if loadSurface throws", async () => {
  clearFederationCache();
  try {
    let mode: "ok" | "contract-reject" | "missing-surface" | "load-throw" = "ok";
    let importCount = 0;

    const renderSurface = (reloadKey: string) => (
      <MemoryRouter>
        <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "r7-s" }}>
          <PluginSurfaceHost
            pluginId="org.example.r7"
            surfaceId="main"
            expectedKind="page"
            reloadKey={reloadKey}
            deps={{
              fetchDescriptor: async () => {
                if (mode === "missing-surface") {
                  return makeDescriptor({ surfaces: [] });
                }
                return makeDescriptor();
              },
              fetchContract: async () => {
                if (mode === "contract-reject") {
                  await new Promise((r) => setTimeout(r, 15));
                  throw new Error("contract endpoint unavailable");
                }
                return CONTRACT;
              },
              loadSurface: async (desc, expose) => {
                if (mode === "load-throw") {
                  throw new Error("remote surface chunk failed");
                }
                return loadFederatedSurface(desc, expose, {
                  reloadPage: () => {},
                  importRemote: async () => {
                    importCount += 1;
                    return {
                      init: async () => {},
                      get: async () => () => ({ default: () => <div>R7 Surface Active</div> }),
                    };
                  },
                });
              },
            }}
          />
        </SessionProvider>
      </MemoryRouter>
    );

    const view = render(renderSurface("s1"));
    await waitFor(() => assert.ok(screen.getByText("R7 Surface Active")));
    assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 1);
    assert.equal(importCount, 1);

    // 1. fetchDescriptor resolves first, then fetchContract rejects -> MUST evict stylesheet & containerCache
    mode = "contract-reject";
    view.rerender(renderSurface("s2"));
    await waitFor(() => assert.ok(screen.getByText("无法加载插件界面")));
    assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 0);

    // Re-mounting same generation/bundle_sha256 proves containerCache was evicted (importCount increments)
    mode = "ok";
    view.rerender(renderSurface("s3"));
    await waitFor(() => assert.ok(screen.getByText("R7 Surface Active")));
    assert.equal(importCount, 2);
    assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 1);

    // 2. evaluateSurfaceGate blocks with `missing-surface` -> MUST evict stylesheet & containerCache
    mode = "missing-surface";
    view.rerender(renderSurface("s4"));
    await waitFor(() => assert.ok(screen.getByText("未找到目标界面入口")));
    assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 0);

    // 3. loadSurface throws after createPluginHostSdk succeeded -> transitions cleanly to blocked
    mode = "load-throw";
    view.rerender(renderSurface("s5"));
    await waitFor(() => assert.ok(screen.getByText("remote surface chunk failed")));
  } finally {
    cleanup();
    clearFederationCache();
  }
});

test("R7-F4b: PluginConfigHost evicts federation cache and stylesheets when UI trust is revoked, ui_assets_present is false, or fetchDescriptor fails", async () => {
  clearFederationCache();
  const origFetch = globalThis.fetch;
  try {
    let descState: "trusted" | "untrusted" | "no-assets" | "fetch-error" = "trusted";
    let remoteImports = 0;

    globalThis.fetch = ((input: string | URL | Request) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
      if (url.endsWith("/config-schema")) {
        return Promise.resolve(
          jsonOk({
            plugin_id: "org.example.r7",
            schema_revision: "rev-1",
            schema: {
              type: "object",
              properties: { note: { type: "string", title: "Fallback Note" } },
            },
          }),
        );
      }
      if (url.endsWith("/config")) {
        return Promise.resolve(
          jsonOk({
            plugin_id: "org.example.r7",
            schema_revision: "rev-1",
            revision: 1,
            values: { note: "hello" },
            secrets: {},
          }),
        );
      }
      return Promise.resolve(jsonOk([]));
    }) as typeof fetch;

    const renderConfigHost = (reloadKey: number) => (
      <MemoryRouter>
        <FeedbackProvider>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "r7-c" }}>
            <PluginConfigHost
              pluginId="org.example.r7"
              reloadKey={reloadKey}
              pluginEditable
              credentialBound={false}
              surfaceDeps={{
                fetchContract: async () => {
                  await new Promise((r) => setTimeout(r, 15));
                  return CONTRACT;
                },
                fetchDescriptor: async () => {
                  if (descState === "fetch-error") {
                    throw new Error("descriptor 404");
                  }
                  if (descState === "untrusted") {
                    return makeDescriptor({ ui_trusted: false });
                  }
                  if (descState === "no-assets") {
                    return makeDescriptor({ ui_assets_present: false });
                  }
                  return makeDescriptor({ ui_trusted: true });
                },
                loadSurface: (desc, expose) =>
                  loadFederatedSurface(desc, expose, {
                    reloadPage: () => {},
                    importRemote: async () => {
                      remoteImports += 1;
                      return {
                        init: async () => {},
                        get: async () => () => ({ default: () => <div>Federated Config Surface</div> }),
                      };
                    },
                  }),
              }}
            />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>
    );

    // 1. Trusted federated config loads and injects <link data-cm-plugin-ui>
    const view = render(renderConfigHost(1));
    await waitFor(() => assert.ok(screen.getByText("Federated Config Surface")));
    assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 1);
    assert.equal(remoteImports, 1);

    // 2. External UI trust revoked -> reloads into PluginSchemaForm AND evicts <link> + containerCache
    descState = "untrusted";
    view.rerender(renderConfigHost(2));
    await waitFor(() => assert.ok(screen.getByLabelText("Fallback Note")));
    assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 0);

    // 3. Re-trusting same bundle_sha256 re-imports remote (proving containerCache was evicted)
    descState = "trusted";
    view.rerender(renderConfigHost(3));
    await waitFor(() => assert.ok(screen.getByText("Federated Config Surface")));
    assert.equal(remoteImports, 2);
    assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 1);

    // 4. ui_assets_present: false evicts <link> + containerCache
    descState = "no-assets";
    view.rerender(renderConfigHost(4));
    await waitFor(() => assert.ok(screen.getByLabelText("Fallback Note")));
    assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 0);

    // 5. Re-trust then fetchDescriptor failure evicts <link> + containerCache
    descState = "trusted";
    view.rerender(renderConfigHost(5));
    await waitFor(() => assert.ok(screen.getByText("Federated Config Surface")));
    assert.equal(remoteImports, 3);
    assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 1);

    descState = "fetch-error";
    view.rerender(renderConfigHost(6));
    await waitFor(() => assert.ok(screen.getByLabelText("Fallback Note")));
    assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 0);
  } finally {
    cleanup();
    clearFederationCache();
    globalThis.fetch = origFetch;
  }
});
