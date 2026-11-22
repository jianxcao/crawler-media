import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import React, { act } from "react";
import { create } from "react-test-renderer";
import ts from "typescript";

const require = createRequire(import.meta.url);
globalThis.IS_REACT_ACT_ENVIRONMENT = true;
const webRoot = fileURLToPath(new URL("../", import.meta.url));

function transpileFile(relPath, resolveImport) {
  const source = readFileSync(path.join(webRoot, relPath), "utf8");
  const js = ts.transpileModule(source, {
    fileName: relPath,
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      jsx: ts.JsxEmit.ReactJSX,
      target: ts.ScriptTarget.ES2022,
    },
  }).outputText;
  const mod = { exports: {} };
  new Function("require", "module", "exports", js)(resolveImport, mod, mod.exports);
  return mod.exports;
}

function textOf(node) {
  if (node == null || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  return (node.children ?? []).map(textOf).join("");
}

function loadSiteForm() {
  return transpileFile("components/settings/sites/site-form.tsx", (spec) => {
    if (spec === "@/lib/api/sites") {
      return { configureSite: async () => ({}) };
    }
    return require(spec);
  });
}

function loadSitesApi(requestStub) {
  return transpileFile("lib/api/sites.ts", (spec) => {
    if (spec === "@/lib/http") {
      return { request: requestStub };
    }
    return require(spec);
  });
}

const HDAREA_CATALOG = {
  profile_id: "hdarea",
  auth_types: ["apikey", "cookie"],
  supports_check_in: true,
};

function makeSite(overrides = {}) {
  return {
    id: "site-hda-1",
    name: "hdarea",
    url: "https://hdarea.club",
    profile_id: "hdarea",
    cookie: null,
    api_key: null,
    rss_url: null,
    proxy: null,
    rate_limit_per_minute: null,
    cdp_url: null,
    downloader_id: null,
    enabled: true,
    auth_type: "cookie",
    maintenance_via: "browser",
    ...overrides,
  };
}

test("SiteForm hides maintenance_via when authType is apikey and shows it for cookie", async () => {
  const { SiteForm } = loadSiteForm();
  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(SiteForm, {
        item: HDAREA_CATALOG,
        site: null,
        busy: false,
        onSubmit: () => {},
      }),
    );
  });

  // Default auth_type for hdarea is "apikey" -> maintenance_via hidden
  assert.equal(
    textOf(renderer.root).includes("登录与签到维护方式"),
    false,
    "maintenance_via must be hidden when authType === 'apikey'",
  );

  // Switch to Cookie -> maintenance_via appears
  const cookieTab = renderer.root
    .findAllByType("button")
    .find((btn) => textOf(btn).trim() === "Cookie");
  assert.ok(cookieTab);
  await act(async () => {
    cookieTab.props.onClick();
  });
  assert.equal(
    textOf(renderer.root).includes("登录与签到维护方式"),
    true,
    "maintenance_via must be visible when authType === 'cookie'",
  );

  await act(async () => {
    renderer.unmount();
  });
});

test("SiteForm switching from cookie to apikey clears stale cookie and resets maintenance_via to http", async () => {
  const { SiteForm } = loadSiteForm();
  const submitted = [];
  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(SiteForm, {
        item: HDAREA_CATALOG,
        site: makeSite({ auth_type: "cookie", maintenance_via: "browser" }),
        busy: false,
        onSubmit: (payload) => submitted.push(payload),
      }),
    );
  });

  const apiKeyTab = renderer.root
    .findAllByType("button")
    .find((btn) => textOf(btn).trim() === "API 密钥");
  await act(async () => {
    apiKeyTab.props.onClick();
  });

  const submitBtn = () =>
    renderer.root
      .findAllByType("button")
      .find((btn) => textOf(btn).includes("保存并重新验证"));

  assert.equal(submitBtn().props.disabled, true, "switching auth type requires new credential");

  const pwInput = renderer.root
    .findAllByType("input")
    .find((inp) => inp.props.type === "password");
  await act(async () => {
    pwInput.props.onChange({ target: { value: "  0123456789abcdef0123456789abcdef  " } });
  });

  assert.equal(submitBtn().props.disabled, false);
  await act(async () => {
    submitBtn().props.onClick();
  });

  assert.equal(submitted.length, 1);
  assert.equal(submitted[0].auth_type, "apikey");
  assert.equal(submitted[0].api_key, "0123456789abcdef0123456789abcdef");
  assert.equal(submitted[0].cookie, "", "must explicitly send cookie: '' to clear stale cookie");
  assert.equal(submitted[0].maintenance_via, "http");

  await act(async () => {
    renderer.unmount();
  });
});

test("SiteForm editing existing apikey site without retyping api_key preserves saved api_key and clears stale cookie", async () => {
  const { SiteForm } = loadSiteForm();
  const submitted = [];
  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(SiteForm, {
        item: HDAREA_CATALOG,
        site: makeSite({ auth_type: "apikey", maintenance_via: "http" }),
        busy: false,
        onSubmit: (payload) => submitted.push(payload),
      }),
    );
  });

  const urlInput = renderer.root
    .findAllByType("input")
    .find((inp) => inp.props.type === "text");
  await act(async () => {
    urlInput.props.onChange({ target: { value: "https://www.hdarea.co" } });
  });

  const submitBtn = renderer.root
    .findAllByType("button")
    .find((btn) => textOf(btn).includes("保存并重新验证"));
  assert.equal(
    submitBtn.props.disabled,
    false,
    "existing apikey site must allow saving URL without re-entering masked api_key",
  );

  await act(async () => {
    submitBtn.props.onClick();
  });

  assert.equal(submitted.length, 1);
  assert.equal(submitted[0].url, "https://www.hdarea.co");
  assert.equal("api_key" in submitted[0], false, "must NOT send empty api_key when unchanged");
  assert.equal(submitted[0].cookie, "");

  await act(async () => {
    renderer.unmount();
  });
});

