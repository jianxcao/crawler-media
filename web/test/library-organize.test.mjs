import assert from "node:assert/strict";
import test from "node:test";

import { buildOrganizeRenames, organizeTargets } from "../lib/library-organize.ts";

const lib = (id, kind, roots, extra = {}) => ({ id, kind, name: id, root_paths: roots, ...extra });

test("targets are the current Library plus other Libraries of the same kind", () => {
  const current = lib("a", "movie", ["/m/a"]);
  const all = [current, lib("b", "movie", ["/m/b"]), lib("t", "tv", ["/t"]), lib("c", "movie", [])];
  assert.deepEqual(
    organizeTargets(current, all).map((l) => l.id),
    ["a", "b"],
    "same kind only, never a rootless Library",
  );
});

test("same-Library organize sends plain from/to without to_library_id", () => {
  const current = lib("a", "movie", ["/m/a"]);
  const renames = [{ from: "/m/a/x.mkv", to: "/m/a/X (2000)/X (2000).mkv" }];
  assert.deepEqual(buildOrganizeRenames(renames, current, current), renames);
});

test("cross-Library organize re-roots targets and names the destination Library", () => {
  const current = lib("a", "movie", ["/m/a", "/m/a2"]);
  const target = lib("b", "movie", ["/m/b"]);
  const renames = [
    { from: "/m/a/x.mkv", to: "/m/a/X (2000)/X (2000).mkv" },
    { from: "/m/a2/y.mkv", to: "/m/a2/Y (2001)/Y (2001).mkv" },
  ];
  assert.deepEqual(buildOrganizeRenames(renames, current, target), [
    { from: "/m/a/x.mkv", to: "/m/b/X (2000)/X (2000).mkv", to_library_id: "b" },
    { from: "/m/a2/y.mkv", to: "/m/b/Y (2001)/Y (2001).mkv", to_library_id: "b" },
  ]);
});

test("a target outside every source root is dropped rather than guessed", () => {
  const current = lib("a", "movie", ["/m/a"]);
  const target = lib("b", "movie", ["/m/b"]);
  assert.deepEqual(buildOrganizeRenames([{ from: "/x.mkv", to: "/elsewhere/x.mkv" }], current, target), []);
});
