# 插件系统实施状态（Phase 1）

更新：2026-11-23（当前开发分支 `plugin_system_development`）。本文准确记录已交付并经验证的 Phase 1 能力、架构约束与仍保持关闭的 Phase 2 范围。

## 1. Phase 1 总体交付（#1–#26）

| 阶段 | 交付内容 | 核心代码位置 |
|---|---|---|
| P0 | WIT 合同 (`plugin-api`)、Wasmtime 50.0.1 沙箱执行 (`plugin-host`)、共享配额与递归拒绝、类型化 `hooks` 通道 | `crates/plugin-api`、`crates/plugin-host`、`crates/hooks` |
| P1 | `.cmplugin` 确定性包与 Ed25519/Minisign 验签 (`plugin-package`)、Trust Store 与 Developer Mode、完整生命周期 (`install`/`enable`/`disable`/`upgrade`/`rollback`/`uninstall`/`cancel`/`state-reset`/`dev`)、分阶段包落盘与版本目录切换 (`staging.rs`、`upgrade_switch.rs`)、崩溃/重启清理恢复 (`recovery.rs`)、Scoped KV/Blob 与 CAS 配额、`jobs.db` 任务投影与因果链防环 (`cause_depth <= 4`)、`/api/v1/plugin-*` 管理 HTTP 接口 | `crates/plugin-package`、`crates/store/src/plugins/`、`crates/jobs/`、`crates/api/src/{plugins,http/plugins}/` |
| P2 | 原子 `indexer::RegistrySnapshot` 与 YAML 命名空间 (`plugin:<id>/<provider>`)、`net-guard` 出站策略与 `CredentialSlot`、WASM `indexer-plugin` & `maintenance-plugin` 世界与 `SealedRefStore` (`dlref-*`)、窄 `BrowserSession` 三重门禁 (内置/Admin 可信 + `browser:*` Site 授权 + CDP 与站点 opt-in)、`site_maintenance_receipts` 与 `builtin.checkin` / `builtin.nexusphp-maintenance` | `crates/net-guard`、`crates/indexer`、`crates/api/src/plugins/{builtins,browser_adapter,browser_check}.rs`、`crates/api/src/site_maintenance.rs` |
| P3–P4 | 11 个核心工作流 Hook（`search.before`、`search.site.after`、`search.after`、`filter.after`、`subscribe.score`、`subscribe.select`、`download.before`、`download.after`、`transfer.rename`、`transfer.completed`、`site.checkin.after`）与 `GET /plugin-hook-catalog`、严格 `.torrent` bencode 元信息解析（i64 溢出与 10 MiB 上限，禁止 `urls=` 回退）、`TorrentSource::PluginRef` 跨入口解析、统一 `DownloadSubmitter` 覆盖全部下载入口、所有权与预算预留及 `Unknown` 对账、`builtin.boost`（`boost.snapshot`、`brush-state`、`cleanup-preview`、`downloader.*`、`boost.run`） | `crates/downloader/`、`crates/api/src/download_service/`、`crates/api/src/plugins/boost/`、`crates/subscribe/`、`crates/api/src/admit.rs` |
| P5 | `transfer.rename` / `transfer.completed` 移至 `recorder.prepare` 之前且在 `LibraryWorkLease` 之外释放、重命名后基于磁盘身份重校验、ADR-0012 Library 删除取消与孤儿 Media/收藏/进度清理 | `crates/library/`、`crates/api/src/plugins/transfer_rename.rs`、`crates/api/src/worker/transfer/` |
| P6–P7 | 事务性 `plugin_outbox`（`app` / `library` / `subscribe`）事件分发与重启 keyset 对账、`PluginUiTrust` 按包 digest 前端信任与 Reauth、`mc_session` Cookie-only 描述符核对与 `/api/v1/plugin-ui-assets/*` 鉴权静态流、`sdk/plugin-ui` + `sdk/plugin-ui-kit` + React Module Federation 宿主、内置 Check-in/Boost UI 包、`tools/plugin` (`plugin-cli`) 开发者工具链与 `dev` 热重载 | `crates/api/src/plugins/{events,event_builders,reconcile,ui_ops,ui_queries,ui_trust_ops,ui_builtins,ui_builtins_assets,dev_sideload}.rs`、`sdk/plugin-ui*`、`web/components/plugins/`、`tools/plugin/` |

