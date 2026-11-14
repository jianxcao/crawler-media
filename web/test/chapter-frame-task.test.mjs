import test from 'node:test';
import assert from 'node:assert/strict';
import { applyFrameStatus, shouldPollFrameTask, createChapterFrameSession } from '../lib/chapter-frame-task.ts';

const task = (overrides = {}) => ({ task_id: 'a', generation: 'g', attempt: 1, state: 'running', total: 2, ready: 0, failed: 0, ...overrides });
const tick = () => new Promise((resolve) => setImmediate(resolve));
const deferred = () => { let resolve; const promise = new Promise((r) => { resolve = r; }); return { promise, resolve }; };
function clock() {
  const timers = new Map(); let id = 0;
  return { setTimeout(fn) { timers.set(++id, fn); return id; }, clearTimeout(key) { timers.delete(key); }, async fire() { const pending = [...timers.values()]; timers.clear(); pending.forEach((fn) => fn()); await tick(); }, pending: () => timers.size };
}
function fixture(loadStatus, loadChapters = async () => ['current']) {
  const time = clock(); const updates = { tasks: [], chapters: [], errors: [], busy: [] };
  const session = createChapterFrameSession({ loadStatus, loadChapters, onTask: (t) => updates.tasks.push(t), onChapters: (c) => updates.chapters.push(c), onError: (e) => updates.errors.push(e), onBusy: (b) => updates.busy.push(b), clock: time });
  return { session, time, updates };
}
test('ordinary status requires task identity and nondecreasing attempt', () => {
  const current = task({ attempt: 2 });
  for (const incoming of [task({ generation: 'old' }), task({ task_id: 'recurrent-a', attempt: 3 }), task({ attempt: 1 })]) assert.equal(applyFrameStatus(current, incoming), current);
  assert.equal(applyFrameStatus(current, task({ attempt: 2, state: 'completed' })).state, 'completed');
  for (const state of ['queued', 'running', 'completed', 'partial', 'failed', 'cancelled', 'superseded']) assert.equal(shouldPollFrameTask(task({ state })), ['queued', 'running'].includes(state));
  assert.equal(shouldPollFrameTask(null), false);
});
test('one flight polls through terminal reload, recovers status failure without claiming completion', async () => {
  const slow = deferred(); let reads = 0;
  const f = fixture(async () => { reads++; if (reads === 1) return { task: task(), empty: false, superseded: false }; if (reads === 2) return slow.promise; if (reads === 3) throw new Error('offline'); return { task: task({ state: 'partial', ready: 1, failed: 1 }), empty: false, superseded: false }; });
  await f.session.discover(); await f.time.fire(); await f.time.fire(); assert.equal(reads, 2);
  slow.resolve({ task: task({ ready: 1 }), empty: false, superseded: false }); await tick(); await f.time.fire();
  assert.equal(f.updates.tasks.at(-1).state, 'running'); assert.equal(f.updates.errors.at(-1), '状态暂不可用');
  await f.time.fire(); assert.equal(f.updates.tasks.at(-1).state, 'partial'); assert.equal(f.updates.chapters.length, 2); assert.equal(f.time.pending(), 0);
  f.session.stop();
});
test('explicit superseded adopts a recurrent generation with a new task identity', async () => {
  const expected = [];
  const f = fixture(async (identity) => { expected.push(identity); return expected.length === 1 ? { task: task(), empty: false, superseded: false } : { task: task({ task_id: 'new-a', state: 'completed', ready: 2 }), empty: false, superseded: true }; });
  await f.session.discover(); await f.time.fire();
  assert.deepEqual(expected[1], { task_id: 'a', generation: 'g' }); assert.equal(f.updates.tasks.at(-1).task_id, 'new-a'); assert.equal(f.updates.chapters.length, 2); assert.equal(f.time.pending(), 0);
});
test('selection stop and newer operations fence late status, chapter reload, errors and finally', async () => {
  const oldStatus = deferred(); const oldChapters = deferred(); let reads = 0; let chapters = 0;
  const f = fixture(async () => (++reads === 1 ? oldStatus.promise : { task: task({ task_id: 'new', state: 'completed' }), empty: false, superseded: false }), async () => (++chapters === 2 ? oldChapters.promise : ['new']));
  const old = f.session.discover(); await tick(); const newer = f.session.discover(); await tick();
  oldStatus.resolve({ task: task(), empty: false, superseded: false }); await old;
  oldChapters.resolve(['obsolete']); await newer;
  assert.equal(f.updates.tasks.at(-1).task_id, 'new'); assert.deepEqual(f.updates.chapters.at(-1), ['new']);
  const late = deferred(); const work = f.session.run(async (operation) => { await late.promise; if (operation.current()) throw new Error('obsolete'); });
  f.session.stop(); const snapshot = JSON.parse(JSON.stringify(f.updates)); late.resolve(); await work; assert.deepEqual(f.updates, snapshot); assert.equal(f.time.pending(), 0);
});
test('new generate aborts old poll and empty targets clear the tracked obligation', async () => {
  const late = deferred(); let reads = 0;
  const f = fixture(async () => ++reads === 1 ? { task: task(), empty: false, superseded: false } : late.promise);
  await f.session.discover(); await f.time.fire();
  await f.session.generate(async () => ({ task: null, empty: true }));
  late.resolve({ task: task({ ready: 1 }), empty: false, superseded: false }); await tick();
  assert.equal(f.updates.tasks.at(-1), null); assert.equal(f.time.pending(), 0);
});

