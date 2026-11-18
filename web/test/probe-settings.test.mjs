import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import React, { act } from "react";
import { create } from "react-test-renderer";
import ts from "typescript";

const require = createRequire(import.meta.url);
globalThis.IS_REACT_ACT_ENVIRONMENT = true;

async function mount(save) {
  const source = readFileSync(new URL("../components/probe-settings-card.tsx", import.meta.url), "utf8");
  const js = ts.transpileModule(source, { compilerOptions: {
    module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022,
  } }).outputText;
  const module = { exports: {} };
  new Function("require", "module", "exports", js)(name => {
    if (name === "@/lib/api/probe-settings") return {
      getProbeSettings: async () => ({ metadata_workers: 3, remote_concurrency: 2, remote_cooldown_ms: 750 }),
      saveProbeSettings: save,
    };
    return require(name);
  }, module, module.exports);
  let renderer;
  await act(async () => { renderer = create(React.createElement(module.exports.ProbeSettingsCard)); });
  return {
    field: label => renderer.root.findByProps({ "aria-label": label }),
    button: () => renderer.root.findByType("button"),
    status: () => renderer.root.findAll(node => node.props.role === "status" || node.props.role === "alert").map(node => node.children.join("")).join(" "),
    async change(label, value) { await act(async () => { this.field(label).props.onChange({ target: { value } }); }); },
    async submit() { await act(async () => { this.button().props.onClick(); }); },
    async unmount() { await act(async () => renderer.unmount()); },
  };
}

test("probe settings loads saved values, validates limits and saves all three values", async () => {
  let stored;
  const ui = await mount(async value => { stored = value; return value; });
  assert.equal(ui.field("媒体信息并发").props.value, 3);
  assert.equal(ui.field("远程读取并发").props.value, 2);
  assert.equal(ui.field("远程读取间隔（毫秒）").props.value, 750);
  assert.equal(ui.button().props.disabled, true);
  for (const [label, bad, valid] of [
    ["媒体信息并发", "9", "4"], ["远程读取并发", "0", "3"],
    ["远程读取间隔（毫秒）", "60001", "0"],
  ]) {
    await ui.change(label, bad);
    assert.equal(ui.button().props.disabled, true);
    await ui.change(label, valid);
  }
  assert.equal(ui.button().props.disabled, false);
  await ui.submit();
  assert.deepEqual(stored, { metadata_workers: 4, remote_concurrency: 3, remote_cooldown_ms: 0 });
  assert.match(ui.status(), /已保存/);
  assert.equal(ui.button().props.disabled, true);
  await ui.unmount();
});

test("probe settings retains edits and shows server failure for retry", async () => {
  const ui = await mount(async () => { throw new Error("服务繁忙"); });
  await ui.change("媒体信息并发", "2");
  await ui.submit();
  assert.equal(ui.field("媒体信息并发").props.value, 2);
  assert.match(ui.status(), /服务繁忙/);
  assert.equal(ui.button().props.disabled, false);
  await ui.unmount();
});
