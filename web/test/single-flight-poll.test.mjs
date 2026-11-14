import assert from "node:assert/strict";
import test from "node:test";

import { startSingleFlightPoll } from "../lib/single-flight-poll.ts";

/** Manual clock: timers fire only when the test advances them. */
function fakeClock() {
  let now = 0;
  let next = 1;
  const timers = new Map();
  return {
    setTimeout: (fn, ms) => {
      const id = next++;
      timers.set(id, { at: now + ms, fn });
      return id;
    },
    clearTimeout: (id) => timers.delete(id),
    async advance(ms) {
      now += ms;
      for (const [id, timer] of [...timers]) {
        if (timer.at <= now) {
          timers.delete(id);
          timer.fn();
        }
      }
      await new Promise((resolve) => setImmediate(resolve));
    },
    pending: () => timers.size,
  };
}

test("review L5: next round is scheduled only after the previous settles", async () => {
  const clock = fakeClock();
  let inFlight = 0;
  let maxInFlight = 0;
  let started = 0;
  const releases = [];
  const stop = startSingleFlightPoll(
    () => {
      started += 1;
      inFlight += 1;
      maxInFlight = Math.max(maxInFlight, inFlight);
      return new Promise((resolve) => releases.push(() => { inFlight -= 1; resolve(true); }));
    },
    5000,
    clock,
  );
  await clock.advance(5000);
  assert.equal(started, 1);
  for (let i = 0; i < 6; i += 1) await clock.advance(5000);
  assert.equal(started, 1, "a slow round blocks the timer");
  releases.shift()();
  await clock.advance(0);
  await clock.advance(5000);
  assert.equal(started, 2);
  assert.equal(maxInFlight, 1);
  stop();
});

test("review L5: stop aborts the in-flight round and schedules nothing", async () => {
  const clock = fakeClock();
  let signal;
  let release;
  const stop = startSingleFlightPoll(
    (s) => {
      signal = s;
      return new Promise((resolve) => { release = () => resolve(true); });
    },
    5000,
    clock,
  );
  await clock.advance(5000);
  assert.equal(signal.aborted, false);
  stop();
  assert.equal(signal.aborted, true);
  release();
  await clock.advance(0);
  await clock.advance(50000);
  assert.equal(clock.pending(), 0);
});

test("review L5: a round returning false ends polling; a failure retries", async () => {
  const clock = fakeClock();
  const results = [Promise.reject(new Error("transient")), Promise.resolve(false)];
  let started = 0;
  startSingleFlightPoll(() => results[started++], 5000, clock);
  await clock.advance(5000);
  await clock.advance(0);
  await clock.advance(5000);
  await clock.advance(0);
  assert.equal(started, 2);
  assert.equal(clock.pending(), 0);
});
