import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import ts from "typescript";

async function load(request) {
  const source = await readFile(new URL("../lib/api/subscriptions.ts", import.meta.url), "utf8");
  const js = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const module = { exports: {} };
  new Function("require", "module", "exports", js)((dependency) => {
    if (dependency === "@/lib/http") return { request };
    return { createSubscriptionBody: (p) => p };
  }, module, module.exports);
  return module.exports;
}
const envelope = (data) => ({ ok: true, data });

test("removal and season cleanup return the server receipts verbatim", async () => {
  const api = await load(async (path) =>
    envelope(path.includes("season-cleanup") ? { queued: true } : { deleted: true }),
  );
  const removed = await api.deleteSubscriptionPermanently("s1", {});
  assert.deepEqual(removed, { deleted: true });
  assert.equal("cleanup_job_id" in removed, false);
  const cleaned = await api.cleanupSubscriptionSeasons("s1", [2], {});
  assert.deepEqual(cleaned, { queued: true });
});

test("mutations do not forge an empty wanted list", async () => {
  const api = await load(async () => envelope({ id: "s1" }));
  const sub = await api.updateSubscription("s1", { fetch_mode: "rss" });
  assert.equal("wanted" in sub, false);
});

test("a failed mutation stays a failure", async () => {
  const api = await load(async () => {
    throw new Error("boom");
  });
  await assert.rejects(api.deleteSubscriptionPermanently("s1", {}), /boom/);
});

test("downloads are scoped to the subscription's wanted hashes", async () => {
  let calls = 0;
  const api = await load(async () => {
    calls += 1;
    return envelope({
      items: [
        { id: "a", info_hash: "aaa", name: "A", state: "downloading", subscriptions: [] },
        { id: "b", info_hash: "bbb", name: "B", state: "downloading", subscriptions: [] },
      ],
    });
  });
  const tasksForEmptyWanted = await api.listActiveSubscriptionDownloads([]);
  assert.deepEqual(tasksForEmptyWanted, []);
  assert.equal(calls, 0, "no wanted hashes means no request");
  const shown = await api.listActiveSubscriptionDownloads(["bbb"]);
  assert.deepEqual(shown.map((t) => t.info_hash), ["bbb"]);
});

test('rule-set API saves atoms directly and reloads every condition unchanged', async () => {
  const atoms = [
    { kind: 'hr', value: null, priority: 100, exclude: true, unknown: 'exclude' },
    { kind: 'platform', value: 'netflix', priority: 234, exclude: false },
    { kind: 'team', value: 'Group-A', priority: 345, exclude: true },
    { kind: 'title_match', value: 'CUSTOM', priority: 456, exclude: false },
  ];
  let stored;
  const api = await load(async (_path, init) => {
    if (init?.body) stored = { id: 'filter-1', ...JSON.parse(init.body) };
    return envelope(init?.body ? stored : [stored]);
  });
  await api.createRuleSet('Strict', atoms, true);
  assert.deepEqual(stored, { id: 'filter-1', name: 'Strict', atoms, keep_old_versions: true });
  const [reloaded] = await api.listRuleSets();
  const renamed = await api.updateRuleSet(reloaded.id, 'Renamed', reloaded.atoms, false);
  assert.deepEqual(renamed, {
    id: 'filter-1', name: 'Renamed', atoms, keep_old_versions: false,
  });
});
