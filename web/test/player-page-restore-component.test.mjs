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

test("a user pause survives renewal and only the new SID starts on explicit play", async () => {
  const ui = await mountPlayer();
  try {
    await ui.pause();
    await ui.event("pagehide"); await ui.event("pageshow");
    assert.equal(ui.video.paused, true);
    assert.equal(ui.calls[1].resume_from_play_session_id, "sid-1");
    assert.equal(ui.reports.filter(report => report.play_session_id === "sid-2").length, 0);
    await ui.play();
    const started = ui.reports.find(report => report.play_session_id === "sid-2");
    assert.equal(started.event, "start");
    assert.equal(started.event_sequence, 1);
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
  test(`a second cache trip during renewal retains predecessor authorization for ${code}`, async () => {
    let finish;
    const pending = new Promise(resolve => { finish = resolve; });
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
      await ui.flush(() => finish(ui.session("obsolete-renewal", 42_000)));
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
