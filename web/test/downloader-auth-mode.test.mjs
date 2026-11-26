import assert from "node:assert/strict";
import { existsSync, readFileSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import test from "node:test";
import vm from "node:vm";
import ts from "typescript";

const require = createRequire(import.meta.url);

function load(relative, mocks = {}) {
  const cache = new Map();

  function resolveLocal(specifier, parentUrl) {
    let target = null;
    if (specifier.startsWith("@/components/settings/downloaders/")) {
      target = new URL(`../${specifier.slice(2)}`, import.meta.url);
    } else if (specifier.startsWith("./") || specifier.startsWith("../")) {
      target = new URL(specifier, parentUrl);
    }
    if (!target) return null;
    for (const suffix of ["", ".tsx", ".ts", "/index.tsx", "/index.ts"]) {
      const candidate = new URL(`${target.href}${suffix}`);
      if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
    }
    return null;
  }

  function loadModule(fileUrl) {
    if (cache.has(fileUrl.href)) return cache.get(fileUrl.href).exports;
    const mod = { exports: {} };
    cache.set(fileUrl.href, mod);
    const code = ts.transpileModule(readFileSync(fileUrl, "utf8"), {
      compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX },
      fileName: fileUrl.pathname,
    }).outputText;
    vm.runInNewContext(
      code,
      {
        module: mod,
        exports: mod.exports,
        require: (name) => {
          if (name in mocks) return mocks[name];
          const localUrl = resolveLocal(name, fileUrl);
          if (localUrl) return loadModule(localUrl);
          return require(name);
        },
        window: { location: { search: "" } },
        URLSearchParams,
        console,
      },
      { filename: fileUrl.pathname },
    );
    return mod.exports;
  }

  return loadModule(new URL(relative, import.meta.url));
}

function formFixture(downloader = null) {
  let current;
  const jsx = (type, props) => ({ type, props });
  const react = {
    useState(initial) {
      const index = current.index++;
      const owner = current;
      if (!(index in owner.states)) {
        owner.states[index] = typeof initial === "function" ? initial() : initial;
      }
      return [
        owner.states[index],
        (next) => {
          owner.states[index] = typeof next === "function" ? next(owner.states[index]) : next;
        },
      ];
    },
    useRef(value) {
      return react.useState(() => ({ current: value }))[0];
    },
    useCallback(fn) {
      return fn;
    },
    useEffect(fn, deps) {
      const index = current.index++;
      const previous = current.deps[index];
      if (!previous || deps.some((dep, i) => dep !== previous[i])) current.effects.push(fn);
      current.deps[index] = deps;
    },
  };
  const noop = () => null;
  const mocks = {
    react,
    "react/jsx-runtime": { jsx, jsxs: jsx, Fragment: "fragment" },
    "@radix-ui/react-dropdown-menu": Object.fromEntries(
      ["Root", "Trigger", "Portal", "Content", "Item"].map((k) => [k, k]),
    ),
    "@/lib/api/downloaders": {},
    "@/components/icons": new Proxy({}, { get: () => noop }),
    "@/components/directory-picker": { DirectoryPicker: noop },
    "@/components/feedback": { useConfirm: () => async () => true },
    "@/lib/backdrop": { useBackdrop: () => ({}) },
    "@/components/modal": { Modal: noop },
    "@/components/transfer-mode-card": { TransferModeCard: noop },
    "@/vendor/liquid-glass": { LiquidGlassButton: noop },
  };
  const { DownloaderForm } = load("../components/settings/downloaders/downloader-form.tsx", mocks);
  const { DownloaderRow } = load("../components/settings/downloaders/downloader-row.tsx", mocks);
  const submitted = [];
  const owner = { states: [], deps: [], effects: [], index: 0 };
  const renderForm = () => {
    current = owner;
    owner.index = 0;
    return DownloaderForm({
      downloader,
      onSubmit: async (payload) => {
        submitted.push(payload);
      },
      onCancel: () => {},
      onError: () => {},
    });
  };
  const renderRow = (rowDownloader) => {
    const rowOwner = { states: [], deps: [], effects: [], index: 0 };
    current = rowOwner;
    return DownloaderRow({
      downloader: rowDownloader,
      expanded: true,
      editing: false,
      onToggle: () => {},
      onEdit: () => {},
      onCloseEdit: () => {},
      onChanged: () => {},
      onDeleted: () => {},
      onRefresh: () => {},
      onError: () => {},
    });
  };
  return { renderForm, renderRow, submitted };
}

function nodes(tree) {
  if (Array.isArray(tree)) return tree.flatMap(nodes);
  if (!tree || typeof tree !== "object") return [];
  return [tree, ...nodes(tree.props?.children)];
}

function textContent(tree) {
  if (typeof tree === "string" || typeof tree === "number") return String(tree);
  if (Array.isArray(tree)) return tree.map(textContent).join(" ");
  if (tree && typeof tree === "object") return textContent(tree.props?.children);
  return "";
}

