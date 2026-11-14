import test from 'node:test';
import assert from 'node:assert/strict';
import { createLibraryItemProbe } from '../lib/library-item-probe.ts';

const deferred = () => { let resolve; let reject; const promise = new Promise((r, j) => { resolve = r; reject = j; }); return { promise, resolve, reject }; };
const tick = () => new Promise((resolve) => setImmediate(resolve));
function fixture(overrides = {}) {
  const visible = { busy: [], results: [], errors: [], reloads: 0 };
  const probe = createLibraryItemProbe({
    request: async () => ({ queued: 2, already_running: false }),
    reload: async () => { visible.reloads++; },
    chapterSession: () => null,
    pageCurrent: () => true,
    onBusy: (busy) => visible.busy.push(busy),
    onResult: (result) => visible.results.push(result),
    onError: (error) => visible.errors.push(error),
    ...overrides,
  });
  return { probe, visible };
}

test('whole-item probe runs without a selected playable chapter file', async () => {
  const submitted = [];
  const f = fixture({ request: async (signal) => { submitted.push({ item: 'whole item', aborted: signal.aborted }); return { queued: 2, already_running: false }; } });
  await f.probe.run();
  assert.deepEqual(submitted, [{ item: 'whole item', aborted: false }]);
  assert.equal(f.visible.reloads, 1); assert.equal(f.visible.results[0].queued, 2);
  assert.deepEqual(f.visible.busy, [true, false]);
});

test('probe response cannot rediscover a chapter selection chosen after submission', async () => {
  const pending = deferred(); let session = null; let discoveries = 0;
  const f = fixture({ request: () => pending.promise, chapterSession: () => session });
  const work = f.probe.run();
  session = { version: () => 1, active: () => true, discover: async () => { discoveries++; } };
  pending.resolve({ queued: 2, already_running: false }); await work;
  assert.equal(discoveries, 0); assert.equal(f.visible.reloads, 1);
});

test('whole-item completion rediscovers only the still-current chapter operation', async () => {
  let version = 1; let discoveries = 0; let pending = deferred();
  const session = { version: () => version, active: (expected) => expected === version, discover: async () => { discoveries++; } };
  const f = fixture({ request: () => pending.promise, chapterSession: () => session });
  const stale = f.probe.run(); version++;
  pending.resolve({ queued: 2, already_running: false }); await stale;
  assert.equal(discoveries, 0);
  pending = deferred(); const current = f.probe.run();
  pending.resolve({ queued: 2, already_running: false }); await current;
  assert.equal(discoveries, 1);
});

test('new probe operations and page disposal fence late result, error, reload and finally', async () => {
  const old = deferred(); let requests = 0; let oldSignal;
  const f = fixture({ request: (signal) => { if (++requests === 1) { oldSignal = signal; return old.promise; } return Promise.resolve({ queued: 3, already_running: false }); } });
  const stale = f.probe.run(); await f.probe.run();
  assert.equal(oldSignal.aborted, true);
  const afterNew = JSON.parse(JSON.stringify(f.visible)); old.resolve({ queued: 99, already_running: false }); await stale;
  assert.deepEqual(f.visible, afterNew);
  const reload = deferred(); const stopped = fixture({ reload: () => reload.promise });
  const work = stopped.probe.run(); await tick(); stopped.probe.stop();
  const snapshot = JSON.parse(JSON.stringify(stopped.visible)); reload.reject(new Error('late reload failure')); await work;
  assert.deepEqual(stopped.visible, snapshot);
});

test('a changed page blocks late feedback even before disposal cleanup', async () => {
  const pending = deferred(); let currentPage = true;
  const f = fixture({ request: () => pending.promise, pageCurrent: () => currentPage });
  const work = f.probe.run(); currentPage = false;
  const snapshot = JSON.parse(JSON.stringify(f.visible)); pending.reject(new Error('old item failed')); await work;
  assert.deepEqual(f.visible, snapshot); assert.equal(f.visible.reloads, 0);
});
