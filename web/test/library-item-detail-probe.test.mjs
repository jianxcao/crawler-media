import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import ts from 'typescript';
import { createLibraryItemProbe } from '../lib/library-item-probe.ts';
import { createChapterFrameSession, shouldPollFrameTask } from '../lib/chapter-frame-task.ts';
import { isPlayableFile } from '../lib/library-file-pages.ts';

const noop = () => undefined;
const tick = () => new Promise((resolve) => setImmediate(resolve));
function elements(tree) {
  if (Array.isArray(tree)) return tree.flatMap(elements);
  if (!tree || typeof tree !== 'object') return [];
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
    '@/lib/image-proxy': { cachedImageUrl: () => '' },
  };
  const source = await readFile(new URL('../components/library-item-detail-view.tsx', import.meta.url), 'utf8');
  const code = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 } }).outputText;
  const module = { exports: {} };
  new Function('require', 'module', 'exports', code)((dependency) => mocks[dependency] ?? fallback, module, module.exports);
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
