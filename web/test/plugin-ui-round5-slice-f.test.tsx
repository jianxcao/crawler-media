import "./plugin-ui-env";

import assert from "node:assert/strict";
import test from "node:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import React, { useState } from "react";
import { MemoryRouter } from "react-router-dom";

import {
  isSdkError,
  usePluginOperation,
  type Operation,
  type PluginHostSDK,
  type PluginSurfaceProps,
} from "@crawler-media/plugin-ui-sdk";
import { PluginSchemaForm, schemaHasSecrets } from "../components/plugins/PluginSchemaForm";
import { PluginSurfaceHost } from "../components/plugins/PluginSurfaceHost";
import type { SessionView } from "../lib/api/auth";
import type { PluginUiContract, UiDescriptorView } from "../lib/api/plugins";
import {
  clearFederationCache,
  loadFederatedSurface,
  type FederatedContainer,
} from "../lib/plugins/federation-loader";
import { createPluginHostSdk } from "../lib/plugins/sdk";
import { mapHttpToSdkError } from "../lib/plugins/sdk-helpers";
import { SessionProvider } from "../lib/session";

const GIB = 1024 ** 3;

const ADMIN_SESSION: SessionView = {
  id: "user-admin-r5",
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
    plugin_id: "org.example.r5",
    plugin_name: "R5 Plugin",
    version: "1.0.0",
    generation: 1,
    lifecycle: "enabled",
    source: "external",
    package_hash: "pkg-r5-1",
    package_sha256: "pkg-r5-1",
    bundle_sha256: "bundle-r5-1",
    subject_user_id: "user-admin-r5",
    subject_role: "admin",
    ui_mode: "federation-react",
    remote_name: "r5Remote",
    entry: "ui/assets/remoteEntry.js",
    entry_url: "/api/v1/plugin-ui-assets/org.example.r5/pkg-r5-1/ui/assets/remoteEntry.js",
    asset_base_url: "/api/v1/plugin-ui-assets/org.example.r5/pkg-r5-1",
    styles: ["ui/assets/style.css"],
    contract: "1.0.0",
    react: "19.3.0",
    react_dom: "19.3.0",
    ui_trusted: true,
    trust: {
      status: "trusted",
      revision: 1,
      granted_at: "2026-11-25T00:00:00Z",
      granted_by_user_id: "user-admin-r5",
      revoked_at: null,
    },
    capabilities: ["read:sites", "read:downloaders"],
    surfaces: [
      {
        id: "settings",
        kind: "config",
        expose: "./Config",
        title: "R5 Settings",
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
        title: "R5 Page",
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

test("R5-F1: PluginSchemaForm & schemaHasSecrets recognize password/x-ui.secret/$ref secrets, handle $ref cycles, and strip all secretKeys from values", async () => {
  assert.equal(
    schemaHasSecrets({
      type: "object",
      properties: { pw: { type: "string", "x-ui": { widget: "password" } } },
    }),
    true,
  );
  assert.equal(
    schemaHasSecrets({
      type: "object",
      properties: { sec: { type: "string", "x-ui": { secret: true } } },
    }),
    true,
  );
  assert.equal(
    schemaHasSecrets({
      type: "object",
      $defs: {
        hop: { $ref: "#/$defs/secret_leaf" },
        secret_leaf: { type: "string", "x-ui": { widget: "password" } },
      },
      properties: { token: { $ref: "#/$defs/hop" } },
    }),
    true,
  );
  assert.equal(
    schemaHasSecrets({
      type: "object",
      $defs: {
        a: { $ref: "#/$defs/b" },
        b: { $ref: "#/$defs/a" },
      },
      properties: { cyc: { $ref: "#/$defs/a" } },
    }),
    false,
  );

  const savedCalls: Array<{
    values: Record<string, unknown>;
    secrets: Record<string, unknown>;
  }> = [];

  try {
    render(
      <PluginSchemaForm
        schema={{
          type: "object",
          required: ["label", "map_only_secret"],
          $defs: {
            indirect_secret: { $ref: "#/$defs/leaf_secret" },
            leaf_secret: { type: "string", "x-ui": { secret: true } },
            cycle_a: { $ref: "#/$defs/cycle_b" },
            cycle_b: { $ref: "#/$defs/cycle_a" },
          },
          properties: {
            label: { type: "string", title: "Regular Label" },
            pw_key: {
              type: "string",
              title: "Password Input",
              "x-ui": { widget: "password" },
              default: "plaintext-default",
            },
            flag_secret: {
              type: "string",
              title: "Flag Secret Input",
              "x-ui": { secret: true },
            },
            ref_secret: {
              $ref: "#/$defs/indirect_secret",
              title: "Ref Secret Input",
            },
            map_only_secret: {
              type: "string",
              title: "Map Only Secret Input",
            },
          },
        }}
        values={{
          label: "clean-value",
          pw_key: "leaked-pw",
          flag_secret: "leaked-flag",
          ref_secret: "leaked-ref",
          map_only_secret: "leaked-map",
        }}
        secrets={{ map_only_secret: { configured: true } }}
        editable
        sites={[]}
        downloaders={[]}
        onSave={async (values, secrets) => {
          savedCalls.push({ values, secrets });
        }}
      />,
    );

    assert.ok(screen.getByLabelText("Regular Label"));
    assert.equal(screen.queryByLabelText("Password Input"), null);
    assert.equal(screen.queryByLabelText("Flag Secret Input"), null);
    assert.equal(screen.queryByLabelText("Ref Secret Input"), null);
    assert.equal(screen.queryByLabelText("Map Only Secret Input"), null);

    assert.ok(screen.getByLabelText("pw_key 操作"));
    assert.ok(screen.getByLabelText("flag_secret 操作"));
    assert.ok(screen.getByLabelText("ref_secret 操作"));
    assert.ok(screen.getByLabelText("map_only_secret 操作"));

    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => assert.equal(savedCalls.length, 1));
    assert.deepEqual(savedCalls[0].values, { label: "clean-value" });
    assert.deepEqual(savedCalls[0].secrets, {});
  } finally {
    cleanup();
  }
});

test("R5-F2: boost Config avoids setState-in-updater under StrictMode and preserves temporary empty/fractional edits across config.values echoes", async () => {
  const origFetch = globalThis.fetch;
  const origConsoleError = console.error;
  const consoleErrors: string[] = [];
  console.error = (...args: unknown[]) => {
    consoleErrors.push(args.map(String).join(" "));
  };
  globalThis.fetch = (async () =>
    new Response(JSON.stringify({ ok: true, data: { items: [] } }), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    })) as typeof fetch;

  try {
    const savedPayloads: Array<Record<string, unknown>> = [];
    let externalPush: ((vals: Record<string, unknown>) => void) | null = null;
    const boostDesc = makeDescriptor({
      plugin_id: "cm.boost",
      source: "external",
      remote_name: "cmBoost",
      package_hash: "cm:boost:1.0.0",
      package_sha256: "cm:boost:1.0.0",
    });
    const BoostConfigStrictStub: React.FC<PluginSurfaceProps> = ({ config }) => {
      const vals = config?.values ?? {};
      const extSig = `${String(vals.min_free_disk_bytes ?? "")}:${String(vals.max_active ?? "")}`;
      const [lastEchoSig, setLastEchoSig] = React.useState(extSig);
      const [minFreeText, setMinFreeText] = React.useState(
        typeof vals.min_free_disk_bytes === "number" ? String(vals.min_free_disk_bytes / GIB) : "",
      );
      const [maxActiveText, setMaxActiveText] = React.useState(
        typeof vals.max_active === "number" ? String(vals.max_active) : "",
      );
      const localEchoRef = React.useRef<string | null>(null);
      React.useEffect(() => {
        if (extSig === lastEchoSig) return;
        setLastEchoSig(extSig);
        if (localEchoRef.current === extSig) {
          localEchoRef.current = null;
          return;
        }
        setMinFreeText(
          typeof vals.min_free_disk_bytes === "number" ? String(vals.min_free_disk_bytes / GIB) : "",
        );
        setMaxActiveText(typeof vals.max_active === "number" ? String(vals.max_active) : "");
      }, [extSig, lastEchoSig, vals.max_active, vals.min_free_disk_bytes]);

      const emitDraft = (nextMin: string, nextMax: string) => {
        const parsedMin = Number(nextMin);
        const minBytes =
          nextMin.trim() && Number.isFinite(parsedMin)
            ? Math.round(parsedMin * GIB)
            : vals.min_free_disk_bytes;
        const parsedMax = Number(nextMax);
        const maxAct =
          nextMax.trim() && Number.isFinite(parsedMax) ? Math.trunc(parsedMax) : vals.max_active;
        localEchoRef.current = `${String(minBytes ?? "")}:${String(maxAct ?? "")}`;
        config?.onDraftChange({ ...vals, min_free_disk_bytes: minBytes, max_active: maxAct });
      };

      return (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            const out: Record<string, unknown> = { ...vals };
            if (minFreeText.trim() && Number.isFinite(Number(minFreeText))) {
              out.min_free_disk_bytes = Math.round(Number(minFreeText) * GIB);
            }
            if (maxActiveText.trim() && Number.isFinite(Number(maxActiveText))) {
              out.max_active = Math.trunc(Number(maxActiveText));
            }
            void config?.save(out);
          }}
        >
          <h2>自动刷流策略与配额设置</h2>
          <label>
            磁盘最低保留空闲 (GiB)
            <input
              value={minFreeText}
              onChange={(e) => {
                setMinFreeText(e.target.value);
                emitDraft(e.target.value, maxActiveText);
              }}
            />
          </label>
          <label>
            最大活动任务数
            <input
              value={maxActiveText}
              onChange={(e) => {
                setMaxActiveText(e.target.value);
                emitDraft(minFreeText, e.target.value);
              }}
            />
          </label>
          <button type="submit">保存刷流配置</button>
        </form>
      );
    };
    const container: FederatedContainer = {
      init: () => {},
      get: async () => () => ({ default: BoostConfigStrictStub }),
    };
    const boostLoader = (descriptor: UiDescriptorView, expose: string) =>
      loadFederatedSurface(descriptor, expose, {
        fetchManifest: async () => [],
        importRemote: async () => container,
      });

    function StatefulHostHarness() {
      const [vals, setVals] = useState<Record<string, unknown>>({
        min_free_disk_bytes: 2 * GIB,
        max_active: 20,
      });
      externalPush = setVals;
      return (
        <PluginSurfaceHost
          pluginId="cm.boost"
          surfaceId="settings"
          expectedKind="config"
          config={{
            values: vals,
            schemaRevision: "1",
            revision: "1",
            editable: true,
            onDraftChange: (next) => setVals(next),
            save: async (next) => {
              savedPayloads.push(next);
            },
          }}
          deps={{
            fetchDescriptor: async () => boostDesc,
            fetchContract: async () => CONTRACT,
            loadSurface: boostLoader,
          }}
        />
      );
    }

    render(
      <React.StrictMode>
        <MemoryRouter>
          <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "r5-b" }}>
            <StatefulHostHarness />
          </SessionProvider>
        </MemoryRouter>
      </React.StrictMode>,
    );

    await waitFor(() => assert.ok(screen.getByText("自动刷流策略与配额设置")));
    const minFreeInput = screen.getByLabelText("磁盘最低保留空闲 (GiB)") as HTMLInputElement;
    const maxActiveInput = screen.getByLabelText("最大活动任务数") as HTMLInputElement;

    fireEvent.change(minFreeInput, { target: { value: "1.5" } });
    assert.equal(minFreeInput.value, "1.5", "fractional GiB edit must not be clobbered by echo");

    fireEvent.change(maxActiveInput, { target: { value: "" } });
    assert.equal(maxActiveInput.value, "", "temporary empty numeric input must not be clobbered by echo");

    const badRenderErrors = consoleErrors.filter((line) =>
      line.includes("Cannot update a component"),
    );
    assert.deepEqual(badRenderErrors, [], "must not call parent setState inside setDraft updater");

    fireEvent.click(screen.getByRole("button", { name: "保存刷流配置" }));
    await waitFor(() => assert.equal(savedPayloads.length, 1));
    assert.equal(savedPayloads[0].min_free_disk_bytes, Math.round(1.5 * GIB));
    assert.equal(typeof savedPayloads[0].max_active, "number");
    assert.equal(Number.isFinite(savedPayloads[0].max_active as number), true);

    act(() => {
      externalPush?.({ min_free_disk_bytes: 6 * GIB, max_active: 45 });
    });
    await waitFor(() => {
      assert.equal(minFreeInput.value, "6");
      assert.equal(maxActiveInput.value, "45");
    });
  } finally {
    cleanup();
    clearFederationCache();
    console.error = origConsoleError;
    globalThis.fetch = origFetch;
  }
});

