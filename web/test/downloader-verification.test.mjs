import test from "node:test";
import assert from "node:assert/strict";

import { verificationState } from "../lib/downloader-verification.ts";

test("verificationState formats successful verify as active", () => {
  assert.deepEqual(verificationState({ ok: true, error: null }), {
    status: "active",
    last_error: null,
  });
});

test("verificationState formats failed verify with error as failed", () => {
  assert.deepEqual(verificationState({ ok: false, error: "bad credentials" }), {
    status: "failed",
    last_error: "bad credentials",
  });
});

test("verificationState supplies a readable fallback for empty failure messages", () => {
  assert.deepEqual(verificationState({ ok: false, error: "  " }), {
    status: "failed",
    last_error: "连接验证失败",
  });
});

test("verificationState formats missing verify as pending", () => {
  assert.deepEqual(verificationState(null), {
    status: "pending",
    last_error: null,
  });
});
