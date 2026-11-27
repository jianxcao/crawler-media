import "./plugin-ui-env";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { afterEach, describe, it } from "node:test";
import { pathToFileURL } from "node:url";

import * as React from "react";
import * as ReactDOM from "react-dom";
import * as ReactJsxRuntime from "react/jsx-runtime";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

import * as PluginUiSdk from "@crawler-media/plugin-ui-sdk";
import {
  PLUGIN_UI_CONTRACT_VERSION,
  PLUGIN_UI_REACT_DOM_VERSION,
  PLUGIN_UI_REACT_VERSION,
  PluginHostProvider,
  isSdkError,
  isTerminalOperation,
  useHostSDK,
  usePluginCommand,
  usePluginConfig,
  usePluginEnvironment,
  usePluginOperation,
  usePluginQuery,
  usePluginTheme,
  type Environment,
  type Operation,
  type PluginHostSDK,
  type PluginSurfaceProps,
  type UiHandle,
} from "@crawler-media/plugin-ui-sdk";
import * as PluginUiKit from "@crawler-media/plugin-ui-kit";
import {
  ActionBar,
  Card,
  ConfirmModal,
  DataTable,
  EmptyState,
  FormField,
  PageHeader,
  StatBadge,
  StatusPill,
} from "@crawler-media/plugin-ui-kit";

afterEach(() => {
  cleanup();
});

function createFakeHost(initialTheme = "silver"): {
  host: PluginHostSDK;
  setTheme: (theme: string) => void;
  pushOperation: (op: Operation) => void;
} {
  let env: Environment = {
    theme: initialTheme,
    language: "zh-CN",
    layout: "sidebar",
    user: { id: "user-admin-1", name: "admin" },
  };
  const envListeners = new Set<() => void>();
  const opListeners = new Map<string, Set<(op: Operation) => void>>();

  const host: PluginHostSDK = {
    context: {
      plugin_id: "org.example.poc",
      package_hash: "0".repeat(64),
      generation: "1",
      surface_id: "page",
      login_epoch: "epoch-1",
    },
    supports: () => true,
    environment: {
      get: () => env,
      subscribe: (listener) => {
        envListeners.add(listener);
        return () => envListeners.delete(listener);
      },
    },
    catalogs: {
      list: async () => ({ items: [{ id: "site-1", name: "DemoPT", capabilities: ["search"] }] }),
    },
    query: async (name) => {
      if (name === "sites.list") {
        return {
          items: [{ id: "site-1", name: "DemoPT", capabilities: ["search"] }],
          next_cursor: null,
        } as never;
      }
      throw { code: "request-failed", message: `unexpected query ${name}` };
    },
    objects: {
      resolve: async (_kind, id) => ({
        handle: `h_${id}` as UiHandle,
        revision: "1",
        expires_at: "2099-01-01T00:00:00Z",
      }),
    },
    search: {
      results: async () => ({ items: [] }),
      open: async () => ({ status: "opened" }),
    },
    commands: {
      submit: async () => ({ operation_id: "op-1", state: "queued" }),
    },
    tasks: {
      list: async () => ({ items: [] }),
      run: async () => ({ operation_id: "op-task-1", state: "queued" }),
      receipts: async () => ({ items: [] }),
    },
    operations: {
      get: async (id) => ({ operation_id: id, state: "running", revision: "1" }),
      watch: (id, listener) => {
        const set = opListeners.get(id) ?? new Set();
        set.add(listener);
        opListeners.set(id, set);
        return () => set.delete(listener);
      },
    },
    endpoint: { request: async () => ({ ok: true }) },
    media: {
      select: async () => null,
      detail: async () => ({ status: "opened" }),
    },
    subscriptions: {
      create: async () => ({ status: "cancelled" }),
      edit: async () => ({ status: "cancelled" }),
    },
    downloads: {
      add: async () => ({ status: "cancelled" }),
      cleanup: async () => ({ status: "cancelled" }),
    },
    libraries: { open: async () => ({ status: "opened" }) },
    files: {
      select: async () => null,
      transfer: async () => ({ status: "cancelled" }),
      scrape: async () => ({ status: "cancelled" }),
    },
    browser: {
      availability: async ({ site_id, purpose }) => ({
        site_id,
        purpose,
        status: "available",
        missing_requirements: [],
      }),
    },
    ui: {
      toast: () => {},
      confirm: async () => true,
      dialog: () => () => {},
    },
    navigation: { open: async () => ({ status: "opened" }) },
    shell: {
      registerAction: () => () => {},
      registerTabs: () => () => {},
    },
  };

  return {
    host,
    setTheme: (theme: string) => {
      env = { ...env, theme };
      for (const listener of envListeners) {
        listener();
      }
    },
    pushOperation: (op: Operation) => {
      const set = opListeners.get(op.operation_id);
      if (set) {
        for (const listener of set) {
          listener(op);
        }
      }
    },
  };
}