test('late terminal chapter reload and old generate cannot overwrite a newer operation', async () => {
  const terminal = deferred(); let chapters = 0; let reads = 0;
  const f = fixture(async () => ({ task: task({ state: ++reads === 1 ? 'running' : 'completed' }), empty: false, superseded: false }), async () => ++chapters === 2 ? terminal.promise : ['new']);
  await f.session.discover(); await f.time.fire();
  await f.session.generate(async () => ({ task: task({ task_id: 'new', state: 'queued' }), empty: false }));
  terminal.resolve(['old terminal']); await tick();
  assert.equal(f.updates.tasks.at(-1).task_id, 'new'); assert.deepEqual(f.updates.chapters.at(-1), ['new']);
  const stale = deferred(); const old = f.session.generate(() => stale.promise);
  await f.session.generate(async () => ({ task: task({ task_id: 'latest', state: 'queued' }), empty: false }));
  stale.resolve({ task: task({ task_id: 'stale' }), empty: false }); await old;
  assert.equal(f.updates.tasks.at(-1).task_id, 'latest'); f.session.stop();
});

test('terminal chapter failure recovers with one read at a time and stops after publishing images', async () => {
  for (const state of ['completed', 'partial', 'failed']) {
    let statusReads = 0; let chapterReads = 0; const recovered = deferred();
    const f = fixture(async () => ({ task: task({ state: ++statusReads === 1 ? 'running' : state, ready: 1 }), empty: false, superseded: false }), async () => {
      chapterReads++;
      if (chapterReads === 1) return ['before'];
      if (chapterReads === 2) throw new Error('terminal chapter GET failed');
      return recovered.promise;
    });
    await f.session.discover(); await f.time.fire();
    assert.equal(f.updates.tasks.at(-1).state, state);
    assert.deepEqual(f.updates.chapters.at(-1), ['before']);
    await f.time.fire(); await f.time.fire();
    assert.equal(chapterReads, 3, 'the failed terminal chapter GET needs a read-only retry');
    assert.equal(statusReads, 2, 'a known terminal task does not need more status reads');
    recovered.resolve(['decoded image URL']); await tick();
    assert.deepEqual(f.updates.chapters.at(-1), ['decoded image URL']);
    assert.equal(f.updates.errors.at(-1), null); assert.equal(f.time.pending(), 0);
    f.session.stop();
  }
});

test('terminal generate reload retries without another POST and drops a late retry after selection stops', async () => {
  const late = deferred(); let posts = 0; let reads = 0;
  const f = fixture(async () => { throw new Error('terminal should not poll status'); }, async () => {
    if (++reads === 1) throw new Error('temporary chapter failure');
    return late.promise;
  });
  await f.session.generate(async () => { posts++; return { task: task({ state: 'completed', ready: 2 }), empty: false }; });
  await f.time.fire();
  assert.equal(reads, 2); assert.equal(posts, 1); assert.equal(f.updates.tasks.at(-1).state, 'completed');
  f.session.stop(); const snapshot = JSON.parse(JSON.stringify(f.updates));
  late.resolve(['old selected file']); await tick();
  assert.deepEqual(f.updates, snapshot); assert.equal(f.time.pending(), 0); assert.equal(posts, 1);
});

test('unavailable initial chapters do not starve active status before terminal chapter-only recovery', async () => {
  let statusReads = 0; let chapterReads = 0;
  const slowStatus = deferred();
  const f = fixture(async () => {
    statusReads++;
    if (statusReads === 1) return { task: task({ state: 'queued' }), empty: false, superseded: false };
    if (statusReads === 2) return slowStatus.promise;
    return { task: task({ state: 'completed', ready: 2 }), empty: false, superseded: false };
  }, async () => { chapterReads++; throw new Error('chapters remain unavailable'); });
  await f.session.discover();
  await f.time.fire(); await f.time.fire();
  assert.equal(statusReads, 2, 'active status must continue despite pending chapter reads');
  assert.equal(chapterReads, 1, 'a slow status request occupies the single flight');
  slowStatus.resolve({ task: task({ ready: 1 }), empty: false, superseded: false }); await tick();
  assert.equal(f.updates.tasks.at(-1).ready, 1);
  assert.equal(chapterReads, 2, 'chapter recovery follows the active status read');
  await f.time.fire();
  assert.equal(f.updates.tasks.at(-1).state, 'completed'); assert.equal(statusReads, 3);
  const terminalReads = chapterReads; await f.time.fire();
  assert.equal(chapterReads, terminalReads + 1); assert.equal(statusReads, 3, 'known terminal retries only chapters');
  f.session.stop(); assert.equal(f.time.pending(), 0);
});

test('active status adopts a superseding task while chapter GETs keep failing without a POST', async () => {
  const expected = []; let chapterReads = 0;
  const f = fixture(async (identity) => {
    expected.push(identity);
    if (expected.length === 1) return { task: task(), empty: false, superseded: false };
    if (expected.length === 2) return { task: task({ task_id: 'replacement', state: 'queued' }), empty: false, superseded: true };
    return { task: task({ task_id: 'replacement', state: 'partial', ready: 1, failed: 1 }), empty: false, superseded: false };
  }, async () => { chapterReads++; throw new Error('still offline'); });
  await f.session.discover(); await f.time.fire();
  assert.equal(f.updates.tasks.at(-1).task_id, 'replacement');
  assert.deepEqual(expected[1], { task_id: 'a', generation: 'g' });
  assert.equal(chapterReads, 2, 'superseded adoption tries chapters once in its round');
  await f.time.fire();
  assert.deepEqual(expected[2], { task_id: 'replacement', generation: 'g' });
  assert.equal(f.updates.tasks.at(-1).state, 'partial');
  await f.time.fire(); assert.equal(expected.length, 3);
  f.session.stop(); assert.equal(f.time.pending(), 0);
});
