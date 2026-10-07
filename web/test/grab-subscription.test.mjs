import assert from "node:assert/strict";
import test from "node:test";

import { parseGrabSubscriptionId } from "../lib/grab-subscription.ts";

test("manual search preserves the exact Subscribe UUID from for_sub", () => {
  const id = "f3111f71-f6c7-46fd-b085-02edb10bb4b0";
  assert.equal(parseGrabSubscriptionId(id), id);
  assert.equal(parseGrabSubscriptionId(null), null);
  assert.equal(parseGrabSubscriptionId("123"), null);
  assert.equal(parseGrabSubscriptionId("123junk"), null);
});
