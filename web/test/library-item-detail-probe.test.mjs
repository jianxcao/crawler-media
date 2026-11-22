import assert from 'node:assert/strict';
import { existsSync, readFileSync, statSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import React from 'react';
import ts from 'typescript';
import { createLibraryItemProbe } from '../lib/library-item-probe.ts';
import { createChapterFrameSession, shouldPollFrameTask } from '../lib/chapter-frame-task.ts';
import { isPlayableFile } from '../lib/library-file-pages.ts';
import { deferred, itemDetailHarness } from './support/library-item-detail-renderer.mjs';

const noop = () => undefined;
const tick = () => new Promise((resolve) => setImmediate(resolve));
function elements(tree) {
  if (Array.isArray(tree)) return tree.flatMap(elements);
  if (!tree || typeof tree !== 'object') return [];
  if (typeof tree.type === 'function' && tree.type !== noop) {
    try {
      const rendered = tree.type(tree.props);
      return rendered && typeof rendered === 'object'
        ? [tree, ...elements(rendered), ...elements(tree.props?.children)]
        : [tree, ...elements(tree.props?.children)];
    } catch {
      return [tree, ...elements(tree.props?.children)];
    }
  }
  return [tree, ...elements(tree.props?.children)];
}

test('item probe button submits without an episode selection after effect setup-cleanup-setup', async () => {
  const detail = { media_item_id: 'whole-item', kind: 'tv', title: 'Episodes', seasons: [1], cast: [], genres: [],
    files: [{ file_id: 'other-episode', units: [{ season: 1, episode: 2 }], presence: 'present', publication: 'published' }] };
  const library = { id: 'library', name: 'Episodes', kind: 'tv' };
  const submitted = []; let reads = 0; let state = 0; const effects = [];
  const feedback = { info: noop, success: noop, error: noop };
  const fallback = new Proxy({}, { get: (_target, key) => key === '__esModule' ? false : noop });
  const mocks = {
    react: { useState: (initial) => [state++ === 0 ? detail : state === 2 ? library : typeof initial === 'function' ? initial() : initial, noop],
      useMemo: (fn) => fn(), useCallback: (fn) => fn, useRef: (current) => ({ current }), useEffect: (effect) => effects.push(effect) },
    'react/jsx-runtime': { jsx: (type, props) => ({ type, props }), jsxs: (type, props) => ({ type, props }) },
    'react-router-dom': { Link: 'a', useNavigate: () => noop },
    '@/lib/library-item-probe': { createLibraryItemProbe },
    '@/lib/chapter-frame-task': { createChapterFrameSession, shouldPollFrameTask },
    '@/lib/library-file-pages': { isPlayableFile },
    '@/lib/api/libraries': { probeItem: async (libraryId, mediaId, signal) => { submitted.push({ libraryId, mediaId, aborted: signal.aborted }); return { queued: 2, already_running: false }; },
      getLibraryItemDetail: async () => { reads++; return detail; }, getLibrary: async () => library },
    '@/lib/api/playback': { fetchPlaybackMarks: async () => ({ is_favorite: false }) },
    '@/components/subscribe-entry': { useSubscribeEntry: () => ({ canSubscribe: false }) },
    '@/components/feedback': { useToast: () => feedback, useConfirm: () => noop },
    '@/lib/permissions': { usePermissions: () => ({ canManageLibraries: true }) },
    '@/lib/backdrop': { useBackdrop: () => ({ setOverrideBackdrop: noop }) },
    '@/lib/ui-prefs': { useTheme: () => ({ id: 'default' }) },
    '@/lib/use-media-query': { useIsMobile: () => false },
    '@/lib/image-proxy': { cachedImageUrl: () => '', imageUrl: () => '' },
  };
  const entryUrl = new URL('../components/library-item-detail-view.tsx', import.meta.url);
  const real = new Set(['@/lib/library-item-format', '@/lib/format']);
  const modules = new Map();
  function resolveFile(baseUrl, specifier) {
    const raw = specifier.startsWith('@/')
      ? new URL(`../${specifier.slice(2)}`, import.meta.url)
      : new URL(specifier, baseUrl);
    for (const suffix of ['', '.tsx', '.ts', '/index.tsx', '/index.ts']) {
      const candidate = new URL(`${raw.href}${suffix}`);
      if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
    }
    throw new Error(`Cannot resolve module ${specifier} from ${baseUrl.href}`);
  }
  function loadLocalModule(specifier, fromUrl = entryUrl) {
    const targetUrl = resolveFile(fromUrl, specifier);
    if (modules.has(targetUrl.href)) return modules.get(targetUrl.href).exports;
    const src = readFileSync(targetUrl, 'utf8');
    const js = ts.transpileModule(src, {
      compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 },
    }).outputText;
    const mod = { exports: {} };
    modules.set(targetUrl.href, mod);
    new Function('require', 'module', 'exports', js)((dependency) => {
      if (mocks[dependency]) return mocks[dependency];
      if (dependency.startsWith('./') || dependency.startsWith('../') || real.has(dependency)) {
        return loadLocalModule(dependency, targetUrl);
      }
      return fallback;
    }, mod, mod.exports);
    return mod.exports;
  }
  const source = await readFile(entryUrl, 'utf8');
  const code = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 } }).outputText;
  const module = { exports: {} };
  new Function('require', 'module', 'exports', code)((dependency) => {
    if (mocks[dependency]) return mocks[dependency];
    if (dependency.startsWith('./') || dependency.startsWith('../') || real.has(dependency)) {
      return loadLocalModule(dependency, entryUrl);
    }
    return fallback;
  }, module, module.exports);
  const tree = module.exports.LibraryItemDetailView({ libraryId: 'library', mediaItemId: 'whole-item' });
  const cleanups = effects.map((effect) => effect());
  cleanups.forEach((cleanup) => { if (typeof cleanup === 'function') cleanup(); });
  effects.forEach((effect) => effect()); await tick();
  const initialReads = reads;
  const buttons = elements(tree).filter((element) => element.type === 'button');
  const generate = buttons.find((button) => button.props.children === '生成场景图');
  const probe = buttons.find((button) => button.props.children === '重新探测媒体信息');
  assert.equal(generate.props.disabled, true, 'TV episode selection has no chapter target yet');
  assert.equal(probe.props.disabled, false);
  probe.props.onClick(); await tick();
  assert.deepEqual(submitted, [{ libraryId: 'library', mediaId: 'whole-item', aborted: false }]);
  assert.equal(reads, initialReads + 1, 'probe completion refreshes item facts without a chapter session');
});

