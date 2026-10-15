# 插件系统 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
> 使用当前环境可用的等价执行技能；不安装不存在的技能。本文是待 review 计划，**全部实现未开始**。修订：r5（对照 `plugin-design-review-r5.md`，见文末「r5 修订记录」）。

**Goal:** 按 [插件系统设计](<../specs/2026-10-15-plugin-system-design.md>)（r4）与 [ADR-0013](<../../adr/0013-wasm-plugin-runtime.md>) 分阶段交付 WASM 组件插件：Hook 总线、自定义索引器、下载干预、榜单订阅、站点/订阅写 API、分发与前端。

**Architecture:** 同步 wasmtime + `PluginHost` 分类有界线程池；固定/分 world WIT + 拒绝桩；`crates/net-guard` 统一出站；`IndexerProvider` / `DownloadService` / `ListProvider` 宿主抽象先于运行时落地。

**Tech Stack:** Rust 2024（MSRV 由 `rust-toolchain.toml` 固定，与 CI 一致）、wasmtime（组件模型，同步；M1a-3 锁定精确版本，rustc 1.85 下最高 31）、wit-bindgen（锁定精确版本，与 `Cargo.lock` 中间接依赖协调）、ureq 3.4.1、rusqlite、arc-swap、aho-corasick、minisign；前端 Vite 7 + React 19 + react-router 7。

**Spec:** [设计](<../specs/2026-10-15-plugin-system-design.md>)；[ADR-0013](<../../adr/0013-wasm-plugin-runtime.md>)。

## Global Constraints

1. 用 [CONTEXT](<../../../CONTEXT.md>) 词汇；domain 无 IO；`store` 不依赖 `hooks`/`plugin-host`；事件在应用服务层事务提交后发出。
2. 不在 tokio worker 上同步调用插件；`BLOCKING_OK` 正向标记 + RAII 复位；release 构建同样检查。标记由 `crates/exec-ctx`（M0a-0，无 wasmtime）提供，`api`/`jobs`/`hooks` 只依赖它。
3. 第三方插件 `http` 在 net-guard 验收（M1a）前全局关闭；不发布 SDK / 不开放第三方分发，直至 M1a 全部验收 + M1b 凭据用例通过。
4. 每个改动 RED→GREEN；触及 crate `cargo test -p <crate>`；里程碑结束 `cargo test --workspace` 与 `pnpm -C web typecheck && pnpm -C web test`。
5. `.rs` ≤800 行，生产函数 ≤120 行；接近上限先拆分。
6. 测试不访问真实网络/生产 DB：fake Fetcher/Downloader/resolver、临时 SQLite、mock qB/Tr。
7. 日志不记录凭据、完整带 passkey 的 URL；插件 `log` 经脱敏。
8. 不创建 GitHub issue；只提交本任务文件。
9. 标 **【订阅】** 的任务可由订阅开发 agent 独立承接；其接口边界在任务中写明，不得越界修改 indexer/downloader 内部。

## Review Focus

1. 插件 panic/超时/busy 不让订阅或搜索失败（M0a/M1a）。
2. `profile()` 快照与 6 处调用方行为不变（M0b）。
3. 三条下载路径都经 `DownloadService`；插件来源禁 URL fallback；Tr 用 `metainfo`（M0b/M1a/M1c）。
4. net-guard：rebinding、映射地址、重定向、代理方案 A、私网授权固定（M1a）。
5. 凭据不出宿主：槽位、脱敏、https 强制（M1b）。
6. 榜单订阅幂等与防循环、用户删除不被重加（M2）。

规模估算：S ≈ ≤1 人日，M ≈ 2–4 人日，L ≈ 1–2 人周。

---

## 文件结构与公共接口地图

