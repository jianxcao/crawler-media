import "./plugin-ui-env";

import assert from "node:assert/strict";
import test from "node:test";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import React from "react";
import { MemoryRouter } from "react-router-dom";

import { FeedbackProvider } from "../components/feedback";
import { PluginManagerView } from "../components/plugins/PluginManagerView";
import { PluginMarketTab } from "../components/plugins/PluginMarketTab";
import { PluginUploadModal } from "../components/plugins/PluginUploadModal";
import type { SessionView } from "../lib/api/auth";
import type {
  PluginCatalogEntryView,
  PluginRepositoryView,
  PluginSummaryView,
  TrustKeyView,
} from "../lib/api/plugins";
import { SessionProvider } from "../lib/session";

const ADMIN_SESSION: SessionView = {
  id: "user-admin-market",
  username: "admin",
  nickname: "Admin",
  avatar_url: null,
  role: "admin",
  capabilities: { allow_subscribe: true, allow_search: true, allow_direct_download: true },
};

function jsonResp(data: unknown, status = 200): Response {
  return new Response(JSON.stringify({ ok: true, data }), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function makeSummary(overrides: Partial<PluginSummaryView> = {}): PluginSummaryView {
  return {
    id: "cm.checkin",
    name: "站点自动签到",
    version: "1.0.0",
    package_hash: "a".repeat(64),
    source: "external",
    revision: 1,
    config_revision: 1,
    generation: 0,
    enabled: false,
    lifecycle: "installed-disabled",
    health: "ok",
    required_grants: ["read:sites", "command:site.maintenance"],
    effective_grants: [],
    active_operation_id: null,
    failure_reason: null,
    ...overrides,
  };
}

function makeCatalogEntry(overrides: Partial<PluginCatalogEntryView> = {}): PluginCatalogEntryView {
  return {
    id: "cm.checkin",
    name: "站点自动签到",
    version: "1.0.0",
    description: "每日自动遍历已启用 PT 站点执行签到维护。",
    authors: ["crawler-media"],
    license: "MIT",
    package_url: "https://plugins.example.test/dist/cm.checkin-1.0.0.cmplugin",
    signature_url: "https://plugins.example.test/dist/cm.checkin-1.0.0.cmplugin.minisig",
    sha256: "521652819eef6e35b0ab63206ae66dd30fdad3e472868d58d5c03ca748a58d08",
    capabilities: ["read:sites", "command:site.maintenance"],
    homepage: "https://plugins.example.test/cm.checkin",
    installed_version: null,
    lifecycle: null,
    update_available: false,
    ...overrides,
  };
}

function renderWithProviders(ui: React.ReactElement, initialRoute = "/settings/plugins") {
  return render(
    <MemoryRouter initialEntries={[initialRoute]}>
      <FeedbackProvider>
        <SessionProvider value={{ session: ADMIN_SESSION, setSession: () => {}, loginEpoch: "mkt-1" }}>
          {ui}
        </SessionProvider>
      </FeedbackProvider>
    </MemoryRouter>,
  );
}

test("PluginManagerView Sources & Keys tab manages custom repositories, developer mode, and Ed25519 trust keys", async () => {
  const origFetch = globalThis.fetch;
  let repos: PluginRepositoryView[] = [
    {
      id: "official",
      name: "crawler-media Official Plugins",
      index_url: "https://raw.githubusercontent.com/jianxcao/crawler-media-plugins/main/registry/index.json",
      enabled: true,
      builtin: true,
    },
  ];
  let devMode = false;
  let settingsRev = 1;
  let trustKeys: TrustKeyView[] = [
    {
      key_id: "860ADD6B5472C243",
      label: "crawler-media Official Plugins",
      public_key: "untrusted comment: official\nRWRDwnJUa90Khi7jSeMH9D+T2XVsggC75qPaPKucTtXW0e2m0fqxWAbu\n",
      fingerprint: "f".repeat(64),
      status: "trusted",
      revision: 1,
      associated_package_count: 2,
      created_at: "2026-11-27T08:00:00Z",
      created_by: ADMIN_SESSION.id,
      revoked_at: null,
      revoked_by: null,
      revoked_reason: null,
    },
  ];
  const savedRepoPayloads: Array<{ repos: PluginRepositoryView[]; idempotencyKey: string | null }> = [];
  const reauthPurposes: string[] = [];

  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    const pathname = new URL(url, "http://localhost").pathname;
    const method = (init?.method ?? "GET").toUpperCase();
    const headers = new Headers(init?.headers);
    const body = typeof init?.body === "string" ? JSON.parse(init.body) : null;

    if (pathname === "/api/v1/plugins" && method === "GET") {
      return jsonResp([makeSummary({ id: "builtin.nexusphp-maintenance", name: "NexusPHP Site Maintenance", source: "builtin", enabled: true, lifecycle: "enabled" })]);
    }
    if (pathname === "/api/v1/plugin-repositories" && method === "GET") {
      return jsonResp({ repositories: repos });
    }
    if (pathname === "/api/v1/plugin-repositories" && method === "PUT") {
      repos = body.repositories;
      savedRepoPayloads.push({ repos, idempotencyKey: headers.get("Idempotency-Key") });
      return jsonResp({ repositories: repos });
    }
    if (pathname === "/api/v1/settings/plugins" && method === "GET") {
      return jsonResp({ developer_mode: devMode, revision: settingsRev, updated_at: null, updated_by: null });
    }
    if (pathname === "/api/v1/plugin-trust-keys" && method === "GET") {
      return jsonResp(trustKeys);
    }
    if (pathname === "/api/v1/plugin-reauth" && method === "POST") {
      reauthPurposes.push(body.purpose);
      return jsonResp({ reauth_token: `tok-${body.purpose}`, expires_at: "2026-11-27T12:00:00Z", purpose: body.purpose });
    }
    if (pathname === "/api/v1/settings/plugins/developer/enable" && method === "POST") {
      devMode = true;
      settingsRev += 1;
      return jsonResp({ developer_mode: true, revision: settingsRev, updated_at: "2026-11-27T09:00:00Z", updated_by: ADMIN_SESSION.id });
    }
    if (pathname === "/api/v1/plugin-trust-keys/prepare" && method === "POST") {
      return jsonResp({
        preparation_id: "prep-key-1",
        key_id: "1122334455667788",
        label: body.label,
        public_key: body.public_key,
        fingerprint: "d".repeat(64),
        consent_token: "consent-key-1",
        expires_at: "2026-11-27T12:00:00Z",
      });
    }
    if (pathname === "/api/v1/plugin-trust-keys" && method === "POST") {
      const created: TrustKeyView = {
        key_id: "1122334455667788",
        label: String(body.label ?? "community-key"),
        public_key: String(body.public_key ?? ""),
        fingerprint: String(body.fingerprint ?? "d".repeat(64)),
        status: "trusted",
        revision: 1,
        associated_package_count: 0,
        created_at: "2026-11-27T09:00:00Z",
        created_by: ADMIN_SESSION.id,
        revoked_at: null,
        revoked_by: null,
        revoked_reason: null,
      };
      trustKeys = [...trustKeys, created];
      return jsonResp(created, 201);
    }
    throw new Error(`Unexpected fetch ${method} ${url}`);
  }) as typeof fetch;

  try {
    renderWithProviders(<PluginManagerView />);
    await waitFor(() => assert.ok(screen.getByText("NexusPHP Site Maintenance")));

    fireEvent.click(screen.getByRole("tab", { name: "仓库与信任密钥" }));
    await waitFor(() => {
      assert.ok(screen.getAllByText("crawler-media Official Plugins").length >= 1);
      const toggleBtn = screen.getAllByRole("button", { name: "已启用" })[0] as HTMLButtonElement;
      assert.equal(toggleBtn.disabled, false);
    });

    fireEvent.change(screen.getByPlaceholderText("仓库 ID (如 community)"), { target: { value: "community" } });
    fireEvent.change(screen.getByPlaceholderText("显示名称"), { target: { value: "社区插件源" } });
    fireEvent.change(screen.getByPlaceholderText("https://.../registry/index.json"), {
      target: { value: "https://community.example.test/registry/index.json" },
    });
    fireEvent.click(screen.getByRole("button", { name: "添加自定义仓库" }));

    await waitFor(() => assert.equal(savedRepoPayloads.length, 1));
    assert.ok(savedRepoPayloads[0].idempotencyKey, "PUT /plugin-repositories must send Idempotency-Key");
    assert.equal(savedRepoPayloads[0].repos.length, 2);
    assert.equal(savedRepoPayloads[0].repos[1].id, "community");
    await waitFor(() => assert.ok(screen.getByText("社区插件源")));

    // Toggle custom repository off
    const toggleButtons = screen.getAllByRole("button", { name: "已启用" });
    fireEvent.click(toggleButtons[1]);
    await waitFor(() => assert.equal(savedRepoPayloads.length, 2));
    assert.equal(savedRepoPayloads[1].repos[1].enabled, false);

    // Enable developer mode with reauth
    fireEvent.click(screen.getByRole("button", { name: "开启开发者模式…" }));
    const devDialog = await screen.findByRole("dialog", { name: "开启开发者模式" });
    fireEvent.change(within(devDialog).getByPlaceholderText("管理员密码"), { target: { value: "admin-secret" } });
    fireEvent.click(within(devDialog).getByRole("button", { name: "确认开启" }));
    await waitFor(() => assert.ok(screen.getByText("已开启")));
    assert.ok(reauthPurposes.includes("developer-mode:enable"));

    // Add Ed25519 trust key (prepare -> reauth -> create)
    fireEvent.click(screen.getByRole("button", { name: "添加信任密钥" }));
    const keyDialog = await screen.findByRole("dialog", { name: "添加 Ed25519 信任密钥" });
    fireEvent.change(within(keyDialog).getByPlaceholderText(/密钥备注名称/), { target: { value: "community-key" } });
    fireEvent.change(within(keyDialog).getByPlaceholderText(/粘贴 Minisign 公钥内容/), {
      target: { value: "untrusted comment: test\nRWS1122334455667788\n" },
    });
    fireEvent.click(within(keyDialog).getByRole("button", { name: "解析并校验公钥" }));
    await waitFor(() => assert.ok(within(keyDialog).getByText(/1122334455667788/)));
    fireEvent.change(within(keyDialog).getByPlaceholderText("管理员密码二次验证"), { target: { value: "admin-secret" } });
    fireEvent.click(within(keyDialog).getByRole("button", { name: "确认添加信任密钥" }));
    await waitFor(() => assert.ok(screen.getByText("community-key")));
    assert.ok(reauthPurposes.includes("trust-key:create"));
  } finally {
    cleanup();
    globalThis.fetch = origFetch;
  }
});