test('non-admin hides probe/chapter action buttons and does not poll probe-status', async () => {
  const view = itemDetailHarness({ tv: true, canManageLibraries: false });
  await view.mount();
  try {
    const labels = view.root().findAllByType('button').map((b) => String(b.props.children ?? ''));
    for (const adminLabel of ['重新获取片头/章节', '生成场景图', '重新探测媒体信息']) {
      assert.ok(!labels.includes(adminLabel), `non-admin must not see button: ${adminLabel}`);
    }
    assert.equal(view.probeStatusCalls.length, 0, 'non-admin must not request admin probe-status');
  } finally {
    await view.unmount();
  }
});

test('admin single-file delete opens confirmation modal and deletes ledger row with deleteFile=true', async () => {
  const view = itemDetailHarness({ tv: false, canManageLibraries: true });
  await view.mount();
  try {
    const deleteBtns = view.root().findAllByType('button').filter((b) => b.props['aria-label'] === '删除此文件');
    assert.ok(deleteBtns.length > 0, 'admin should see per-file delete button');
    await React.act(async () => {
      deleteBtns[0].props.onClick();
    });
    const checkbox = view.root().findAllByType('input').find((i) => i.props.type === 'checkbox');
    assert.ok(checkbox, 'DeleteFileDialog must render a confirmation checkbox');
    await React.act(async () => {
      checkbox.props.onChange({ target: { checked: true } });
    });
    const confirmBtn = view.root().findAllByType('button').find((b) => String(b.children?.join?.('') ?? b.props.children ?? '').includes('删除此文件') && b.props.type === 'button' && !b.props['aria-label']);
    assert.ok(confirmBtn, 'DeleteFileDialog must render confirm delete button');
    await React.act(async () => {
      await confirmBtn.props.onClick();
    });
    assert.deepEqual(view.deletedLedgerRows, [{ id: 'file-a', deleteFile: true }]);
  } finally {
    await view.unmount();
  }
});

test('stale toggleFavorite completion after changing mediaItemId does not overwrite new item state', async () => {
  const slowMark = deferred();
  const view = itemDetailHarness({
    tv: false,
    mark: (body) => (body.media_item_id === 'media-1' ? slowMark.promise : Promise.resolve()),
  });
  await view.mount('media-1');
  try {
    const pendingClick = view.markFavorite();
    await view.remount('media-2');
    assert.equal(view.favoriteDisabled(), false, 'navigating to new item must reset marking lock');
    await view.resolve(slowMark);
    await pendingClick;
    assert.equal(view.favoritePressed(), false, 'stale rejection/completion on old item must not flip new item favorite');
  } finally {
    await view.unmount();
  }
});
