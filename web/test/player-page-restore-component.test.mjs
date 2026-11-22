/* global Event */
import assert from "node:assert/strict";
import test from "node:test";
import { mountPlayer } from "./support/player-renderer.mjs";

test("production component renews each cached grant with saved position and a fresh event sequence", async () => {
  const ui = await mountPlayer();
  try {
    await ui.seek(42_000);
    await ui.event("pagehide");
    assert.equal(ui.reports.at(-1).event, "progress");
    assert.equal(ui.reports.at(-1).paused, true);
    assert.equal(ui.reports.at(-1).position_ms, 42_000);
    assert.equal(ui.video.paused, true);
    await ui.event("pageshow");
    assert.equal(ui.calls.length, 2);
    assert.equal(ui.calls[1].resume_from_play_session_id, "sid-1");
    assert.equal(ui.calls[1].start_ms, 42_000);
    assert.equal(ui.calls[1].file_id, "file-1");
    assert.equal(ui.engines.at(-1).options.streamUrl, "/stream/sid-2");
    assert.equal(ui.reports.find(report => report.play_session_id === "sid-2").event_sequence, 1);
    await ui.event("pagehide"); await ui.event("pageshow");
    assert.equal(ui.calls.at(-1).resume_from_play_session_id, "sid-2");
    await ui.event("pagehide", false);
    assert.equal(ui.reports.at(-1).event, "stop");
    const stopped = ui.reports.filter(report => report.event === "stop").length;
    assert.equal(ui.metrics.length, 1);
    await ui.unmount();
    assert.equal(ui.reports.filter(report => report.event === "stop").length, stopped);
    assert.equal(ui.metrics.length, 1);
  } catch (error) { await ui.unmount(); throw error; }
});

test("a paused renewal exposes its play control only when ready and starts on the user's click", async () => {
  const ui = await mountPlayer();
  try {
    await ui.pause();
    await ui.event("pagehide"); await ui.event("pageshow");
    assert.equal(ui.video.paused, true);
    assert.equal(ui.calls[1].resume_from_play_session_id, "sid-1");
    assert.equal(ui.reports.filter(report => report.play_session_id === "sid-2").length, 0);
    await ui.flush(() => {
      ui.video.readyState = 1;
      ui.video.dispatchEvent(new Event("loadedmetadata"));
    });
    assert.equal(ui.renderer.root.findByType("PlayerCenterControls").props.visible, false);
    await ui.flush(() => {
      ui.video.readyState = 3;
      ui.video.dispatchEvent(new Event("canplay"));
    });
    const controls = ui.renderer.root.findByType("PlayerCenterControls");
    assert.equal(controls.props.paused, true);
    assert.equal(controls.props.visible, true);
    assert.equal(ui.reports.filter(report => report.play_session_id === "sid-2").length, 0);
    await ui.flush(() => controls.props.onTogglePlay());
    assert.equal(ui.video.paused, false);
    const started = ui.reports.find(report => report.play_session_id === "sid-2");
    assert.equal(started.event, "start");
    assert.equal(started.event_sequence, 1);
  } finally { await ui.unmount(); }
});

test("a ready paused renewal can make another cache trip before the user plays", async () => {
  const ui = await mountPlayer();
  try {
    await ui.pause();
    for (const predecessor of ["sid-1", "sid-2"]) {
      await ui.event("pagehide"); await ui.event("pageshow");
      assert.equal(ui.calls.at(-1).resume_from_play_session_id, predecessor);
      await ui.flush(() => {
        ui.video.readyState = 3;
        ui.video.dispatchEvent(new Event("canplay"));
      });
      assert.equal(ui.video.paused, true);
      assert.equal(ui.renderer.root.findByType("PlayerCenterControls").props.visible, true);
    }
    assert.equal(ui.calls.length, 3);
    assert.equal(ui.reports.some(report => report.play_session_id !== "sid-1"), false);
  } finally { await ui.unmount(); }
});

