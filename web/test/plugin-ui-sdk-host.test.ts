import "./plugin-ui-env";

import assert from "node:assert/strict";
import test from "node:test";
import React from "react";

import { isSdkError } from "@crawler-media/plugin-ui-sdk";
import { fetchPluginUiDescriptor, type UiDescriptorView } from "../lib/api/plugins";
import {
  clearFederationCache,
  descriptorCacheKey,
  evictPluginFederationCache,
  isRemoteLoaded,
  loadFederatedSurface,
} from "../lib/plugins/federation-loader";
import { createPluginHostSdk } from "../lib/plugins/sdk";

function makeDescriptor(overrides: Partial<UiDescriptorView> = {}): UiDescriptorView {
  return {
    plugin_id: "cm.checkin",
    plugin_name: "Site Check-in Orchestrator",
    version: "1.0.0",
    generation: 3,
    lifecycle: "enabled",
    source: "external",
    package_hash: "pkg-hash-abc",
    package_sha256: "pkg-hash-abc",
    bundle_sha256: "bundle-hash-xyz",
    subject_user_id: "user-admin-1",
    subject_role: "admin",
    ui_mode: "federation-react",
    remote_name: "cmCheckin",
    entry: "ui/assets/remoteEntry.js",
    entry_url: "/api/v1/plugin-ui-assets/cm.checkin/pkg-hash-abc/ui/assets/remoteEntry.js",
    asset_base_url: "/api/v1/plugin-ui-assets/cm.checkin/pkg-hash-abc",
    contract: "1.0.0",
    react: "19.3.0",
    react_dom: "19.3.0",
    ui_trusted: true,
    trust: {
      status: "trusted",
      revision: 1,
      granted_at: "2026-11-23T00:00:00Z",
      granted_by_user_id: "system",
      revoked_at: null,
    },
    capabilities: ["read:sites", "read:history", "command:site.maintenance", "task:run:check-in"],
    surfaces: [
      {
        id: "main",
        kind: "page",
        expose: "./Page",
        title: "Site Check-in",
        icon: "calendar-check",
        location: "/settings/plugins/cm.checkin?tab=page&view=main",
        mountable: true,
        editable: false,
        required_role: "admin",
      },
    ],
    ...overrides,
  };
}

const ADMIN_SESSION = {
  id: "user-admin-1",
  username: "admin",
  nickname: "Admin",
  avatar_url: null,
  role: "admin" as const,
  capabilities: { allow_subscribe: true, allow_search: true, allow_direct_download: true },
};

test("fetchPluginUiDescriptor uses cookie credentials only and omits Authorization Bearer", async () => {
  const storageMap = new Map<string, string>([["mc_token", "secret-bearer-token"]]);
  Object.defineProperty(window, "localStorage", {
    value: {
      getItem: (k: string) => storageMap.get(k) ?? null,
      setItem: (k: string, v: string) => storageMap.set(k, v),
      removeItem: (k: string) => storageMap.delete(k),
    },
    configurable: true,
  });

  const captured: Array<{ url: string; credentials?: RequestCredentials; authHeader: string | null }> = [];
  const origFetch = globalThis.fetch;
  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    const headers = new Headers(init?.headers);
    captured.push({
      url,
      credentials: init?.credentials,
      authHeader: headers.get("Authorization"),
    });
    return new Response(JSON.stringify({ ok: true, data: makeDescriptor() }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  }) as typeof fetch;

  try {
    const desc = await fetchPluginUiDescriptor("cm.checkin");
    assert.equal(desc.subject_user_id, "user-admin-1");
    assert.equal(captured.length, 1);
    assert.equal(captured[0].url, "/api/v1/plugins/cm.checkin/ui");
    assert.equal(captured[0].credentials, "include");
    assert.equal(captured[0].authHeader, null);
  } finally {
    globalThis.fetch = origFetch;
  }
});