## 2. 关键安全与并发不变量

- **Zero `.await`/IO/WASM under Locks**：Store writer 互斥锁按库（`app` / `subscribe` / `library` / `cache`）及 `jobs.db` 独立隔离，不嵌套持锁；任何互斥锁及 `LibraryWorkLease` 均不跨 `.await`、网络请求、文件 IO 或 WASM 调用持有；API 只读路径走统一 `ReaderPool`。
- **出站与凭据隔离 (`net-guard`)**：WASM Guest 永不接触原始 Site `cookie` / `api_key` / `cdp_url` / 代理密码；`CredentialSlot` 仅对绑定 Origin 的 `SiteRelative` 注入，跨 Origin 重定向自动剥离敏感头且禁用 `urls=` 下载回退；公共 URL 受 Manifest `[[http]]` 精确匹配与 5 MiB 响应上限约束。
- **Browser 隔离三门禁**：第三方未在实例可信名单中的插件无法开启 `BrowserSession`；启用还必须同时满足全局 CDP 开关、站点 `maintenance_via=browser` 或 `browser_search=true`，以及对应站点作用域的 `browser:maintenance` / `browser:search` 授权（`builtin.checkin` 在 Manifest 中声明 `browser:maintenance`，默认不自动全局授权，按站点显式授权）。
- **UI 同页面联邦边界**：React Module Federation 远程代码属于与当前 Admin 同浏览器上下文的受信任前端代码，通过独立 `plugin_ui_trust(plugin_id, package_sha256, bundle_sha256, granted_by_user_id, granted_at, revoked_at, revision)` 授权、Cookie-only `subject_user_id` 预检、`/api/v1/plugin-ui-assets/*`（`nosniff` + `Cache-Control: private, no-store`）和 `X-Plugin-Package` / `X-Plugin-Generation` 来源校验协同防护；静态媒体图片端点（`/posters/*`、`/fanart/*`、`/stills/*`、`/chapters/*`、`/libraries/{id}/cover`）保持公开未鉴权语义。
- **探测限流常量锁定**：`DEFAULT_REMOTE_READ_LIMIT = 1` 与 `DEFAULT_REMOTE_READ_COOLDOWN = Duration::from_millis(500)`（`crates/marker/src/media_read.rs`；`ProbeSettings::default()` 位于 `crates/api/src/probe_settings.rs`）保持未修改。

## 3. 开发者 CLI 与 SDK

`tools/plugin`（crate `plugin-cli`，集成于 `crawler-media plugin` 子命令）提供完整插件工程命令：

```sh
crawler-media plugin scaffold [--world hook-plugin|task-plugin|endpoint-plugin|indexer-plugin|maintenance-plugin] [--api 1.0.0] [--ui react] [--id <id>] [--name <name>] <dir>
crawler-media plugin validate <dir-or-.cmplugin>
crawler-media plugin test <dir> [--offline]
crawler-media plugin build <dir>
crawler-media plugin pack <dir> --output <out.cmplugin>
crawler-media plugin sign <pkg.cmplugin> --key <key_file>
crawler-media plugin dev <dir> --instance <url> [--token <token>] [--once|--watch]
```

本地 `sdk/` workspace 包含 `crawler-plugin-sdk`、`policy-fixture`、`task-fixture`、`endpoint-fixture`、`indexer-fixture`、`maintenance-fixture`、`plugin-ui` 与 `plugin-ui-kit`。

## 4. 明确延迟到 Phase 2 的范围（未开放）

以下能力在 Phase 1 中刻意不交付：manifest 中声明被明确拒绝，且无对应后端/UI/E2E 注册入口：
- `media-provider`（刮削/ catalog 来源插件）
- `downloader-plugin`（下载器适配器插件）
- `notification-plugin`（通知渠道插件）
- `site.create.before`、`site.request.before`、`subscribe.create.before`、`playback.started`、`scrape.before` 等 Phase 2 扩展 Hook