test("a grant arriving while cached is discarded and the pending user request restarts", async () => {
  let finish;
  const pending = new Promise(resolve => { finish = resolve; });
  let ui;
  ui = await mountPlayer({ startMsOverride: 42_000, grant: (_body, attempt) => attempt === 1 ? pending : ui.session("fresh", 42_000) });
  try {
    await ui.event("pagehide");
    await ui.flush(() => finish(ui.session("stale", 0)));
    assert.equal(ui.engines.length, 0);
    await ui.event("pageshow");
    assert.equal(ui.calls.length, 2);
    assert.equal(ui.calls[1].resume_from_play_session_id, undefined);
    assert.equal(ui.calls[1].start_ms, 42_000);
    assert.equal(ui.engines.at(-1).options.streamUrl, "/stream/fresh");
    assert.equal(ui.reports[0].play_session_id, "fresh");
  } finally { await ui.unmount(); }
});

test("late progress replies cannot end the renewed identity", async () => {
  let finish;
  let delay = false;
  const ui = await mountPlayer({ progress: body => {
    if (delay && body.event === "progress") return new Promise(resolve => { finish = resolve; });
    return {};
  } });
  try {
    delay = true;
    await ui.pause();
    delay = false;
    await ui.event("pagehide"); await ui.event("pageshow");
    await ui.flush(() => finish({ ended_by_admin: true }));
    assert.equal(ui.renderer.root.findAllByProps({ role: "alert" }).length, 0);
    await ui.play();
    assert.ok(ui.reports.some(report => report.play_session_id === "sid-2" && report.event === "start"));
  } finally { await ui.unmount(); }
});

test("structured administrator errors use the existing ended message", async () => {
  let ui;
  const failure = Object.assign(new Error("server message"), { details: { ok: false, error: { code: "playback.ended_by_admin" } } });
  ui = await mountPlayer({ grant: (body, attempt) => attempt === 1 ? {
    play_session_id: "old", session_id: null, stream_url: "/old", start_ms: 0,
    decision: { outcome: "plan", tier: 0, container: "mp4", video: {}, audio: {}, subtitles: [] }, subtitle_urls: [],
  } : Promise.reject(failure) });
  try {
    await ui.event("pagehide"); await ui.event("pageshow");
    assert.match(JSON.stringify(ui.renderer.toJSON()), /管理员已结束本次播放/);
  } finally { await ui.unmount(); }
});

test("local progress scopes reuse their sharing entry without member renewal parameters", async () => {
  const ui = await mountPlayer({ scope: { base: "/shared-playback", progress: "local", telemetry: false } });
  try {
    await ui.seek(15_000);
    await ui.event("pagehide"); await ui.event("pageshow");
    assert.equal(ui.calls[1].start_ms, 15_000);
    assert.equal(ui.calls[1].resume_from_play_session_id, undefined);
  } finally { await ui.unmount(); }
});

test("manual retry clears an administrator terminal state and starts a new renewal chain", async () => {
  let ui;
  ui = await mountPlayer({ grant: (_body, attempt) => {
    if (attempt === 2) throw Object.assign(new Error("ended"), { details: { error: { code: "playback.ended_by_admin" } } });
    return {
      play_session_id: `sid-${attempt}`, session_id: null, stream_url: `/stream/sid-${attempt}`, start_ms: 0,
      decision: { outcome: "plan", tier: 0, container: "mp4", video: {}, audio: {}, subtitles: [] }, subtitle_urls: [],
    };
  } });
  try {
    await ui.event("pagehide"); await ui.event("pageshow");
    await ui.flush(() => ui.renderer.root.findAllByType("button").find(node => node.children.join("") === "重试").props.onClick());
    assert.equal(ui.calls[2].resume_from_play_session_id, undefined);
    await ui.event("pagehide"); await ui.event("pageshow");
    assert.equal(ui.calls.length, 4);
    assert.equal(ui.calls[3].resume_from_play_session_id, "sid-3");
  } finally { await ui.unmount(); }
});

test("a cached error page keeps its manual retry usable without auto renewal", async () => {
  const ui = await mountPlayer({ grant: (_body, attempt) => {
    if (attempt === 1) throw new Error("offline");
    return { play_session_id: "retry", stream_url: "/retry", start_ms: 0, decision: { outcome: "plan", tier: 0, container: "mp4", video: {}, audio: {}, subtitles: [] }, subtitle_urls: [] };
  } });
  try {
    await ui.event("pagehide"); await ui.event("pageshow");
    assert.equal(ui.calls.length, 1);
    await ui.flush(() => ui.renderer.root.findAllByType("button").find(node => node.children.join("") === "重试").props.onClick());
    assert.equal(ui.calls.length, 2);
  } finally { await ui.unmount(); }
});


