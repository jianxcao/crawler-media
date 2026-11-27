import "./plugin-ui-env";

import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import React from "react";
import { MemoryRouter, Route, Routes } from "react-router-dom";

import { isSdkError, type PluginSurfaceProps, type UiHandle } from "@crawler-media/plugin-ui-sdk";
import PluginAppRoutePage from "../app/(app)/plugin-apps/[pluginId]/[surfaceId]/page";
import PluginDetailPage from "../app/(app)/settings/plugins/[pluginId]/page";
import { AppShell } from "../components/app-shell";
import { FeedbackProvider } from "../components/feedback";
import { PluginConfigHost } from "../components/plugins/PluginConfigHost";
import { PluginDetailView } from "../components/plugins/PluginDetailView";
import { PluginSchemaForm } from "../components/plugins/PluginSchemaForm";
import { SearchResults } from "../components/search-results";
import type { SessionView } from "../lib/api/auth";
import type { PluginUiContract, UiDescriptorView } from "../lib/api/plugins";
import { reconcilePagedStreamItems, type TorrentHit } from "../lib/api/search";
import { DownloadTasksProvider } from "../lib/download-tasks";
import { JobsProvider } from "../lib/jobs";
import { createPluginHostSdk } from "../lib/plugins/sdk";
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
    plugin_id: "org.example.cred",
    plugin_name: "Cred Plugin",
    version: "1.0.0",
    generation: 2,
    lifecycle: "disabled",
    source: "external",
    package_hash: "pkg-cred-1",
    package_sha256: "pkg-cred-1",
    bundle_sha256: "bundle-cred-1",
    subject_user_id: "user-admin-1",
    subject_role: "admin",
    ui_mode: "federation-react",
    remote_name: "credRemote",
    entry: "ui/assets/remoteEntry.js",
    entry_url: "/api/v1/plugin-ui-assets/org.example.cred/pkg-cred-1/ui/assets/remoteEntry.js",
    asset_base_url: "/api/v1/plugin-ui-assets/org.example.cred/pkg-cred-1",
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
        title: "Cred Settings",
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

function makeHit(siteId: string, id: string, title: string): TorrentHit {
  return {
    site_id: siteId,
    site_name: siteId.toUpperCase(),
    id,
    title,
    enclosure: `https://tracker.example/dl/${id}.torrent`,
    size_bytes: 1024 ** 3,
    seeders: 10,
    leechers: 1,
    snatched: 5,
    upload_time: "2026-11-24T00:00:00Z",
    free: false,
    hr: false,
    imdb_id: null,
    detail_url: null,
    category: "Movies",
    release: {
      title,
      year: 2024,
      season: null,
      episode: null,
      episode_to: null,
      resolution: "1080p",
      source: "WEB-DL",
      codec: "H.264",
      hdr: null,
      group: "GRP",
      confidence: "high",
    },
  };
}

