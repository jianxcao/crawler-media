import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import React, { act } from "react";
import { create } from "react-test-renderer";
import ts from "typescript";

const require = createRequire(import.meta.url);
globalThis.IS_REACT_ACT_ENVIRONMENT = true;

async function mount(save, remoteReads = { paused: false }, getProbe = async () => ({ metadata_workers: 3, remote_concurrency: 2, remote_cooldown_ms: 750 })) {
  const source = readFileSync(new URL("../components/probe-settings-card.tsx", import.meta.url), "utf8");
  const js = ts.transpileModule(source, { compilerOptions: {
    module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022,
  } }).outputText;
  const module = { exports: {} };
  new Function("require", "module", "exports", js)(name => {
    if (name === "@/lib/api/probe-settings") return {
      getProbeSettings: getProbe,
      saveProbeSettings: save,
      getRemoteReadsSettings: async () => remoteReads,
      saveRemoteReadsSettings: async paused => ({ paused }),
    };
    return require(name);
  }, module, module.exports);
  let renderer;
  await act(async () => { renderer = create(React.createElement(module.exports.ProbeSettingsCard)); });
  const button = label => renderer.root.findAllByType("button").find(node => node.children.join("") === label);
  return {
    field: label => renderer.root.findByProps({ "aria-label": label }),
    button: () => button("保存探测设置"),
    status: () => renderer.root.findAll(node => node.props.role === "status" || node.props.role === "alert").map(node => node.children.join("")).join(" "),
    async change(label, value) { await act(async () => { this.field(label).props.onChange({ target: { value } }); }); },
    async submit() { await act(async () => { this.button().props.onClick(); }); },
    async press(label) { await act(async () => { button(label).props.onClick(); }); },
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

test("STRM reads can be paused and resumed while speed settings have invalid edits", async () => {
  const ui = await mount(async () => { throw new Error("unexpected speed save"); }, { paused: true });
  await ui.change("远程读取并发", "0");
  assert.equal(ui.button().props.disabled, true);
  assert.match(ui.status(), /已暂停/);
  await ui.press("恢复 STRM 后台读取");
  assert.match(ui.status(), /已启用/);
  await ui.press("暂停 STRM 后台读取");
  assert.match(ui.status(), /已暂停/);
  assert.equal(ui.field("远程读取并发").props.value, 0);
  await ui.unmount();
});

test("probe settings disables inputs while saving and offers retry on initial load failure", async () => {
  let resolveSave;
  const savePromise = new Promise(resolve => { resolveSave = resolve; });
  const savingUi = await mount(async () => savePromise);
  await savingUi.change("媒体信息并发", "4");
  let pendingSubmit;
  await act(async () => { pendingSubmit = savingUi.button().props.onClick(); });
  assert.equal(savingUi.field("媒体信息并发").props.disabled, true);
  assert.equal(savingUi.field("远程读取并发").props.disabled, true);
  assert.equal(savingUi.field("远程读取间隔（毫秒）").props.disabled, true);
  resolveSave({ metadata_workers: 4, remote_concurrency: 2, remote_cooldown_ms: 750 });
  await act(async () => { await pendingSubmit; });
  assert.equal(savingUi.field("媒体信息并发").props.disabled, false);
  await savingUi.unmount();

  let attempts = 0;
  const retryUi = await mount(
    async x => x,
    { paused: false },
    async () => {
      attempts += 1;
      if (attempts === 1) throw new Error("网络异常");
      return { metadata_workers: 2, remote_concurrency: 1, remote_cooldown_ms: 500 };
    },
  );
  assert.match(retryUi.status(), /网络异常/);
  await retryUi.press("重试");
  assert.equal(retryUi.field("媒体信息并发").props.value, 2);
  await retryUi.unmount();
});