| 新增 / 主要修改 | 职责 |
|---|---|
| `crates/exec-ctx`（新，M0a） | `BlockingGuard`、`InPluginGuard`、`is_blocking_ok()`、`in_plugin()`、`spawn_blocking_marked` |
| `crates/hooks`（改） | `HookPipeline`、`HookKind{Filter,Gate,Notify}`、载荷、优先级、链路预算、熔断 |
| `crates/plugin-api`（新） | `wit/*.wit`、版本常量、生成绑定 |
| `crates/plugin-host`（新） | 引擎、实例池、分类池、导入实现、调度、审计、安装/签名 |
| `crates/net-guard`（新） | `NetPolicy`、`GuardedAgent`、resolver/connector、地址判定 |
| `crates/indexer`（改） | `IndexerProvider`、`YamlProvider`、`ArcSwap<ProfileSet>`、`snapshot()`、`capabilities()` |
| `crates/downloader`（改） | `DownloadSubmitter` 端口（`DownloadPlan`/`SubmitOutcome`/`Origin`）、`AddOptions{origin,…}`、`AddSource`、`add_with`、`set_tags/set_category`、`TorrentFetcher` 注入 |
| `crates/subscribe`（改） | 埋点、`SubscribeError::Denied`、defer 字段使用 |
| `crates/store`（改） | 迁移：plugin_*、downloader_added、list_*、private_grants、wanted defer 列 |
| `crates/jobs`（改） | `JobKind::{PluginTask, SiteStats, ListSync}` |
| `crates/api`（改） | `DownloadService`、插件管理 HTTP、端点路由、image_proxy 迁移 |
| `web/` | §19 页面与 `lib/api/plugins.ts` |

---

## M0a：Hook 总线（无 WASM）

依赖：无。总规模：L。

### Task M0a-0：`crates/exec-ctx` 执行上下文标记（S）
- 新 crate，无 wasmtime / tokio 以外依赖：`BlockingGuard::enter()`、`InPluginGuard::enter(id)`（RAII，drop 与 panic 均复位）、`is_blocking_ok()`、`in_plugin()`、`spawn_blocking_marked(f)`
- 接入：`crates/api/src/blocking_budget.rs:53/74`（`run`/`run_to_completion` 闭包入口）、`api/src/job_loop.rs:113`（job worker）、`http/downloaders/submission.rs:290`（`spawn_blocking` 处）
- `hooks` 定义 `trait PluginInvoker`（运行时注入，M1a 由 `plugin-host` 实现），`hooks` 不依赖 wasmtime
- 测试：复用线程后标记不泄漏；panic 后复位；`HookPipeline` 在未标记线程上返回 `WrongContext`
- 依赖：无

### Task M0a-1：`HookPipeline` 类型与合并语义（M）
- 文件：`crates/hooks/src/pipeline.rs`（新）、`lib.rs`、`bus.rs`（保留适配层）
- 新类型：`HookId`、`HookKind`、`FilterHook<T>`、`GateDecision{Allow,Deny(reason),Abstain,Defer(secs)}`、`ChainBudget`、`HookPipeline::{run_filter, run_gate, emit_notify}`
- 规则：优先级 `(priority, plugin_id)`；失败/超时=abstain；仅显式 Deny 阻断；`upgrade.decide`/`transfer.intercept` 只能收紧
- 测试：`hooks/tests/pipeline_merge.rs`（deny 优先、abstain 默认、panic hook 视为 abstain、链路预算耗尽后剩余 abstain）
- 验收：旧 `Bus` 测试（`bus_reentrancy.rs`、`login_checkin.rs`）全绿

### Task M0a-2：熔断与指标骨架（S）
- 文件：`crates/hooks/src/breaker.rs`
- 类型：`Breaker{Closed,HalfOpen{next_probe},Open}`；最小样本 20；区分 `PluginFault` / `UpstreamFault`
- 测试：半开指数退避、上游错误不计入
- 依赖：M0a-1

### Task M0a-3：Notify 有界队列（S）
- 文件：`crates/hooks/src/notify_queue.rs`
- 有界 MPSC + 固定 worker；按实体 key 保序；满即丢弃计数
- 测试：保序、丢弃计数

### Task M0a-4：共享 `PluginHost` 句柄占位 + 替换临时 Bus（S）
- 文件：`crates/api/src/check_in.rs:19`、`crates/api/src/http/sites.rs:481/525`、`AppState`
- `AppState.hooks: Arc<HookPipeline>`；删除临时 `Bus::new()`
- 测试：签到/站点验证路径仍触发 Login/CheckIn 钩子

### Task M0a-5【订阅】：订阅流程埋点与 `SubscribeError::Denied`（M）
- 文件：`crates/subscribe/src/{add.rs,collect.rs,types.rs,choose.rs}`
- `add.rs`：`subscribe.score`（admit 后）、`subscribe.select`（choose 后）。**不**在订阅 crate 埋 `download.before/after`——它们只在 `DownloadService` 触发一次（M0b-4，P5-H1）
- `collect.rs:376-383`：`transfer.intercept/completed/failed`；`:282`：`upgrade.decide`（不挂 495–510 二次确认）
- `types.rs`：`SubscribeError::Hook` → `Denied{plugin, reason}`
- 测试：`subscribe/tests/hook_points.rs`（每点触发一次、deny 中止、abstain 不影响、panic 不影响）
- 接口边界：只调用 `HookPipeline`，不改 indexer/downloader