test("Defect 1: PluginSchemaForm omits untouched keep secrets, requires site_id on credential-bound secret writes, and PluginConfigHost isolates attribution & schema-derived hasSecrets", async () => {
  const origFetch = globalThis.fetch;
  try {
    // 1a. Untouched "keep" secrets omitted from outSecrets
    const formCalls: Array<{
      values: Record<string, unknown>;
      secrets: Record<string, unknown>;
    }> = [];
    render(
      <PluginSchemaForm
        schema={{
          type: "object",
          properties: {
            note: { type: "string", default: "" },
            api_token: { type: "string", "x-ui": { widget: "secret" } },
          },
        }}
        values={{ note: "initial" }}
        secrets={{ api_token: { configured: true } }}
        editable={true}
        sites={[]}
        downloaders={[]}
        onSave={async (values, secrets) => {
          formCalls.push({ values, secrets });
        }}
      />,
    );
    fireEvent.change(screen.getByLabelText(/note/), { target: { value: "updated" } });
    fireEvent.click(screen.getByRole("button", { name: /保存配置/ }));
    await waitFor(() => assert.equal(formCalls.length, 1));
    assert.deepEqual(formCalls[0].values, { note: "updated" });
    assert.deepEqual(
      formCalls[0].secrets,
      {},
      "Untouched keep secret keys must be omitted so secrets is {}",
    );
    cleanup();

    // 1b. Credential-bound PluginDetailView -> PluginConfigHost:
    // - non-secret save sends secrets: {} without site_id
    // - secret replace/clear requires site_id selector and passes site_id to PUT /config
    const putCalls: Array<{ body: Record<string, unknown>; headers: Headers }> = [];
    const SITE_UUID = "11111111-1111-4111-8111-111111111111";
    let currentRev = 3;
    let currentValues: Record<string, unknown> = { note: "hello" };
    let currentSecrets: Record<string, { configured: boolean }> = {};
    globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
      const method = init?.method ?? "GET";
      if (url.endsWith("/plugins/org.example.cred") && method === "GET") {
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              id: "org.example.cred",
              name: "Cred Plugin",
              version: "1.0.0",
              package_hash: "pkg-cred-1",
              source: "external",
              signing_key_id: null,
              revision: currentRev,
              config_revision: 1,
              generation: 2,
              enabled: false,
              lifecycle: "disabled",
              health: "healthy",
              required_grants: [],
              effective_grants: [],
              active_operation_id: null,
              failure_reason: null,
              state_schema: 1,
              credential_bound: true,
              task_count: 0,
              version_count: 1,
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (url.endsWith("/plugins/org.example.cred/ui")) {
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
              plugin_id: "org.example.cred",
              schema_revision: "s1",
              schema: {
                type: "object",
                properties: {
                  note: { type: "string", default: "" },
                  cookie_secret: { type: "string", writeOnly: true },
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
              plugin_id: "org.example.cred",
              revision: currentRev,
              config_revision: 1,
              schema_revision: "s1",
              values: currentValues,
              secrets: currentSecrets,
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (url.endsWith("/config") && method === "PUT") {
        const parsed = JSON.parse(String(init?.body ?? "{}")) as Record<string, unknown>;
        putCalls.push({ body: parsed, headers: new Headers(init?.headers) });
        currentRev += 1;
        currentValues = (parsed.values as Record<string, unknown>) ?? {};
        currentSecrets = { cookie_secret: { configured: true } };
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              plugin_id: "org.example.cred",
              revision: currentRev,
              config_revision: 2,
              schema_revision: "s1",
              values: currentValues,
              secrets: currentSecrets,
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (url.endsWith("/sites")) {
        return new Response(
          JSON.stringify({
            ok: true,
            data: [{ id: SITE_UUID, name: "PT Alpha" }],
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
      <MemoryRouter initialEntries={["/settings/plugins/org.example.cred?tab=config"]}>
        <FeedbackProvider>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
            <PluginDetailView pluginId="org.example.cred" />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );

    const noteInput = await waitFor(() => screen.getByLabelText(/note/));
    fireEvent.change(noteInput, { target: { value: "non-secret-edit" } });
    fireEvent.click(screen.getByRole("button", { name: /保存配置/ }));
    await waitFor(() => assert.ok(screen.getByText(/配置版本 #4/)));
    assert.equal(putCalls.length, 1);
    assert.deepEqual(putCalls[0].body.secrets, {});
    assert.equal(putCalls[0].body.site_id, undefined);

    // Switch cookie_secret to replace without choosing site_id -> blocked with alert
    const replaceSelect = await waitFor(() => screen.getByLabelText(/cookie_secret 操作/));
    fireEvent.change(replaceSelect, { target: { value: "replace" } });
    const newSecretInput = await waitFor(() => screen.getByLabelText(/cookie_secret 新密钥/));
    fireEvent.change(newSecretInput, { target: { value: "sec-val" } });
    fireEvent.click(screen.getByRole("button", { name: /保存配置/ }));
    await waitFor(() => assert.ok(screen.getByRole("alert")));
    assert.equal(putCalls.length, 1, "Must not PUT secret write without site_id on credential_bound plugin");

    const siteSelect = screen.getByLabelText(/site_id|绑定站点/);
    fireEvent.change(siteSelect, { target: { value: SITE_UUID } });
    fireEvent.click(screen.getByRole("button", { name: /保存配置/ }));
    await waitFor(() => assert.equal(putCalls.length, 2));
    assert.equal(putCalls[1].body.site_id, SITE_UUID);
    assert.deepEqual(putCalls[1].body.secrets, {
      cookie_secret: { action: "replace", value: "sec-val" },
    });
    cleanup();

    // Standalone PluginConfigHost without credentialBound prop falls back to detailView.credential_bound
    // and pre-populates site_id from values.site_id on secret clear
    currentValues = { note: "bound", site_id: SITE_UUID };
    render(
      <MemoryRouter>
        <FeedbackProvider>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
            <PluginConfigHost pluginId="org.example.cred" />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );
    const clearSelect = await waitFor(() => screen.getByLabelText(/cookie_secret 操作/));
    fireEvent.change(clearSelect, { target: { value: "clear" } });
    fireEvent.click(screen.getByRole("button", { name: /保存配置/ }));
    await waitFor(() => assert.equal(putCalls.length, 3));
    assert.equal(putCalls[2].body.site_id, SITE_UUID);
    assert.deepEqual(putCalls[2].body.secrets, { cookie_secret: { action: "clear" } });
    cleanup();
    currentSecrets = {};

    // 1c. Federated PluginConfigHost:
    // - derives hasSecrets from schemaView.schema even when configView.secrets is {}
    // - attaches X-Plugin-Package/Generation ONLY on federated configController.save, NOT on host-only secret form
    putCalls.length = 0;
    const RemoteCfg: React.FC<PluginSurfaceProps> = ({ config }) => (
      <button type="button" onClick={() => void config?.save({ note: "from-federated" })}>
        远程保存公开配置
      </button>
    );

    render(
      <MemoryRouter>
        <FeedbackProvider>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
            <PluginConfigHost
              pluginId="org.example.cred"
              credentialBound={false}
              pluginEditable={true}
              surfaceDeps={{
                fetchDescriptor: async () => makeDescriptor(),
                fetchContract: async () => CONTRACT,
                loadSurface: async () => RemoteCfg,
              }}
            />
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );

    const remoteSaveBtn = await waitFor(() => screen.getByRole("button", { name: "远程保存公开配置" }));
    // Host-only secret form must be mounted even though configView.secrets was {}
    const hostSecretSelect = screen.getByLabelText(/cookie_secret 操作/);
    assert.ok(hostSecretSelect);

    fireEvent.click(remoteSaveBtn);
    await waitFor(() => assert.ok(screen.getByText(/配置版本 #7/)));
    assert.equal(putCalls.length, 1);
    assert.equal(putCalls[0].headers.get("X-Plugin-Package"), "pkg-cred-1");
    assert.equal(putCalls[0].headers.get("X-Plugin-Generation"), "2");

    fireEvent.change(screen.getByLabelText(/cookie_secret 操作/), { target: { value: "replace" } });
    const hostSecretInput = await waitFor(() => screen.getByLabelText(/cookie_secret 新密钥/));
    fireEvent.change(hostSecretInput, { target: { value: "host-only-sec" } });
    fireEvent.click(screen.getByRole("button", { name: /保存配置/ }));
    await waitFor(() => assert.equal(putCalls.length, 2));
    assert.equal(
      putCalls[1].headers.get("X-Plugin-Package"),
      null,
      "Host-only secret form must NOT attach X-Plugin-Package",
    );
    assert.equal(
      putCalls[1].headers.get("X-Plugin-Generation"),
      null,
      "Host-only secret form must NOT attach X-Plugin-Generation",
    );
  } finally {
    globalThis.fetch = origFetch;
    cleanup();
  }
});

test("Defect 2: reconcilePagedStreamItems deduplicates finalItems and SearchResults.loadMore avoids React 19 / StrictMode race", async () => {
  const h1 = makeHit("s1", "t1", "Alpha");
  const h2a = makeHit("s1", "t2", "Beta");
  const h2b = makeHit("s1", "t2", "Beta Duplicate");
  const rec = reconcilePagedStreamItems([h1], new Set(["s1/t2"]), [h1, h2a, h2b]);
  assert.equal(rec.added, 1, "Duplicate keys within finalItems must only count once");
  assert.equal(rec.items.length, 2, "Duplicate keys within finalItems must be deduplicated");

  const origFetch = globalThis.fetch;
  const origWinFetch = window.fetch;
  const origRaf = globalThis.requestAnimationFrame;
  const origCaf = globalThis.cancelAnimationFrame;
  globalThis.requestAnimationFrame = ((cb: FrameRequestCallback) =>
    setTimeout(() => cb(0), 0) as unknown as number) as typeof requestAnimationFrame;
  globalThis.cancelAnimationFrame = ((id: number) => clearTimeout(id)) as typeof cancelAnimationFrame;
  try {
    const encoder = new TextEncoder();
    const sseBody = (events: Array<{ event: string; data: unknown }>) =>
      new ReadableStream({
        start(ctrl) {
          for (const ev of events) {
            ctrl.enqueue(
              encoder.encode(`event: ${ev.event}\ndata: ${JSON.stringify(ev.data)}\n\n`),
            );
          }
          ctrl.close();
        },
      });

    globalThis.fetch = (async (input: string | URL | Request) => {
      const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
      if (url.includes("/search/torrents/stream")) {
        const u = new URL(url, "http://localhost");
        const page = u.searchParams.get("page");
        if (!page) {
          return new Response(
            sseBody([
              { event: "start", data: { keyword: "dune", sites: [{ site_id: "s1", site_name: "S1" }] } },
              {
                event: "site_result",
                data: { site_id: "s1", site_name: "S1", count: 1, elapsed_ms: 5, items: [h1] },
              },
              {
                event: "done",
                data: { total: 1, elapsed_ms: 6, sites: [{ site_id: "s1", site_name: "S1", count: 1, error: null, elapsed_ms: 5 }], items: [h1] },
              },
            ]),
            { status: 200, headers: { "Content-Type": "text/event-stream" } },
          );
        }
        if (page === "2") {
          const h3Filtered = makeHit("s1", "t3", "Gamma Filtered");
          return new Response(
            sseBody([
              { event: "start", data: { keyword: "dune", sites: [{ site_id: "s1", site_name: "S1" }] } },
              {
                event: "site_result",
                data: { site_id: "s1", site_name: "S1", count: 3, elapsed_ms: 5, items: [h1, h2a, h3Filtered] },
              },
              {
                event: "done",
                data: {
                  total: 2,
                  elapsed_ms: 6,
                  sites: [{ site_id: "s1", site_name: "S1", count: 2, error: null, elapsed_ms: 5 }],
                  items: [h1, h2a, h2b],
                },
              },
            ]),
            { status: 200, headers: { "Content-Type": "text/event-stream" } },
          );
        }
        return new Response(
          sseBody([
            { event: "start", data: { keyword: "dune", sites: [{ site_id: "s1", site_name: "S1" }] } },
            {
              event: "site_result",
              data: { site_id: "s1", site_name: "S1", count: 2, elapsed_ms: 4, items: [h1, h2a] },
            },
            {
              event: "done",
              data: {
                total: 2,
                elapsed_ms: 5,
                sites: [{ site_id: "s1", site_name: "S1", count: 2, error: null, elapsed_ms: 4 }],
                items: [h1, h2a],
              },
            },
          ]),
          { status: 200, headers: { "Content-Type": "text/event-stream" } },
        );
      }
      return new Response(JSON.stringify({ ok: true, data: [] }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }) as typeof fetch;
    window.fetch = globalThis.fetch;

    render(
      <React.StrictMode>
        <MemoryRouter initialEntries={["/search?q=dune&view=list"]}>
          <FeedbackProvider>
            <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
              <SearchResults
                query={{
                  keyword: "dune",
                  scope: { label: null, categories: [], siteIds: [], posterMode: false, skipHistory: false },
                }}
              />
            </SessionProvider>
          </FeedbackProvider>
        </MemoryRouter>
      </React.StrictMode>,
    );

    const loadPage2 = await waitFor(() =>
      screen.getByRole("button", { name: "加载更多（第 2 页）" }),
    );
    fireEvent.click(loadPage2);

    // Page 2 added h2a -> must NOT prematurely mark exhausted even under React 19 deferred batching + StrictMode
    const loadPage3 = await waitFor(() =>
      screen.getByRole("button", { name: "加载更多（第 3 页）" }),
    );
    assert.ok(screen.getByText(/共 2 条结果/));
    assert.equal(screen.queryByText(/Gamma Filtered/), null);

    // Page 3 returns only already-seen items -> now exhausted must become true
    fireEvent.click(loadPage3);
    await waitFor(() => assert.ok(screen.getByText("已加载全部 3 页结果")));
  } finally {
    globalThis.requestAnimationFrame = origRaf;
    globalThis.cancelAnimationFrame = origCaf;
    globalThis.fetch = origFetch;
    window.fetch = origWinFetch;
    cleanup();
  }
});

test("Defect 4: AppShell guards malformed URI in navIdFromPath and plugin route pages do not double-decode useParams", async () => {
  const origFetch = globalThis.fetch;
  const origMatchMedia = window.matchMedia;
  window.matchMedia = ((query: string) => ({
    matches: false,
    media: query,
    onchange: null,
    addListener: () => {},
    removeListener: () => {},
    addEventListener: () => {},
    removeEventListener: () => {},
    dispatchEvent: () => false,
  })) as typeof window.matchMedia;
  const requestedUrls: string[] = [];
  globalThis.fetch = (async (input: string | URL | Request) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    requestedUrls.push(url);
    return new Response(
      JSON.stringify({ ok: false, error: { code: "plugin.missing", message: "not found" } }),
      { status: 404, headers: { "Content-Type": "application/json" } },
    );
  }) as typeof fetch;

  try {
    // 4a. Malformed percent encoding under /plugin-apps/... must not throw URIError in AppShell
    render(
      <MemoryRouter initialEntries={["/plugin-apps/%E0%A4%A/main"]}>
        <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
          <JobsProvider>
            <DownloadTasksProvider>
              <AppShell>
                <div>SHELL_SURVIVED</div>
              </AppShell>
            </DownloadTasksProvider>
          </JobsProvider>
        </SessionProvider>
      </MemoryRouter>,
    );
    assert.ok(screen.getByText("SHELL_SURVIVED"));
    cleanup();

    // 4b. Literal `%` in pluginId encoded once as `%25` in URL -> useParams yields `org.example.%raw`;
    // route pages must NOT call decodeURIComponent a second time (which throws URIError)!
    requestedUrls.length = 0;
    render(
      <MemoryRouter initialEntries={["/settings/plugins/org.example.%25raw"]}>
        <FeedbackProvider>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
            <Routes>
              <Route path="/settings/plugins/:pluginId" element={<PluginDetailPage />} />
            </Routes>
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );
    await waitFor(() =>
      assert.ok(requestedUrls.some((u) => u.includes("/plugins/org.example.%25raw"))),
    );
    cleanup();

    requestedUrls.length = 0;
    render(
      <MemoryRouter initialEntries={["/plugin-apps/org.example.%25raw/surf%2520id"]}>
        <FeedbackProvider>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "ep-1" }}>
            <Routes>
              <Route path="/plugin-apps/:pluginId/:surfaceId" element={<PluginAppRoutePage />} />
            </Routes>
          </SessionProvider>
        </FeedbackProvider>
      </MemoryRouter>,
    );
    await waitFor(() =>
      assert.ok(requestedUrls.some((u) => u.includes("/plugins/org.example.%25raw/ui"))),
    );
  } finally {
    window.matchMedia = origMatchMedia;
    globalThis.fetch = origFetch;
    cleanup();
  }
});

test("Defect 5: sdk.tasks.run awaits auto-enable terminal state before /run, and sdk.downloads.cleanup rejects multiple handles", async () => {
  let pollCount = 0;
  let shouldSucceed = false;
  let taskEnabled = false;
  let taskRev = 1;
  const runCalls: string[] = [];
  let confirmCalls = 0;

  const fakeFetch: typeof fetch = async (input, init) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    const method = init?.method ?? "GET";
    const headers = new Headers(init?.headers);

    if (url.includes("/tasks?") && method === "GET") {
      return new Response(
        JSON.stringify({
          ok: true,
          data: [
            {
              id: "sync-task",
              definition_revision: taskRev,
              enabled: taskEnabled,
              schedule: { interval_secs: 3600 },
              config: {},
            },
          ],
          next_cursor: null,
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.endsWith("/tasks/sync-task/update") && method === "POST") {
      pollCount = 0;
      return new Response(
        JSON.stringify({ ok: true, data: { operation_id: "op-auto-en", state: "queued" } }),
        { status: 202, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.endsWith("/plugin-operations/op-auto-en")) {
      pollCount += 1;
      if (pollCount === 1) {
        return new Response(
          JSON.stringify({ ok: true, data: { operation_id: "op-auto-en", state: "running", revision: 1 } }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (shouldSucceed) {
        taskEnabled = true;
        taskRev = 2;
        return new Response(
          JSON.stringify({ ok: true, data: { operation_id: "op-auto-en", state: "succeeded", revision: 2 } }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      return new Response(
        JSON.stringify({
          ok: true,
          data: { operation_id: "op-auto-en", state: "failed", revision: 2, error: "policy denied enable" },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.endsWith("/tasks/sync-task/run") && method === "POST") {
      runCalls.push(headers.get("If-Match") ?? "");
      return new Response(
        JSON.stringify({ ok: true, data: { operation_id: "op-run-ok", state: "queued" } }),
        { status: 202, headers: { "Content-Type": "application/json" } },
      );
    }
    throw new Error(`Unexpected fetch: ${method} ${url}`);
  };

  const host = createPluginHostSdk({
    descriptor: makeDescriptor({ lifecycle: "enabled" }),
    surfaceId: "main",
    session: ADMIN_SESSION,
    loginEpoch: "ep-5",
    theme: "silver",
    navigate: () => {},
    notifyToast: () => {},
    openConfirm: async () => {
      confirmCalls += 1;
      return true;
    },
    openDialog: () => () => {},
    fetchImpl: fakeFetch,
  });

  try {
    // 5a. Queued -> running -> failed auto-enable MUST throw request-failed and NOT call /run
    await assert.rejects(
      () => host.sdk.tasks.run("sync-task", {}, { expectedRevision: "0", idempotencyKey: "k-en-fail" }),
      (err: unknown) =>
        isSdkError(err) && err.code === "request-failed" && /policy denied enable/.test(err.message),
    );
    assert.equal(runCalls.length, 0, "Failed auto-enable must not proceed to /run");
    assert.ok(pollCount >= 2, "Must poll operations.get until terminal state");

    // Queued -> running -> succeeded auto-enable proceeds to /run with updated revision "2"
    shouldSucceed = true;
    const okRef = await host.sdk.tasks.run("sync-task", {}, { expectedRevision: "0", idempotencyKey: "k-en-ok" });
    assert.equal(okRef.operation_id, "op-run-ok");
    assert.deepEqual(runCalls, ['"2"']);

    // 5b. Multi-handle cleanup rejected upfront in both modes without prompting confirm
    for (const mode of ["task-only", "files"] as const) {
      await assert.rejects(
        () =>
          host.sdk.downloads.cleanup({
            owned_task_handles: ["uh1.a.1" as UiHandle, "uh1.b.2" as UiHandle],
            mode,
          }),
        (err: unknown) =>
          isSdkError(err) &&
          err.code === "request-failed" &&
          err.message === "cleanup supports one owned_task_handle at a time",
      );
    }
    assert.equal(confirmCalls, 0, "Multi-handle cleanup must fail upfront before openConfirm");
  } finally {
    host.dispose();
  }
});

test("Defect 6: web/lib/api/plugins exposes repository & upload flows without hardcoded builtin UI bundle scripts", async () => {
  const apiSource = fs.readFileSync(
    path.resolve(import.meta.dirname, "../lib/api/plugins.ts"),
    "utf8",
  );
  assert.match(apiSource, /listPluginRepositories/);
  assert.match(apiSource, /savePluginRepositories/);
  assert.match(apiSource, /getPluginRepositoryCatalog/);
  assert.match(apiSource, /stagePluginUpload/);
  assert.equal(apiSource.includes("ui_builtins_assets"), false);
});
