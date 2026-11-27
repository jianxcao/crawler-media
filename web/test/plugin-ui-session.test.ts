import "./plugin-ui-env";

import assert from "node:assert/strict";
import test from "node:test";

import { getSession, login, logout, type SessionView } from "../lib/api/auth";
import type { UiDescriptorView } from "../lib/api/plugins";
import { createPluginHostSdk } from "../lib/plugins/sdk";
import { mapHttpToSdkError } from "../lib/plugins/sdk-helpers";
import {
  AUTH_EPOCH_STORAGE_KEY,
  AUTH_SESSION_CHANNEL,
  watchSessionIdentity,
} from "../lib/session";

test("SessionView includes id on login and getSession, and broadcasts cross-tab session changes", async () => {
  const posted: Array<{ channel: string; payload: unknown }> = [];
  const origBC = (globalThis as Record<string, unknown>).BroadcastChannel;
  const origFetch = globalThis.fetch;
  const storageMap = new Map<string, string>();
  const fakeLocalStorage = {
    getItem: (k: string) => storageMap.get(k) ?? null,
    setItem: (k: string, v: string) => storageMap.set(k, v),
    removeItem: (k: string) => storageMap.delete(k),
  };
  Object.defineProperty(window, "localStorage", {
    value: fakeLocalStorage,
    configurable: true,
  });

  class FakeBroadcastChannel {
    name: string;
    constructor(name: string) {
      this.name = name;
    }
    postMessage(payload: unknown) {
      posted.push({ channel: this.name, payload });
    }
    close() {}
  }
  (globalThis as Record<string, unknown>).BroadcastChannel = FakeBroadcastChannel;

  globalThis.fetch = (async (input: string | URL | Request) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    if (url.endsWith("/auth/login")) {
      return new Response(
        JSON.stringify({
          ok: true,
          data: {
            token: "tok-admin-1",
            user: { id: "user-admin-1", login: "admin", role: "admin" },
          },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.endsWith("/auth/me")) {
      return new Response(
        JSON.stringify({
          ok: true,
          data: { id: "user-admin-1", login: "admin", role: "admin" },
        }),
        { status: 200, headers: { "Content-Type": "application/json" } },
      );
    }
    return new Response(JSON.stringify({ ok: true, data: null }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  }) as typeof fetch;

  try {
    const loggedIn = await login("admin", "secret");
    assert.equal(loggedIn.id, "user-admin-1");
    assert.equal(posted.length, 1);
    assert.equal(posted[0].channel, AUTH_SESSION_CHANNEL);
    assert.deepEqual(posted[0].payload, { kind: "login", userId: "user-admin-1" });
    assert.ok(storageMap.get(AUTH_EPOCH_STORAGE_KEY)?.includes("user-admin-1"));

    const me = await getSession();
    assert.equal(me.id, "user-admin-1");

    await logout();
    assert.equal(posted.length, 2);
    assert.deepEqual(posted[1].payload, { kind: "logout" });
  } finally {
    globalThis.fetch = origFetch;
    if (origBC === undefined) {
      delete (globalThis as Record<string, unknown>).BroadcastChannel;
    } else {
      (globalThis as Record<string, unknown>).BroadcastChannel = origBC;
    }
  }
});

test("watchSessionIdentity fails closed on BroadcastChannel, storage fallback, and window focus user switch", async () => {
  const channelListeners: Array<(ev: { data: unknown }) => void> = [];
  const winListeners = new Map<string, Array<(ev: unknown) => void>>();
  const docListeners = new Map<string, Array<() => void>>();
  const mismatches: string[] = [];

  let serverUser: { id: string; role: "admin" | "member" } = { id: "u-1", role: "admin" };

  const dispose = watchSessionIdentity({
    currentSession: { id: "u-1", role: "admin" },
    checkSession: async () => serverUser,
    onIdentityMismatch: (reason) => mismatches.push(reason),
    channelFactory: () => ({
      addEventListener: (_type, fn) => channelListeners.push(fn),
      removeEventListener: (_type, fn) => {
        const idx = channelListeners.indexOf(fn);
        if (idx >= 0) channelListeners.splice(idx, 1);
      },
      close: () => {},
    }),
    targetWindow: {
      addEventListener: (type, fn) => {
        const list = winListeners.get(type) ?? [];
        list.push(fn);
        winListeners.set(type, list);
      },
      removeEventListener: (type, fn) => {
        const list = winListeners.get(type) ?? [];
        const idx = list.indexOf(fn);
        if (idx >= 0) list.splice(idx, 1);
      },
    },
    targetDocument: {
      visibilityState: "visible",
      addEventListener: (type, fn) => {
        const list = docListeners.get(type) ?? [];
        list.push(fn);
        docListeners.set(type, list);
      },
      removeEventListener: (type, fn) => {
        const list = docListeners.get(type) ?? [];
        const idx = list.indexOf(fn);
        if (idx >= 0) list.splice(idx, 1);
      },
    },
  });

  try {
    // Same user login message does not mismatch
    channelListeners.forEach((fn) => fn({ data: { kind: "login", userId: "u-1" } }));
    assert.deepEqual(mismatches, []);

    // Different user login on BroadcastChannel triggers immediate reload/mismatch
    channelListeners.forEach((fn) => fn({ data: { kind: "login", userId: "u-2" } }));
    assert.deepEqual(mismatches, ["broadcast"]);

    // Storage fallback: token changed in another tab
    winListeners.get("storage")?.forEach((fn) =>
      fn({ key: "mc_token", oldValue: "tok-1", newValue: "tok-2" }),
    );
    assert.deepEqual(mismatches, ["broadcast", "storage"]);

    // Focus re-check when same user -> ok
    winListeners.get("focus")?.forEach((fn) => fn({}));
    await new Promise((r) => setTimeout(r, 10));
    assert.deepEqual(mismatches, ["broadcast", "storage"]);

    // Focus re-check when server identity swapped -> mismatch
    serverUser = { id: "u-3", role: "member" };
    winListeners.get("focus")?.forEach((fn) => fn({}));
    await new Promise((r) => setTimeout(r, 10));
    assert.deepEqual(mismatches, ["broadcast", "storage", "focus-mismatch"]);
  } finally {
    dispose();
  }
});

test("sdk.endpoint.request blocks dot-segment traversal, respects text/plain vs JSON essence, fences disposal, and maps error codes", async () => {
  assert.equal(mapHttpToSdkError(409, "plugin.stale_authority", "stale").code, "stale-generation");
  assert.equal(mapHttpToSdkError(409, "plugin.unknown_effect", "unk").code, "result-unknown");

  const adminSession: SessionView = {
    id: "u-ep-1",
    username: "admin",
    nickname: "Admin",
    avatar_url: null,
    role: "admin",
    capabilities: { allow_subscribe: true, allow_search: true, allow_direct_download: true },
  };
  const desc: UiDescriptorView = {
    plugin_id: "org.example.epsec",
    plugin_name: "EP Sec",
    version: "1.0.0",
    generation: 1,
    lifecycle: "enabled",
    source: "external",
    package_hash: "pkg-ep",
    package_sha256: "pkg-ep",
    bundle_sha256: "b-ep",
    subject_user_id: "u-ep-1",
    subject_role: "admin",
    ui_mode: "federation-react",
    remote_name: "epSecRemote",
    entry: "ui/assets/remoteEntry.js",
    entry_url: "/api/v1/plugin-ui-assets/org.example.epsec/pkg-ep/ui/assets/remoteEntry.js",
    asset_base_url: "/api/v1/plugin-ui-assets/org.example.epsec/pkg-ep",
    contract: "1.0.0",
    react: "19.3.0",
    react_dom: "19.3.0",
    ui_trusted: true,
    trust: { status: "trusted", revision: 1, granted_at: null, granted_by_user_id: "u-ep-1", revoked_at: null },
    capabilities: [],
    styles: [],
    surfaces: [],
  };

  let fetchCalls = 0;
  let disposeRef: (() => void) | null = null;
  const fakeFetch: typeof fetch = async (input) => {
    fetchCalls += 1;
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    if (url.endsWith("/endpoints/plain-json-lookalike")) {
      return new Response('{"ok":false}', {
        status: 200,
        headers: { "Content-Type": "text/plain; charset=utf-8" },
      });
    }
    if (url.endsWith("/endpoints/raw-ok-false")) {
      return new Response(JSON.stringify({ ok: false, healthy: false }), {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }
    if (url.endsWith("/endpoints/bad-json")) {
      return new Response("{not-json", {
        status: 200,
        headers: { "Content-Type": "application/json" },
      });
    }
    if (url.endsWith("/endpoints/wrong-context")) {
      return new Response(
        JSON.stringify({
          ok: false,
          error: { code: "plugin.wrong_context", message: "forbidden in GET" },
        }),
        { status: 409, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.endsWith("/endpoints/plan-expired")) {
      return new Response(
        JSON.stringify({
          ok: false,
          error: { code: "plugin.cleanup_plan_expired", message: "expired" },
        }),
        { status: 409, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.endsWith("/endpoints/not-drained")) {
      return new Response(
        JSON.stringify({
          ok: false,
          error: { code: "plugin.not_drained", message: "operation in progress" },
        }),
        { status: 409, headers: { "Content-Type": "application/json" } },
      );
    }
    if (url.endsWith("/endpoints/abort-after-dispose")) {
      disposeRef?.();
      throw new Error("AbortError");
    }
    throw new Error(`Unexpected fetch: ${url}`);
  };

  const handle = createPluginHostSdk({
    descriptor: desc,
    surfaceId: "main",
    session: adminSession,
    loginEpoch: "ep-sec-1",
    theme: "silver",
    navigate: () => {},
    notifyToast: () => {},
    openConfirm: async () => true,
    openDialog: () => () => {},
    fetchImpl: fakeFetch,
  });
  disposeRef = () => handle.dispose();

  try {
    for (const bad of ["", "/", ".", "../config", "../../users", "a/../b", "a\\b", "a\nb"]) {
      await assert.rejects(
        () => handle.sdk.endpoint.request(bad, {}),
        (err: { code?: string }) => err.code === "request-failed",
        `must reject endpointId=${JSON.stringify(bad)}`,
      );
    }
    assert.equal(fetchCalls, 0, "invalid endpointId must not invoke fetch");

    const plainText = await handle.sdk.endpoint.request("plain-json-lookalike", {});
    assert.equal(plainText, '{"ok":false}');

    const rawOkFalse = await handle.sdk.endpoint.request("raw-ok-false", {});
    assert.deepEqual(rawOkFalse, { ok: false, healthy: false });

    await assert.rejects(
      () => handle.sdk.endpoint.request("bad-json", {}),
      (err: { code?: string }) => err.code === "request-failed",
    );

    await assert.rejects(
      () => handle.sdk.endpoint.request("wrong-context", {}),
      (err: { code?: string; httpStatus?: number }) =>
        err.code === "request-failed" && err.httpStatus === 409,
    );

    await assert.rejects(
      () => handle.sdk.endpoint.request("plan-expired", {}),
      (err: { code?: string; httpStatus?: number }) =>
        err.code === "stale-handle" && err.httpStatus === 409,
    );

    await assert.rejects(
      () => handle.sdk.endpoint.request("not-drained", {}),
      (err: { code?: string; httpStatus?: number }) =>
        err.code === "busy" && err.httpStatus === 409,
    );

    await assert.rejects(
      () => handle.sdk.endpoint.request("abort-after-dispose", {}),
      (err: { code?: string }) => err.code === "host-disposed",
    );
  } finally {
    handle.dispose();
  }
});
