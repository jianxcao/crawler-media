import "./plugin-ui-env";

import assert from "node:assert/strict";
import test from "node:test";

import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import React from "react";

import { PluginSchemaForm } from "../components/plugins/PluginSchemaForm";
import type { UiDescriptorView } from "../lib/api/plugins";
import {
  clearFederationCache,
  descriptorCacheKey,
  evictPluginFederationCache,
  type FederatedContainer,
  loadFederatedSurface,
} from "../lib/plugins/federation-loader";

function makeDescriptor(overrides: Partial<UiDescriptorView> = {}): UiDescriptorView {
  return {
    plugin_id: "org.example.r10-ui",
    plugin_name: "R10 UI",
    version: "1.0.0",
    generation: 1,
    lifecycle: "enabled",
    source: "external",
    package_hash: "hash-r10-v1",
    package_sha256: "hash-r10-v1",
    bundle_sha256: "bundle-r10-v1",
    subject_user_id: "user-admin",
    subject_role: "admin",
    ui_mode: "federated",
    remote_name: "cm_r10_ui",
    entry: "ui/remoteEntry.js",
    entry_url: "/api/v1/plugin-ui-assets/org.example.r10-ui/hash-r10-v1/ui/remoteEntry.js",
    asset_base_url: "/api/v1/plugin-ui-assets/org.example.r10-ui/hash-r10-v1",
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
        title: "Main",
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

test("R10-F1a: optional nested objects with populated sibling edits fail required validation instead of silently deleting user edits", async () => {
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
          protocol: {
            type: "string",
            title: "Proxy Protocol",
            enum: ["http", "socks5"],
          },
          verify_tls: {
            type: "boolean",
            title: "Verify TLS",
          },
        },
      },
      rules: {
        type: "array",
        title: "Rules JSON",
        items: {
          type: "object",
          required: ["name"],
          properties: {
            name: { type: "string" },
            proxy: {
              type: "object",
              required: ["host"],
              properties: {
                host: { type: "string" },
                protocol: { type: "string", enum: ["http", "socks5"] },
                verify_tls: { type: "boolean" },
              },
            },
          },
        },
      },
    },
  };

  try {
    // 1) Initial values = {}: user sets protocol="socks5" & verify_tls=true without filling host
    const view = render(
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

    fireEvent.change(screen.getByLabelText("Proxy Protocol"), { target: { value: "socks5" } });
    fireEvent.click(screen.getByLabelText("Verify TLS"));
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));

    assert.match((await screen.findByRole("alert")).textContent ?? "", /proxy\.host/);
    assert.equal(saves.length, 0);
    view.unmount();

    // 2) JSON-mode items with incomplete optional proxy object must also fail required check
    const jsonView = render(
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
    fireEvent.change(screen.getByLabelText("Rules JSON"), {
      target: {
        value: '[{"name":"r1","proxy":{"protocol":"socks5","verify_tls":true}}]',
      },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    assert.match((await screen.findByRole("alert")).textContent ?? "", /proxy\.host/);
    assert.equal(saves.length, 0);
    jsonView.unmount();

    // 3) Saved initial proxy where user clears host="" AND edits protocol="socks5" must fail required check
    render(
      <PluginSchemaForm
        schema={schema}
        values={{ proxy: { host: "10.0.0.1", protocol: "http", verify_tls: false } }}
        secrets={{}}
        editable
        sites={[]}
        downloaders={[]}
        onSave={async (vals) => {
          saves.push(vals);
        }}
      />,
    );
    fireEvent.change(screen.getByLabelText("Proxy Host"), { target: { value: "" } });
    fireEvent.change(screen.getByLabelText("Proxy Protocol"), { target: { value: "socks5" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    assert.match((await screen.findByRole("alert")).textContent ?? "", /proxy\.host/);
    assert.equal(saves.length, 0);
  } finally {
    cleanup();
  }
});

test("R10-F1b: saved optional nested object with default-bearing required field and boolean default=true clears to omitted", async () => {
  const saves: Array<Record<string, unknown>> = [];
  const schema = {
    type: "object",
    properties: {
      tls: {
        type: "object",
        title: "TLS",
        required: ["port"],
        properties: {
          port: { type: "integer", title: "TLS Port", default: 8443 },
          verify: { type: "boolean", title: "Verify", default: true },
        },
      },
    },
  };

  try {
    // Clearing TLS Port while leaving Verify checked omits tls
    const v1 = render(
      <PluginSchemaForm
        schema={schema}
        values={{ tls: { port: 8443, verify: true } }}
        secrets={{}}
        editable
        sites={[]}
        downloaders={[]}
        onSave={async (vals) => {
          saves.push(vals);
        }}
      />,
    );
    fireEvent.change(screen.getByLabelText("TLS Port"), { target: { value: "" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => assert.equal(saves.length, 1));
    assert.deepEqual(saves[0], {});
    v1.unmount();

    // Clearing TLS Port AND unchecking Verify also omits tls
    render(
      <PluginSchemaForm
        schema={schema}
        values={{ tls: { port: 8443, verify: true } }}
        secrets={{}}
        editable
        sites={[]}
        downloaders={[]}
        onSave={async (vals) => {
          saves.push(vals);
        }}
      />,
    );
    fireEvent.change(screen.getByLabelText("TLS Port"), { target: { value: "" } });
    fireEvent.click(screen.getByLabelText("Verify"));
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => assert.equal(saves.length, 2));
    assert.deepEqual(saves[1], {});
  } finally {
    cleanup();
  }
});

test("R10-F2: required empty-string enum on initial values={} shows placeholder and accepts direct '' selection, and required string accepts '' in JSON items & pre-saved values", async () => {
  const saves: Array<Record<string, unknown>> = [];
  const schema = {
    type: "object",
    required: ["sep", "prefix"],
    properties: {
      sep: {
        type: "string",
        title: "Separator",
        enum: ["", ",", ";"],
      },
      prefix: {
        type: "string",
        title: "Prefix",
      },
      retries: {
        type: "integer",
        title: "Retries",
      },
      rules: {
        type: "array",
        title: "Rules",
        items: {
          type: "object",
          required: ["name", "prefix"],
          properties: {
            name: { type: "string", minLength: 1 },
            prefix: { type: "string" },
          },
        },
      },
    },
  };

  try {
    // 2A: Fresh values = {}: Separator must NOT look pre-selected as "", fails until selected, and allows selecting "" directly
    const fresh = render(
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
    const sepSelect = screen.getByLabelText("Separator") as HTMLSelectElement;
    assert.notEqual(sepSelect.value, "");

    fireEvent.change(screen.getByLabelText("Prefix"), { target: { value: "ok" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    assert.match((await screen.findByRole("alert")).textContent ?? "", /sep/);
    assert.equal(saves.length, 0);

    // Selecting "" directly fires onChange and succeeds alongside JSON item with prefix: ""
    fireEvent.change(sepSelect, { target: { value: "" } });
    fireEvent.change(screen.getByLabelText("Rules"), {
      target: { value: '[{"name":"r1","prefix":""}]' },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => assert.equal(saves.length, 1));
    assert.deepEqual(saves[0], {
      sep: "",
      prefix: "ok",
      rules: [{ name: "r1", prefix: "" }],
    });
    fresh.unmount();

    // 2B: Pre-saved backend config with prefix: "" allows editing sibling retries without rejecting prefix
    render(
      <PluginSchemaForm
        schema={schema}
        values={{ sep: ",", prefix: "", retries: 2 }}
        secrets={{}}
        editable
        sites={[]}
        downloaders={[]}
        onSave={async (vals) => {
          saves.push(vals);
        }}
      />,
    );
    fireEvent.change(screen.getByLabelText("Retries"), { target: { value: "3" } });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    await waitFor(() => assert.equal(saves.length, 2));
    assert.deepEqual(saves[1], { sep: ",", prefix: "", retries: 3 });
  } finally {
    cleanup();
  }
});

test("R10-F3: in-flight loadFederatedSurface respects concurrent eviction and never re-injects evicted stylesheets or clobbers newer cache entries", async () => {
  clearFederationCache();
  try {
    const desc = makeDescriptor();
    const key = descriptorCacheKey(desc);

    // Part 1: Eviction while importRemote is in-flight prevents stylesheet injection on resolution
    let resolveFirst!: (c: FederatedContainer) => void;
    const firstImport = new Promise<FederatedContainer>((res) => {
      resolveFirst = res;
    });

    const firstLoad = loadFederatedSurface(desc, "./Page", {
      importRemote: () => firstImport,
    });

    evictPluginFederationCache(desc.plugin_id);

    resolveFirst({
      init: async () => {},
      get: async () => () => ({ default: () => React.createElement("div", null, "v1") }),
    });

    await assert.rejects(firstLoad, /evicted/);
    assert.equal(document.head.querySelector(`link[data-cm-plugin-ui="${key}"]`), null);

    // Part 2: Evicted in-flight rejection must NOT delete a newer containerCache entry or its stylesheets
    let rejectStale!: (err: Error) => void;
    const staleImport = new Promise<FederatedContainer>((_res, rej) => {
      rejectStale = rej;
    });

    const staleLoad = loadFederatedSurface(desc, "./Page", {
      importRemote: () => staleImport,
    });

    evictPluginFederationCache(desc.plugin_id);

    const Comp = await loadFederatedSurface(desc, "./Page", {
      importRemote: async () => ({
        init: async () => {},
        get: async () => () => ({ default: () => React.createElement("div", null, "fresh") }),
      }),
    });
    assert.equal(typeof Comp, "function");
    assert.notEqual(document.head.querySelector(`link[data-cm-plugin-ui="${key}"]`), null);

    rejectStale(new Error("network aborted"));
    await assert.rejects(staleLoad, /network aborted/);

    // Fresh stylesheet & cached container must remain intact!
    assert.notEqual(document.head.querySelector(`link[data-cm-plugin-ui="${key}"]`), null);
    let secondImportCalled = false;
    await loadFederatedSurface(desc, "./Page", {
      importRemote: async () => {
        secondImportCalled = true;
        throw new Error("should have hit cache");
      },
    });
    assert.equal(secondImportCalled, false);
  } finally {
    clearFederationCache();
    cleanup();
  }
});