### Task M0a-6【订阅】：订阅/站点领域事件（提交后发出）（M）
- 文件：`crates/api/src/subscribe_create.rs`、`http/subscriptions*`、`management/subscribes/*`、`http/sites.rs`
- 事件：`subscribe.added/updated/deleted/completed`、`site.added/updated/deleted/enabled-changed`
- 测试：事务回滚不发事件；提交后发一次

### Task M0a-7：`plugin_audit` 迁移（S）
- 迁移：`plugin_audit(id, plugin_id, action, target, detail_json, at)`
- 文件：`crates/store/src/schema/plugin_audit.rs`

**M0a 验收**：`exec-ctx` 已接入三处 `spawn_blocking` 入口，未标记线程调用 `HookPipeline` 返回 `WrongContext`；`cargo test -p hooks -p subscribe -p api` 绿；panic/超时 hook 不影响订阅；事件仅提交后发出。

---

## M0b：宿主抽象

依赖：M0a-1。总规模：L。

### Task M0b-1：`ProfileSet` Arc 化 + `snapshot()`（M）
- 文件：`crates/indexer/src/{profile.rs,engine.rs}`
- `ProfileSet { profiles: HashMap<String, Arc<Profile>> }`；`Indexer.profiles: ArcSwap<ProfileSet>`；`profile(&self,id)->Option<Arc<Profile>>`；`snapshot()->Arc<ProfileSet>`；`ProfileSet::extend(source, profiles)`（冲突默认拒绝、记录来源）
- 改 6 处调用方：`api/src/check_in.rs:25-26`、`:129-134`、`http/sites.rs:143/150/236`、`http/settings.rs:267`（`settings.rs:262-267` 改为取一次 `snapshot()`）；`catalog.rs` 用快照；测试 `indexer/tests/goldens.rs:279` 的 `profile()` 调用同步改签名
- 测试：替换 ProfileSet 后进行中的 fan-out 仍用旧快照；extend 冲突拒绝

### Task M0b-2：`IndexerProvider` + `YamlProvider` + `plugin:` 分派（M）
- 文件：`crates/indexer/src/provider.rs`（新）、`engine.rs`
- trait：`search/rss/detail/download_url/verify_login/check_in/user_stats/describe`；`ProviderRegistry`（`ArcSwap`）；`Indexer::capabilities(&Site)->Option<Capabilities>`
- 改 `check_in.rs` 分支为能力查询
- 测试：YAML 站点经 Provider 行为与改造前快照一致（golden）；`plugin:` 前缀未注册 → `SiteFailure`

### Task M0b-3：`AddOptions` / `AddSource` / `add_with`（M）
- 文件：`crates/downloader/src/{lib.rs,qbit.rs,transmission.rs,memory.rs}`
- `AddOptions{origin: Origin{Builtin, Plugin(String)},save_path,category,tags,paused,upload_limit,download_limit,sequential,first_last_piece}`；`AddSource{Url,ResolvedUrl,File(Vec<u8>),Magnet}`；`origin=Plugin` 时拒绝 `Url/ResolvedUrl`（不 fallback）；旧 `add/add_resolved/add_with_options` 默认转调
- qB：`File` 用内存 multipart part；Tr：`File` 用 `metainfo`(base64)
- 新增 `set_tags/set_category`
- 测试：mock qB 收到 multipart 文件；mock Tr 收到 `metainfo`；`origin=Plugin` + Url → 错误；旧签名行为不变

### Task M0b-4：`DownloadService`（同步核心 + async 外壳）（M）
- 端口：`crates/downloader/src/submit.rs`（新）定义 `trait DownloadSubmitter { fn submit(&self, plan: DownloadPlan, origin: Origin) -> Result<SubmitOutcome, SubmitError> }`
- 文件：`crates/api/src/download_service.rs`（新，`impl DownloadSubmitter`）；`subscribe/src/add.rs` 的 `admit_and_add` 由 `D: Downloader` 改为依赖 `&dyn DownloadSubmitter`（`RunInput` 增字段）；`http/downloaders/submission.rs:246`、`admit.rs` 改调用 service
- `submit_blocking(plan, origin)`：**唯一**触发 `download.before`（Gate+Filter）与 `download.after`（Notify）的位置；`submit_async` = `exec_ctx::spawn_blocking_marked`
- 测试：三条路径都经过 service（计数 fake）；订阅路径 `download.before` 只触发一次；手动提交被 deny 时不提交

