import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import ts from 'typescript';

const nodes = tree => Array.isArray(tree) ? tree.flatMap(nodes) : !tree || typeof tree !== 'object' ? [] : [tree, ...nodes(tree.props?.children)];
const settle = async () => { for (let i = 0; i < 10; i++) await Promise.resolve(); };

async function mount(refresh, cryptoApi = globalThis.crypto) {
  const source = await readFile(new URL('../components/library-media-info-dialog.tsx', import.meta.url), 'utf8');
  const code = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } }).outputText;
  const slots = []; let index = 0, closed = 0; const receipts = [];
  const useState = initial => { const i = index++; if (!(i in slots)) slots[i] = initial; return [slots[i], value => { slots[i] = typeof value === 'function' ? value(slots[i]) : value; }]; };
  const useRef = current => { const i = index++; return slots[i] ??= { current }; };
  const jsx = (type, props) => ({ type, props });
  const module = { exports: {} };
  new Function('require', 'module', 'exports', 'crypto', code)(name => {
    if (name === 'react') return { useState, useRef };
    if (name === 'react/jsx-runtime') return { jsx, jsxs: jsx };
    if (name === '@/components/modal') return { Modal: 'modal' };
    if (name === '@/lib/api/libraries') return { refreshLibraryMediaInfo: refresh, createMediaInfoRequestId: () => '00000000-0000-4000-8000-000000000001' };
    throw new Error(name);
  }, module, module.exports, cryptoApi);
  const render = () => { index = 0; return module.exports.LibraryMediaInfoDialog({ library: { id: 'lib', name: '电影' }, onClose: () => { closed++; }, onSubmitted: receipt => receipts.push(receipt) }); };
  return { render, receipts, closed: () => closed };
}

test('media information dialog defaults to missing and submits force only when selected', async () => {
  for (const force of [false, true]) {
    const calls = [], receipt = { mode: force ? 'force' : 'missing', total: 4, queued: 2, skipped: 2 };
    const ui = await mount(async (...args) => { calls.push(args); return receipt; });
    let tree = ui.render();
    const radios = nodes(tree).filter(node => node.type === 'input' && node.props.type === 'radio');
    assert.equal(radios.length, 2);
    assert.equal(radios.find(node => node.props.value === 'missing').props.checked, true);
    if (force) radios.find(node => node.props.value === 'force').props.onChange();
    tree = ui.render();
    nodes(tree).find(node => node.type === 'button' && node.props.children === '开始抓取').props.onClick();
    await settle();
    assert.equal(calls.length, 1);
    assert.deepEqual(calls[0].slice(0, 2), ['lib', force ? 'force' : 'missing']);
    assert.match(calls[0][2], /^[a-f0-9-]{36}$/);
    assert.deepEqual(ui.receipts, [receipt]);
    assert.equal(ui.closed(), 1);
  }
});

test('failed submission stays open with an actionable error and supports retry', async () => {
  let attempts = 0; const identities = [];
  const ui = await mount(async (_id, _mode, requestId) => { identities.push(requestId); if (++attempts === 1) throw new Error('服务繁忙'); return { mode: 'missing', total: 1, queued: 1, skipped: 0 }; });
  const submit = () => nodes(ui.render()).find(node => node.type === 'button' && node.props.children === '开始抓取').props.onClick();
  const firstClick = nodes(ui.render()).find(node => node.type === 'button' && node.props.children === '开始抓取').props.onClick;
  firstClick(); firstClick(); await settle();
  assert.equal(attempts, 1, 'repeated clicks while submitting must not create another request');
  assert.equal(ui.closed(), 0);
  assert.equal(nodes(ui.render()).find(node => node.props?.role === 'alert').props.children, '服务繁忙');
  submit(); await settle();
  assert.equal(attempts, 2);
  assert.ok(identities[0]);
  assert.equal(identities[0], identities[1], 'retry must reuse already submitted file tasks');
  assert.equal(ui.closed(), 1);
});

test('cancel closes the media information dialog without a request', async () => {
  const ui = await mount(async () => assert.fail('cancel must not submit'));
  nodes(ui.render()).find(node => node.type === 'button' && node.props.children === '取消').props.onClick();
  assert.equal(ui.closed(), 1);
});

test('media information dialog can submit on an ordinary HTTP origin', async () => {
  let submitted = false;
  const ui = await mount(async () => {
    submitted = true;
    return { mode: 'missing', total: 0, queued: 0, skipped: 0 };
  }, { getRandomValues: globalThis.crypto.getRandomValues.bind(globalThis.crypto) });
  nodes(ui.render()).find(node => node.type === 'button' && node.props.children === '开始抓取').props.onClick();
  await settle();
  assert.equal(submitted, true);
  assert.equal(ui.closed(), 1);
});
