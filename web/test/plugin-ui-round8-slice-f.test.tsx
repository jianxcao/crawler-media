import "./plugin-ui-env";

import assert from "node:assert/strict";
import test from "node:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import React from "react";
import { MemoryRouter } from "react-router-dom";

import { FeedbackProvider } from "../components/feedback";
import { PluginConfigHost } from "../components/plugins/PluginConfigHost";
import { PluginSchemaForm } from "../components/plugins/PluginSchemaForm";
import type { SessionView } from "../lib/api/auth";
import type { PluginUiContract, SecretWriteAction } from "../lib/api/plugins";
import { SessionProvider } from "../lib/session";

const ADMIN_SESSION: SessionView = {
  id: "user-admin-r8",
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

function jsonOk(data: unknown): Response {
  return new Response(JSON.stringify({ ok: true, data }), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
}

test("R8-F2a & R8-F2b: PluginSchemaForm honors explicit #plugin-cfg-secret-site-id selection and synchronizes secretSiteId with multi-site sites[]", async () => {
  const saves: Array<{
    values: Record<string, unknown>;
    secrets: Record<string, SecretWriteAction>;
    siteId?: string;
  }> = [];

  try {
    // 2.1: Schema with `site_id`: explicit selection in `凭证绑定站点 (site_id)` must NOT be overridden by initial `values.site_id`
    render(
      <PluginSchemaForm
        schema={{
          type: "object",
          additionalProperties: false,
          properties: {
            site_id: { type: "string", title: "目标站点", "x-ui": { widget: "site" } },
            api_key: { type: "string", format: "password", title: "API Key" },
          },
        }}
        values={{ site_id: "site-a" }}
        secrets={{ api_key: { configured: true } }}
        editable
        credentialBound
        sites={[
          { id: "site-a", name: "Site A" },
          { id: "site-b", name: "Site B" },
        ]}
        downloaders={[]}
        onSave={async (values, secrets, siteId) => {
          saves.push({ values, secrets, siteId });
        }}
      />,
    );

    fireEvent.change(screen.getByLabelText("api_key 操作"), { target: { value: "replace" } });
    fireEvent.change(screen.getByLabelText("api_key 新密钥"), { target: { value: "sec-for-b" } });

    // Selecting "" ("请选择站点") in `#plugin-cfg-secret-site-id` must block submit even though `values.site_id` is "site-a"
    fireEvent.change(screen.getByLabelText("凭证绑定站点 (site_id)"), { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    assert.ok(await screen.findByRole("alert"));
    assert.equal(saves.length, 0);

    // Selecting "site-b" in `#plugin-cfg-secret-site-id` must submit `siteId === "site-b"`
    fireEvent.change(screen.getByLabelText("凭证绑定站点 (site_id)"), {
      target: { value: "site-b" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => assert.equal(saves.length, 1));
    assert.equal(saves[0].siteId, "site-b");
    cleanup();

    // 2.2: Multi-site `sites[]` synchronizes `secretSiteId` when sites are checked/unchecked
    const multiSaves: Array<{
      values: Record<string, unknown>;
      secrets: Record<string, SecretWriteAction>;
      siteId?: string;
    }> = [];
    render(
      <PluginSchemaForm
        schema={{
          type: "object",
          additionalProperties: false,
          properties: {
            sites: {
              type: "array",
              title: "启用站点",
              items: { type: "string", "x-ui": { widget: "site" } },
            },
            api_key: { type: "string", format: "password", title: "API Key" },
          },
        }}
        values={{ sites: ["site-a"] }}
        secrets={{ api_key: { configured: true } }}
        editable
        credentialBound
        sites={[
          { id: "site-a", name: "Site A" },
          { id: "site-b", name: "Site B" },
        ]}
        downloaders={[]}
        onSave={async (values, secrets, siteId) => {
          multiSaves.push({ values, secrets, siteId });
        }}
      />,
    );

    // Uncheck Site A, check Site B, then replace secret -> `secretSiteId` must automatically switch from "site-a" to "site-b"
    fireEvent.click(screen.getByLabelText("Site A"));
    fireEvent.click(screen.getByLabelText("Site B"));
    fireEvent.change(screen.getByLabelText("api_key 操作"), { target: { value: "replace" } });
    fireEvent.change(screen.getByLabelText("api_key 新密钥"), { target: { value: "sec-b-2" } });
    assert.equal(
      (screen.getByLabelText("凭证绑定站点 (site_id)") as HTMLSelectElement).value,
      "site-b",
    );
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => assert.equal(multiSaves.length, 1));
    assert.deepEqual(multiSaves[0].values, { sites: ["site-b"] });
    assert.equal(multiSaves[0].siteId, "site-b");
  } finally {
    cleanup();
  }
});

test("R8-F2c: PluginConfigHost sends top-level site_id on credential-bound keep saves so per-site secret validation & configured state stay accurate", async () => {
  const origFetch = globalThis.fetch;
  const putBodies: Array<Record<string, unknown>> = [];
  try {
    globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
      const url =
        typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
      const method = (init?.method ?? "GET").toUpperCase();
      if (url.endsWith("/api/v1/sites")) {
        return jsonOk([
          { id: "site-a", name: "Site A", enabled: true },
          { id: "site-b", name: "Site B", enabled: true },
        ]);
      }
      if (url.endsWith("/api/v1/downloaders")) {
        return jsonOk([]);
      }
      if (url.endsWith("/config-schema")) {
        return jsonOk({
          plugin_id: "org.example.r8-cb",
          schema_revision: "1",
          schema: {
            type: "object",
            additionalProperties: false,
            required: ["site_id", "api_key"],
            properties: {
              site_id: { type: "string", title: "站点", "x-ui": { widget: "site" } },
              api_key: { type: "string", format: "password", title: "API Key" },
            },
          },
        });
      }
      if (url.endsWith("/plugins/org.example.r8-cb/config") && method === "GET") {
        return jsonOk({
          plugin_id: "org.example.r8-cb",
          revision: 3,
          config_revision: 1,
          schema_revision: "1",
          values: { site_id: "site-a" },
          secrets: { api_key: { configured: true } },
        });
      }
      if (url.endsWith("/plugins/org.example.r8-cb/config") && method === "PUT") {
        const parsed = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
        putBodies.push(parsed);
        return jsonOk({
          plugin_id: "org.example.r8-cb",
          revision: 4,
          config_revision: 2,
          schema_revision: "1",
          values: parsed.values,
          secrets: { api_key: { configured: parsed.site_id === "site-a" } },
        });
      }
      throw new Error(`Unexpected fetch: ${method} ${url}`);
    }) as typeof fetch;

    render(
      <MemoryRouter>
        <FeedbackProvider>
          <SessionProvider
            value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "r8-c" }}
          >
            <PluginConfigHost
              pluginId="org.example.r8-cb"
              pluginEditable
              credentialBound
              surfaceDeps={{
                fetchContract: async () => CONTRACT,
                fetchDescriptor: async () => {
                  throw new Error("no federated ui");
                },
              }}
            />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );

    const siteSelect = await screen.findByLabelText("站点");
    fireEvent.change(siteSelect, { target: { value: "site-b" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));

    await waitFor(() => assert.equal(putBodies.length, 1));
    assert.equal(putBodies[0].site_id, "site-b");
    await waitFor(() => assert.ok(screen.getByText("api_key · 未配置")));
  } finally {
    cleanup();
    globalThis.fetch = origFetch;
  }
});

test("R8-F3: optional nested objects with boolean, array, or auto-materialized sibling defaults collapse back to omitted when cleared", async () => {
  const schema = {
    type: "object",
    additionalProperties: false,
    properties: {
      proxy_bool: {
        type: "object",
        title: "Bool Proxy",
        required: ["host"],
        additionalProperties: false,
        properties: {
          host: { type: "string", title: "Bool Host" },
          tls: { type: "boolean", title: "Bool TLS" },
        },
      },
      proxy_arr: {
        type: "object",
        title: "Array Proxy",
        required: ["host"],
        additionalProperties: false,
        properties: {
          host: { type: "string", title: "Arr Host" },
          no_proxy: {
            type: "array",
            title: "No Proxy",
            items: { type: "string" },
          },
        },
      },
      proxy_def: {
        type: "object",
        title: "Default Sibling Proxy",
        required: ["host", "port"],
        additionalProperties: false,
        properties: {
          host: { type: "string", title: "Def Host" },
          port: { type: "integer", title: "Def Port", default: 8080 },
        },
      },
    },
  };

  const saves: Array<Record<string, unknown>> = [];
  try {
    render(
      <PluginSchemaForm
        schema={schema}
        values={{
          proxy_bool: { host: "10.0.0.1", tls: false },
          proxy_arr: { host: "10.0.0.2", no_proxy: ["localhost"] },
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

    // Repro A & B: Clear `Bool Host`, `Arr Host`, and `No Proxy`
    fireEvent.change(screen.getByLabelText("Bool Host"), { target: { value: "" } });
    fireEvent.change(screen.getByLabelText("Arr Host"), { target: { value: "" } });
    fireEvent.change(screen.getByLabelText("No Proxy"), { target: { value: "" } });

    // Repro C: Transiently type into unset `Def Host` (auto-filling `Def Port = 8080`) then backspace `Def Host` to ""
    fireEvent.change(screen.getByLabelText("Def Host"), { target: { value: "x" } });
    assert.equal((screen.getByLabelText("Def Port") as HTMLInputElement).value, "8080");
    fireEvent.change(screen.getByLabelText("Def Host"), { target: { value: "" } });

    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => assert.equal(saves.length, 1));
    assert.deepEqual(saves[0], {});
  } finally {
    cleanup();
  }
});

test("R8-F4: client validation parity for cleared constrained strings/arrays, closed objects + strict JSON types, and secret constraints/required", async () => {
  const schema = {
    type: "object",
    additionalProperties: false,
    required: ["api_key"],
    properties: {
      webhook: { type: "string", title: "Webhook URL", pattern: "^https://" },
      prefix: { type: "string", title: "Tag Prefix", minLength: 3, default: "abc" },
      qualities: {
        type: "array",
        title: "Qualities",
        minItems: 1,
        default: ["hd"],
        items: { type: "string" },
      },
      rules: {
        type: "array",
        title: "Rules JSON",
        items: {
          type: "object",
          additionalProperties: false,
          required: ["name"],
          properties: {
            name: { type: "string" },
            enabled: { type: "boolean" },
            prio: { type: "integer", minimum: 1 },
          },
        },
      },
      api_key: {
        type: "string",
        format: "password",
        title: "API Key",
        minLength: 6,
        pattern: "^sk-",
        enum: ["sk-valid1", "sk-valid2"],
      },
      nested_opt: {
        type: "object",
        title: "Nested Optional",
        additionalProperties: false,
        required: ["token"],
        properties: {
          endpoint: { type: "string", title: "Nested Endpoint" },
          token: { type: "string", writeOnly: true, title: "Nested Token" },
        },
      },
    },
  };

  const saves: Array<{
    values: Record<string, unknown>;
    secrets: Record<string, SecretWriteAction>;
  }> = [];

  try {
    render(
      <PluginSchemaForm
        schema={schema}
        values={{ stale_legacy_key: "must-be-dropped" }}
        secrets={{
          api_key: { configured: false },
          "nested_opt.token": { configured: false },
        }}
        editable
        sites={[]}
        downloaders={[]}
        onSave={async (values, secrets) => {
          saves.push({ values, secrets });
        }}
      />,
    );

    // 4.3a: Unconfigured required secret `api_key` with action = `keep` on non-credential-bound plugin must fail
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    assert.match((await screen.findByRole("alert")).textContent ?? "", /api_key/);
    assert.equal(saves.length, 0);

    // 4.3b: Required secret `api_key` with action = `clear` must fail
    fireEvent.change(screen.getByLabelText("api_key 操作"), { target: { value: "clear" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    assert.match((await screen.findByRole("alert")).textContent ?? "", /必填密钥/);

    // 4.3c: Secret `replace` enforces minLength / pattern / enum
    fireEvent.change(screen.getByLabelText("api_key 操作"), { target: { value: "replace" } });
    fireEvent.change(screen.getByLabelText("api_key 新密钥"), { target: { value: "short" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    assert.match((await screen.findByRole("alert")).textContent ?? "", /长度不能小于 6/);

    fireEvent.change(screen.getByLabelText("api_key 新密钥"), { target: { value: "bad-prefix" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    assert.match((await screen.findByRole("alert")).textContent ?? "", /格式不匹配/);

    fireEvent.change(screen.getByLabelText("api_key 新密钥"), { target: { value: "sk-other" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    assert.match((await screen.findByRole("alert")).textContent ?? "", /枚举/);

    fireEvent.change(screen.getByLabelText("api_key 新密钥"), { target: { value: "sk-valid1" } });

    // 4.2: Array-of-objects rejects unknown properties and coerced/wrong JSON primitive types
    const rulesInput = screen.getByLabelText("Rules JSON");
    for (const badJson of [
      '[{"name": "r1", "unknown": 1}]',
      '[{"name": 123}]',
      '[{"name": "r1", "enabled": "yes"}]',
      '[{"name": "r1", "prio": true}]',
      '[{"name": "r1", "prio": []}]',
    ]) {
      fireEvent.change(rulesInput, { target: { value: badJson } });
      fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
      assert.ok(await screen.findByRole("alert"), `expected alert for ${badJson}`);
      assert.equal(saves.length, 0);
    }
    fireEvent.change(rulesInput, {
      target: { value: '[{"name": "r1", "enabled": true, "prio": 2}]' },
    });

    // 4.1: Type & clear optional `webhook` (pattern), clear `prefix` (minLength + default), clear `qualities` (minItems + default)
    fireEvent.change(screen.getByLabelText("Webhook URL"), { target: { value: "bad" } });
    fireEvent.change(screen.getByLabelText("Webhook URL"), { target: { value: "" } });
    fireEvent.change(screen.getByLabelText("Tag Prefix"), { target: { value: "" } });
    fireEvent.change(screen.getByLabelText("Qualities"), { target: { value: "" } });

    // `nested_opt` is omitted, so `nested_opt.token` is NOT required unless `nested_opt` is populated
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => assert.equal(saves.length, 1));
    assert.deepEqual(saves[0].values, {
      rules: [{ name: "r1", enabled: true, prio: 2 }],
    });
    assert.deepEqual(saves[0].secrets, {
      api_key: { action: "replace", value: "sk-valid1" },
    });
  } finally {
    cleanup();
  }
});