test("SPA navigation disposes the old cache snapshot and stops the original SID once", async () => {
  const ui = await mountPlayer();
  try {
    await ui.event("pagehide");
    await ui.navigate();
    assert.equal(ui.reports.filter(report => report.event === "stop" && report.play_session_id === "sid-1").length, 1);
    assert.equal(ui.calls.length, 2);
    assert.equal(ui.calls[1].media_item_id, "next-media");
    await ui.event("pageshow");
    assert.equal(ui.calls.length, 2);
    assert.equal(ui.calls[1].resume_from_play_session_id, undefined);
  } finally { await ui.unmount(); }
});


test("renewal preserves selected tracks until source_changed clears obsolete tracks", async () => {
  const ui = await mountPlayer({ grant: (body, attempt) => ({
    play_session_id: `sid-${attempt}`, session_id: null, stream_url: `/stream/sid-${attempt}`, start_ms: attempt === 1 ? 0 : 12_000,
    resume_status: attempt === 1 ? "compatible" : "source_changed",
    watch: { position_ms: 12_000, duration_ms: 20_000, audio_track: null, subtitle_track: null },
    decision: { outcome: "plan", tier: 0, container: "mp4", video: {}, audio: { track_ref: "audio-1" }, subtitles: [] }, subtitle_urls: [],
  }) });
  try {
    await ui.flush(() => ui.renderer.root.findByType("PlayerControls").props.onSelectSubtitle("old-track"));
    await ui.seek(42_000);
    await ui.event("pagehide"); await ui.event("pageshow");
    assert.equal(ui.calls[1].audio_track, "audio-1");
    assert.equal(ui.calls[1].subtitle_track, "old-track");
    const controls = ui.renderer.root.findByType("PlayerControls");
    assert.equal(controls.props.selectedSubtitle, null);
    assert.equal(controls.props.positionMs, 12_000);
    assert.equal(controls.props.durationMs, 20_000);
  } finally { await ui.unmount(); }
});

for (const [code, message] of [
  ["playback.ended_by_admin", "管理员已结束本次播放"],
  ["playback.resume_superseded", "newer playback selected"],
]) {
  test(`a second cache trip with an aborted renewal retains predecessor authorization for ${code}`, async () => {
    let fail;
    const pending = new Promise((_resolve, reject) => { fail = reject; });
    const ui = await mountPlayer({ grant: (body, attempt) => {
      if (attempt === 1) return {
        play_session_id: "original", session_id: null, stream_url: "/original", start_ms: 0,
        decision: { outcome: "plan", tier: 0, container: "mp4", video: {}, audio: { track_ref: "audio-1" }, subtitles: [] }, subtitle_urls: [],
      };
      if (attempt === 2) return pending;
      if (body.resume_from_play_session_id === "original") {
        throw Object.assign(new Error(message), { status: 409, details: { error: { code } } });
      }
      return {
        play_session_id: "unauthorized", session_id: null, stream_url: "/unauthorized", start_ms: 0,
        decision: { outcome: "plan", tier: 0, container: "mp4", video: {}, audio: {}, subtitles: [] }, subtitle_urls: [],
      };
    } });
    try {
      await ui.seek(42_000);
      await ui.event("pagehide"); await ui.event("pageshow");
      assert.equal(ui.calls[1].resume_from_play_session_id, "original");
      await ui.event("pagehide");
      await ui.flush(() => fail(new Error("aborted while cached")));
      await ui.event("pageshow");
      assert.equal(ui.calls.length, 3);
      assert.equal(ui.calls[2].resume_from_play_session_id, "original");
      assert.equal(ui.calls[2].start_ms, 42_000);
      assert.equal(ui.calls[2].audio_track, "audio-1");
      assert.match(JSON.stringify(ui.renderer.toJSON()), new RegExp(message));
      assert.equal(ui.engines.length, 1, "a rejected renewal must not attach another stream");
      assert.equal(ui.reports.some(report => report.play_session_id === "unauthorized"), false);
    } finally { await ui.unmount(); }
  });
}