test("SiteForm switching from apikey to cookie clears stale api_key, and keeping existing cookie omits cookie", async () => {
  const { SiteForm } = loadSiteForm();
  const submitted = [];
  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(SiteForm, {
        item: HDAREA_CATALOG,
        site: makeSite({ auth_type: "apikey", maintenance_via: "http" }),
        busy: false,
        onSubmit: (payload) => submitted.push(payload),
      }),
    );
  });

  const cookieTab = renderer.root
    .findAllByType("button")
    .find((btn) => textOf(btn).trim() === "Cookie");
  await act(async () => {
    cookieTab.props.onClick();
  });

  const submitBtn = () =>
    renderer.root
      .findAllByType("button")
      .find((btn) => textOf(btn).includes("保存并重新验证"));
  assert.equal(submitBtn().props.disabled, true);

  const textarea = renderer.root.findByType("textarea");
  await act(async () => {
    textarea.props.onChange({ target: { value: " c_secure_uid=99; c_secure_pass=abc " } });
  });
  await act(async () => {
    submitBtn().props.onClick();
  });

  assert.equal(submitted.length, 1);
  assert.equal(submitted[0].auth_type, "cookie");
  assert.equal(submitted[0].cookie, "c_secure_uid=99; c_secure_pass=abc");
  assert.equal(submitted[0].api_key, "", "must explicitly send api_key: '' to clear stale api_key");

  await act(async () => {
    renderer.unmount();
  });

  // Also check editing existing cookie site without retyping cookie
  const submittedKeep = [];
  let r2;
  await act(async () => {
    r2 = create(
      React.createElement(SiteForm, {
        item: HDAREA_CATALOG,
        site: makeSite({ auth_type: "cookie", maintenance_via: "http" }),
        busy: false,
        onSubmit: (payload) => submittedKeep.push(payload),
      }),
    );
  });
  const keepBtn = r2.root
    .findAllByType("button")
    .find((btn) => textOf(btn).includes("保存并重新验证"));
  assert.equal(keepBtn.props.disabled, false);
  await act(async () => {
    keepBtn.props.onClick();
  });
  assert.equal(submittedKeep.length, 1);
  assert.equal("cookie" in submittedKeep[0], false, "must NOT send empty cookie when unchanged");
  assert.equal(submittedKeep[0].api_key, "");

  await act(async () => {
    r2.unmount();
  });
});

test("checkInSite posts to /sites/{id}/check-in and reloads site snapshot", async () => {
  const calls = [];
  const refreshed = makeSite({ auth_type: "apikey" });
  const sitesApi = loadSitesApi(async (url, init) => {
    calls.push({ url, method: init?.method ?? "GET" });
    if (url === "/sites/site-hda-1/check-in") {
      return { ok: true, data: { ok: true } };
    }
    if (url === "/sites/site-hda-1") {
      return { ok: true, data: refreshed };
    }
    throw new Error(`unexpected ${url}`);
  });

  assert.equal(typeof sitesApi.checkInSite, "function");
  const out = await sitesApi.checkInSite("site-hda-1");
  assert.deepEqual(out, refreshed);
  assert.deepEqual(calls, [
    { url: "/sites/site-hda-1/check-in", method: "POST" },
    { url: "/sites/site-hda-1", method: "GET" },
  ]);
});

test("configureSite serializes url: '' when SiteForm leaves URL blank so backend profile default URL applies", async () => {
  const { SiteForm } = loadSiteForm();
  const submitted = [];
  let renderer;
  await act(async () => {
    renderer = create(
      React.createElement(SiteForm, {
        item: HDAREA_CATALOG,
        site: null,
        busy: false,
        onSubmit: (payload) => submitted.push(payload),
      }),
    );
  });

  const pwInput = renderer.root
    .findAllByType("input")
    .find((inp) => inp.props.type === "password");
  await act(async () => {
    pwInput.props.onChange({ target: { value: "0123456789abcdef0123456789abcdef" } });
  });

  const submitBtn = renderer.root
    .findAllByType("button")
    .find((btn) => textOf(btn).includes("保存并验证"));
  await act(async () => {
    submitBtn.props.onClick();
  });
  await act(async () => {
    renderer.unmount();
  });

  assert.equal(submitted.length, 1);
  assert.equal("url" in submitted[0], false, "SiteForm omits blank url in payload");

  const postedBodies = [];
  const createdSite = makeSite({ auth_type: "apikey" });
  const sitesApi = loadSitesApi(async (url, init) => {
    assert.equal(url, "/sites");
    assert.equal(init?.method, "POST");
    postedBodies.push(JSON.parse(init.body));
    return { ok: true, data: createdSite };
  });

  await sitesApi.configureSite("hdarea", submitted[0]);
  assert.equal(postedBodies.length, 1);
  assert.equal("url" in postedBodies[0], true, "POST /sites body must always include url");
  assert.equal(postedBodies[0].url, "");
  assert.equal(postedBodies[0].profile_id, "hdarea");

  await sitesApi.configureSite("hdarea", { ...submitted[0], url: "  https://custom.hdarea.co  " });
  assert.equal(postedBodies[1].url, "https://custom.hdarea.co");
});