describe("Host Vite Module Federation config", () => {
  it("configures @module-federation/vite singletons while preserving worker.format and proxies", async () => {
    const configSource = fs.readFileSync(
      path.resolve(import.meta.dirname, "../vite.config.ts"),
      "utf8",
    );
    assert.match(configSource, /@module-federation\/vite/);
    assert.match(configSource, /singleton:\s*true/);
    assert.match(configSource, /react-dom/);
    assert.match(configSource, /@crawler-media\/plugin-ui-sdk/);
    assert.match(configSource, /@crawler-media\/plugin-ui-kit/);
    assert.match(configSource, /format:\s*"es"/);
    assert.match(configSource, /"\/api"/);
    assert.match(configSource, /"\/posters"/);
  });
});

describe("@crawler-media/plugin-ui-sdk & @crawler-media/plugin-ui-kit contracts", () => {
  it("pins exact 1.0.0 contract and React 19.3.0 versions", () => {
    assert.equal(PLUGIN_UI_CONTRACT_VERSION, "1.0.0");
    assert.equal(PLUGIN_UI_REACT_VERSION, "19.3.0");
    assert.equal(PLUGIN_UI_REACT_DOM_VERSION, "19.3.0");
    assert.equal(React.version, "19.3.0");
    assert.equal(isTerminalOperation("succeeded"), true);
    assert.equal(isTerminalOperation("running"), false);
    assert.equal(isSdkError({ code: "permission-denied", message: "no" }), true);
    assert.equal(isSdkError({ code: "bogus", message: "no" }), false);
  });

  it("renders all 9 UI kit primitives and React hooks across silver/paper/netflix themes", async () => {
    const { host, setTheme, pushOperation } = createFakeHost("silver");
    let lastSubmittedOp = "";

    function DemoSurface() {
      const sdk = useHostSDK();
      const env = usePluginEnvironment();
      const theme = usePluginTheme();
      const cfg = usePluginConfig();
      const sites = usePluginQuery("sites.list", { limit: 10 });
      const cmd = usePluginCommand("site.maintenance");
      const [modalOpen, setModalOpen] = React.useState(false);
      const watchedOp = usePluginOperation(cmd.lastOperation?.operation_id ?? null);

      return React.createElement(
        "div",
        { "data-testid": "theme-box", "data-theme": theme },
        React.createElement(PageHeader, {
          title: `Plugin ${sdk.context.plugin_id}`,
          subtitle: `${env.language}:${theme}`,
          badge: React.createElement(StatusPill, { label: theme, tone: "info" }),
          actions: React.createElement(ActionBar, {
            items: [
              {
                id: "run",
                label: "Run Check",
                tone: "primary",
                onClick: () => {
                  void cmd
                    .submit(
                      { site_handle: "s1" as UiHandle, action: "check-in" },
                      { expectedRevision: "1", idempotencyKey: "k1" },
                    )
                    .then((op) => {
                      lastSubmittedOp = op.operation_id;
                    });
                },
              },
              {
                id: "confirm",
                label: "Open Confirm",
                onClick: () => setModalOpen(true),
              },
            ],
          }),
        }),
        React.createElement(
          Card,
          { title: "Overview", description: "State and config" },
          React.createElement(StatBadge, {
            label: "Sites",
            value: String(sites.data?.items.length ?? 0),
            tone: "ok",
          }),
          React.createElement(StatBadge, {
            label: "Op State",
            value: watchedOp?.state ?? "idle",
          }),
          React.createElement(
            FormField,
            { label: "Mode", htmlFor: "mode-input", description: String(cfg?.revision ?? "") },
            React.createElement("input", {
              id: "mode-input",
              defaultValue: String(cfg?.values.mode ?? "fast"),
            }),
          ),
          React.createElement(DataTable<{ id: string; name: string }>, {
            columns: [
              { key: "id", header: "ID", render: (r) => r.id },
              { key: "name", header: "Name", render: (r) => r.name },
            ],
            rows: sites.data?.items ?? [],
            getRowKey: (r) => r.id,
          }),
          React.createElement(EmptyState, {
            title: "Nothing queued",
            description: "Ready",
          }),
        ),
        React.createElement(ConfirmModal, {
          open: modalOpen,
          title: "Proceed?",
          message: "Confirm plugin action",
          tone: "danger",
          onConfirm: () => setModalOpen(false),
          onCancel: () => setModalOpen(false),
        }),
      );
    }

    render(
      React.createElement(
        PluginHostProvider,
        {
          host,
          surfaceId: "page",
          config: {
            values: { mode: "safe" },
            schemaRevision: "1",
            revision: "rev-42",
            editable: true,
            onDraftChange: () => {},
            save: async () => {},
          },
        },
        React.createElement(DemoSurface),
      ),
    );

    await waitFor(() => {
      assert.ok(screen.getByText("DemoPT"));
    });
    assert.equal(screen.getByTestId("theme-box").getAttribute("data-theme"), "silver");

    for (const nextTheme of ["paper", "netflix"]) {
      act(() => {
        setTheme(nextTheme);
      });
      await waitFor(() => {
        assert.equal(screen.getByTestId("theme-box").getAttribute("data-theme"), nextTheme);
      });
    }

    fireEvent.click(screen.getByRole("button", { name: "Run Check" }));
    await waitFor(() => {
      assert.equal(lastSubmittedOp, "op-1");
    });
    act(() => {
      pushOperation({ operation_id: "op-1", state: "succeeded", revision: "2" });
    });
    await waitFor(() => {
      assert.ok(screen.getByText("succeeded"));
    });

    fireEvent.click(screen.getByRole("button", { name: "Open Confirm" }));
    assert.ok(screen.getByRole("dialog", { name: "Proceed?" }));
    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => {
      assert.equal(screen.queryByRole("dialog", { name: "Proceed?" }), null);
    });
  });
});