### Task M0b-5：`downloader_added` 两阶段 + 对账（M）
- 迁移：`downloader_added(downloader_id, ownership_tag, info_hash NULL, source, plugin_id NULL, state, added_at, PRIMARY KEY(downloader_id, ownership_tag))`
- 文件：`crates/store/src/downloader_added.rs`、`api/src/download_service.rs`、`jobs`（每小时对账）
- 对账：按 tag/label 回填 info_hash；`submitting` >10min 且查无 → 删除；Tr 无 labels → 使用 `torrent-add` 返回的 `hashString`（`transmission.rs:250-258`）写入 info_hash 并按 hash 对账，无 hash 即 foreign；**不**按名称+大小匹配
- 测试：崩溃模拟（submitting 残留）、回填、无 labels 时按 hash 回填、无 hash 视为 foreign

### Task M0b-6【订阅】：wanted defer 列（S）
- 迁移：`subscribe_wanted` 增 `deferred_until`、`defer_count`
- 文件：`crates/store/src/subscribe_wanted.rs`、`subscribe/src/add.rs`（读取并跳过未到期）
- 测试：72h/10 次上限后转 reject

**M0b 验收**：YAML 站点行为不变；三条下载路径经 `DownloadService`；`cargo test --workspace` 绿。

---

## M1a：运行时与安全网

依赖：M0a、M0b。总规模：L+。

### Task M1a-1：`crates/net-guard`（L）
- `NetPolicy`、`GuardedAgent`、自定义 `Resolver` + `Connector`（connect 前再校验）、全局单播允许集、嵌入 IPv4 抽取、主机名规范化、端口规则、逐跳重定向（≤5，禁降级）、代理方案 A、`private_grants` 解析固定
- 迁移：`private_grants(scope, target, port, path_patterns, resolved_cidrs, granted_by, granted_at)`
- 测试：§6.2 第 10 点全部（rebinding、mapped/NAT64/6to4/Teredo、十进制/八进制主机、重定向内网、代理非确认主机、私网授权解析变化）

### Task M1a-2：既有出站迁移到 net-guard（M）
- `api/src/http/image_proxy.rs`（`fetch_image`，路由 `/api/v1/images/proxy`，插件来源签名参数）、`media_posters/http.rs:20`
- 插件来源 .torrent：**唯一路径**为 `DownloadService` 用 `GuardedAgent`（站点实例策略）抓取 → `AddSource::File`；种子内 `announce`/`announce-list`/`url-list`、magnet `tr=`/`ws=` 按地址判定校验（r4 R4-M5）
- `downloader/src/qbit.rs::download_torrent_bytes`：仅服务内置站点，改为调用宿主注入的 `TorrentFetcher`（`GuardedAgent`）
- `downloader/src/http.rs::agent_for_url`：**下载器实例策略**（只允许该实例 `host:port`），保留 `is_local_endpoint` 代理绕过；不套插件地址判定
- 测试：现有 image_proxy 测试保留；插件来源 qB 不 fallback；Tr 收 `metainfo`；种子含内网 announce → 拒绝；下载器在私网仍可连

### Task M1a-3：`crates/plugin-api` WIT（M）
- `wit/{types,log,kv,blob,config,clocks,http,hooks,lifecycle,…}.wit`；worlds `hook-plugin`/`indexer-plugin`/`list-plugin`/`channel-plugin`
- `wit/deps/` vendor `wasi:random@0.2.0` 等依赖（`wit-deps` 锁定）
- 版本锁：`wasmtime = "=31.x"`、`wit-bindgen = "=0.46.x"`（具体补丁号实施时定），新增 `rust-toolchain.toml`
- CI：`wasm-tools component wit` 校验 `wit/` 与**从设计 §18.3 抽取的代码块**
- 测试：用设计 §18.3–18.5（同 review r5 附录 A）最小样例，host 端 `bindgen!` 与 guest 端 `generate!` 均 `cargo check` 通过

