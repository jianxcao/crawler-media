const { test } = require('node:test');
const assert = require('node:assert/strict');
const { ReviewSession, LocalPlayback, hashLocalFile } = require('./annotation-review.js');
const hash = 'sha256:' + 'a'.repeat(64);
function inputs() {
  const cases = ['intro', 'outro'].map(kind => ({ case_id: kind, media_group: 'm1',
    split: 'development', source_hash: hash, duration_ms: 180000, kind }));
  return [{ corpus_version: 'test', evidence_kind: 'synthetic', cases },
    cases.map(c => ({ case_id: c.case_id, source_hash: hash, state: 'present',
      safe_intervals_ms: [[30000, 90000]], protected_intervals_ms: [[0, 30000], [90000, 180000]],
      start_range_ms: [30000, 31000], end_range_ms: [89000, 90000], reviewers: ['a', 'b'], adjudicated: true })),
    cases.map(c => ({ case_id: c.case_id, source_hash: hash, state: 'matched',
      interval_ms: [28467, 88800], reason: 'private-prediction', run_id: 'run-private' }))];
}
function ready(session) {
  session.verifySource(hash);
  session.confirmAudio('global stream index 1; single audio track externally verified');
  session.completeReview();
}
test('predictions remain inaccessible until an explicit completed review and reveal, per case', () => {
  const session = new ReviewSession(...inputs());
  assert.equal(session.view().prediction, null);
  assert.doesNotMatch(JSON.stringify(session.view()), /private-prediction|run-private|28467/);
  assert.throws(() => session.reveal(), /review/);
  assert.throws(() => session.completeReview(), /source/);
  session.verifySource(hash);
  assert.throws(() => session.completeReview(), /audio/);
  ready(session);
  assert.equal(session.view().prediction, null);
  session.reveal();
  assert.deepEqual(session.view().errors, { start_ms: -1533, end_ms: -200 });
  session.select('outro');
  assert.equal(session.view().prediction, null);
  assert.throws(() => session.reveal(), /review/);
});
test('missing labels are insufficient and never permit review completion or invented truth', () => {
  const [manifest, , predictions] = inputs();
  const session = new ReviewSession(manifest, [], predictions);
  session.verifySource(hash); session.confirmAudio('single audio verified');
  assert.equal(session.view().evidence, 'insufficient');
  assert.equal(session.view().label, null);
  assert.throws(() => session.completeReview(), /label/);
});
test('exact A1 fields, identities and interval constraints are enforced', () => {
  const mutations = [
    x => { x[0].cases[0].extra = 1; }, x => { x[1][0].old_start = 1; },
    x => { x[2][0].interval_ms = [-1, 200]; }, x => { x[1][0].source_hash = 'wrong'; },
    x => { x[1].pop(); }, x => { x[2].push(x[2][0]); },
    x => { x[1][0].safe_intervals_ms = [[0, 40000]]; },
    x => { x[0].cases[1].split = 'holdout'; },
    x => { x[2][0].state = 'error'; }, x => { x[1][0].reviewers = ['a', 'a']; },
    x => { x[0].acceptance_policy = { frozen: 'true' }; },
  ];
  for (const mutate of mutations) { const data = inputs(); mutate(data); assert.throws(() => new ReviewSession(...data)); }
  const data = inputs(); data[1][0].state = 'ambiguous'; data[1][0].safe_intervals_ms = [];
  data[1][0].start_range_ms = null; data[1][0].end_range_ms = null; data[1][0].adjudicated = false;
  const session = new ReviewSession(...data); ready(session); session.reveal();
  assert.equal(session.view().errors, null);
});
test('source mismatch and replacement invalidate review, without exposing hidden predictions', () => {
  const session = new ReviewSession(...inputs()); ready(session); session.reveal();
  assert.throws(() => session.verifySource('sha256:wrong'), /mismatch/);
  assert.equal(session.view().prediction, null);
  assert.equal(session.view().sourceVerified, false);
  session.verifySource(hash); assert.throws(() => session.completeReview(), /audio/);
});
test('views cannot mutate loaded truth and context bounds are original timeline ±10 seconds', () => {
  const session = new ReviewSession(...inputs());
  session.view().label.start_range_ms[0] = 0;
  assert.deepEqual(session.context('label', 'start'), { start_ms: 20000, end_ms: 41000 });
  assert.throws(() => session.context('prediction', 'start'), /hidden/);
  ready(session); session.reveal();
  assert.deepEqual(session.context('prediction', 'start'), { start_ms: 18467, end_ms: 38467 });
  const data = inputs(); data[1][0].start_range_ms = [0, 0];
  assert.equal(new ReviewSession(...data).context('label', 'start').start_ms, 0);
});
function media() {
  return { src: '', currentTime: 0, muted: true, volume: 0, paused: true,
    pause() { this.paused = true; }, load() {}, async play() { this.paused = false; },
    removeAttribute(name) { delete this[name]; } };
}
test('local playback preserves original time, audible audio, visible play errors, and URL lifetime', async () => {
  const video = media(); const live = new Set(); let id = 0;
  const urls = { createObjectURL() { const url = 'blob:' + ++id; live.add(url); return url; },
    revokeObjectURL(url) { live.delete(url); } };
  const playback = new LocalPlayback(video, urls);
  playback.replace({ name: 'one' }); const first = video.src;
  await playback.playContext({ start_ms: 18467, end_ms: 38467 });
  assert.equal(video.currentTime, 18.467); assert.equal(video.muted, false); assert.equal(video.volume, 1);
  video.currentTime = 38.467; playback.tick(); assert.equal(video.paused, true);
  playback.replace({ name: 'two' }); assert.equal(live.has(first), false); assert.equal(live.has(video.src), true);
  video.play = async () => { throw new Error('decoder unavailable'); };
  await assert.rejects(playback.playContext({ start_ms: 0, end_ms: 10000 }), /decoder unavailable/);
  playback.dispose(); assert.equal(live.size, 0); assert.equal(video.src, undefined);
});