test("DownloaderForm shows auth toggle for qBittorrent, renders v5.2+ guide, and validates API Key", async () => {
  const { renderForm, submitted } = formFixture(null);
  let tree = renderForm();
  const findBtn = (label) =>
    nodes(tree).find((n) => n.type === "button" && textContent(n).includes(label));

  assert.ok(findBtn("用户名与密码"), "qBittorrent must expose password mode button");
  const apiKeyBtn = findBtn("API Key");
  assert.ok(apiKeyBtn, "qBittorrent must expose API Key（v5.2+）mode button");

  // Switch to Transmission: API Key toggle must hide.
  findBtn("Transmission").props.onClick();
  tree = renderForm();
  assert.equal(findBtn("API Key"), undefined, "Transmission must not show qB API Key toggle");

  // Switch back to qBittorrent and choose API Key mode.
  findBtn("qBittorrent").props.onClick();
  tree = renderForm();
  findBtn("API Key").props.onClick();
  tree = renderForm();

  const fullText = textContent(tree);
  assert.match(fullText, /v5\.2\.0/, "must explain qBittorrent v5.2.0+ requirement");
  assert.match(fullText, /WebUI/, "must explain WebUI path");
  assert.match(fullText, /qbt_/, "must show qbt_ prefix example");

  // Fill name & URL only -> Save stays disabled while API Key is empty on create.
  const inputs = nodes(tree).filter((n) => n.type === "input");
  inputs[0].props.onChange({ target: { value: "Docker qB" } });
  inputs[1].props.onChange({ target: { value: "http://127.0.0.1:8080" } });
  tree = renderForm();
  assert.equal(findBtn("保存").props.disabled, true, "empty API Key on create must disable Save");

  // Enter API key -> Save becomes enabled and submits apikey payload.
  const apiKeyInput = nodes(tree).find(
    (n) => n.type === "input" && String(n.props?.placeholder ?? "").includes("qbt_"),
  );
  assert.ok(apiKeyInput, "must render dedicated API Key input with qbt_ placeholder");
  apiKeyInput.props.onChange({ target: { value: "qbt_test_key_520" } });
  tree = renderForm();
  const saveBtn = findBtn("保存");
  assert.equal(saveBtn.props.disabled, false);
  await saveBtn.props.onClick();
  assert.equal(submitted[0].auth_type, "apikey");
  assert.equal(submitted[0].api_key, "qbt_test_key_520");
});

test("editing existing API key downloader keeps authType=apikey and allows empty key", () => {
  const { renderForm } = formFixture({
    id: "dl-1",
    name: "qB v5.3",
    kind: "qbittorrent",
    url: "http://127.0.0.1:8080",
    auth_type: "apikey",
    has_api_key: true,
    has_password: false,
    path_maps: [],
    is_default: true,
    status: "active",
  });
  const tree = renderForm();
  const saveBtn = nodes(tree).find((n) => n.type === "button" && textContent(n).includes("保存"));
  assert.equal(saveBtn.props.disabled, false, "existing has_api_key must allow empty re-save");
  const apiKeyInput = nodes(tree).find(
    (n) => n.type === "input" && /已配置|留空/.test(String(n.props?.placeholder ?? "")),
  );
  assert.ok(apiKeyInput, "must indicate API key is already configured in placeholder");
});

test("toDownloaderInput serializes apikey and password mode transitions properly", async () => {
  const calls = [];
  const api = load("../lib/api/downloaders.ts", {
    "@/lib/http": {
      request: async (path, init) => {
        calls.push({ path, method: init?.method, body: JSON.parse(init?.body ?? "null") });
        return { data: { id: "1", status: "pending" } };
      },
    },
    "../downloader-verification": load("../lib/downloader-verification.ts"),
  });

  // Create with API key: clears username/password and sends api_key.
  await api.createDownloader({
    name: "qB5",
    client_type: "qbittorrent",
    url: "http://127.0.0.1:8080",
    auth_type: "apikey",
    api_key: "qbt_live_123",
  });
  assert.equal(calls[0].body.api_key, "qbt_live_123");
  assert.equal(calls[0].body.username, "");
  assert.equal(calls[0].body.password, "");

  // Edit in apikey mode with blank api_key (keep existing secret) omits api_key.
  await api.updateDownloader("1", {
    name: "qB5-renamed",
    client_type: "qbittorrent",
    url: "http://127.0.0.1:8080",
    auth_type: "apikey",
    api_key: null,
  });
  assert.equal("api_key" in calls[1].body, false, "blank api_key on edit must omit field to preserve stored key");

  // Switch back to password mode: explicitly sends api_key: "" to clear old key.
  await api.updateDownloader("1", {
    name: "qB5",
    client_type: "qbittorrent",
    url: "http://127.0.0.1:8080",
    auth_type: "password",
    username: "admin",
    password: "pw",
  });
  assert.equal(calls[2].body.api_key, "");
  assert.equal(calls[2].body.username, "admin");
  assert.equal(calls[2].body.password, "pw");

  // Switch kind to transmission: explicitly sends api_key: "" to drop stale qB key.
  await api.updateDownloader("1", {
    name: "tr1",
    client_type: "transmission",
    url: "http://127.0.0.1:9091/transmission/rpc",
    username: "tr",
    password: "secret",
  });
  assert.equal(calls[3].body.kind, "transmission");
  assert.equal(calls[3].body.api_key, "");
});

test("DownloaderRow displays API Key auth mode cleanly when auth_type is apikey", () => {
  const { renderRow } = formFixture();
  const tree = renderRow({
    id: "dl-2",
    name: "qB5",
    kind: "qbittorrent",
    url: "http://127.0.0.1:8080",
    auth_type: "apikey",
    has_api_key: true,
    path_maps: [],
    is_default: true,
    status: "active",
  });
  const stat = nodes(tree).find((n) => n.props?.label === "认证方式");
  assert.ok(stat, "must render DetailStat for 认证方式");
  assert.match(String(stat.props.value), /API Key/);
});