test("PluginHostSDK attaches X-Plugin-Package/Generation headers, avoids entity If-Match on commands, and fences disposal", async () => {
  const calls: Array<{ url: string; headers: Headers; body: unknown }> = [];
  let mode: "ok" | "stale-gen" | "stale-handle" = "ok";

  const fakeFetch: typeof fetch = async (input, init) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    const headers = new Headers(init?.headers);
    const body = typeof init?.body === "string" ? JSON.parse(init.body) : null;
    calls.push({ url, headers, body });

    if (mode === "stale-gen") {
      return new Response(
        JSON.stringify({ ok: false, error: { code: "plugin.stale_generation", message: "stale generation" } }),
        { status: 409, headers: { "Content-Type": "application/json" } },
      );
    }
    if (mode === "stale-handle") {
      return new Response(
        JSON.stringify({ ok: false, error: { code: "plugin.stale_handle", message: "handle expired" } }),
        { status: 403, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.endsWith("/ui/query") && body?.name === "downloaders.list") {
      return new Response(
        JSON.stringify({ ok: true, data: { items: [{ id: "dl-1", name: "qB", kind: "qbittorrent", enabled: true }], total: 1 } }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.endsWith("/ui/query") && body?.name === "checkin.receipts") {
      return new Response(
        JSON.stringify({
          ok: true,
          data: {
            items: [
              {
                receipt_id: "r-1",
                site_id: "site-1",
                action: "check-in",
                state: "completed",
                outcome: "already-completed",
                evidence: "already checked in today",
                updated_at: "2026-11-23T01:00:00Z",
              },
            ],
            next_cursor: null,
          },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    return new Response(
      JSON.stringify({ ok: true, data: { operation_id: "op-99", state: "queued" } }),
      { status: 202, headers: { "Content-Type": "application/json" } },
    );
  };

  const handle = createPluginHostSdk({
    descriptor: makeDescriptor(),
    surfaceId: "main",
    session: ADMIN_SESSION,
    loginEpoch: "user-admin-1:epoch-1",
    theme: "silver",
    navigate: () => {},
    notifyToast: () => {},
    openConfirm: async () => true,
    openDialog: () => () => {},
    getPluginRevision: () => "4",
    fetchImpl: fakeFetch,
  });

  const dls = await handle.sdk.catalogs.list("downloaders", {});
  assert.deepEqual(dls.items[0].capabilities, []);

  const receipts = await handle.sdk.query("checkin.receipts", { site_id: "site-1" });
  assert.equal(receipts.items[0].state, "already-completed");
  assert.equal(receipts.items[0].observed_at, "2026-11-23T01:00:00Z");

  // When plugin.revision is 4 and site.revision (expectedRevision) is 1, If-Match MUST NOT be "1"!
  const op = await handle.sdk.commands.submit(
    "site.maintenance",
    { site_handle: "uh1.abc.def" as never, action: "check-in" },
    { expectedRevision: "1", idempotencyKey: "k-1" },
  );
  assert.equal(op.operation_id, "op-99");
  const cmdCall = calls[calls.length - 1];
  assert.notEqual(cmdCall.headers.get("If-Match"), "\"1\"");
  assert.equal(cmdCall.headers.get("If-Match"), "\"4\"");
  assert.equal(cmdCall.headers.get("X-Plugin-Package"), "pkg-hash-abc");
  assert.equal(cmdCall.headers.get("X-Plugin-Generation"), "3");
  assert.equal(cmdCall.headers.get("Idempotency-Key"), "k-1");

  mode = "stale-gen";
  await assert.rejects(
    () => handle.sdk.query("sites.list", {}),
    (err: unknown) => isSdkError(err) && err.code === "stale-generation",
  );

  mode = "stale-handle";
  await assert.rejects(
    () => handle.sdk.objects.resolve("site", "site-1"),
    (err: unknown) => isSdkError(err) && err.code === "stale-handle",
  );

  const countBeforeDispose = calls.length;
  handle.dispose();
  await assert.rejects(
    () => handle.sdk.query("sites.list", {}),
    (err: unknown) => isSdkError(err) && err.code === "host-disposed",
  );
  assert.equal(calls.length, countBeforeDispose);
});

test("sdk.tasks auto-enables disabled tasks on run, forwards cursors, and normalizes task receipts; disabled catalogs use config.options", async () => {
  let taskEnabled = false;
  let taskRev = 1;
  const seenCalls: Array<{ url: string; method: string; headers: Headers; body: unknown }> = [];

  const fakeFetch: typeof fetch = async (input, init) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    const method = init?.method ?? "GET";
    const headers = new Headers(init?.headers);
    const body = typeof init?.body === "string" ? JSON.parse(init.body) : null;
    seenCalls.push({ url, method, headers, body });

    if (url.includes("/tasks?") && method === "GET") {
      return new Response(
        JSON.stringify({
          ok: true,
          data: [
            {
              id: "check-in",
              definition_revision: taskRev,
              enabled: taskEnabled,
              schedule: { interval_secs: 86400 },
              config: { site_id: "s-1" },
            },
          ],
          next_cursor: "cur-tasks-2",
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.endsWith("/tasks/check-in/update") && method === "POST") {
      taskEnabled = true;
      taskRev += 1;
      return new Response(
        JSON.stringify({ ok: true, data: { operation_id: "op-en-1", state: "succeeded" } }),
        { status: 202, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.endsWith("/tasks/check-in/run") && method === "POST") {
      return new Response(
        JSON.stringify({ ok: true, data: { operation_id: "op-run-1", state: "queued" } }),
        { status: 202, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.includes("/tasks/check-in/receipts")) {
      return new Response(
        JSON.stringify({
          ok: true,
          data: [
            {
              operation_id: "op-rc-1",
              scope: [{ Site: "site-scoped-1" }],
              state: "succeeded",
              phase: "done",
              observed_at: "2026-11-24T00:00:00Z",
            },
          ],
          next_cursor: "cur-rc-2",
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.endsWith("/ui/query") && body?.name === "config.options") {
      return new Response(
        JSON.stringify({
          ok: true,
          data: { items: [{ id: "site-cfg-1", name: "Disabled Option Site" }], next_cursor: null },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    throw new Error(`Unexpected fetch: ${method} ${url}`);
  };

  const disabledHost = createPluginHostSdk({
    descriptor: makeDescriptor({ lifecycle: "disabled" }),
    surfaceId: "settings",
    session: ADMIN_SESSION,
    loginEpoch: "ep-1",
    theme: "silver",
    navigate: () => {},
    notifyToast: () => {},
    openConfirm: async () => true,
    openDialog: () => () => {},
    fetchImpl: fakeFetch,
  });

  const cfgSites = await disabledHost.sdk.catalogs.list("sites", { limit: 10 });
  assert.equal(cfgSites.items[0].id, "site-cfg-1");
  assert.equal((seenCalls[0].body as { name: string }).name, "config.options");

  const listPage = await disabledHost.sdk.tasks.list({ cursor: "cur-tasks-1", limit: 25 });
  assert.ok(seenCalls[1].url.includes("cursor=cur-tasks-1"));
  assert.equal(listPage.next_cursor, "cur-tasks-2");
  assert.deepEqual(listPage.items[0], {
    task_id: "check-in",
    title: "check-in",
    enabled: false,
    definition_revision: "1",
  });

  const runRes = await disabledHost.sdk.tasks.run(
    "check-in",
    {},
    { expectedRevision: "0", idempotencyKey: "manual-k1" },
  );
  assert.equal(runRes.operation_id, "op-run-1");
  const updateCall = seenCalls.find((c) => c.url.endsWith("/tasks/check-in/update"))!;
  assert.equal(updateCall.headers.get("If-Match"), "\"1\"");
  assert.equal(updateCall.headers.get("Idempotency-Key"), "manual-k1:enable");
  const runCall = seenCalls.find((c) => c.url.endsWith("/tasks/check-in/run"))!;
  assert.equal(runCall.headers.get("If-Match"), "\"2\"");

  const rcPage = await disabledHost.sdk.tasks.receipts("check-in", { cursor: "00000000-0000-0000-0000-000000000001" });
  assert.equal(rcPage.items[0].site_id, "site-scoped-1");
  assert.equal(rcPage.items[0].state, "completed");
  assert.equal(rcPage.next_cursor, "cur-rc-2");
  disabledHost.dispose();
});

test("loadFederatedSurface scopes cache by subject_user_id, loads descriptor.styles, and triggers reload on package switch", async () => {
  clearFederationCache();
  const imports: Array<{ url: string; sharedKeys: string[]; reactEqual: boolean }> = [];
  let reloadCount = 0;
  const StubCompA: React.ComponentType<unknown> = () => null;
  const StubCompB: React.ComponentType<unknown> = () => null;

  assert.notEqual(
    descriptorCacheKey(makeDescriptor({ subject_user_id: "u1" })),
    descriptorCacheKey(makeDescriptor({ subject_user_id: "u2" })),
  );

  // Descriptor with empty styles mounts 0 stylesheet links
  const comp1 = await loadFederatedSurface(
    makeDescriptor({ generation: 1, bundle_sha256: "b1", styles: [] }),
    "./Page",
    {
      importRemote: async (url) => ({
        init: async (scope) => {
          const reactProvider = scope.react?.["19.3.0"];
          const providedReact = reactProvider ? await (await reactProvider.get())() : null;
          imports.push({
            url,
            sharedKeys: Object.keys(scope),
            reactEqual: providedReact === React,
          });
        },
        get: async () => () => ({ default: StubCompA }),
      }),
    },
  );
  assert.equal(comp1, StubCompA);
  assert.equal(isRemoteLoaded("cmCheckin"), true);
  assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 0);
  assert.equal(imports[0].reactEqual, true);

  // Same descriptor hits cache
  const comp1Cached = await loadFederatedSurface(
    makeDescriptor({ generation: 1, bundle_sha256: "b1", styles: [] }),
    "./Page",
    {
      importRemote: async () => {
        throw new Error("should use cached container");
      },
    },
  );
  assert.equal(comp1Cached, StubCompA);

  // Switching package_hash/bundle_sha256 on same remote_name triggers reload hook and loads hashed CSS
  const comp2 = await loadFederatedSurface(
    makeDescriptor({
      generation: 2,
      package_hash: "pkg-hash-v2",
      bundle_sha256: "b2",
      styles: ["ui/assets/style-hashed.css"],
    }),
    "./Page",
    {
      reloadPage: () => {
        reloadCount += 1;
      },
      importRemote: async (url) => ({
        init: async () => {
          imports.push({ url, sharedKeys: [], reactEqual: true });
        },
        get: async () => () => ({ default: StubCompB }),
      }),
    },
  );
  assert.equal(comp2, StubCompB);
  assert.equal(reloadCount, 1);
  const links = Array.from(document.head.querySelectorAll("link[data-cm-plugin-ui]")) as HTMLLinkElement[];
  assert.equal(links.length, 1);
  assert.ok(links[0].href.endsWith("/ui/assets/style-hashed.css"));
  clearFederationCache();
});

test("sdk.media.detail and sdk.navigation.open('media') resolve library, tmdb, douban, and fallback item routes", async () => {
  const navigated: string[] = [];
  let mediaFixture: Record<string, unknown> | null = null;
  let rejectMedia = false;

  const fakeFetch: typeof fetch = async (input, init) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    const body = typeof init?.body === "string" ? JSON.parse(init.body) : null;
    if (url.endsWith("/ui/query") && body?.name === "media.get") {
      if (rejectMedia || !mediaFixture) {
        return new Response(
          JSON.stringify({ ok: false, error: { code: "media.missing", message: "not found" } }),
          { status: 404, headers: { "Content-Type": "application/json" } },
        );
      }
      return new Response(JSON.stringify({ ok: true, data: mediaFixture }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }
    throw new Error(`Unexpected fetch: ${url}`);
  };

  const handle = createPluginHostSdk({
    descriptor: makeDescriptor(),
    surfaceId: "main",
    session: ADMIN_SESSION,
    loginEpoch: "ep-nav-1",
    theme: "silver",
    navigate: (to) => navigated.push(to),
    notifyToast: () => {},
    openConfirm: async () => true,
    openDialog: () => () => {},
    fetchImpl: fakeFetch,
  });

  try {
    mediaFixture = { id: "m-lib", title: "Lib Movie", library_id: "lib-movies", kind: "movie", tmdb_id: 550 };
    await handle.sdk.media.detail({ media_id: "m-lib" });
    assert.equal(navigated.at(-1), "/library/lib-movies/item/m-lib");

    mediaFixture = { id: "m-tmdb", title: "TMDB Show", kind: "tv", tmdb_id: 1396 };
    await handle.sdk.media.detail({ media_id: "m-tmdb" });
    assert.equal(navigated.at(-1), "/media/tv/1396");

    mediaFixture = { id: "m-db", title: "Douban Film", kind: "movie", douban_id: "26794435" };
    await handle.sdk.media.detail({ media_id: "m-db" });
    assert.equal(navigated.at(-1), "/media/douban/26794435");

    rejectMedia = true;
    await handle.sdk.media.detail({ media_id: "m-orphan" });
    assert.equal(navigated.at(-1), "/media/item/m-orphan");

    await handle.sdk.navigation.open("media", {
      media_id: "m-1",
      library_id: "lib-a",
    });
    assert.equal(navigated.at(-1), "/library/lib-a/item/m-1");

    await handle.sdk.navigation.open("media", {
      media_id: "m-2",
      kind: "movie",
      tmdb_id: "603",
    });
    assert.equal(navigated.at(-1), "/media/movie/603");

    await handle.sdk.navigation.open("media", {
      media_id: "m-3",
      douban_id: "1292052",
    });
    assert.equal(navigated.at(-1), "/media/douban/1292052");

    await handle.sdk.navigation.open("media", { media_id: "m-4" });
    assert.equal(navigated.at(-1), "/media/item/m-4");
  } finally {
    handle.dispose();
  }
});

test("PluginHostSDK normalizes Revision strings across queries and guards expectedRevision in tasks.run", async () => {
  const taskRunHeaders: string[] = [];

  const fakeFetch: typeof fetch = async (input, init) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    const headers = new Headers(init?.headers);
    const body = typeof init?.body === "string" ? JSON.parse(init.body) : null;

    if (url.endsWith("/ui/query")) {
      if (body?.name === "handles.resolve") {
        return new Response(
          JSON.stringify({
            ok: true,
            data: { handle: "uh1.num.rev", kind: "site", revision: 7, expires_at: "2026-11-25T00:00:00Z" },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (body?.name === "boost.snapshot") {
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              revision: 9,
              used_bytes: 100,
              reserved_bytes: 50,
              retained_file_bytes: 0,
              disk: { available_bytes: 1000, observed_at: 1700000000, status: "available" },
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (body?.name === "media.get") {
        return new Response(
          JSON.stringify({ ok: true, data: { id: "med-1", title: "Dune", kind: "movie", year: 2021 } }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (body?.name === "subscribes.list") {
        return new Response(
          JSON.stringify({
            ok: true,
            data: { items: [{ id: "sub-1", media_id: "med-1", tracking_state: "active" }], total: 1 },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
      if (body?.name === "downloads.list") {
        return new Response(
          JSON.stringify({
            ok: true,
            data: {
              items: [{ id: "dlop-1", downloader_id: null, info_hash: "deadbeef", state: "seeding" }],
              next_cursor: null,
            },
          }),
          { status: 200, headers: { "Content-Type": "application/json" } },
        );
      }
    }
    if (url.endsWith("/tasks/check-in/run")) {
      taskRunHeaders.push(headers.get("If-Match") ?? "");
      return new Response(
        JSON.stringify({ ok: true, data: { operation_id: "op-tr-1", state: "queued" } }),
        { status: 202, headers: { "Content-Type": "application/json" } },
      );
    }
    throw new Error(`Unexpected fetch: ${url}`);
  };

  const handle = createPluginHostSdk({
    descriptor: makeDescriptor(),
    surfaceId: "main",
    session: ADMIN_SESSION,
    loginEpoch: "ep-rev-1",
    theme: "silver",
    navigate: () => {},
    notifyToast: () => {},
    openConfirm: async () => true,
    openDialog: () => () => {},
    fetchImpl: fakeFetch,
  });

  try {
    const resolved = await handle.sdk.objects.resolve("site", "site-1");
    assert.equal(typeof resolved.revision, "string");
    assert.equal(resolved.revision, "7");

    // Passing resolved.revision AND a raw numeric expectedRevision must both work without TypeError
    await handle.sdk.tasks.run("check-in", {}, { expectedRevision: resolved.revision, idempotencyKey: "k-str" });
    await handle.sdk.tasks.run("check-in", {}, { expectedRevision: 5 as unknown as string, idempotencyKey: "k-num" });
    assert.deepEqual(taskRunHeaders, ['"7"', '"5"']);

    const snap = await handle.sdk.query("boost.snapshot", {});
    assert.equal(typeof snap.revision, "string");
    assert.equal(snap.revision, "9");

    const media = await handle.sdk.query("media.get", { media_id: "med-1" });
    assert.equal(typeof media.revision, "string");
    assert.equal(media.title, "Dune");

    const subs = await handle.sdk.query("subscribes.list", {});
    assert.equal(typeof subs.items[0].revision, "string");
    assert.equal(typeof subs.items[0].title, "string");
    assert.ok(subs.items[0].title.length > 0);

    const dls = await handle.sdk.query("downloads.list", {});
    assert.equal(typeof dls.items[0].revision, "string");
    assert.equal(dls.items[0].name, "deadbeef");
    assert.equal(dls.items[0].downloader_id, "");
  } finally {
    handle.dispose();
  }
});

test("evictPluginFederationCache removes only target plugin stylesheet links and containerCache entries", async () => {
  clearFederationCache();
  assert.equal(typeof evictPluginFederationCache, "function");

  let alphaImports = 0;
  const StubComp: React.ComponentType<unknown> = () => null;
  await loadFederatedSurface(
    makeDescriptor({
      plugin_id: "org.example.alpha",
      remote_name: "alphaRemote",
      styles: ["ui/assets/alpha.css"],
    }),
    "./Page",
    {
      importRemote: async () => {
        alphaImports += 1;
        return { init: async () => {}, get: async () => () => ({ default: StubComp }) };
      },
    },
  );
  await loadFederatedSurface(
    makeDescriptor({
      plugin_id: "org.example.beta",
      remote_name: "betaRemote",
      styles: ["ui/assets/beta.css"],
    }),
    "./Page",
    {
      importRemote: async () => ({
        init: async () => {},
        get: async () => () => ({ default: StubComp }),
      }),
    },
  );

  assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 2);
  evictPluginFederationCache("org.example.alpha");

  const remaining = Array.from(document.head.querySelectorAll("link[data-cm-plugin-ui]"));
  assert.equal(remaining.length, 1);
  assert.match(remaining[0].getAttribute("data-cm-plugin-ui") ?? "", /^org\.example\.beta:/);

  // Loading alpha again must re-import because its containerCache entry was evicted
  await loadFederatedSurface(
    makeDescriptor({
      plugin_id: "org.example.alpha",
      remote_name: "alphaRemote",
      styles: ["ui/assets/alpha.css"],
    }),
    "./Page",
    {
      importRemote: async () => {
        alphaImports += 1;
        return { init: async () => {}, get: async () => () => ({ default: StubComp }) };
      },
    },
  );
  assert.equal(alphaImports, 2);
  clearFederationCache();
});

test("endpoint.request supports multi-segment paths, explicit method, raw EndpointResponse bodies, and files.selection normalization", async () => {
  const seen: Array<{ url: string; method: string }> = [];
  const fakeFetch: typeof fetch = async (input, init) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    const method = init?.method ?? "GET";
    seen.push({ url, method });
    if (url.includes("/endpoints/v1/kv-get")) {
      return new Response(JSON.stringify({ items: [1, 2] }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }
    if (url.includes("/endpoints/boost-snapshot")) {
      return new Response("rev=1;tasks=0;res=0", {
        status: 200,
        headers: { "Content-Type": "text/plain; charset=utf-8" },
      });
    }
    if (url.endsWith("/ui/query")) {
      return new Response(
        JSON.stringify({
          ok: true,
          data: { handle: "uh1.sel", revision: 3, expires_at: "2026-12-01T00:00:00Z" },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    return new Response(JSON.stringify({ ok: false, error: { code: "plugin.missing", message: "missing" } }), {
      status: 404,
      headers: { "Content-Type": "application/json" },
    });
  };

  const handle = createPluginHostSdk({
    descriptor: makeDescriptor({ capabilities: ["read:library"] }),
    surfaceId: "main",
    session: ADMIN_SESSION,
    loginEpoch: "ep-1",
    theme: "silver",
    navigate: () => {},
    notifyToast: () => {},
    openConfirm: async () => true,
    openDialog: () => () => {},
    fetchImpl: fakeFetch,
  });

  try {
    const rawJson = await handle.sdk.endpoint.request("v1/kv-get", {
      method: "DELETE",
      query: { site_id: "s1" },
    });
    assert.deepEqual(rawJson, { items: [1, 2] });
    assert.ok(seen[0].url.endsWith("/plugins/cm.checkin/endpoints/v1/kv-get?site_id=s1"));
    assert.equal(seen[0].method, "DELETE");

    const plain = await handle.sdk.endpoint.request("/boost-snapshot", {});
    assert.equal(plain, "rev=1;tasks=0;res=0");

    const sel = await handle.sdk.query("files.selection", {
      library_id: "lib-1",
      file_ids: ["f-1"],
      expected_revisions: ["3"],
    });
    assert.equal(sel.handle, "uh1.sel");
    assert.equal(sel.revision, "3");
    assert.equal(sel.expires_at, "2026-12-01T00:00:00Z");
  } finally {
    handle.dispose();
  }
});