interface RemoteBuildResult {
  remoteEntryPath: string;
  uiRootDir: string;
  assetFiles: string[];
}

async function buildFederatedRemote(
  workDir: string,
  remoteName: string,
  label: string,
): Promise<RemoteBuildResult> {
  const remoteDir = path.join(workDir, remoteName);
  const srcDir = path.join(remoteDir, "src");
  fs.mkdirSync(srcDir, { recursive: true });

  fs.writeFileSync(
    path.join(remoteDir, "package.json"),
    JSON.stringify({ name: remoteName, private: true, type: "module" }),
  );
  fs.writeFileSync(
    path.join(srcDir, "Page.tsx"),
    `import * as React from "react";
import { usePluginTheme, type PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";
import { ActionBar, Card, PageHeader, StatBadge, StatusPill } from "@crawler-media/plugin-ui-kit";

declare global {
  var __CM_POC_REACT_REFS__: Set<unknown> | undefined;
}

export default function RemotePage({ host, surfaceId }: PluginSurfaceProps) {
  const [count, setCount] = React.useState(0);
  const theme = usePluginTheme(host);
  if (globalThis.__CM_POC_REACT_REFS__) {
    globalThis.__CM_POC_REACT_REFS__.add(React.useState);
    globalThis.__CM_POC_REACT_REFS__.add(React.createElement);
  }
  return (
    <Card title={"${label}:" + surfaceId}>
      <PageHeader title="${label} Header" badge={<StatusPill label={theme} tone="ok" />} />
      <StatBadge label="Clicks" value={String(count)} />
      <ActionBar
        items={[
          {
            id: "inc",
            label: "Increment ${label}",
            tone: "primary",
            onClick: () => setCount((c) => c + 1),
          },
        ]}
      />
    </Card>
  );
}
`,
  );
  fs.writeFileSync(
    path.join(srcDir, "Config.tsx"),
    `import { useState } from "react";
import type { PluginSurfaceProps } from "@crawler-media/plugin-ui-sdk";
import { Card, FormField } from "@crawler-media/plugin-ui-kit";

export default function RemoteConfig({ config }: PluginSurfaceProps) {
  const [draft, setDraft] = useState(String(config?.values.token ?? "ok"));
  return (
    <Card title="${label} Config">
      <FormField label="Setting">
        <input value={draft} onChange={(e) => setDraft(e.target.value)} />
      </FormField>
    </Card>
  );
}
`,
  );
  fs.writeFileSync(
    path.join(srcDir, "virtual-entry.ts"),
    `export {};\n`,
  );

  const webRoot = path.resolve(import.meta.dirname, "..");
  const viteBin = path.join(webRoot, "node_modules/vite/bin/vite.js");
  const reactPluginPath = pathToFileURL(
    path.join(webRoot, "node_modules/@vitejs/plugin-react/dist/index.js"),
  ).href;
  const mfPluginPath = pathToFileURL(
    path.join(webRoot, "node_modules/@module-federation/vite/lib/index.js"),
  ).href;

  fs.writeFileSync(
    path.join(remoteDir, "vite.config.mjs"),
    `import { defineConfig } from ${JSON.stringify(
      pathToFileURL(path.join(webRoot, "node_modules/vite/dist/node/index.js")).href,
    )};
import react from ${JSON.stringify(reactPluginPath)};
import { federation } from ${JSON.stringify(mfPluginPath)};

export default defineConfig({
  root: ${JSON.stringify(remoteDir)},
  logLevel: "silent",
  resolve: {
    alias: {
      react: ${JSON.stringify(path.join(webRoot, "node_modules/react"))},
      "react-dom": ${JSON.stringify(path.join(webRoot, "node_modules/react-dom"))},
      "@crawler-media/plugin-ui-sdk": ${JSON.stringify(path.join(webRoot, "../sdk/plugin-ui/src/index.ts"))},
      "@crawler-media/plugin-ui-kit": ${JSON.stringify(path.join(webRoot, "../sdk/plugin-ui-kit/src/index.tsx"))},
    },
  },
  plugins: [
    react(),
    federation({
      name: ${JSON.stringify(remoteName)},
      filename: "assets/remoteEntry.js",
      exposes: {
        "./Page": "./src/Page.tsx",
        "./Config": "./src/Config.tsx",
      },
      dts: false,
      manifest: false,
      shared: {
        react: { singleton: true, requiredVersion: "19.3.0", import: false },
        "react-dom": { singleton: true, requiredVersion: "19.3.0", import: false },
        "react/jsx-runtime": { singleton: true, requiredVersion: "19.3.0", import: false },
        "@crawler-media/plugin-ui-sdk": {
          singleton: true,
          requiredVersion: "1.0.0",
          import: false,
        },
        "@crawler-media/plugin-ui-kit": {
          singleton: true,
          requiredVersion: "1.0.0",
          import: false,
        },
      },
    }),
  ],
  build: {
    outDir: "ui",
    emptyOutDir: true,
    target: "es2022",
    minify: false,
    sourcemap: false,
    rollupOptions: {
      input: "./src/virtual-entry.ts",
    },
  },
});
`,
  );

  execFileSync(process.execPath, [viteBin, "build", "--config", "vite.config.mjs"], {
    cwd: remoteDir,
    stdio: "pipe",
  });

  const uiRootDir = path.join(remoteDir, "ui");
  const collected: string[] = [];
  const walk = (dir: string) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      if (entry.isDirectory()) {
        walk(full);
      } else {
        const rel = `ui/${path.relative(uiRootDir, full).replaceAll("\\", "/")}`;
        if (!rel.endsWith(".html") && !rel.endsWith(".map")) {
          collected.push(rel);
        }
      }
    }
  };
  walk(uiRootDir);

  const manifestFiles = collected
    .filter((rel) => rel !== "ui/asset-manifest.json")
    .map((rel) => {
      const bytes = fs.readFileSync(path.join(remoteDir, rel));
      return {
        path: rel,
        sha256: createHash("sha256").update(bytes).digest("hex"),
        size: bytes.length,
        content_type: rel.endsWith(".css") ? "text/css" : "application/javascript",
      };
    });
  fs.writeFileSync(
    path.join(uiRootDir, "asset-manifest.json"),
    JSON.stringify({ version: 1, files: manifestFiles }, null, 2),
  );

  return {
    remoteEntryPath: path.join(uiRootDir, "assets/remoteEntry.js"),
    uiRootDir,
    assetFiles: manifestFiles.map((f) => f.path),
  };
}

