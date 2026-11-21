import assert from "node:assert/strict";
import test from "node:test";
import { buildRuleSetDraft, ruleSetSummary, upgradeTargetLabel } from "../lib/rule-set-summary.ts";
import { DEFAULT_LADDER } from "../lib/upgrade-ladder.ts";

const atom = (kind, value, priority = 50, exclude = false) => ({
  kind,
  value,
  priority,
  exclude,
});

function baseDraftInput(overrides = {}) {
  return {
    resolutions: [],
    mediaSources: [],
    codecFamilies: new Set(),
    codecExtras: [],
    platformsAllow: [],
    platformsBlock: [],
    platformExtras: { allow: [], block: [] },
    groupsAllow: "",
    groupsBlock: "",
    hdrLevels: [],
    hdrBlock: [],
    subLangs: [],
    audioLangs: [],
    preferredSites: [],
    freeOnly: false,
    excludeHr: false,
    hrStrict: false,
    minSeeders: "",
    sizeMin: "",
    sizeMax: "",
    upgradeSource: "",
    cutoffResolution: "",
    upgradeLadder: [...DEFAULT_LADDER],
    ...overrides,
  };
}

test("upgradeTargetLabel returns null without wash_target and formats configured ladder dimensions", () => {
  assert.equal(upgradeTargetLabel([atom("resolution", "2160p")]), null);

  const label = upgradeTargetLabel([
    atom("resolution", "1080p"),
    atom("wash_target", "2160p remux"),
    atom("upgrade_ladder", "resolution,source,hdr,video_codec,platform"),
    atom("hdr", "DV"),
    atom("video_codec", "HEVC"),
    atom("platform", "netflix"),
  ]);
  assert.equal(label, "2160p Remux · DV · HEVC · Netflix");
});

test("ruleSetSummary normalizes HDR families and handles withoutUpgrade / keepOldVersions", () => {
  const fullHdr = ["DV", "HDR10+", "HDR10", "HLG"].map((v) => atom("hdr", v));
  assert.deepEqual(ruleSetSummary(fullHdr), ["必须 HDR"]);
  assert.deepEqual(ruleSetSummary([atom("hdr", "SDR")]), ["排除 HDR"]);

  const washAtoms = [atom("resolution", "2160p"), atom("wash_target", "2160p blu-ray")];
  assert.deepEqual(ruleSetSummary(washAtoms, { keepOldVersions: true }), [
    "2160p",
    "洗到 2160p 蓝光（保留旧版）",
  ]);
  assert.deepEqual(ruleSetSummary(washAtoms, { withoutUpgrade: true }), ["2160p"]);
});

test("buildRuleSetDraft validates size range and wash target constraints", () => {
  assert.equal(
    buildRuleSetDraft(baseDraftInput({ sizeMin: "500", sizeMax: "100" })).error,
    "体积下限不能大于上限",
  );
  assert.equal(
    buildRuleSetDraft(baseDraftInput({ mediaSources: ["web-dl"], upgradeSource: "remux" })).error,
    "洗版目标片源必须在允许的片源范围内",
  );
  assert.equal(
    buildRuleSetDraft(
      baseDraftInput({
        resolutions: ["1080p"],
        upgradeSource: "web-dl",
        cutoffResolution: "2160p",
      }),
    ).error,
    "洗版目标分辨率必须在允许的分辨率范围内",
  );

  const valid = buildRuleSetDraft(
    baseDraftInput({
      resolutions: ["2160p", "1080p"],
      freeOnly: true,
      upgradeSource: "remux",
    }),
  );
  assert.equal(valid.error, null);
  assert.deepEqual(
    valid.atoms.map((a) => [a.kind, a.value, a.priority]),
    [
      ["resolution", "2160p", 100],
      ["resolution", "1080p", 50],
      ["free", null, 50],
      ["wash_target", "2160p remux", 50],
    ],
  );
});