test("R5-F3: SDK transport & mapHttpToSdkError normalize backend SchemaFieldError[] and preserve Record<string, string>", async () => {
  const mappedFromArray = mapHttpToSdkError(422, "plugin.invalid_config", "validation failed", [
    { path: "/values/max_active", code: "maximum", message: "must be <= 200" },
    { path: "/values/site_id", code: "required" },
  ] as unknown as Record<string, string>);
  assert.deepEqual(mappedFromArray.fieldErrors, {
    "/values/max_active": "must be <= 200",
    "/values/site_id": "required",
  });

  const mappedFromRecord = mapHttpToSdkError(400, "plugin.invalid_config", "bad config", {
    hold_days: "must be >= 0",
  });
  assert.deepEqual(mappedFromRecord.fieldErrors, { hold_days: "must be >= 0" });

  const managed = createPluginHostSdk({
    descriptor: makeDescriptor(),
    surfaceId: "main",
    session: ADMIN_SESSION,
    loginEpoch: "r5-sdk",
    theme: "silver",
    navigate: () => {},
    notifyToast: () => {},
    openConfirm: async () => false,
    openDialog: () => () => {},
    fetchImpl: (async () =>
      new Response(
        JSON.stringify({
          ok: false,
          error: {
            code: "plugin.invalid_config",
            message: "config validation failed",
            fields: [
              { path: "/values/budget_bytes", code: "minimum", message: "must be non-negative" },
              { path: "/values/downloader_id", code: "type" },
            ],
          },
        }),
        { status: 422, headers: { "Content-Type": "application/json" } },
      )) as typeof fetch,
  });

  try {
    await assert.rejects(
      () => managed.sdk.endpoint.request("validate", { body: { x: 1 } }),
      (err: unknown) => {
        assert.equal(isSdkError(err), true);
        if (!isSdkError(err)) return false;
        assert.deepEqual(err.fieldErrors, {
          "/values/budget_bytes": "must be non-negative",
          "/values/downloader_id": "type",
        });
        return true;
      },
    );
  } finally {
    managed.dispose();
  }
});