### Task M1a-4：`crates/plugin-host` 引擎与分类池（L）
- `Engine`（同步、epoch、pooling）、`InstancePre` 缓存、`Module::serialize` 缓存
- 池：provider/hook/task/notify，有界队列，busy 语义；复用 `exec-ctx` 的 `InPluginGuard`/`BlockingGuard`（不重新定义）；实现 `hooks::PluginInvoker`；每调用从 `InstancePre` 新建实例；卡住线程计数
- 三重预算；`StoreLimits` 按运行时
- 测试：死循环 200ms 终止；队列满 busy；重入拒绝；非阻塞上下文 `WrongContext`（release）；卡住线程计数

### Task M1a-5：导入实现 + 拒绝桩（M）
- log（限速+脱敏）、kv/blob（`plugin_kv`、`plugin_blob`、`plugin_quota` 迁移）、config、clocks、http（allowlist + net-guard + response 资源）、notify.send
- 安装时 imports 静态比对；禁 `wasi:http/filesystem/sockets`
- 测试：未授权导入 → permission-denied；配额并发不超

### Task M1a-6：安装、签名、开发者模式（M）
- 迁移：`plugins(id, version, state, manifest_json, signed, enabled, …)`、`plugin_trust_keys`
- `.cmplugin` 解包（zip-slip/bomb 防护）、minisign 校验、本地信任库
- 设置：`plugins.developer_mode`、`plugins.third_party_network`
- 测试：zip-slip 拒绝；未签名需确认；关开发者模式停用未签名插件

### Task M1a-7：插件管理 HTTP API（M）
- 端点（全部 `require_admin` 组，`/api/v1` 前缀，`{ok,data}` 信封）：`GET /plugins`、`GET/PATCH /plugins/:id`、`/plugins/:id/config-schema`、`/plugins/:id/audit`、`/plugins/:id/metrics`、`POST /plugins/install`（仅 `.cmplugin`）、`POST /plugins/:id/upgrade`（返回权限 diff）、`POST /plugins/:id/rollback`、`DELETE /plugins/:id`、`POST /plugins/:id/tasks/:taskId/run`、`GET/PATCH /settings/plugins`、`POST /settings/plugins/developer/enable`；权限列表返回 `sensitive` 标记（由 §6.1 生成）
- 文件：`crates/api/src/http/plugins/*`；仅 Admin
- 测试：Member 403；配置 schema 校验

### Task M1a-8：`JobKind::PluginTask` + 热重载（S）
- `jobs/src/kinds.rs`；`schedule.register`；开发者模式文件监听
- 测试：cron 注册/注销；`max_tasks`

### Task M1a-9：Rust SDK + 示例（M）
- `sdk/crawler-plugin-sdk`（工作区内，暂不发布）；示例 `examples/plugins/keyword-blocklist`
- 测试：示例插件端到端 `search.after`

### Task FE0 / FE1a / FE1b：见「前端」节

**M1a 验收**：设计 §15 M1a 全部条目；第三方网络开关在未通过时强制 off。

---

## M1b：自定义索引器

进入条件：M1a net-guard 验收通过。依赖：M0b-1/2、M1a。总规模：L。

### Task M1b-1：`WasmProvider`（M）
- `crates/plugin-host/src/provider.rs`；`describe()` → 目录；`torrent-raw` → `Torrent` 校验
- 测试：fake guest 返回非法字段被拒；busy → `SiteFailure{busy}`

### Task M1b-2：凭据槽位注入与脱敏（M）
- 槽位：cookie/authorization/x-api-key/query 参数名；`{{` 拒绝；`auth=site` 强制 https；跨主机重定向禁止
- Aho-Corasick 脱敏（原文/URL 编码/HTML 实体/JSON 转义/base64）
- 测试：凭据回显用例全集

### Task M1b-3：Torznab 实例类型规则 + 私网授权 UI 后端（M）
- Jackett/Prowlarr/通用模式表；enclosure 凭据剥离重填
- API：站点创建支持 `profile_id: plugin:…`、`allow_private`
- 测试：Prowlarr `/{n}/api`、Jackett `/dl/`、路径外拒绝

### Task M1b-4：`search.before/after` 接入 + `add` 输出约束（S）
- Indexer 门面；`add` 仅自身 indexer；`rescore-cap`（区间 10%，下限 10）