test("PluginMarketTab supports repo switching, filtering, 1-click install, and upgrade with state-reset retry", async () => {
  const origFetch = globalThis.fetch;
  let installed: PluginSummaryView[] = [
    makeSummary({ id: "cm.boost", name: "自动刷流", version: "1.0.0", lifecycle: "enabled", enabled: true, revision: 2 }),
  ];
  let prepareUpgradeAttempts = 0;
  let changedCalls = 0;
  const apiCalls: Array<{ method: string; url: string; body: unknown }> = [];

  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    const method = (init?.method ?? "GET").toUpperCase();
    const body = typeof init?.body === "string" ? JSON.parse(init.body) : null;
    apiCalls.push({ method, url, body });

    if (url.endsWith("/api/v1/plugin-repositories")) {
      return jsonResp({
        repositories: [
          { id: "official", name: "crawler-media Official Plugins", index_url: "https://plugins.example.test/index.json", enabled: true, builtin: true },
          { id: "mirror", name: "Mirror Repo", index_url: "https://mirror.example.test/index.json", enabled: true, builtin: false },
        ],
      });
    }
    if (url.endsWith("/api/v1/plugin-repositories/official/catalog")) {
      return jsonResp({
        repo_id: "official",
        repo_name: "crawler-media Official Plugins",
        plugins: [
          makeCatalogEntry({ id: "cm.checkin", name: "站点自动签到", version: "1.0.0" }),
          makeCatalogEntry({
            id: "cm.boost",
            name: "自动刷流",
            description: "自动管理 PT 刷流配额与磁盘水位。",
            version: "1.1.0",
            installed_version: "1.0.0",
            lifecycle: "enabled",
            update_available: true,
            capabilities: ["read:sites", "read:boost", "command:downloader.delete-files"],
          }),
        ],
      });
    }
    if (url.endsWith("/api/v1/plugin-repositories/mirror/catalog")) {
      return jsonResp({
        repo_id: "mirror",
        repo_name: "Mirror Repo",
        plugins: [makeCatalogEntry({ id: "org.mirror.notifier", name: "Mirror Notifier", version: "0.9.0", capabilities: ["kv"] })],
      });
    }
    if (url.endsWith("/api/v1/plugin-installations/prepare")) {
      return jsonResp({
        operation_id: "op-prep-inst",
        state: "succeeded",
        result: {
          preparation_id: "prep-inst-1",
          plugin_id: "cm.checkin",
          version: "1.0.0",
          package_hash: "h-checkin-1",
          signature_status: "signed",
          signing_key_id: "860ADD6B5472C243",
          fingerprint: "f".repeat(64),
          consent_token: "consent-inst-1",
          expires_at: "2026-11-27T12:00:00Z",
          grant_diff: {
            added: [{ capability: "read:sites", scope: "Global", sensitive: false }],
            kept: [],
            removed: [],
            required: ["read:sites"],
          },
          impact: { summary: "将安装官方签名插件 cm.checkin", requires_reauth: false, warns: [] },
        },
      }, 202);
    }
    if (url.endsWith("/api/v1/plugin-installations")) {
      installed = [...installed, makeSummary({ id: "cm.checkin", version: "1.0.0", revision: 1 })];
      return jsonResp({ operation_id: "op-exec-inst", state: "succeeded" }, 202);
    }
    if (url.endsWith("/api/v1/plugins/cm.checkin") && method === "GET") {
      return jsonResp(installed.find((p) => p.id === "cm.checkin"));
    }
    if (url.endsWith("/api/v1/plugins/cm.checkin/enable")) {
      installed = installed.map((p) => (p.id === "cm.checkin" ? { ...p, enabled: true, lifecycle: "enabled", revision: 2 } : p));
      return jsonResp({ operation_id: "op-enable-chk", state: "succeeded" }, 202);
    }
    if (url.endsWith("/api/v1/plugins/cm.boost") && method === "GET") {
      return jsonResp(installed.find((p) => p.id === "cm.boost"));
    }
    if (url.endsWith("/api/v1/plugins/cm.boost/config")) {
      return jsonResp({ plugin_id: "cm.boost", schema_revision: "s1", revision: 1, values: { dry_run: true }, secrets: {} });
    }
    if (url.endsWith("/api/v1/plugins/cm.boost/upgrades/prepare")) {
      prepareUpgradeAttempts += 1;
      if (prepareUpgradeAttempts === 1) {
        return new Response(
          JSON.stringify({ ok: false, error: { code: "plugin.state_schema_incompatible", message: "State schema 1 -> 2 需重置私有状态" } }),
          { status: 409, headers: { "Content-Type": "application/json" } },
        );
      }
      return jsonResp({
        operation_id: "op-prep-up",
        state: "succeeded",
        result: {
          preparation_id: "prep-up-1",
          plugin_id: "cm.boost",
          target_version: "1.1.0",
          version: "1.1.0",
          package_hash: "h-boost-110",
          signature_status: "signed",
          signing_key_id: "860ADD6B5472C243",
          state_policy: body.state_policy,
          target_config_digest: "d".repeat(64),
          consent_token: "consent-up-1",
          expires_at: "2026-11-27T12:00:00Z",
          state_diff: { old_state_schema: 1, new_state_schema: 2, incompatible: true, reset_preview: { kv_entries: 3, blob_entries: 0 } },
          config_diff: { old_schema_revision: "abcd1234", new_schema_revision: "ef567890", existing_valid: true, errors: [] },
          grant_diff: { added: [], kept: [{ capability: "read:boost", scope: "Global", sensitive: false }], removed: [], required: ["read:boost"] },
          impact: { summary: "升级 cm.boost 至 v1.1.0", requires_reauth: true, warns: [] },
        },
      }, 202);
    }
    if (url.endsWith("/api/v1/plugin-reauth")) {
      return jsonResp({ reauth_token: "reauth-upgrade-1", expires_at: "2026-11-27T12:00:00Z", purpose: body.purpose });
    }
    if (url.endsWith("/api/v1/plugins/cm.boost/upgrade")) {
      installed = installed.map((p) => (p.id === "cm.boost" ? { ...p, version: "1.1.0", revision: 3 } : p));
      return jsonResp({ operation_id: "op-exec-up", state: "succeeded" }, 202);
    }
    throw new Error(`Unexpected fetch ${method} ${url}`);
  }) as typeof fetch;

  try {
    renderWithProviders(
      <PluginMarketTab installedPlugins={installed} onChanged={() => { changedCalls += 1; }} onOpenSources={() => {}} />,
    );

    await waitFor(() => assert.ok(screen.getByText("站点自动签到")));
    assert.ok(screen.getByText("自动刷流"));
    assert.ok(screen.getByText("可升级 v1.0.0 → v1.1.0"));

    // 1. Switch repository and search
    fireEvent.change(screen.getByLabelText("选择插件仓库"), { target: { value: "mirror" } });
    await waitFor(() => assert.ok(screen.getByText("Mirror Notifier")));
    fireEvent.change(screen.getByLabelText("选择插件仓库"), { target: { value: "official" } });
    await waitFor(() => assert.ok(screen.getByText("站点自动签到")));
    fireEvent.change(screen.getByPlaceholderText("搜索插件名称、ID 或说明…"), { target: { value: "签到" } });
    assert.ok(screen.getByText("站点自动签到"));
    assert.equal(screen.queryByText("自动刷流") === null, true);

    // 2. 1-click install from repo
    fireEvent.click(screen.getByRole("button", { name: "安装" }));
    const instDialog = await screen.findByRole("dialog", { name: "确认安装插件" });
    await waitFor(() => assert.ok(within(instDialog).getByText("cm.checkin · v1.0.0")));
    assert.ok(within(instDialog).getByText(/已签名验证 · 860ADD6B5472C243/));
    fireEvent.click(within(instDialog).getByRole("button", { name: "确认安装" }));
    await waitFor(() => assert.equal(screen.queryByRole("dialog", { name: "确认安装插件" }) === null, true));
    assert.equal(changedCalls, 1);

    const prepInstCall = apiCalls.find((c) => c.url.endsWith("/api/v1/plugin-installations/prepare"));
    assert.deepEqual(prepInstCall?.body, { repo_id: "official", plugin_id: "cm.checkin", version: "1.0.0" });
    assert.ok(apiCalls.some((c) => c.url.endsWith("/api/v1/plugins/cm.checkin/enable")));

    // 3. 1-click upgrade with incompatible state retry + reset reauth
    fireEvent.change(screen.getByPlaceholderText("搜索插件名称、ID 或说明…"), { target: { value: "" } });
    fireEvent.click(await screen.findByRole("button", { name: "升级到 v1.1.0" }));
    const upDialog = await screen.findByRole("dialog", { name: "确认升级插件" });
    await waitFor(() => assert.ok(within(upDialog).getByRole("alert")));
    fireEvent.click(within(upDialog).getByRole("button", { name: "以重置状态模式重试" }));
    await waitFor(() => assert.ok(within(upDialog).getByText(/将清空 KV 3 项/)));
    fireEvent.change(within(upDialog).getByLabelText(/管理员密码二次验证/), { target: { value: "admin-pw" } });
    fireEvent.click(within(upDialog).getByRole("button", { name: "确认升级" }));
    await waitFor(() => assert.equal(screen.queryByRole("dialog", { name: "确认升级插件" }) === null, true));
    assert.equal(changedCalls, 2);
  } finally {
    cleanup();
    globalThis.fetch = origFetch;
  }
});