test('browser hashing is bounded and computes the whole selected file', async () => {
  const file = new Blob(['abc']);
  assert.equal(await hashLocalFile(file, require('node:crypto').webcrypto),
    'sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad');
  await assert.rejects(hashLocalFile({ size: 129 * 1024 * 1024 }, require('node:crypto').webcrypto), /128 MiB/);
});

for (const replaceSource of [false, true]) test(`late rejected playback preserves newer context (replace=${replaceSource})`, async () => {
  const video = media(); const live = new Set(); let id = 0;
  const playback = new LocalPlayback(video, {
    createObjectURL() { const url = 'blob:' + ++id; live.add(url); return url; },
    revokeObjectURL(url) { live.delete(url); },
  });
  playback.replace({ name: 'first' });
  let rejectOld;
  video.play = () => new Promise((resolve, reject) => { rejectOld = reject; });
  const old = playback.playContext({ start_ms: 0, end_ms: 10000 }).catch(() => {});
  if (replaceSource) playback.replace({ name: 'second' });
  const currentSource = video.src;
  video.play = async () => { video.paused = false; };
  await playback.playContext({ start_ms: 20000, end_ms: 40000 });
  rejectOld(new DOMException('old request aborted by newer playback', 'AbortError'));
  await old;
  assert.equal(video.src, currentSource);
  assert.equal(live.has(currentSource), true);
  assert.equal(video.paused, false);
  video.currentTime = 40; playback.tick();
  assert.equal(video.paused, true, 'new context must still pause at its own end');
});

// The actual page script is exercised through DOM events; no source-text assertions.
function reviewPage() {
  const vm = require('node:vm');
  const elements = new Map();
  function element() {
    const listeners = new Map();
    return { ...media(), textContent: '', value: '', checked: false, disabled: false, hidden: false, files: [], duration: 180,
      addEventListener(event, listener) { if (!listeners.has(event)) listeners.set(event, []); listeners.get(event).push(listener); },
      emit(event) { return Promise.all((listeners.get(event) || []).map(listener => listener())); },
      getAttribute(name) { return this[name]; }, replaceChildren() {}, append() {},
    };
  }
  const get = id => { if (!elements.has(id)) elements.set(id, element()); return elements.get(id); };
  const live = new Set(); let id = 0;
  const context = vm.createContext({ MarkerReview: { ReviewSession, LocalPlayback, hashLocalFile },
    document: { getElementById: get, createElement: element }, window: element(),
  });
  // Use a real public LocalPlayback with only the browser's URL service replaced.
  context.MarkerReview.LocalPlayback = class extends LocalPlayback {
    constructor(video) { super(video, {
      createObjectURL() { const url = 'blob:' + ++id; live.add(url); return url; },
      revokeObjectURL(url) { live.delete(url); },
    }); }
  };
  vm.runInContext(require('node:fs').readFileSync(require('node:path').join(__dirname, 'annotation-review-page.js'), 'utf8'), context);
  return { get, live };
}
async function bindPage(page) {
  const data = inputs();
  for (const [index, name] of ['manifest', 'labels', 'predictions'].entries())
    page.get(name + '-file').files = [{ size: 1, async text() { return JSON.stringify(data[index]); } }];
  await page.get('load').emit('click'); page.get('cases').value = 'intro';
  // Large-file external attestation is a public supported path; no file hash is invented as computed.
  page.get('video-file').files = [{ name: 'large-local.mp4', size: 129 * 1024 * 1024 }];
  await page.get('video-file').emit('change');
  page.get('external-hash').value = hash; page.get('external-confirm').checked = true;
  await page.get('external-verify').emit('click'); await page.get('video').emit('loadedmetadata');
}
test('page ignores an old rejection delivered after a newer same-source context, but reports current rejection', async () => {
  const page = reviewPage(); await bindPage(page);
  const video = page.get('video'); const currentSource = video.src;
  let rejectOld;
  video.play = () => new Promise((resolve, reject) => { rejectOld = reject; });
  const old = page.get('label-start').emit('click');
  video.play = async () => { video.paused = false; };
  rejectOld(new DOMException('superseded context', 'AbortError'));
  // Core observes this rejection first; a new click then precedes the page's catch continuation.
  let next;
  queueMicrotask(() => { next = page.get('label-end').emit('click'); });
  await old; await next;
  assert.equal(video.src, currentSource, 'old page catch must not dispose current source');
  assert.equal(page.live.has(currentSource), true);
  assert.equal(page.get('label-end').disabled, false);
  assert.equal(page.get('error').textContent, '');
  video.currentTime = 100; await video.emit('timeupdate'); assert.equal(video.paused, true);
  video.play = async () => { throw new Error('current decoder failure'); };
  await page.get('label-start').emit('click');
  assert.match(page.get('error').textContent, /current decoder failure/);
  assert.equal(page.get('complete').disabled, true);
  assert.equal(page.get('label-start').disabled, true);
  assert.equal(video.src, undefined);
});