### Task M1b-5：示例插件 torznab-bridge / public-bt / rss-only（M）

### Task FE1c（站点部分）

**M1b 验收**：Jackett/Prowlarr 站点参与订阅搜索；凭据用例全过。

---

## M1c：下载干预

进入条件：M1a net-guard 验收。依赖：M0b-3/4/5、M1a。总规模：L。

### Task M1c-1：`download.before/after` 真实接线（M，宿主 agent 承接；订阅 agent 仅负责 M0b-6 的 defer 读取/跳过）
- `DownloadService::submit_blocking` 内把 M0b-4 的占位管道换成插件调用；`download-plan` 修改校验（下载器存在、保存路径在允许根下）；defer 持久化
- 测试：modify/reject/defer；closed fail_mode → defer + 首次通知

### Task M1c-2：`downloader` 导入（read/control/add）（M）
- 复用 M1a-2 的唯一路径（宿主抓取 + `AddSource::File` + 种子内地址校验）；配额（每小时 10、200 GiB）
- 测试：插件 URL 不出现在下载器参数中

### Task M1c-3：删除护栏（M）
- delete（`downloader_added` 命中走 `remove_owned`）、回收站、20/次 200/日、`downloader:foreign`、`protect(hash, until)`
- 迁移：`downloader_protect(downloader_id, info_hash, until, plugin_id)`
- 测试：foreign 默认拒；保护期内拒删；超配额转待办

### Task M1c-4：`transfer.*` Gate/Notify 接线（S）【订阅】

### Task M1c-5：示例 downloader-router / free-only / auto-clean / hr-guard（M）

### Task FE1c（下载部分）

**M1c 验收**：设计 §15 M1c 条目。

---

## M2：榜单 + 站点 + 写操作

依赖：M1a；榜单依赖 M1a-3 的 `list-plugin` world。总规模：L+。

### Task M2-1：`ListProvider` + 内置迁移（M）
- `crates/media` 或 `api`：豆瓣/Bangumi/AniList 客户端实现 `ListProvider`；`WasmListProvider`
- 测试：分页、错误分类

### Task M2-2【订阅】：榜单订阅表与 `JobKind::ListSync`（L）
- 迁移：`list_subscriptions(...)`、`list_sync_items(list_sub_id, media_key, state, first_seen, last_action)`
- 流程：拉取 → 识别 → 去重（订阅/库/历史/忽略）→ 规则 → auto/suggest
- 幂等 `(source, media_key, season)`；配额 20/次 100/日；用户删除 → `user_removed`
- 测试：重复同步不重复建；删除后不重加；配额转 suggest

### Task M2-3【订阅】：`subscribe-write` 导入 + 防循环（M）
- 经应用服务层；`cause-chain` 深度 ≤3；插件不收自身事件；内联 Gate `try_acquire`
- 测试：两插件互写不死锁；环被丢弃

### Task M2-4【订阅】：`subscribe.episodes-refresh` / `completion-check`（S）

### Task M2-5：站点 API（read / ★credentials / write-session）+ 事件（M）
- 限频每站每小时 10；allowlist ⊆ 站点主机校验
- 测试：子集校验拒绝；限频

### Task M2-6：login/checkin/user-stats 能力 + `JobKind::SiteStats`（M）

### Task M2-7：`library-read` 只读视图（S）

### Task M2-8：`transfer.rename`（S）【订阅】

### Task M2-9：榜单 HTTP API（S）
- `GET/POST/PATCH/DELETE /list-subscriptions`、`/:id/pending`、`accept|ignore`；**全部 admin 路由**（v1 成员不可见，日后需 `allow_list_subscribe` 能力）

### Task FE2

**M2 验收**：豆瓣想看 → 自动订阅 → 下载入库；用户删除后不重加。

---

## M3：集成

依赖：M1a、M2。总规模：M+。
- M3-1 notify-channel 多渠道路由 + `notify.before`（M）
- M3-2 插件 HTTP 端点 `/api/v1/plugins/:id/endpoints/*`（user/admin/apikey，不走信封）+ apikey 轮换（M）
- M3-3 删除/历史联动事件（S）
- M3-4 `GET /plugins/:id/widgets`（admin 路由，首页仅 Admin 渲染）（S）
- M3-5 探索源复用 List Provider（S）
- FE3

## M4：分发与媒体

