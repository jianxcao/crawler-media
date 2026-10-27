import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
test('metadata language has separate CAS API and durable operation UI', async () => {
  const api = await readFile(new URL('../lib/api/metadata-language.ts', import.meta.url), 'utf8');
  const ui = await readFile(new URL('../components/metadata-language-setting.tsx', import.meta.url), 'utf8');
  assert.match(api, /settings\/metadata-language/);
  assert.match(api, /expected_revision/);
  assert.match(api, /operations.*retry/);
  for (const text of ['operations', 'failed', 'error', '重启', 'setInterval', 'retryMetadataLanguageOperation']) assert.ok(ui.includes(text));
  const settings = await readFile(new URL('../components/metadata-settings-section.tsx', import.meta.url), 'utf8');
  assert.match(settings, /<MetadataLanguageSetting/);
});

import ts from 'typescript';
async function mountedLanguage(api) {
  const source = await readFile(new URL('../components/metadata-language-setting.tsx', import.meta.url), 'utf8');
  const js = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022, jsx: ts.JsxEmit.ReactJSX } }).outputText;
  const slots = [], effects = [], cleanups = [];
  let index = 0, poll = null;
  const changed = (previous, deps) => !previous || deps.some((dep, i) => dep !== previous[i]);
  const react = {
    useState(initial) { const at = index++; if (!(at in slots)) slots[at] = initial; return [slots[at], next => { slots[at] = typeof next === 'function' ? next(slots[at]) : next; }]; },
    useCallback(fn, deps) { const at = index++; if (!slots[at] || changed(slots[at].deps, deps)) slots[at] = { fn, deps }; return slots[at].fn; },
    useEffect(fn, deps) { const at = index++; if (changed(slots[at], deps)) effects.push(() => { cleanups[at]?.(); cleanups[at] = fn(); }); slots[at] = deps; },
  };
  const jsx = (type, props) => ({ type, props });
  const module = { exports: {} };
  new Function('require', 'module', 'exports', 'setInterval', 'clearInterval', js)(name => {
    if (name === 'react') return react;
    if (name === 'react/jsx-runtime') return { jsx, jsxs: jsx };
    if (name === '@/lib/http') return { HttpError: Error };
    if (name === '@/lib/api/metadata-language') return api;
    throw new Error(name);
  }, module, module.exports, callback => { poll = callback; return 1; }, () => { poll = null; });
  const render = () => { index = 0; const tree = module.exports.MetadataLanguageSetting(); effects.splice(0).forEach(fn => fn()); return tree; };
  return { render, tick: async () => { assert.ok(poll, 'unfinished accepted retry must stay observable'); poll(); await settle(); } };
}
function nodes(tree) {
  if (Array.isArray(tree)) return tree.flatMap(nodes);
  if (!tree || typeof tree !== 'object') return [];
  return [tree, ...nodes(tree.props?.children)];
}
const settle = async () => { for (let i = 0; i < 10; i++) await Promise.resolve(); };
const retryButton = tree => nodes(tree).find(node => node.type === 'button' && node.props.children === '重试');
const languageState = state => ({ language: 'en-US', revision: 1, operations: [{ library_id: 'lib', operation_id: 'same-id', state, error: state === 'failed' ? 'retry fault' : null }] });

test('accepted retry followed immediately by another failure allows repeated retry', async () => {
  let attempts = 0;
  const fixture = await mountedLanguage({ getMetadataLanguage: async () => languageState('failed'), retryMetadataLanguageOperation: async op => { assert.equal(op.operation_id, 'same-id'); attempts++; } });
  fixture.render(); await settle();
  retryButton(fixture.render()).props.onClick(); await settle();
  let button = retryButton(fixture.render());
  assert.equal(button.props.disabled, false, 'a repeated durable failure must not strand Retry');
  button.props.onClick(); await settle();
  assert.equal(attempts, 2);
  assert.equal(retryButton(fixture.render()).props.disabled, false);
});

test('accepted retry remains observed when driver startup is delayed', async () => {
  let current = 'failed';
  const fixture = await mountedLanguage({ getMetadataLanguage: async () => languageState(current), retryMetadataLanguageOperation: async () => {} });
  fixture.render(); await settle();
  retryButton(fixture.render()).props.onClick(); await settle(); fixture.render();
  await fixture.tick(); fixture.render();
  current = 'applying'; await fixture.tick();
  assert.ok(nodes(fixture.render()).some(node => node.props?.role === 'status'));
  current = 'succeeded'; await fixture.tick();
  const tree = fixture.render();
  assert.equal(retryButton(tree), undefined);
  assert.ok(nodes(tree).some(node => node.type === 'span' && JSON.stringify(node.props.children).includes('已完成')));
});
