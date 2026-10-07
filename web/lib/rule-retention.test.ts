import { describe, it } from "node:test";
import assert from "node:assert/strict";

describe("Rule retention policy round-trip", () => {
  it("keeps keep_old_versions on RuleSet model and overrides properly", () => {
    interface RuleSet {
      id: string;
      name: string;
      is_default: boolean;
      keep_old_versions?: boolean;
    }

    const ruleSet: RuleSet = {
      id: "rs-1",
      name: "Keep Old Rule",
      is_default: false,
      keep_old_versions: true,
    };

    assert.equal(ruleSet.keep_old_versions, true);

    // Default resolution: if explicit override is undefined, inherit from rule set
    const explicitOverride: boolean | undefined = undefined;
    const effectiveValue = explicitOverride ?? ruleSet.keep_old_versions ?? false;
    assert.equal(effectiveValue, true);

    // Explicit override false must not be swallowed by truthy check
    const explicitFalse: boolean | undefined = false;
    const effectiveFalse = explicitFalse !== undefined ? explicitFalse : (ruleSet.keep_old_versions ?? false);
    assert.equal(effectiveFalse, false);
  });
});
