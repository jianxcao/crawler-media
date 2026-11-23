import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import React, { act } from "react";
import { create } from "react-test-renderer";
import ts from "typescript";

const require = createRequire(import.meta.url);
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const webRoot = fileURLToPath(new URL("../", import.meta.url));

function loadLogsApi({ fetchEventSource, redirectToLoginOn401 }) {
  const js = ts.transpileModule(readFileSync(path.join(webRoot, "lib/api/logs.ts"), "utf8"), {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
    },
  }).outputText;
  const mod = { exports: {} };
  new Function("require", "module", "exports", js)(
    (name) => {
      if (name === "@/lib/http") {
        return {
          request: async () => ({ ok: true, data: { total: 0, entries: [] } }),
          resolveRequestUrl: (p) => `/api/v1${p}`,
          getAuthToken: () => "tok-1",
          redirectToLoginOn401,
        };
      }
      if (name === "@microsoft/fetch-event-source") {
        return { fetchEventSource };
      }
      return require(name);
    },
    mod,
    mod.exports,
  );
  return mod.exports;
}

test("subscribeSystemLogs combines external signal with unsubscribe controller and aborts cleanly", () => {
  const calls = [];
  const { subscribeSystemLogs } = loadLogsApi({
    fetchEventSource: async (url, options) => {
      calls.push({ url, options });
    },
    redirectToLoginOn401: () => {},
  });

  const external = new AbortController();
  const unsub = subscribeSystemLogs({
    onLog() {},
    signal: external.signal,
  });
  assert.equal(calls.length, 1);
  assert.equal(calls[0].options.signal.aborted, false);
  unsub();
  assert.equal(
    calls[0].options.signal.aborted,
    true,
    "calling unsub() must abort stream even when caller passed options.signal",
  );

  const external2 = new AbortController();
  subscribeSystemLogs({
    onLog() {},
    signal: external2.signal,
  });
  assert.equal(calls[1].options.signal.aborted, false);
  external2.abort();
  assert.equal(calls[1].options.signal.aborted, true);
});

for (const status of [401, 403]) {
  test(`subscribeSystemLogs stops retrying and calls redirectToLoginOn401 on HTTP ${status}`, async () => {
    const calls = [];
    const redirects = [];
    const errors = [];
    const { subscribeSystemLogs } = loadLogsApi({
      fetchEventSource: async (_url, options) => {
        calls.push(options);
      },
      redirectToLoginOn401: (code) => redirects.push(code),
    });

    subscribeSystemLogs({
      onLog() {},
      onError: (err) => errors.push(err),
    });

    const opts = calls[0];
    assert.equal(typeof opts.onopen, "function", "subscribeSystemLogs must validate response in onopen");

    let caught;
    try {
      await opts.onopen(new Response(null, { status }));
      assert.fail("onopen must throw on 401/403");
    } catch (err) {
      caught = err;
    }

    assert.deepEqual(redirects, [status]);
    assert.equal(opts.signal.aborted, true);
    assert.throws(() => opts.onerror(caught), (err) => err === caught);
    assert.equal(errors.length, 1);
  });
}

test("SystemLogsSection passes ConfirmOptions description and confirmLabel to confirm() and loads logs once", async () => {
  const confirms = [];
  let listCalls = 0;
  const js = ts.transpileModule(
    readFileSync(path.join(webRoot, "components/system-logs-section.tsx"), "utf8"),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        jsx: ts.JsxEmit.ReactJSX,
        target: ts.ScriptTarget.ES2022,
        esModuleInterop: true,
      },
    },
  ).outputText;
  const mod = { exports: {} };
  new Function("require", "module", "exports", js)(
    (name) => {
      if (name === "@tanstack/react-virtual") {
        return {
          useVirtualizer: () => ({
            getTotalSize: () => 0,
            getVirtualItems: () => [],
            scrollToIndex() {},
            measureElement() {},
          }),
        };
      }
      if (name === "@/lib/api/logs") {
        return {
          listSystemLogs: async () => {
            listCalls += 1;
            if (listCalls > 5) {
              throw new Error("Infinite loadLogs loop detected");
            }
            return { total: 0, entries: [] };
          },
          clearSystemLogs: async () => ({ cleared: true }),
          subscribeSystemLogs: () => () => {},
          exportLogsUrl: () => "/api/v1/system/logs/export",
        };
      }
      if (name === "@/components/feedback") {
        return {
          // Intentionally return a new object on every render to verify loadLogs does not loop.
          useToast: () => ({ success() {}, error() {}, info() {} }),
          useConfirm: () => async (opts) => {
            confirms.push(opts);
            return false;
          },
        };
      }
      return require(name);
    },
    mod,
    mod.exports,
  );

  let renderer;
  await act(async () => {
    renderer = create(React.createElement(mod.exports.SystemLogsSection));
  });

  try {
    assert.equal(listCalls, 1, "SystemLogsSection must fetch initial logs once on mount");
    const clearBtn = renderer.root
      .findAllByType("button")
      .find((b) => b.children.join("") === "清空");
    assert.ok(clearBtn);
    await act(async () => {
      await clearBtn.props.onClick();
    });
    assert.equal(confirms.length, 1);
    assert.equal(confirms[0].title, "清空日志");
    assert.equal(typeof confirms[0].description, "string");
    assert.equal(confirms[0].confirmLabel, "清空");
    assert.equal("content" in confirms[0], false);
    assert.equal("confirmText" in confirms[0], false);
  } finally {
    await act(async () => {
      renderer.unmount();
    });
  }
});