test("R5-F4: usePluginOperation immediately clears stale Operation when switching between two non-null operationIds", async () => {
  const listeners = new Map<string, (op: Operation) => void>();
  const fakeHost = {
    operations: {
      watch(id: string, cb: (op: Operation) => void) {
        listeners.set(id, cb);
        return () => {
          listeners.delete(id);
        };
      },
    },
  } as unknown as PluginHostSDK;

  const seenSnapshots: Array<string | null> = [];
  function OperationView({ opId }: { opId: string | null }) {
    const op = usePluginOperation(opId, fakeHost);
    seenSnapshots.push(op ? `${op.operation_id}:${op.state}` : null);
    return <div data-testid="op-status">{op ? `${op.operation_id}:${op.state}` : "pending"}</div>;
  }

  try {
    const view = render(<OperationView opId="op-1" />);
    act(() => {
      listeners.get("op-1")?.({
        operation_id: "op-1",
        state: "succeeded",
        revision: "1",
      });
    });
    assert.equal(screen.getByTestId("op-status").textContent, "op-1:succeeded");

    seenSnapshots.length = 0;
    view.rerender(<OperationView opId="op-2" />);
    assert.equal(screen.getByTestId("op-status").textContent, "pending");
    assert.equal(
      seenSnapshots.some((s) => s?.startsWith("op-1:")),
      false,
      "switching opId from op-1 to op-2 must never expose stale op-1 Operation",
    );

    act(() => {
      listeners.get("op-2")?.({
        operation_id: "op-2",
        state: "running",
        revision: "1",
      });
    });
    assert.equal(screen.getByTestId("op-status").textContent, "op-2:running");
  } finally {
    cleanup();
  }
});