function createGenerationalServer() {
  let nextId = 0;
  let latestGen = 0;
  let endedByAdmin = false;
  const sessions = new Map();
  const mint = (startMs) => {
    const sid = `sid-${++nextId}`;
    latestGen += 1;
    sessions.set(sid, { gen: latestGen });
    return {
      play_session_id: sid, session_id: null, stream_url: `/stream/${sid}`, start_ms: startMs ?? 0,
      decision: {
        outcome: "plan", tier: 0, file_id: "file-1", container: "mp4",
        video: { action: "copy" }, audio: { action: "copy", track_ref: "audio-1" },
        subtitles: [], audio_tracks: [],
      },
      subtitle_urls: [], watch: { position_ms: startMs ?? 0, duration_ms: 1000_000 }, source: null,
    };
  };
  return {
    issue(body) {
      if (body.resume_from_play_session_id) {
        if (endedByAdmin) {
          throw Object.assign(new Error("管理员已结束本次播放"), {
            status: 409, details: { error: { code: "playback.ended_by_admin" } },
          });
        }
        const prev = sessions.get(body.resume_from_play_session_id);
        if (!prev || prev.gen < latestGen) {
          throw Object.assign(new Error("newer playback selected"), {
            status: 409, details: { error: { code: "playback.resume_superseded" } },
          });
        }
      }
      return mint(body.start_ms);
    },
    endByAdmin() { endedByAdmin = true; },
    takeoverFromOtherPage() { latestGen += 1; },
  };
}

test("consecutive BFCache round trips promote an in-flight renewal so the second restore resumes from B", async () => {
  const server = createGenerationalServer();
  let deliverB;
  const pendingB = new Promise(resolve => { deliverB = resolve; });
  let mintedB;
  const ui = await mountPlayer({
    grant: (body, attempt) => {
      if (attempt === 2) {
        mintedB = server.issue(body);
        return pendingB;
      }
      return server.issue(body);
    },
  });
  try {
    await ui.seek(42_000);
    await ui.event("pagehide"); await ui.event("pageshow");
    assert.equal(ui.calls[1].resume_from_play_session_id, "sid-1");
    assert.equal(mintedB.play_session_id, "sid-2");
    await ui.event("pagehide");
    await ui.flush(() => deliverB(mintedB));
    assert.equal(ui.engines.length, 1, "cached renewal must not attach a stream while suspended");
    assert.equal(ui.reports.some(report => report.play_session_id === "sid-2"), false);
    await ui.event("pageshow");
    assert.equal(ui.calls.length, 3);
    assert.equal(ui.calls[2].resume_from_play_session_id, "sid-2");
    assert.equal(ui.calls[2].start_ms, 42_000);
    assert.equal(ui.calls[2].audio_track, "audio-1");
    assert.equal(ui.engines.at(-1).options.streamUrl, "/stream/sid-3");
    const started = ui.reports.find(report => report.play_session_id === "sid-3");
    assert.ok(started, "restored session must report progress under the current SID");
    assert.equal(started.event_sequence, 1);
  } finally { await ui.unmount(); }
});

for (const [label, trigger, expectedMessage] of [
  ["admin termination", server => server.endByAdmin(), /管理员已结束本次播放/],
  ["takeover by another page", server => server.takeoverFromOtherPage(), /newer playback selected/],
]) {
  test(`promoted in-flight renewal still fails cleanly after ${label} while cached`, async () => {
    const server = createGenerationalServer();
    let deliverB;
    const pendingB = new Promise(resolve => { deliverB = resolve; });
    let mintedB;
    const ui = await mountPlayer({
      grant: (body, attempt) => {
        if (attempt === 2) {
          mintedB = server.issue(body);
          return pendingB;
        }
        return server.issue(body);
      },
    });
    try {
      await ui.seek(42_000);
      await ui.event("pagehide"); await ui.event("pageshow");
      await ui.event("pagehide");
      await ui.flush(() => deliverB(mintedB));
      trigger(server);
      await ui.event("pageshow");
      assert.equal(ui.calls.length, 3);
      assert.equal(ui.calls[2].resume_from_play_session_id, "sid-2");
      assert.match(JSON.stringify(ui.renderer.toJSON()), expectedMessage);
      assert.equal(ui.engines.length, 1);
      assert.equal(ui.reports.some(report => report.play_session_id === "sid-3"), false);
    } finally { await ui.unmount(); }
  });
}