describe("Two-remote Vite 7.3.6 + React 19.3.0 Module Federation PoC", () => {
  it("loads two simultaneous federated remotes sharing one React 19.3.0 instance without hook errors", async () => {
    const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "cm-mf-poc-"));
    try {
      const [alpha, beta] = await Promise.all([
        buildFederatedRemote(tmpRoot, "cm_poc_alpha", "Alpha"),
        buildFederatedRemote(tmpRoot, "cm_poc_beta", "Beta"),
      ]);

      for (const built of [alpha, beta]) {
        assert.ok(fs.existsSync(built.remoteEntryPath), "remoteEntry.js missing");
        assert.ok(built.assetFiles.includes("ui/assets/remoteEntry.js"));
        for (const rel of built.assetFiles) {
          assert.equal(rel.endsWith(".html"), false);
          assert.equal(rel.endsWith(".map"), false);
          const content = fs.readFileSync(path.join(path.dirname(built.uiRootDir), rel), "utf8");
          assert.equal(
            content.includes("createInvalidHookError") || content.includes("react-dom.production"),
            false,
            `Duplicate react bundle leaked into ${rel}`,
          );
        }
      }

      const shareScope = {
        react: {
          "19.3.0": {
            get: async () => () => React,
            loaded: 1,
            from: "crawler_media_host",
            scope: "default",
          },
        },
        "react-dom": {
          "19.3.0": {
            get: async () => () => ReactDOM,
            loaded: 1,
            from: "crawler_media_host",
            scope: "default",
          },
        },
        "react/jsx-runtime": {
          "19.3.0": {
            get: async () => () => ReactJsxRuntime,
            loaded: 1,
            from: "crawler_media_host",
            scope: "default",
          },
        },
        "@crawler-media/plugin-ui-sdk": {
          "1.0.0": {
            get: async () => () => PluginUiSdk,
            loaded: 1,
            from: "crawler_media_host",
            scope: "default",
          },
        },
        "@crawler-media/plugin-ui-kit": {
          "1.0.0": {
            get: async () => () => PluginUiKit,
            loaded: 1,
            from: "crawler_media_host",
            scope: "default",
          },
        },
      };

      const loadContainerExport = async (entryFile: string, expose: string) => {
        const container = (await import(pathToFileURL(entryFile).href)) as {
          init: (scope: typeof shareScope) => Promise<void> | void;
          get: (expose: string) => Promise<() => { default: React.ComponentType<PluginSurfaceProps> }>;
        };
        await container.init(shareScope);
        const factory = await container.get(expose);
        return factory().default;
      };

      const [AlphaPage, BetaPage, AlphaConfig] = await Promise.all([
        loadContainerExport(alpha.remoteEntryPath, "./Page"),
        loadContainerExport(beta.remoteEntryPath, "./Page"),
        loadContainerExport(alpha.remoteEntryPath, "./Config"),
      ]);

      const reactHookRefs = new Set<unknown>([React.useState]);
      const reactCreateRefs = new Set<unknown>([React.createElement]);
      (globalThis as { __CM_POC_REACT_REFS__?: Set<unknown> }).__CM_POC_REACT_REFS__ = new Set([
        React.useState,
        React.createElement,
      ]);

      const consoleErrors: string[] = [];
      const origError = console.error;
      console.error = (...args: unknown[]) => {
        consoleErrors.push(args.map(String).join(" "));
      };

      try {
        const { host } = createFakeHost("paper");
        render(
          React.createElement(
            "div",
            null,
            React.createElement(AlphaPage, { host, surfaceId: "page" }),
            React.createElement(BetaPage, { host, surfaceId: "page" }),
            React.createElement(AlphaConfig, {
              host,
              surfaceId: "config",
              config: {
                values: { token: "shared-secret" },
                schemaRevision: "1",
                revision: "1",
                editable: true,
                onDraftChange: () => {},
                save: async () => {},
              },
            }),
          ),
        );

        assert.ok(screen.getByText("Alpha Header"));
        assert.ok(screen.getByText("Beta Header"));
        assert.ok(screen.getByText("Alpha Config"));
        fireEvent.click(screen.getByRole("button", { name: "Increment Alpha" }));
        fireEvent.click(screen.getByRole("button", { name: "Increment Beta" }));
        fireEvent.click(screen.getByRole("button", { name: "Increment Beta" }));

        await waitFor(() => {
          assert.equal(screen.getAllByText("1").length, 1);
          assert.equal(screen.getAllByText("2").length, 1);
        });
        const recorded = (globalThis as { __CM_POC_REACT_REFS__?: Set<unknown> })
          .__CM_POC_REACT_REFS__;
        assert.equal(reactHookRefs.size, 1);
        assert.equal(reactCreateRefs.size, 1);
        assert.equal(recorded?.size, 2, "Both remotes must share exact host React hook & element functions");
        assert.equal(
          consoleErrors.some((msg) => msg.toLowerCase().includes("hook")),
          false,
          `Unexpected React hook errors: ${consoleErrors.join("\n")}`,
        );
      } finally {
        console.error = origError;
        delete (globalThis as { __CM_POC_REACT_REFS__?: Set<unknown> }).__CM_POC_REACT_REFS__;
      }
    } finally {
      fs.rmSync(tmpRoot, { recursive: true, force: true });
    }
  });
});
