import assert from "node:assert/strict";
import test from "node:test";
import { retryStartupTimeout } from "./startup-retry.mjs";

test("startup retries a timeout once after a delay", async () => {
  let calls = 0;
  const timeout = { status: 0 };
  const delays = [];
  const result = await retryStartupTimeout(async () => {
    if (++calls === 1) throw timeout;
    return "session";
  }, (error) => error === timeout, async (ms) => { delays.push(ms); });
  assert.equal(result, "session");
  assert.equal(calls, 2);
  assert.deepEqual(delays, [1000]);
});

test("startup does not retry other errors or a second timeout", async () => {
  const timeout = { status: 0 };
  let calls = 0;
  await assert.rejects(retryStartupTimeout(async () => {
    calls++;
    throw timeout;
  }, () => true, async () => {}), (error) => error === timeout);
  assert.equal(calls, 2);
  calls = 0;
  await assert.rejects(retryStartupTimeout(async () => {
    calls++;
    throw timeout;
  }, () => false, async () => {}), (error) => error === timeout);
  assert.equal(calls, 1);
});