test("R5-F5: loadFederatedSurface evicts stylesheets on import/init failure and validates factory & default export", async () => {
  clearFederationCache();
  try {
    const desc = makeDescriptor({ bundle_sha256: "bundle-fail-1" });

    // 1. importRemote failure leaves zero <link data-cm-plugin-ui> in document.head
    await assert.rejects(
      () =>
        loadFederatedSurface(desc, "./Page", {
          importRemote: async () => {
            throw new Error("remote network failure");
          },
        }),
      /remote network failure/,
    );
    assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 0);

    // 2. container.init failure leaves zero <link data-cm-plugin-ui> AND allows clean retry
    await assert.rejects(
      () =>
        loadFederatedSurface(desc, "./Page", {
          importRemote: async () => ({
            init: async () => {
              throw new Error("share scope init failure");
            },
            get: async () => () => ({ default: () => <div>Never</div> }),
          }),
        }),
      /share scope init failure/,
    );
    assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 0);

    // Retry for the same key succeeds and mounts stylesheet
    const ValidComp = () => <div>Recovered</div>;
    const loaded = await loadFederatedSurface(desc, "./Page", {
      importRemote: async () => ({
        init: async () => {},
        get: async () => () => ({ default: ValidComp }),
      }),
    });
    assert.equal(loaded, ValidComp);
    assert.equal(document.head.querySelectorAll("link[data-cm-plugin-ui]").length, 1);

    // 3. Invalid factory (non-function) throws descriptive Error
    const descBadFactory = makeDescriptor({
      plugin_id: "org.example.r5-bad-factory",
      remote_name: "r5BadFactory",
      bundle_sha256: "bundle-bf",
    });
    await assert.rejects(
      () =>
        loadFederatedSurface(descBadFactory, "./Page", {
          importRemote: async () => ({
            init: async () => {},
            get: async () => null as unknown as () => { default: typeof ValidComp },
          }),
        }),
      /factory/i,
    );

    // 4. Invalid default export (undefined / primitive) throws descriptive Error, while React.memo works
    const descBadDefault = makeDescriptor({
      plugin_id: "org.example.r5-bad-default",
      remote_name: "r5BadDefault",
      bundle_sha256: "bundle-bd",
    });
    await assert.rejects(
      () =>
        loadFederatedSurface(descBadDefault, "./MissingDefault", {
          importRemote: async () => ({
            init: async () => {},
            get: async (exp) => {
              if (exp === "./MissingDefault") {
                return () => ({}) as { default: typeof ValidComp };
              }
              if (exp === "./PrimitiveDefault") {
                return () => ({ default: "not-a-component" as unknown as typeof ValidComp });
              }
              const MemoComp = React.memo(() => <div>Memoized</div>);
              return () => ({ default: MemoComp as unknown as typeof ValidComp });
            },
          }),
        }),
      /default/i,
    );
    await assert.rejects(
      () => loadFederatedSurface(descBadDefault, "./PrimitiveDefault"),
      /default/i,
    );
    const memoLoaded = await loadFederatedSurface(descBadDefault, "./MemoDefault");
    assert.equal(typeof memoLoaded, "object");
  } finally {
    clearFederationCache();
  }
});
