/* global Event, EventTarget */
import assert from "node:assert/strict";
import test from "node:test";
import { bindPlaybackPageLifecycle } from "../lib/player/page-lifecycle.ts";
import { initialPlayerState, playerReducer } from "../lib/player/machine.ts";

function page(target, type, persisted = true) {
  const event = new Event(type);
  Object.defineProperty(event, "persisted", { value: persisted });
  target.dispatchEvent(event);
}
function fixture(overrides = {}) {
  const target = new EventTarget();
  const reports = [];
  let state = { ...initialPlayerState, phase: "playing", startMs: 42_000, attempt: 2 };
  let snapshot = {
    unitKey: "media:1:2", playSessionId: "old-session", phase: "playing",
    positionMs: 42_000, wantsPlay: true, announced: true, endedByAdmin: false, ...overrides,
  };
  const dispose = bindPlaybackPageLifecycle(target, {
    snapshot: () => snapshot,
    suspend: saved => reports.push({ event: "progress", paused: true, play_session_id: saved.playSessionId, position_ms: saved.positionMs }),
    restore: saved => { state = playerReducer(state, { type: "restart", startMs: saved.positionMs }); reports.push({ event: "restore", saved }); },
    stop: saved => reports.push({ event: "stop", play_session_id: saved.playSessionId }),
  });
  return { target, reports, dispose, state: () => state, change: patch => { snapshot = { ...snapshot, ...patch }; } };
}

test("BFCache preserves progress and restarts once at the saved position", () => {
  const f = fixture();
  page(f.target, "pageshow");
  assert.deepEqual(f.reports, []);
  page(f.target, "pagehide");
  assert.deepEqual(f.reports, [{ event: "progress", paused: true, play_session_id: "old-session", position_ms: 42_000 }]);
  page(f.target, "pageshow");
  assert.equal(f.state().phase, "session-starting");
  assert.equal(f.state().startMs, 42_000);
  assert.equal(f.state().attempt, 3);
  assert.equal(f.state().session, null);
  page(f.target, "pageshow");
  assert.equal(f.reports.length, 2);
  f.dispose();
});

test("paused intent and pending first grants survive a cache trip", () => {
  for (const overrides of [{ wantsPlay: false }, { phase: "session-starting", playSessionId: null, announced: false }]) {
    const f = fixture(overrides);
    page(f.target, "pagehide");
    page(f.target, "pageshow");
    assert.equal(f.reports.at(-1).event, "restore");
    assert.equal(f.reports.at(-1).saved.wantsPlay, overrides.wantsPlay ?? true);
    f.dispose();
  }
});

test("changed identities and terminal pages cannot resume a cached session", () => {
  for (const change of [
    { unitKey: "media:1:3" }, { playSessionId: "new-session" }, { endedByAdmin: true },
    ...["error", "consent", "ended", "idle"].map(phase => ({ phase })),
  ]) {
    const f = fixture();
    page(f.target, "pagehide");
    f.change(change);
    page(f.target, "pageshow");
    assert.equal(f.reports.length, 1, JSON.stringify(change));
    f.dispose();
  }
});

test("ordinary leave stops once and dispose revokes the stored snapshot", () => {
  const f = fixture();
  page(f.target, "pagehide", false);
  page(f.target, "pageshow");
  assert.deepEqual(f.reports, [{ event: "stop", play_session_id: "old-session" }]);
  page(f.target, "pagehide");
  f.dispose();
  page(f.target, "pageshow");
  assert.equal(f.reports.length, 2);
});

test("two cache round trips consume the identity current for each generation", () => {
  const f = fixture();
  page(f.target, "pagehide"); page(f.target, "pageshow");
  f.change({ playSessionId: "next-session", positionMs: 55_000 });
  page(f.target, "pagehide"); page(f.target, "pageshow");
  assert.equal(f.reports.at(-1).saved.playSessionId, "next-session");
  assert.equal(f.state().startMs, 55_000);
  assert.equal(f.state().attempt, 4);
  f.dispose();
});

test("in-flight renewal promoted while cached restores with the successor identity", () => {
  const f = fixture({ phase: "session-starting", playSessionId: "sid-1", announced: false });
  page(f.target, "pagehide");
  f.change({ playSessionId: "sid-2", renewedFromPlaySessionId: "sid-1" });
  page(f.target, "pageshow");
  assert.equal(f.reports.at(-1).event, "restore");
  assert.equal(f.reports.at(-1).saved.playSessionId, "sid-2");
  f.dispose();
});
