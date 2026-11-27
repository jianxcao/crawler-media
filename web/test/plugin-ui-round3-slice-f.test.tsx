import "./plugin-ui-env";

import assert from "node:assert/strict";
import test from "node:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import React from "react";
import { MemoryRouter } from "react-router-dom";

import type { PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";
import { FeedbackProvider } from "../components/feedback";
import { PluginConfigHost } from "../components/plugins/PluginConfigHost";
import type { SessionView } from "../lib/api/auth";
import type { PluginUiContract, UiDescriptorView } from "../lib/api/plugins";
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
    plugin_id: "org.example.round3",
    plugin_name: "Round3 Plugin",
    version: "1.0.0",
    generation: 2,
    lifecycle: "disabled",
    source: "external",
    package_hash: "pkg-r3-1",
    package_sha256: "pkg-r3-1",
    bundle_sha256: "bundle-r3-1",
    subject_user_id: "user-admin-1",
    subject_role: "admin",
    ui_mode: "federation-react",
    remote_name: "r3Remote",
    entry: "ui/assets/remoteEntry.js",
    entry_url: "/api/v1/plugin-ui-assets/org.example.round3/pkg-r3-1/ui/assets/remoteEntry.js",
    asset_base_url: "/api/v1/plugin-ui-assets/org.example.round3/pkg-r3-1",
    contract: "1.0.0",
    react: "19.3.0",
    react_dom: "19.3.0",
    ui_trusted: true,
    trust: {
      status: "trusted",
      revision: 1,
      granted_at: "2026-11-24T00:00:00Z",
      granted_by_user_id: "user-admin-1",
      revoked_at: null,
    },
    capabilities: ["read:sites"],
    surfaces: [
      {
        id: "settings",
        kind: "config",
        expose: "./Config",
        title: "Round3 Settings",
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

test("Round 3 Slice F: clearing optional integer / site_id / downloader_id omits keys from PUT /config", async () => {
  const origFetch = globalThis.fetch;
  try {
    const putBodies: Array<Record<string, unknown>> = [];
    let currentRev = 1;
    let storedValues: Record<string, unknown> = {
      title: "keep-me",
      max_retries: 5,
      ratio_floor: null,
      site_id: "site-1",
      downloader_id: "dl-1",
    };
    globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
      const method = init?.method ?? "GET";
      if (url.endsWith("/plugins/org.example.round3/ui")) {
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
              plugin_id: "org.example.round3",
              schema_revision: "s-r3",
              schema: {
                type: "object",
                required: ["title"],
                properties: {
                  title: { type: "string", title: "标题" },
                  max_retries: { type: "integer", title: "最大重试", minimum: 1 },
                  ratio_floor: { type: "number", title: "最低分享率", minimum: 0.5 },
                  site_id: { type: "string", title: "目标站点", "x-ui": { widget: "site" } },
                  downloader_id: { type: "string", title: "下载器", "x-ui": { widget: "downloader" } },
                },
              },
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (url.endsWith("/config") && method === "GET") {
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              plugin_id: "org.example.round3",
              revision: currentRev,
              config_revision: 1,
              schema_revision: "s-r3",
              values: storedValues,
              secrets: {},
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (url.endsWith("/config") && method === "PUT") {
        const parsed = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
        putBodies.push(parsed);
        currentRev += 1;
        storedValues = (parsed.values as Record<string, unknown>) ?? {};
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              plugin_id: "org.example.round3",
              revision: currentRev,
              config_revision: 2,
              schema_revision: "s-r3",
              values: storedValues,
              secrets: {},
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (url.endsWith("/sites")) {
        return new Response(
          JSON.stringify({ ok: true, data: [{ id: "site-1", name: "Alpha PT" }] }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (url.endsWith("/downloaders")) {
        return new Response(
          JSON.stringify({ ok: true, data: [{ id: "dl-1", name: "qB Primary" }] }),
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
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
            <PluginConfigHost
              pluginId="org.example.round3"
              credentialBound={false}
              pluginEditable={true}
            />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );

    const retriesInput = await waitFor(() => screen.getByLabelText("最大重试"));
    fireEvent.change(retriesInput, { target: { value: "" } });
    fireEvent.change(screen.getByLabelText("目标站点"), { target: { value: "" } });
    fireEvent.change(screen.getByLabelText("下载器"), { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: /保存配置/ }));

    await waitFor(() => assert.equal(putBodies.length, 1));
    const sentValues = putBodies[0].values as Record<string, unknown>;
    assert.deepEqual(sentValues, { title: "keep-me" });
    assert.equal("max_retries" in sentValues, false);
    assert.equal("ratio_floor" in sentValues, false);
    assert.equal("site_id" in sentValues, false);
    assert.equal("downloader_id" in sentValues, false);
  } finally {
    globalThis.fetch = origFetch;
    cleanup();
  }
});

test("Round 3 Slice F: federated PluginConfigHost host secret save preserves schema defaults via mergedValues", async () => {
  const origFetch = globalThis.fetch;
  try {
    const putBodies: Array<Record<string, unknown>> = [];
    let currentRev = 1;
    globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
      const method = init?.method ?? "GET";
      if (url.endsWith("/config-schema")) {
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              plugin_id: "org.example.round3",
              schema_revision: "s-r3",
              schema: {
                type: "object",
                properties: {
                  mode: { type: "string", default: "balanced" },
                  batch_size: { type: "integer", default: 25 },
                  api_secret: { type: "string", writeOnly: true },
                },
              },
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (url.endsWith("/config") && method === "GET") {
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              plugin_id: "org.example.round3",
              revision: currentRev,
              config_revision: 0,
              schema_revision: "s-r3",
              values: {},
              secrets: { api_secret: { configured: false } },
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (url.endsWith("/config") && method === "PUT") {
        const parsed = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
        putBodies.push(parsed);
        currentRev += 1;
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              plugin_id: "org.example.round3",
              revision: currentRev,
              config_revision: 1,
              schema_revision: "s-r3",
              values: parsed.values ?? {},
              secrets: { api_secret: { configured: true } },
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

    const PassiveRemoteCfg: React.FC<PluginSurfaceProps> = () => <div>远程面板已挂载</div>;

    render(
      <MemoryRouter>
        <FeedbackProvider>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
            <PluginConfigHost
              pluginId="org.example.round3"
              credentialBound={false}
              pluginEditable={true}
              surfaceDeps={{
                fetchDescriptor: async () => makeDescriptor(),
                fetchContract: async () => CONTRACT,
                loadSurface: async () => PassiveRemoteCfg,
              }}
            />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );

    await waitFor(() => screen.getByText("远程面板已挂载"));
    fireEvent.change(screen.getByLabelText(/api_secret 操作/), { target: { value: "replace" } });
    const secInput = await waitFor(() => screen.getByLabelText(/api_secret 新密钥/));
    fireEvent.change(secInput, { target: { value: "top-secret" } });
    fireEvent.click(screen.getByRole("button", { name: /保存配置/ }));

    await waitFor(() => assert.equal(putBodies.length, 1));
    assert.deepEqual(putBodies[0].values, {
      mode: "balanced",
      batch_size: 25,
    });
    assert.deepEqual(putBodies[0].secrets, {
      api_secret: { action: "replace", value: "top-secret" },
    });
  } finally {
    globalThis.fetch = origFetch;
    cleanup();
  }
});
