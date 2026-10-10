import assert from "node:assert/strict";
import test from "node:test";
import { probeStatusText, shouldPollProbeDetails } from "../lib/probe-status.ts";

test("partial failure waits without keeping detail polling alive", () => {
  const stage = {
    status: "failed",
    failure_count: 1,
    next_retry_at_ms: 1_060_000,
    error_kind: "http_403",
  };
  assert.equal(probeStatusText(stage, 1_000_000), "读取被拒绝，60 秒后重试");
  assert.equal(shouldPollProbeDetails([stage]), false);
});

test("active stages keep detail polling", () => {
  const stage = {
    status: "running",
    failure_count: 0,
    next_retry_at_ms: null,
    error_kind: null,
  };
  assert.equal(shouldPollProbeDetails([stage]), true);
});