test("PluginUploadModal stages local .cmplugin/.minisig multipart upload and supports switching to upgrade", async () => {
  const origFetch = globalThis.fetch;
  const uploadState: {
    captured: { fields: string[]; hasFile: boolean; hasSig: boolean; ifMatch: string | null; idem: string | null } | null;
  } = { captured: null };
  let completedCalls = 0;

  globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
    const method = (init?.method ?? "GET").toUpperCase();
    const headers = new Headers(init?.headers);
    const body = typeof init?.body === "string" ? JSON.parse(init.body) : null;

    if (url.endsWith("/api/v1/plugin-reauth")) {
      return jsonResp({ reauth_token: `tok-${body.purpose}`, expires_at: "2026-11-27T12:00:00Z", purpose: body.purpose });
    }
    if (url.endsWith("/api/v1/plugin-uploads")) {
      const form = init?.body as FormData;
      const fields: string[] = [];
      form.forEach((_v, k) => fields.push(k));
      uploadState.captured = {
        fields,
        hasFile: form.get("file") instanceof Blob,
        hasSig: form.get("signature") instanceof Blob,
        ifMatch: headers.get("If-Match"),
        idem: headers.get("Idempotency-Key"),
      };
      return jsonResp({
        upload_id: form.get("upload_id"),
        hash: "9".repeat(64),
        signature_status: "signed",
        signing_key_id: "860ADD6B5472C243",
        fingerprint: "e".repeat(64),
        expires_at: "2026-11-27T12:00:00Z",
      }, 201);
    }
    if (url.endsWith("/api/v1/plugin-installations/prepare")) {
      return jsonResp({
        operation_id: "op-up-inst-prep",
        state: "succeeded",
        result: {
          preparation_id: "prep-u-1",
          plugin_id: "cm.checkin",
          version: "1.2.0",
          package_hash: "9".repeat(64),
          signature_status: "signed",
          signing_key_id: "860ADD6B5472C243",
          fingerprint: "e".repeat(64),
          consent_token: "consent-u-1",
          expires_at: "2026-11-27T12:00:00Z",
          grant_diff: { added: [], kept: [{ capability: "read:sites", scope: "Global", sensitive: false }], removed: [], required: ["read:sites"] },
          impact: { summary: "本地包预检通过", requires_reauth: false, warns: [] },
        },
      }, 202);
    }
    if (url.endsWith("/api/v1/plugins/cm.checkin") && method === "GET") {
      return jsonResp(makeSummary({ id: "cm.checkin", version: "1.0.0", revision: 4 }));
    }
    if (url.endsWith("/api/v1/plugins/cm.checkin/config")) {
      return jsonResp({ plugin_id: "cm.checkin", schema_revision: "s1", revision: 1, values: { interval_secs: 86400 }, secrets: {} });
    }
    if (url.endsWith("/api/v1/plugins/cm.checkin/upgrades/prepare")) {
      return jsonResp({
        operation_id: "op-u-up-prep",
        state: "succeeded",
        result: {
          preparation_id: "prep-u-up-1",
          plugin_id: "cm.checkin",
          target_version: "1.2.0",
          version: "1.2.0",
          package_hash: "9".repeat(64),
          signature_status: "signed",
          signing_key_id: "860ADD6B5472C243",
          state_policy: "preserve",
          target_config_digest: "c".repeat(64),
          consent_token: "consent-u-up-1",
          expires_at: "2026-11-27T12:00:00Z",
          state_diff: { old_state_schema: 1, new_state_schema: 1, incompatible: false },
          config_diff: { old_schema_revision: "11112222", new_schema_revision: "33334444", existing_valid: true, errors: [] },
          grant_diff: { added: [], kept: [{ capability: "read:sites", scope: "Global", sensitive: false }], removed: [], required: ["read:sites"] },
          impact: { summary: "升级到 v1.2.0", requires_reauth: false, warns: [] },
        },
      }, 202);
    }
    if (url.endsWith("/api/v1/plugins/cm.checkin/upgrade")) {
      return jsonResp({ operation_id: "op-u-up-exec", state: "succeeded" }, 202);
    }
    if (url.endsWith("/api/v1/plugins/cm.checkin/enable")) {
      return jsonResp({ operation_id: "op-u-en", state: "succeeded" }, 202);
    }
    throw new Error(`Unexpected fetch ${method} ${url}`);
  }) as typeof fetch;

  try {
    renderWithProviders(
      <PluginUploadModal
        open={true}
        onClose={() => {}}
        installedPlugins={[makeSummary({ id: "cm.checkin", version: "1.0.0", revision: 4 })]}
        onCompleted={() => { completedCalls += 1; }}
      />,
    );

    const dialog = await screen.findByRole("dialog", { name: "本地上传安装或升级插件" });
    const fileInputs = dialog.querySelectorAll('input[type="file"]');
    assert.equal(fileInputs.length, 2);

    const pkgFile = new File(["PK\x03\x04dummy"], "cm.checkin-1.2.0.cmplugin", { type: "application/zip" });
    const sigFile = new File(["untrusted comment: sig\nRWS...\n"], "cm.checkin-1.2.0.cmplugin.minisig", { type: "text/plain" });
    fireEvent.change(fileInputs[0], { target: { files: [pkgFile] } });
    fireEvent.change(fileInputs[1], { target: { files: [sigFile] } });
    fireEvent.change(within(dialog).getByLabelText(/管理员密码二次验证/), { target: { value: "admin-pw" } });
    fireEvent.submit(within(dialog).getByRole("button", { name: "上传并预检" }).closest("form")!);

    await waitFor(() => assert.ok(within(dialog).getByText(/插件 cm.checkin 已存在/)));
    const captured = uploadState.captured;
    assert.ok(captured);
    assert.equal(captured.ifMatch, '"0"');
    assert.ok(captured.idem);
    assert.equal(captured.hasFile, true);
    assert.equal(captured.hasSig, true);
    assert.ok(captured.fields.includes("upload_id"));
    assert.ok(captured.fields.includes("reauth_token"));

    fireEvent.click(within(dialog).getByRole("button", { name: "转为升级预览" }));
    await waitFor(() => assert.ok(within(dialog).getByText("状态与配置迁移预览")));
    fireEvent.click(within(dialog).getByRole("button", { name: "确认升级" }));
    await waitFor(() => assert.equal(completedCalls, 1));
  } finally {
    cleanup();
    globalThis.fetch = origFetch;
  }
});