test("FeedbackApi confirm overload rejects excess properties in TypeScript", () => {
  const feedbackPath = path.join(webRoot, "components/feedback.tsx");
  const feedbackSource = readFileSync(feedbackPath, "utf8");
  const probeFile = path.join(webRoot, "components/__confirm_overload_probe__.tsx");
  const probeCode = `
    import { useConfirm } from "./feedback";
    export function Probe() {
      const confirm = useConfirm();
      void confirm({ title: "bad", content: "invalid", confirmText: "nope" });
      return null;
    }
  `;
  const configPath = path.join(webRoot, "tsconfig.json");
  const configJson = ts.readConfigFile(configPath, (p) => readFileSync(p, "utf8")).config;
  const parsed = ts.parseJsonConfigFileContent(configJson, ts.sys, webRoot);
  const host = ts.createCompilerHost(parsed.options);
  const origGetSourceFile = host.getSourceFile.bind(host);
  const origFileExists = host.fileExists.bind(host);
  host.fileExists = (fileName) =>
    path.resolve(fileName) === probeFile ? true : origFileExists(fileName);
  host.getSourceFile = (fileName, languageVersion) => {
    if (path.resolve(fileName) === probeFile) {
      return ts.createSourceFile(fileName, probeCode, languageVersion);
    }
    return origGetSourceFile(fileName, languageVersion);
  };
  const program = ts.createProgram([probeFile, feedbackPath], parsed.options, host);
  const diagnostics = ts.getPreEmitDiagnostics(program, program.getSourceFile(probeFile));
  assert.ok(
    diagnostics.some((d) => ts.flattenDiagnosticMessageText(d.messageText, "\n").includes("content")),
    `Expected TS excess property error for invalid 'content' on confirm(), got: ${diagnostics.map((d) => d.messageText).join("; ")}`,
  );
  assert.doesNotMatch(feedbackSource, /confirm:\s*<O extends ConfirmOptions>/);
});

test("SystemLogsSection keys virtual rows by log id and auto-scrolls on SSE appends at the 3,000-log cap", async () => {
  let latestVirtualizerOptions = null;
  const scrollToIndexCalls = [];
  let onSseLog = null;

  const initialEntries = Array.from({ length: 3000 }, (_, i) => ({
    id: i + 1,
    timestamp: 1_700_000_000_000 + i * 1000,
    level: "INFO",
    target: "api",
    message: `initial log ${i + 1}`,
  }));

  const js = ts.transpileModule(
    readFileSync(path.join(webRoot, "components/system-logs-section.tsx"), "utf8"),
    {
      compilerOptions: {
        module: ts.ModuleKind.CommonJS,
        jsx: ts.JsxEmit.ReactJSX,
        target: ts.ScriptTarget.ES2022,
        esModuleInterop: true,
      },
    },
  ).outputText;
  const mod = { exports: {} };
  const virtualizerStub = {
    getTotalSize: () => 3000 * 28,
    getVirtualItems: () => [{ index: 2999, key: 3000, start: 2999 * 28, size: 28 }],
    scrollToIndex(index, opts) {
      scrollToIndexCalls.push({ index, opts });
    },
    measureElement() {},
  };

  new Function("require", "module", "exports", js)(
    (name) => {
      if (name === "@tanstack/react-virtual") {
        return {
          useVirtualizer: (opts) => {
            latestVirtualizerOptions = opts;
            return virtualizerStub;
          },
        };
      }
      if (name === "@/lib/api/logs") {
        return {
          listSystemLogs: async () => ({ total: 3000, entries: initialEntries }),
          clearSystemLogs: async () => ({ cleared: true }),
          subscribeSystemLogs: ({ onLog }) => {
            onSseLog = onLog;
            return () => {};
          },
          exportLogsUrl: () => "/api/v1/system/logs/export",
        };
      }
      if (name === "@/components/feedback") {
        return {
          useToast: () => ({ success() {}, error() {}, info() {} }),
          useConfirm: () => async () => false,
        };
      }
      return require(name);
    },
    mod,
    mod.exports,
  );

  let renderer;
  await act(async () => {
    renderer = create(React.createElement(mod.exports.SystemLogsSection));
  });

  try {
    assert.equal(typeof latestVirtualizerOptions?.getItemKey, "function", "useVirtualizer must receive getItemKey");
    assert.equal(latestVirtualizerOptions.getItemKey(0), 1);
    assert.equal(latestVirtualizerOptions.getItemKey(2999), 3000);
    assert.equal(scrollToIndexCalls.length, 1, "initial 3,000 logs must auto-scroll once");
    assert.deepEqual(scrollToIndexCalls[0], { index: 2999, opts: { align: "end" } });

    // Append log #3001 at the 3,000-entry ring buffer cap (filteredLogs.length stays 3000!)
    await act(async () => {
      onSseLog({
        id: 3001,
        timestamp: 1_700_003_000_000,
        level: "INFO",
        target: "api",
        message: "appended at cap 3001",
      });
    });

    assert.equal(latestVirtualizerOptions.getItemKey(0), 2, "shifted head item key must follow log.id");
    assert.equal(latestVirtualizerOptions.getItemKey(2999), 3001, "tail item key must follow new log.id");
    assert.equal(
      scrollToIndexCalls.length,
      2,
      "appending at the 3,000-log cap must still trigger scrollToIndex when autoScroll is enabled",
    );
    assert.deepEqual(scrollToIndexCalls[1], { index: 2999, opts: { align: "end" } });
  } finally {
    await act(async () => {
      renderer.unmount();
    });
  }
});