test("late renewal cannot be claimed after switching episodes or changing device identity", async () => {
  for (const mode of ["navigate", "device-change"]) {
    const server = createGenerationalServer();
    let deliverB;
    const pendingB = new Promise(resolve => { deliverB = resolve; });
    let mintedB;
    const ui = await mountPlayer({
      grant: (body, attempt) => {
        if (attempt === 2) {
          mintedB = server.issue(body);
          return pendingB;
        }
        return server.issue(body);
      },
    });
    try {
      await ui.event("pagehide"); await ui.event("pageshow");
      await ui.event("pagehide");
      if (mode === "navigate") {
        await ui.navigate();
        await ui.flush(() => deliverB(mintedB));
        assert.equal(ui.calls[2].media_item_id, "next-media");
        assert.equal(ui.calls[2].resume_from_play_session_id, undefined);
      } else {
        ui.setDeviceId("device-2");
        await ui.flush(() => deliverB(mintedB));
        await ui.event("pageshow");
        assert.equal(ui.calls.length, 3);
        assert.equal(ui.calls[2].resume_from_play_session_id, "sid-1", "a changed device must not claim the old device's late renewal");
        assert.match(JSON.stringify(ui.renderer.toJSON()), /newer playback selected/);
        assert.equal(ui.engines.length, 1);
      }
    } finally { await ui.unmount(); }
  }
});

test("consecutive BFCache round trips await an in-flight renewal settling AFTER pageshow #2 and chain B -> C", async () => {
  const server = createGenerationalServer();
  let deliverB;
  const pendingB = new Promise(resolve => { deliverB = resolve; });
  let mintedB;
  const ui = await mountPlayer({
    grant: (body, attempt) => {
      if (attempt === 2) {
        mintedB = server.issue(body);
        return pendingB;
      }
      return server.issue(body);
    },
  });
  try {
    await ui.seek(42_000);
    await ui.event("pagehide"); await ui.event("pageshow");
    assert.equal(ui.calls.length, 2);
    assert.equal(ui.calls[1].resume_from_play_session_id, "sid-1");
    assert.equal(mintedB.play_session_id, "sid-2");
    await ui.event("pagehide");
    await ui.event("pageshow");
    assert.equal(ui.calls.length, 2, "must not fire a duplicate sid-1 renewal while Request #1 is in flight");
    await ui.flush(() => deliverB(mintedB));
    assert.equal(ui.calls.length, 3);
    assert.equal(ui.calls[2].resume_from_play_session_id, "sid-2");
    assert.equal(ui.calls[2].start_ms, 42_000);
    assert.equal(ui.calls[2].audio_track, "audio-1");
    assert.equal(ui.engines.at(-1).options.streamUrl, "/stream/sid-3");
    const started = ui.reports.find(report => report.play_session_id === "sid-3");
    assert.ok(started, "restored session must report progress under sid-3");
    assert.equal(started.event_sequence, 1);
  } finally { await ui.unmount(); }
});

for (const [label, trigger, expectedMessage] of [
  ["admin termination", server => server.endByAdmin(), /管理员已结束本次播放/],
  ["takeover by another page", server => server.takeoverFromOtherPage(), /newer playback selected/],
]) {
  test(`in-flight renewal settling AFTER pageshow #2 still fails cleanly after ${label}`, async () => {
    const server = createGenerationalServer();
    let deliverB;
    const pendingB = new Promise(resolve => { deliverB = resolve; });
    let mintedB;
    const ui = await mountPlayer({
      grant: (body, attempt) => {
        if (attempt === 2) {
          mintedB = server.issue(body);
          return pendingB;
        }
        return server.issue(body);
      },
    });
    try {
      await ui.seek(42_000);
      await ui.event("pagehide"); await ui.event("pageshow");
      await ui.event("pagehide");
      trigger(server);
      await ui.event("pageshow");
      assert.equal(ui.calls.length, 2, "must await Request #1 before sending next renewal");
      await ui.flush(() => deliverB(mintedB));
      assert.equal(ui.calls.length, 3);
      assert.equal(ui.calls[2].resume_from_play_session_id, "sid-2");
      assert.match(JSON.stringify(ui.renderer.toJSON()), expectedMessage);
      assert.equal(ui.engines.length, 1);
      assert.equal(ui.reports.some(report => report.play_session_id === "sid-3"), false);
    } finally { await ui.unmount(); }
  });
}