依赖：M1a-6。总规模：L。
- M4-1 远程仓库索引、TOFU 钉扎、升级/回滚、吊销同步、权限 diff；端点 `GET/POST/DELETE /plugin-repos`、`GET /plugin-repos/:id/index`、`POST /plugin-repos/:id/pin`（L）
- M4-2 媒体识别链（M）
- M4-3 媒体服务器事件 + `library.refresh`（★read:user-scoped，用户级授权）（M）
- FE2（市场部分）/ FE4

## M5：外部服务与交互

依赖：M3。总规模：L。
- M5-1 gRPC/JSON-RPC 传输层，复用调度/熔断（L）
- M5-2 Bot 命令与消息交互（M）

---

## 前端（`web/`）

| 任务 | 内容 | 文件 | 依赖 | 规模 |
|---|---|---|---|---|
| FE0 | 设置分区「插件」（「系统」组与 logs 并列）、`?tab=installed\|repos\|developer`、`main.tsx` 新增 `settings/plugins/:id`、Admin 门闸、`accessiblePathFor` 限定榜单路由 | `src/main.tsx`、`lib/mock-data.ts`、`lib/permissions.ts`、`components/plugins/plugins-section.tsx` | — | S |
| FE1a | 已安装列表、详情壳、启停、开发者模式（重新鉴权）、侧载上传（仅 `.cmplugin`）；`lib/api/plugins.ts` 按 `{ok,data}` 定义 | `lib/api/plugins.ts`、`components/plugins/*` | M1a-7 | M |
| FE1b | JSON Schema + x-ui 表单、权限确认（★高亮逐项勾选）、审计/指标、熔断徽章 | `plugin-config-form.tsx`、`permission-consent-dialog.tsx`、`plugin-audit-log.tsx`、`plugin-metrics-panel.tsx` | M1a-7 | M |
| FE1c | 站点目录插件分组 + 私网授权；下载活动拒绝/延后/protect | `site-config-section.tsx`、`lib/api/sites.ts`、`download-tasks.tsx`、`lib/download-attention.ts` | M1b-3、M1c | M |
| FE2 | 榜单订阅（`/subscriptions?tab=lists` + `/list-subscriptions/:id`，仅 Admin）与待确认队列、订阅 `source` 列、仓库市场与升级 diff | `list-subscriptions-view.tsx`、`list-pending-queue.tsx`、`subscriptions-view.tsx`、`repos-section.tsx` | M2-9、M4-1 | L |
| FE3 | 通知渠道多选、仪表盘 widget、任务立即运行 | `plugin-widget-host.tsx`、通知设置 | M3 | M |
| FE4 | 钉扎/吊销提示、用户级播放事件授权 | — | M4 | S |

前端测试：`web/test` 下 `node --test` 单元（schema 渲染映射、权限分类、`source` 标签）；`pnpm typecheck`。

## 自审与验收
- [ ] 每个里程碑对照设计 §15 验收条目逐项勾选。
- [ ] 标【订阅】任务的接口边界未越界。
- [ ] 第三方网络开关在 net-guard 未验收时为 off。
- [ ] 无真实网络/生产 DB 的测试依赖。

## r5 修订记录

| 复审项 | 计划中的处理 |
|---|---|
| P5-H1 | M0a-5 删除 `download.*` 埋点；M0b-4 新增 `downloader::DownloadSubmitter` 端口，`DownloadService` 唯一触发；M1c-1 改由宿主 agent 承接 |
| P5-H2 | 新增 M0a-0 `crates/exec-ctx` 并接入 `blocking_budget`/job worker/`submission.rs`；M0a 验收补 `WrongContext`；M1a-4 复用 |
| P5-M1/M2/M3 | M1a-3 vendor WASI 依赖、锁版本、CI 校验 §18.3 抽取片段、以 §18.3–18.5 最小样例做 host/guest `cargo check` |
| P5-M4 | M0b-5 删除名称+大小降级，改 `hashString` |
| P5-M5 | M0b-3 增 `origin`；M1a-2 唯一路径 + `agent_for_url` 下载器实例策略 + 种子内地址校验 |
| P5-M6/M7 | FE0/FE2 路由与 admin 限定；M2-9 admin 路由 |
| P5-L3/L6/L7/L8 | M3-2 前缀；FE1a 仅 `.cmplugin`；M1a-7/M3-4/M4-1 补端点；M0b-1 goldens 与 settings 快照、Tech Stack 版本锁 |
