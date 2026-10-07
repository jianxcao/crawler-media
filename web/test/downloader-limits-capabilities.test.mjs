import test from "node:test";
import assert from "node:assert/strict";

import {
  DOWNLOADER_LIMITS_CAPABILITIES,
  editableLimitFields,
} from "../lib/downloader-limits-capabilities.ts";

test("editableLimitFields returns only supported speed fields when queue and alt_speed are unsupported", () => {
  const fields = editableLimitFields({ speed: true, queue: false, alt_speed: false });
  assert.deepEqual(fields, ["download_limit_bytes", "upload_limit_bytes"]);
});

test("default capabilities only expose speed editing", () => {
  const fields = editableLimitFields(DOWNLOADER_LIMITS_CAPABILITIES);
  assert.deepEqual(fields, ["download_limit_bytes", "upload_limit_bytes"]);
  assert.equal(DOWNLOADER_LIMITS_CAPABILITIES.queue, false);
  assert.equal(DOWNLOADER_LIMITS_CAPABILITIES.alt_speed, false);
});