test("structured HTTP error from Request #1 settling AFTER pageshow #2 fails without duplicating sid-1", async () => {
  let failB;
  const pendingB = new Promise((_resolve, reject) => { failB = reject; });
  const ui = await mountPlayer({
    grant: (_body, attempt) => {
      if (attempt === 1) return {
        play_session_id: "sid-1", session_id: null, stream_url: "/stream/sid-1", start_ms: 0,
        decision: { outcome: "plan", tier: 0, file_id: "file-1", container: "mp4", video: { action: "copy" }, audio: { action: "copy", track_ref: "audio-1" }, subtitles: [], audio_tracks: [] },
        subtitle_urls: [], watch: { position_ms: 0, duration_ms: 1000_000 }, source: null,
      };
      if (attempt === 2) return pendingB;
      throw new Error("unexpected third request");
    },
  });
  try {
    await ui.event("pagehide"); await ui.event("pageshow");
    await ui.event("pagehide"); await ui.event("pageshow");
    assert.equal(ui.calls.length, 2);
    await ui.flush(() => failB(Object.assign(new Error("ended"), {
      status: 409, details: { error: { code: "playback.ended_by_admin" } },
    })));
    assert.equal(ui.calls.length, 2);
    assert.match(JSON.stringify(ui.renderer.toJSON()), /管理员已结束本次播放/);
    assert.equal(ui.engines.length, 1);
  } finally { await ui.unmount(); }
});

test("transport/abort failure of Request #1 settling AFTER pageshow #2 retries with sid-1", async () => {
  const server = createGenerationalServer();
  let failB;
  const pendingB = new Promise((_resolve, reject) => { failB = reject; });
  const ui = await mountPlayer({
    grant: (body, attempt) => {
      if (attempt === 2) return pendingB;
      return server.issue(body);
    },
  });
  try {
    await ui.seek(42_000);
    await ui.event("pagehide"); await ui.event("pageshow");
    assert.equal(ui.calls[1].resume_from_play_session_id, "sid-1");
    await ui.event("pagehide"); await ui.event("pageshow");
    assert.equal(ui.calls.length, 2);
    await ui.flush(() => failB(new Error("network aborted")));
    assert.equal(ui.calls.length, 3);
    assert.equal(ui.calls[2].resume_from_play_session_id, "sid-1");
    assert.equal(ui.calls[2].start_ms, 42_000);
    assert.equal(ui.calls[2].audio_track, "audio-1");
    assert.equal(ui.engines.at(-1).options.streamUrl, "/stream/sid-2");
    assert.ok(ui.reports.some(report => report.play_session_id === "sid-2" && report.event === "start"));
  } finally { await ui.unmount(); }
});

test("late renewal settling AFTER pageshow #2 cannot be claimed after deviceId change or episode navigation", async () => {
  for (const mode of ["device-change", "navigate"]) {
    const server = createGenerationalServer();
    let deliverB;
    const pendingB = new Promise(resolve => { deliverB = resolve; });
    let mintedB;
    const ui = await mountPlayer({
      grant: (body, attempt) => {
        if (attempt === 2) {
          mintedB = server.issue(body);
          return pendingB;
        }
        return server.issue(body);
      },
    });
    try {
      await ui.event("pagehide"); await ui.event("pageshow");
      await ui.event("pagehide");
      if (mode === "device-change") {
        ui.setDeviceId("device-2");
        await ui.event("pageshow");
        assert.equal(ui.calls.length, 3, "a changed device must not await or claim the old device's renewal");
        assert.equal(ui.calls[2].resume_from_play_session_id, "sid-1");
        assert.match(JSON.stringify(ui.renderer.toJSON()), /newer playback selected/);
        await ui.flush(() => deliverB(mintedB));
        assert.equal(ui.engines.length, 1);
      } else {
        await ui.event("pageshow");
        await ui.navigate();
        const callsBeforeDeliver = ui.calls.length;
        await ui.flush(() => deliverB(mintedB));
        assert.equal(ui.calls.length, callsBeforeDeliver, "late renewal from previous unit must not trigger another session request");
        assert.ok(ui.calls.slice(2).length >= 1);
        assert.ok(
          ui.calls.slice(2).every(c => c.media_item_id === "next-media" && c.resume_from_play_session_id === undefined),
          "navigation to next-media must not inherit previous unit's renewal predecessor",
        );
        assert.notEqual(ui.engines.at(-1).options.streamUrl, "/stream/sid-2");
      }
    } finally { await ui.unmount(); }
  }
});

