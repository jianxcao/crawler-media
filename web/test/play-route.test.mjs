import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import vm from "node:vm";
import { Children } from "react";
import { createRoutesFromElements, matchRoutes, Routes } from "react-router-dom";
import ts from "typescript";

const require = createRequire(import.meta.url);

function applicationRoutes() {
  let mounted;
  const page = () => null;
  const modules = new Proxy({}, { get: () => page });
  const code = ts.transpileModule(readFileSync(new URL("../src/main.tsx", import.meta.url), "utf8"), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
  }).outputText;
  vm.runInNewContext(code, {
    exports: {},
    document: { getElementById: () => ({}) },
    require: (name) => {
      if (name === "react-dom/client") return { createRoot: () => ({ render: (tree) => { mounted = tree; } }) };
      if (name.startsWith("@/")) return modules;
      return require(name);
    },
  });
  function findRoutes(element) {
    if (element?.type === Routes) return element;
    return Children.toArray(element?.props?.children).map(findRoutes).find(Boolean);
  }
  return createRoutesFromElements(findRoutes(mounted).props.children);
}

const routes = applicationRoutes();
const mediaId = "23b90f20-b326-47c0-ae53-73cd523ace33";

test("the application routes a Movie playback link without an episode segment", () => {
  const matches = matchRoutes(routes, `/play/${mediaId}`);
  assert.equal(matches.at(-1).params.mediaItemId, mediaId);
  assert.equal(matches.at(-1).params.unit, undefined);
});

test("the application passes an Episode segment to the playback page", () => {
  const matches = matchRoutes(routes, `/play/${mediaId}/s01e03?t=180`);
  assert.equal(matches.at(-1).params.mediaItemId, mediaId);
  assert.equal(matches.at(-1).params.unit, "s01e03");
});

test("extra segments do not resolve to a playback unit", () => {
  const matches = matchRoutes(routes, `/play/${mediaId}/s01e03/extra`);
  assert.equal(matches.at(-1).params.mediaItemId, undefined);
});