test("3rd BFCache round-trip restores with chained Request #2 sid-3 and attaches /stream/sid-4", async () => {
  const server = createGenerationalServer();
  let deliverB;
  const pendingB = new Promise(resolve => { deliverB = resolve; });
  let deliverC;
  const pendingC = new Promise(resolve => { deliverC = resolve; });
  let mintedB;
  let mintedC;
  const ui = await mountPlayer({
    grant: (body, attempt) => {
      if (attempt === 2) {
        mintedB = server.issue(body);
        return pendingB;
      }
      if (attempt === 3) {
        mintedC = server.issue(body);
        return pendingC;
      }
      return server.issue(body);
    },
  });
  try {
    await ui.seek(42_000);
    // Round-trip #1: fires Request #1 (sid-1 -> sid-2), left in flight
    await ui.event("pagehide"); await ui.event("pageshow");
    assert.equal(ui.calls[1].resume_from_play_session_id, "sid-1");
    assert.equal(mintedB.play_session_id, "sid-2");

    // Round-trip #2: pageshow #2 awaits Request #1 while visible; deliver sid-2 -> chains Request #2 (sid-2 -> sid-3)
    await ui.event("pagehide"); await ui.event("pageshow");
    await ui.flush(() => deliverB(mintedB));
    assert.equal(ui.calls.length, 3);
    assert.equal(ui.calls[2].resume_from_play_session_id, "sid-2");
    assert.equal(mintedC.play_session_id, "sid-3");

    // Round-trip #3: suspend while Request #2 is in flight, then settle sid-3 while suspended
    await ui.event("pagehide");
    await ui.flush(() => deliverC(mintedC));
    assert.equal(ui.engines.length, 1, "sid-3 settling in BFCache must not attach a stream while suspended");

    // pageshow #3 must recognize sid-3 as chained from sid-2 and request sid-4
    await ui.event("pageshow");
    assert.equal(ui.calls.length, 4);
    assert.equal(ui.calls[3].resume_from_play_session_id, "sid-3");
    assert.equal(ui.engines.at(-1).options.streamUrl, "/stream/sid-4");
  } finally { await ui.unmount(); }
});

test("declined BFCache restore clears pageSuspendedRef and manual 重试 starts a fresh session", async () => {
  const ui = await mountPlayer({
    grant: (_body, attempt) => ({
      play_session_id: `sid-${attempt}`,
      session_id: null,
      stream_url: `/stream/sid-${attempt}`,
      start_ms: 0,
      decision: {
        outcome: "plan",
        tier: 4,
        file_id: "file-1",
        container: "mp4",
        video: { action: "transcode" },
        audio: { action: "copy", track_ref: null },
        subtitles: [],
        audio_tracks: [],
      },
      subtitle_urls: [],
      watch: { position_ms: 0, duration_ms: 1000_000 },
      source: null,
    }),
  });
  try {
    await ui.event("pagehide");
    // While suspended in BFCache, fallback tier 4 fails -> enters terminal error phase
    await ui.flush(() => ui.engines[0].options.onFailed("decode failed", "decode-stalled"));
    assert.match(JSON.stringify(ui.renderer.toJSON()), /decode failed/);
    // pageshow declines restore because phase is "error", but must unsuspend
    await ui.event("pageshow");
    assert.equal(ui.calls.length, 1);
    const retryBtn = ui.renderer.root
      .findAllByType("button")
      .find(node => node.children.join("") === "重试");
    assert.ok(retryBtn, "error overlay must show 重试 button");
    await ui.flush(() => retryBtn.props.onClick());
    assert.equal(ui.calls.length, 2, "clicking 重试 after declined restore must start a fresh session");
    assert.equal(ui.calls[1].resume_from_play_session_id, undefined);
    assert.equal(ui.engines.at(-1).options.streamUrl, "/stream/sid-2");
  } finally { await ui.unmount(); }
});
