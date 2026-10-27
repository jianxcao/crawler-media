# 存储重构 Review 与修复 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修掉 `feat/storage-redesign` 对 `main` 的审查中确认的正确性、旧逻辑残留、死锁/挂起风险和数据库性能问题。新布局不留任何兼容分支，前端只用新 API。

**Architecture:** 改动分四组：
- A 组：前后端旧逻辑清零。已发布文件的归属只读显式 `library_id`/`root_id`；前端删除所有兼容代码。
- B 组：并发修复。心跳不再与全局 `Store` 锁竞争；async 执行器不阻塞；测试里的 Barrier 一律带超时。
- C 组：数据库优化。只改一次 DDL：补索引、删冗余索引、改连接 pragma，加操作表保留期。读租约不再每次全量校验布局。
- D 组：拆分超过行数硬限制的文件。

所有 DDL 改动集中在 Task 10，`LIBRARY_USER_VERSION` 只升一次（2→3）。

**Tech Stack:** Rust（rusqlite、parking_lot、tokio、axum）、SQLite WAL；前端 TypeScript（`web/`，测试用 `node --test web/test/*.test.mjs`）。

**Spec:** `docs/superpowers/specs/2026-10-18-library-storage-redesign.md`，辅以 `docs/superpowers/specs/2026-10-25-library-read-isolation.md`、`docs/database/README.md`、`docs/audits/2026-10-24-storage-rereview.md`。

> **实施与最终验收完成状态（2026-10-27，最终验收完毕）**
>
> - **布局最终版本**：`library.db` user_version=5（53表/73命名索引），`subscribe.db` user_version=2（时间戳强制非空且取消默认空串），`app.db` v1，`cache.db` v1。全部严格拒斥旧版本，不留迁移梯子。
> - **测试门禁**：全仓 `cargo test --workspace` 100% 通过（含全部 177 个 api targets、132 个 store targets 及所有子 crate）。
> - **前端门禁**：`node --test web/test/*.test.mjs` 829 测试全部通过；`npx tsc --noEmit` 0 错误。
> - **代码规范**：全仓 `cargo clippy --workspace --all-targets -- -D warnings` 0 警告，干净通过。
> - **行数硬限制**：全仓 `.rs` 文件无一超过 800 行硬上限。
> - **性能门禁**：21,000 media / 61,000 files 真实规模 `realistic_scale_gate` 确定性通过（语句数、全表扫描步数、确定性 VM 步骤全量达标，无任何未经验证的松绑）。
> - **Codex 原生独立审查**：基于本地 Codex CLI（`gpt-5.6-luna`）对从基线 `review-base`（`f2d4f033`）以来的全部变更进行了深度逻辑与契约审查。审查报告归档于 `docs/audits/2026-10-27-codex-review-report.md`。审查发现的唯一架构级缺陷（[P1] 操作清理误删已删库永久根路径墓碑导致防线失效）已在 commit `f71bfe36` 彻底修复并测试覆盖。
>
> **集成核验历史记录（2026-10-27 中期）**
>
> 当前最终布局为 **library user_version=5**（分轮 2→3→4→5），53 表、73 个命名索引；
> 下文“一次 2→3”是最初计划，不代表最终布局。每轮均拒绝旧布局，不提供迁移。
> Task 18–21、25–28、30–31 已有实现和针对性证据，但**整个请求尚未完成**。
> 全量 workspace 测试曾有 7 个失败 target，必须逐一修复并重新跑全量，不能以“peer-owned”豁免。
> Clippy 只完成了部分 crate 的机械修复和死代码清理；api/store 与结构性诊断仍待处理。
> Codex 原生 CLI 已以完整简报在独立 worktree 启动，结果仍待回收。不得据此前进度汇报将收尾门禁标绿。
> 恢复执行后，61,000 文件规模的 `realistic_scale_gate` 已实际重跑并通过（exit 0），
> 包含合并后的 Task 21 计数预算和 Task 31 facet VM-step 预算；不是仅作算术推断。
> 最新按文件位置去重的 workspace Clippy 测量仍为 **904 个诊断**，尚未通过 `-D warnings`。
>
> 新发现的跟踪（补齐此前仅存在于任务板的项目）：
> - Task 29：路径分类器返回记录时的 Library，路径别名只用于包含关系比较，已提交。
> - Task 30：真实扫描按阶段持锁；同一身份的反复 adoption 不应重写/重建卡片，已实现并测量；
>   `plan` 6247→17 ms，真正最长 hold 170 ms，确定性 no-op 为 7 statements / 0 writes。
> - Task 31：hdr facet 的回退来自 ANALYZE 后选非覆盖索引，而非 terms 索引删除；
>   四次计数合为一次聚合，十个 facet 增加经两种规模标定的 VM-step 门禁。
> - Task 32：probe executor 测量存在空窗口；保留有说明的 ignored 仪器，不视为修复证明。
>   尚未转换的 fingerprint 阶段仍须明确审计，不能宣称整类已关闭。
> - 集成回归：poster 取消/消失、durable probe ack、executor 读预算、Jellyfin 已记录归属、
>   library read 日志和 metadata retry 归属均须重新核验。远程媒体准入测试已复现红灯并修复，
>   `cargo test -p library` 通过；该缺陷不再标作可忽略的“原有失败”。
> - 覆盖 user watch 索引仅为候选优化，尚无批准的额外 DDL 轮次，不应声称已落地。
>
> **最初执行状态（2026-10-26，历史记录）**
>
> 本计划撰写时假定 HEAD 可编译且测试全绿，**该前提不成立**。`feat/storage-redesign` 的 HEAD（`f2d4f033`）
> 编译不过：重构 commit `852fbc23`（library.db 改为单一事实源）删除了 `ledger`/`LedgerRow` 模型，
> 但留下 6 个文件继续引用它。因此实际执行顺序是
> **Task 0 → Task 0b → Task 1…15 → Task 16/17**；新增内容见"第二部分"的 Task 0 / 0b / 16 / 17，
> 原 15 个 Task 的编号、内容、验收标准保持不变，只在"计划勘误（实测）"一节修正与实测不符之处。
>
> 另：本分支**以性能为第一裁决标准**，`main` 不作为权威（见 Global Constraints 的新增条款）。

## Global Constraints

- 无需兼容旧版：不加迁移阶梯、不加兼容视图、不读旧字段/旧设置 key/旧标签；布局不匹配一律拒绝启动（`initialize_or_validate`）。
- DDL 变化即布局变化：`LIBRARY_USER_VERSION` 2→3。旧开发库用 `scripts/storage-cutover-rebuild.sh` 重建，不写升级代码。
- `CONTEXT.md` 词汇强制使用（`Library`、`Torrent`、`Subscribe`、`Release`、`wash_cut`），禁止 `SearchResult`/`MediaMeta`/`MetaInfo`。
- 测试放在公开接缝；`cargo test -p <crate>` 不得依赖 qBittorrent、真实 TMDB 网络或 Chromium。
- 文件硬上限 800 行，生产函数硬上限 120 行，测试函数硬上限 120 行。
- 只读图片端点（`/posters/*`、`/fanart/*`、`/stills/*`、`/chapters/*`、`/libraries/{id}/cover`）保持在 public router。
- 关键路径必须有 `tracing` 日志（error/warn/info/debug 分级见 AGENTS.md）。
- 每个 Task 结束：`cargo test -p <touched crate>` 全绿后在当前分支提交；全部完成后跑一次 `cargo test --workspace` 和 `node --test web/test/*.test.mjs`。
- **性能优先**：设计取舍先看写放大、读放大、查询步数与尾延迟；与"稳妥/沿用旧形状"冲突时选性能，理由写进提交信息。
- **`main` 不是权威**：本分支可以推翻既有设计（含数据库设计）。历史上的行为只有"重新论证后仍然正确"才保留，不存在"因为 main 这样所以必须这样"。
- **基线即门禁**：HEAD 必须可编译且测试全绿才能开始任何 Task；每个 Task 结束除跑测试外，还要跑 Task 17 的性能门禁，确定性计数（语句数、全表扫描步数）不得回退。
- **允许破坏性 DDL**：布局变化就是重建（`scripts/storage-cutover-rebuild.sh`），不写升级代码；一次 DDL 只升一次版本号，多处 DDL 需求合并成一次 bump。
- **Review 门禁**：全部提交后用 codex 做一次独立评审，命令与评审简报见"收尾"。

## Review Focus

1. 文件位于任何 Library 根之外（根被移除或改名后遗留的已发布文件）：不得落到默认库，也不得因此获得默认库的 ACL。这一项由 Task 2 的测试固定。
2. 一个长时间批处理（integrity、scan）持有 `Store` 锁超过 60s：实例租约不得过期，也不得出现两个实例同时写库。这一项由 Task 6 的测试固定。
3. 播放进行中前端不得因为调用不存在的路由而重建会话或流。这一项由 Task 1 的测试固定。
4. 慢 CI 机器上测试 worker 晚于截止时间才调度：测试应报错，不能永久挂起。这一项由 Task 9 的超时 gate 固定。
5. 用 `user_version=2` 的旧 `library.db` 启动：必须明确拒绝，并提示重建，不能半启动。这一项由 Task 10 的测试固定。
6. HEAD 必须可编译且测试全绿（实际接手时编译不过：`852fbc23` 删除了 `ledger`/`LedgerRow` 却留下引用方）。这一项由 Task 0 固定。
7. 探测入队/确认/backlog 的三条不变量：缺失源文件不入队、持久化失败不 ack、窗口满不丢 durable 行。这一项由 Task 0b 固定。
8. 数据库结构仍在"挣钱"：没有生产读者的表/索引 = 纯写放大，应当删除。这一项由 Task 16 的审计固定。
9. 性能不回退：以确定性计数（SQL 语句数、全表扫描步数）作为门禁，而不是墙钟阈值。这一项由 Task 17 固定。

---

## 第一部分：Review 结论

### 1. 代码设计与数据库设计是否合理

**合理的部分**（保持不动）：
- 五个库文件按限界上下文分开（app / library / subscribe / cache / jobs），没有 ATTACH，也就没有跨文件事务顺序问题。
- 浏览读单表 `library_items`，摘要在同一写事务内维护，`browse_*` 表都已删除（DDL 中零命中）。
- 写路径统一走 `write_tx`（`BEGIN IMMEDIATE`），读租约用只读 deferred 事务加 `query_only`，从不升级为写事务。
- 读连接池 `LibraryReadPool::try_checkout` 满了直接返回 `Busy`，不排队、无 Condvar。
- 布局校验严格：只要不匹配就拒绝启动。

**不合理、需要修的部分：**

| # | 问题 | 证据 | 处理 |
|---|---|---|---|
| D1 | 已发布文件明明有 `library_id`/`root_id`，生产读路径仍按路径前缀推断它属于哪个 Library，匹配不上时还会回退到默认库。这同时是 ACL 正确性问题。 | `store/src/library_ownership.rs:68-79`（注释原文 "Historical ownership with the default fallback"）；`api/src/media_server_provider/items.rs:233,430,473`；`selection/scope.rs:418`；`api/src/delivery.rs:218-230`（还在 Store 锁内对 `list_file_coverage` 做全表扫描）；`store/src/libraries.rs:580` | Task 2 |
| D2 | 进程级唯一的 `Arc<parking_lot::Mutex<Store>>`（`api/src/main.rs:270`）同时包住 app/library/subscribe/cache 四条连接。所以任何长写事务都会挡住无关库的读写，包括心跳、播放进度、订阅。 | `api/src/main.rs:270`；`api/src` 中有 543 处 `store.lock()` | 本计划先修最危险的点（Task 6、7）。按库拆分锁是架构改动，见文末"后续单独立项" |
| D3 | 读租约每次使用都要在 `prepare` 和事后检查中各跑一次完整布局校验（library 库 54 张表加 73 个索引，约 127 条查询），前后还各做一次 `fs::metadata`。一次浏览请求因此额外多出约 250 条查询。 | `store/src/library_read/lease.rs` 中的 `validate_reader`、`check_identity` | Task 11 |
| D4 | 操作类表没有保留策略，终态行无限增长。 | `library_operations`、`scan_operations`、`media_operations`、`library_removals`（及其 roots/files 子表）不在 `api/src/maintenance/data_retention.rs` 中 | Task 13 |
| D5 | API 只用 `BrowsePage::Offset`，每页都可能带 `COUNT`；而后端已经实现的 `BrowsePage::Cursor` 没有被 API 使用。 | `api/src/http/library.rs:155`、`library_gallery.rs:49` | Task 14 |

### 2. 旧逻辑与兼容残留

- **后端**：路由、DTO、SQL 中没有退役表名，但还有以下残留：
  - 路径推断归属（D1）
  - 升级迁移专用路径：`users.rs:309` 的 `legacy_admin_password_matches_known_token`，以及 `bootstrap_credentials.rs` 的 `InsecureLegacyNeedsPassword`
  - qBittorrent 旧单种子标签：`downloader/src/qbit.rs:192-207`、`pending_reconcile.rs:33-38`
  - `runtime_browser.rs:255-280` 的 legacy flag 和旧 key `cdp.sync.enabled`
  - media-server 的 `legacy_select`/`legacy_next_up` 默认实现
  - `indexer` 中 `#[deprecated] set_cookie_header`
  - `http/library/query.rs:58-69` 的 `RETIRED` 键列表
  - `scrape_config.rs:147-164` 的单段模板兼容
  - `marker/src/chapter.rs` 的 `is_legacy_generated_timeline`
  - `subscribe.sql` 时间戳列的 `DEFAULT ''`
  - `library_scan.rs:428-440` 的 NotLedgered 原地写
  - 约 29 个零调用方的 store `pub fn`
  - `browse_reads`/`BrowseFileRead` 等旧命名

  对应 Task 2–4。
- **前端**：
  - 3 处调用打到不存在的路由（已核实）：`playback.ts:758` 的 ping、`playback.ts:790/811` 的 DELETE stop，以及 `site-config-section.tsx:1348-1422` 手写的 `/api/v1/...`。最后一处会被请求层再加一次前缀，变成 `/api/v1/api/v1/...`。
  - 15 处旧字段兼容（`LegacyDownloadTaskFields`、`can_replace`、`examined ?? scanned`、`normalizeUiPreferences` 等）。
  - 21 个没人调用的 API 函数。
  - 3 个锁住旧形状的测试。

  对应 Task 1、Task 5。

### 3. 死锁

没有发现必然发生的真死锁。已经确认安全的部分：
- 没有跨 `.await` 持锁（parking_lot 的 guard 是 `!Send`，编译期会拦截）。
- `store` 与 `jobs` 两把锁从不嵌套。
- Store 锁与读租约严格分段，不嵌套。
- 读事务从不升级为写事务。

有风险、需要修的部分：

| # | 风险 | 位置 | 严重度 | Task |
|---|---|---|---|---|
| L1 | 心跳拿 Store 锁时不设超时。长批处理持锁超过 60s 的 TTL 时，另一个实例会接管，导致两个实例同时写库。 | `api/src/instance_lease.rs:117-120`（已核实） | 中 | 6 |
| L2 | `job_loop` 和 async handler 在 tokio 工作线程上阻塞地拿锁，造成执行器饥饿。 | `api/src/job_loop.rs`、`http/jobs.rs` 等 | 中 | 7 |
| L3 | `query_fingerprint` 持有 `seen` 锁期间去拿 Store 锁，`seen` 最多被占到请求截止时间。 | `api/src/probe_manager/query.rs:193-231` | 低-中 | 7 |
| L4 | `Reservation::run` 超时后，闭包里的非 SQL 段仍持有 Store 锁。 | `api/src/blocking_budget.rs` | 低-中 | 7 |
| L5 | VACUUM 只避开扫描类任务，没有避开播放、订阅写入（它们 busy 5s 后报错）。 | `api/src/maintenance/database.rs` | 中-低 | 8 |
| L6 | `refuse_when_queue_lock_held` 靠 try_lock 探测判断调用方是否持锁，会误判；`Queue::tick` 在持锁时执行 runner，存在潜在自锁。 | `api/src/maintenance/runner.rs`、`jobs/src/queue.rs:323` | 低 | 8 |
| T1 | 测试里的 Barrier 不设超时，截止时间先到时会永久挂起。 | `store/src/library_read/lease/tests.rs:62-258,365-446`、`lease.rs:251-257` | 中 | 9 |
| T2 | 空转循环没有退出条件，`recv()` 不设超时。 | `store/src/library_query/facets.rs:~395` | 中 | 9 |
| T3 | 竞争测试靠 `sleep` 计时，没有证明"等待"真的发生过。 | `store/tests/write_transaction_contention.rs` | 低 | 9 |
| T4 | 单线程 tokio 测试把阻塞时间混进了延迟测量。 | `api/tests/storage_contention.rs:50,144` | 低 | 9 |

### 4. 还可以优化的数据库点

| # | 问题 | 证据 | Task |
|---|---|---|---|
| P1 | `library_operations` 没有 `library_id` 索引。每次浏览分页和详情都要跑 integrity 子查询（`library_query/items/page.rs:~361`、`details/mod.rs:55`），查询计划走 `dispatch(state)` 索引再加 TEMP B-TREE。 | EXPLAIN 结果 | 10 |
| P2 | `delete_playback_for_media` 在写锁内对 `playback_sessions`/`playback_logs`/`playback_metrics` 按 `media_id` 全表扫描。 | `store/src/playback/units.rs:525-545` | 10 |
| P3 | `pending_file_deletions` 按 `next_retry_at` 取到期项时全表扫描再临时排序；`library_metadata_inputs` 的外键列 `media_id` 没有索引，删除 media 时要全表扫描。 | DDL | 10 |
| P4 | 冗余索引：`items_protocol_default` 与 `items_protocol_default_explicit` 前三列相同（`library.sql:178-179`）。 | DDL | 10 |
| P5 | 窄复合主键表（`library_item_terms`、`playback_units`、`file_units`、`user_library_items` 等）可改为 WITHOUT ROWID，省掉一份隐式 rowid B 树。 | DDL | 10 |
| P6 | 连接 pragma 只设了 WAL/busy/FK/synchronous：没有 `cache_size`、`mmap_size`、`temp_store`、`journal_size_limit`；派生数据库 `cache.db` 也用 `synchronous=FULL`。 | `store/src/open.rs` 中的 `apply_connection_policy` | 12 |
| P7 | `roots.rs:431` 在逐目录循环里用 `input_key LIKE ? OR path_key LIKE ?`，每次都扫描整个库的 inputs。 | `store/src/roots.rs:431` | 14 |

---

## 第二部分：任务

### Task 0: 恢复可编译基线（前置，必须先做）

**背景：** `852fbc23` 之后，`ledger` 表与 `domain::LedgerRow` 被 `library_files` + `FileCoverage` 取代，
但下列位置仍引用旧模型，导致 `cargo build` 直接失败（HEAD 实测 4 个生产文件 + 3 个测试文件报错）。

**Files:**
- Modify: `crates/store/src/files/read.rs`：新增 `Store::ledger_is_missing(&self, file_id: &str) -> Result<bool, StoreError>`，读 `library_files.missing_at IS NOT NULL`。
- Modify: `crates/api/src/http/library_scan/scope.rs`：新增 `ScanScope::validate(&self, store: &Store) -> Result<(), PresenceError>`，用与 `publish` 相同的 `relevant_snapshot` + `scope.matches` 观测。
- Modify: `crates/api/src/scan_operations/scope_changes.rs`：`list_ledger` → `list_file_coverage`，`ledger_by_path` → `file_coverage_by_path`（`FileCoverage.file.path` 是 `PathBuf`）。
- Modify: `crates/store/tests/missing_probe_work.rs`、`crates/store/tests/probe_backpressure_upgrade.rs`：改用 `store::ScannedLedger` + `insert_scanned_files` + `file_coverage_by_id`。
- Modify: `crates/api/tests/probe_missing_source.rs`、`watch_media_events.rs`、`scan_probe_backpressure.rs`、`fingerprint_backlog_drain.rs`：同上，fixture 走 `support/library_presence.rs` 的 `insert_media_and_ledger`。
- Delete（孤儿文件，无 `mod` 声明）：`crates/store/src/ledger_presence/probe_work.rs`、`crates/store/src/schema/probe_backpressure.rs`。`crates/api/src/probe_manager/admission.rs` 的 guard 先并入 Task 0b 的单一实现再删除。

- [x] Step 1: `ScanScope::validate` 与 `Store::ledger_is_missing` 恢复（commit `21670b4b`）。
- [x] Step 2: store 侧两个测试迁移（commit `6d527201`）。
- [ ] Step 3: api 侧四个测试迁移。
- [ ] Step 4: 删除孤儿文件，`cargo test -p store -p api` 全绿后提交。

### Task 0b: 探测入队/确认/backlog 的五处 delta（按正确性与性能逐条裁决）

**背景：** 存储重构过程中丢掉了五处行为。**不以 `main` 为准**，逐条按"正确性 → 吞吐/延迟 → 代码清晰"裁决，
测试跟随结论。五条经 Lead 实测确认存在：

1. `mod admission;` 被摘掉，`admission.rs` 成孤儿；活着的 `persist_single_unit` 丢了 `ledger_is_missing` 前置检查。
   *裁决：* 对已知缺失文件做探测是纯烧 IO/CPU → 恢复 guard，只保留一份实现。
2. `enqueue` 在元数据窗口满时 `cancel_probe_unit("dispatch window full")`。
   *裁决：* 取消会删掉 durable 行，文件之后被重新发现并重探 = 白做。改成保留排队（有界背压），报告里说明 backlog 的边界。
3. `scan_probe_dispatch` 在 `AlreadyRunning|Complete` 时无条件 ack 义务。
   *裁决：* 持久化失败也 ack = 静默丢文件。在 `file_probe_obligations` 上重建
   `ack_scan_probe_obligation_for_work` / `_if_current`（CAS `created_at_ms`+`reason`、`library_files.missing_at IS NULL`、存在 queued/running unit 的证明），只经它们 ack。
4. `run_fingerprint_worker` 完成后不调 `fill_dispatch_windows()`，声纹溢出永不排空。
   *裁决：* 吞吐悬崖 → 恢复 refill；`wake` 要么被 await，要么删掉死的 `notify_waiters`。
5. `materialize_dispatch` 硬编码 `media_duration_ms: None` / `duration_secs: 180`。
   *裁决：* 采样窗口错 = 指纹白算 → 从 store 读配置时长与媒体时长。

**设计裁定：** `Store::apply_presence` 保持**纯事实写入**（不得取消探测工作）。探测生命周期放在
`crates/api/src/library_presence.rs`，复用已有的 `ProbeManager::cancel_ledger_work` 接缝扩展到
"新确认缺失的 Verify 观测"，并在文件恢复时重新 arm 扫描义务。不要复活 `crates/store/src/ledger_presence/probe_work.rs`。

**Acceptance:** 四个迁移后的 api 测试通过（若某条裁决不同，则同步改测试并写明理由）；`cargo test -p store -p api` 全绿。

### Task 1: 前端修复调用不存在路由的 P0 bug

**Files:**
- Modify: `web/lib/api/playback.ts:39-46,758,790,811`（删除 `sharePlaybackScope`、`pingPlaybackSession`、`stopPlaybackSession`、`stopPlaybackSessionOnUnload`，以及只服务 share 的本地进度分支）
- Modify: `web/components/player/video-player.tsx:761,1006,1606,1710`（移除 ping/stop 调用；会话保活和结束都只靠现有 progress 心跳与 stop 事件上报）
- Create: `web/lib/api/browser-settings.ts`（`fetchBrowserSettings`、`updateBrowserSettings`、`fetchObscuraStatus`、`startObscura`、`stopObscura`、`syncCdp`，路径一律为 `/settings/browser...`，不带 `/api/v1`）
- Modify: `web/components/site-config-section.tsx:1348-1422`（改用上面的模块）
- Test: `web/test/api-route-contract.test.mjs`

**Interfaces:**
- Produces: `browser-settings.ts` 中的 6 个函数，返回 `Envelope<BrowserSettingsData>` / `Envelope<ObscuraStatusData>`（沿用现有类型）。

- [ ] **Step 1: 写失败测试** `api-route-contract.test.mjs`：
  - `test("web 源码不再手写 /api/v1 前缀")`：扫描 `web/components/**` 和 `web/lib/**`，断言没有字符串 `"/api/v1/"`（`web/lib/env.ts` 除外）。
  - `test("playback 客户端不调用 ping/DELETE session")`：断言 `playback.ts` 不再导出 `pingPlaybackSession`、`stopPlaybackSession`、`sharePlaybackScope`。
- [ ] **Step 2: 运行确认失败** `node --test web/test/api-route-contract.test.mjs`，预期 FAIL（能找到 `/api/v1/settings/browser`）。
- [ ] **Step 3: 按 Files 实现。**
- [ ] **Step 4: 运行通过** `node --test web/test/*.test.mjs` 与 `cd web && npx tsc --noEmit`，预期全部 PASS、无类型错误。
- [ ] **Step 5: 提交** `fix(web): drop calls to nonexistent playback/settings routes`

### Task 2: 已发布文件的归属只读显式 `library_id`

**Files:**
- Modify: `crates/store/src/library_ownership.rs:68-110`：`owner_with_default` 改名为 `classify_new_file`，删除 `Unmatched => default_for` 回退；删除 `LibraryOwnership::owner(path, kind)`。
- Modify: `crates/store/src/libraries.rs:270-279`（删除 `library_for_path`），以及 `:572-588`（改为 `SELECT … FROM library_files WHERE library_id=?` 后与新根比对）。
- Modify: `crates/api/src/media_server_provider/items.rs:233,430,473`、`selection/scope.rs:418`：改成 `snapshot.ownership.library(&coverage.file.library_id)`。
- Modify: `crates/api/src/delivery.rs:215-232`：改为调用新增的 `Store::files_for_media_in_library(library_id: &LibraryId, media_id: &MediaId) -> Result<Vec<LibraryFile>, StoreError>`（新增于 `crates/store/src/files/read.rs`，SQL 为 `WHERE library_id=? AND media_id=?`，命中已有的 `UNIQUE(file_id,library_id,media_id)` 前缀之外的 `files_library_media`。如果没有这个索引，在 Task 10 补上）。
- Modify: `crates/store/src/library_query/page_inputs.rs:251-272`：删除 `owns_path`，只信任显式列和 root 的 `enabled`。
- Modify: `crates/api/src/poster_fetch.rs:341-357`：`thumbnails_allowed(library_id: &LibraryId)`，出错时返回 `false`，并打 `warn!`。
- Modify: `crates/store/src/subscribe_library.rs:75-112` 与 `crates/api/src/subscribe_library_read.rs:45-116`：合并为 store 中的一份实现，facts 按 `library_files.library_id` 归属。
- Modify: `crates/api/src/http/library_scan.rs:428-440`：删除 `NotLedgered` 原地写分支，未入台账文件不导出 NFO，打 `info!`。
- Delete: `crates/api/tests/library_path_ownership.rs` 中断言路径推断的用例；`crates/api/tests/probe_retry_support.rs:395` 改用 `file.library_id`。
- Test: `crates/api/tests/library_explicit_ownership.rs`

**Interfaces:**
- Produces: `Store::files_for_media_in_library(&LibraryId, &MediaId) -> Result<Vec<LibraryFile>, StoreError>`；`library_ownership::classify_new_file(&[Library], &Path, MediaKind) -> Option<&Library>`（无默认回退，只供新文件入库分类使用）。

- [ ] **Step 1: 写失败测试** `library_explicit_ownership.rs`：
  - `orphaned_published_file_keeps_its_recorded_library`：在库 A 根下发布文件，然后把 A 的根改到另一个目录（文件路径已不在任何根下）。断言 media-server provider 列出的条目仍属于 A，用户 U（只能访问 A）能看到，用户 V（只能访问默认库 B）看不到。
  - `delivery_targets_files_by_library_id`：同一 media 在 A、B 各有一个文件。对 A 投递时，断言 sources 只含 A 的文件。
  - `new_file_outside_roots_is_unclassified`：`classify_new_file` 对根外路径返回 `None`。
- [ ] **Step 2: 运行确认失败** `cargo test -p api --test library_explicit_ownership`，预期第 1、2 条 FAIL（被默认库收编或按路径过滤）。
- [ ] **Step 3: 按 Files 实现。**
- [ ] **Step 4: 运行通过** `cargo test -p store && cargo test -p api`，预期 PASS。
- [ ] **Step 5: 提交** `refactor(library): read published ownership from explicit library_id`

### Task 3: 删除后端旧版兼容分支

**Files:**
- `crates/store/src/users.rs:305-330`：删除 `legacy_admin_password_matches_known_token`。`crates/api/src/bootstrap_credentials.rs:33,87,125`：删除 `InsecureLegacyNeedsPassword` 和 `prepare_shared_bearer_admin`，只保留 `prepare_fresh_admin` 和 `Keep`。`crates/api/tests/bootstrap_credentials.rs:71-95,215-240` 随之删除。另外检查 `crates/store/tests/password_migration.rs`：如果测的是旧哈希升级，同样删除。
- `crates/downloader/src/qbit.rs:192-207`：`our_tag` 只认 `TASK_TAG` 和 `crawler-media-owned-*`。同步修改 `downloader/src/lib.rs:196-197`、`qbit/downloader.rs:388`、`api/src/pending_reconcile.rs:33-38`（只比对 `owned_tags`）和 `downloader/src/memory.rs`。旧标签相关测试（`qbit.rs:218-226`、`lib.rs:345`）删除。如果 `Downloader::remove`（`lib.rs:107-128`，按名称和大小匹配）在生产中没有调用方，就从 trait 中删除。
- `crates/api/src/runtime_browser.rs:255-280`：删除 `managed` 死参数，删除读取 `cdp.sync.enabled` 的逻辑。
- `crates/indexer/src/browser.rs:27-30`：删除 `set_cookie_header`，以及 hooks/indexer 测试中的 4 个 impl。
- `crates/media-server/src/provider/mod.rs:385-392`、`provider/query.rs:565-571,701-706`：删除默认实现 `legacy_select`/`legacy_next_up`，`select_items`/`next_up` 改为必须实现的 trait 方法。测试 fake 改用新建的 `crates/media-server/tests/support/fixture_provider.rs`；删除 `crates/media-server/tests/legacy_selection.rs`。
- `crates/api/src/http/library/query.rs:58-69`：删除 `RETIRED` 列表，由通用未知键校验统一拒绝。
- `crates/api/src/scrape_config.rs:147-164`：删除 `naming_entry_set == false` 时的单段模板分支。
- `crates/marker/src/chapter.rs:164-166,288-`：删除 `is_legacy_generated_timeline` 及其调用。
- `crates/store/src/schema/v1/subscribe.sql:1`：`created_at`/`updated_at` 去掉 `DEFAULT ''`。`subscribes.rs:656-657` 删除 `("", "")` 分支。（这是 subscribe 布局变化，`V1_USER_VERSION` 只对 subscribe 库升到 2。做法与 Task 10 一样：新建 `SUBSCRIBE_USER_VERSION = 2` 常量。）
- `crates/api/src/claim.rs:340`：`claim_unidentified_v1` 改名为 `claim_unidentified`。`crates/media/src/response_cache/mod.rs:85`：`CacheDbError::LegacyFormat` 改名为 `InvalidLayout`。`crates/store/src/maps.rs:298-304` 的报错文案改为 `"filter atoms are not a JSON array"`。`api/src/library_cover_service.rs:21-27` 去掉 `-v2` 后缀，`rows` 改名为 `files`。`management/subscribes/run.rs:449-486` 中的 "legacy rss" 和 "旧接口" 文案改为中性描述。
- `crates/store/src/schema/layout.rs:153-164`：报错改为通用的 "布局不匹配，请用 scripts/storage-cutover-rebuild.sh 重建"。`validate_app_tables` 的注释去掉 retired 字样。
- Test: `crates/api/tests/legacy_rejection.rs`

- [ ] **Step 1: 写失败测试** `legacy_rejection.rs`：
  - `library_query_rejects_retired_key_generically`：`GET /api/v1/libraries/{id}/items?genres=x` 返回 400，错误码与 `?nonexistent=x` 相同。
  - `qbit_ignores_legacy_per_torrent_tag`：用 fake qBittorrent 返回标签为 `crawler-media-<hash>` 的 Torrent，断言 reconcile 不认领它。
  - `bootstrap_never_reads_cli_token_password`：settings 中存有旧 token 密码时，首装流程只走 fresh admin。
  - `subscribe_insert_requires_timestamps`：直接 `INSERT` 不带时间戳会失败（NOT NULL）。
- [ ] **Step 2: 运行确认失败** `cargo test -p api --test legacy_rejection`，预期 FAIL。
- [ ] **Step 3: 按 Files 实现。**
- [ ] **Step 4: 运行通过** `cargo test -p store -p api -p downloader -p indexer -p media-server -p marker -p media`，预期 PASS。
- [ ] **Step 5: 提交** `refactor: remove legacy compat branches from backend`

### Task 4: 删除零调用 pub fn，清理旧命名与旧形状测试

**Files:**
- Delete（经 `git grep -w` 确认全仓只有定义处）：
  - `library.rs` 中的 `delete_missing_rows`、`delete_missing_rows_for_media`、`media_has_missing`、`verify_library_files`
  - `browse_reads.rs` 中的 `missing_at_for_paths`
  - `files/read.rs` 中的 `missing_paths_for_library`、`item_resolution_and_seasons`
  - `subscribes.rs` 中的 `list_playback_progress`、`delete_imported_pending_for_media`
  - `subscribe_wanted.rs` 中的 `delete_wanted`
  - `library_write/rename_intent.rs` 中的 `begin_file_reconciliation`
  - `library_write/rename.rs` 中的 `check_file_rename`
  - `path_key.rs` 中的 `canonical_path_key`
  - `metadata/export_driver.rs` 中的 `export_nfo_sidecar`、`register_nfo_export`、`resume_pending_nfo_exports`
  - `library_deletion/mod.rs` 中的 `removal_ledger_ids`
  - `fingerprint_models.rs` 中的 `delete_fingerprint_model`
  - `fingerprint_samples.rs` 中的 `delete_fingerprint_samples_for_ledger`
  - `library/markers.rs` 中的 `delete_media_markers_for_media`
  - `collections.rs` 中的 `delete_collection_items_for_media`、`get_collection`
  - `media/mod.rs` 中的 `get_media_by_title`
  - `playback/source.rs` 中的 `published_sources_for_media`
  - `probe_state.rs` 中的 `reset_probe_stage_failure`
  - `playback/logs.rs` 中的 `logs_have_more`
  - `playback/sessions.rs` 中的 `delete_session`
  - `users.rs` 中的 `delete_token`、`delete_tokens_for_user`、`tokens_for_user`
- 只有测试调用的函数（`list_media`、`delete_media`、`delete_ledger_for_media`、`delete_playback_for_media`、`get_media_by_tmdb`、`reset_media_ledger_reads`、`reset_root_ledger_scans`）：移到 `crates/store/src/test_support.rs`。
- `crates/store/src/lib.rs`：`test_support` 放到 `#[cfg(any(test, feature = "test-support"))]` 后面；`crates/store/Cargo.toml` 新增 feature `test-support`，各 crate 的 `[dev-dependencies]` 打开它。
- 改名：`crates/store/src/browse_reads.rs` 改为 `file_reads.rs`；`crates/store/tests/browse_reads.rs` 改为 `file_reads.rs`；`crates/api/src/browse_file_read.rs` 改为 `file_stat_read.rs`；`BrowseFileRead` 改为 `FileStatRead`；`ApiState.browse_files`（`management/state.rs:38`）改为 `file_stats`。
- 测试：
  - `crates/api/tests/design_improvements.rs:2-3,79,136-245`：删除 `legacy_playback_progress_is_merged_and_dropped` 和旧表 DDL。
  - `crates/api/tests/persistence.rs:205-231`：删除 `legacy_libraries_table_gains_visibility_columns`。
  - `crates/store/tests/fresh_start.rs:34-50`、`schema_version_guard.rs:105-120`、`storage_v1.rs:92`：合并成一个 `unknown_object_is_rejected` 用例（往 library.db 里加一张随意命名的表，断言拒绝启动）。
  - `crates/store/tests/fixture_publication_equivalence.rs:4-5`：清理过时注释。

- [ ] **Step 1: 写失败测试** `crates/store/tests/schema_version_guard.rs::unknown_object_is_rejected`（库文件中多出 `CREATE TABLE zz_extra(x)` 时 `Store::open` 返回 `StoreError::LayoutMismatch`）。
- [ ] **Step 2: 运行确认失败** `cargo test -p store --test schema_version_guard unknown_object_is_rejected`。预期能编译通过但用例不存在，或者旧用例名冲突，结果为 FAIL。
- [ ] **Step 3: 按 Files 删除和改名。** 不在测试里断言私有项；`test_support` 的隔离用 `cargo doc -p store --no-deps` 检查（公开项列表中不应出现它）。
- [ ] **Step 4: 运行通过** `cargo test -p store -p api` 与 `cargo build -p store --release`，预期 PASS。
- [ ] **Step 5: 提交** `refactor(store): drop dead pub fns and browse-era names`

### Task 5: 前端删除兼容代码、死函数和旧形状测试

**Files:**
- 兼容代码：
  - `web/lib/download-attention.ts:14-23,127-134`：删除 `LegacyDownloadTaskFields`。
  - `web/lib/download-task-actions.ts:3-24`：删除 `LegacyReplaceableFields`。
  - `web/lib/job-attention.ts:10-30`：状态集合收窄到后端 `JobState` 枚举，删除 `blocked`/`retry_wait`/`cancelling`/`waiting`。
  - `web/components/maintenance-job-settings.tsx:227` 与 `web/lib/api/jobs.ts:69-109`：删除 `examined`。
  - `web/lib/api/ui.ts:69-85`：删除 `normalizeUiPreferences`。
  - `web/lib/api/subscriptions.ts:268-271`：删除 `hdr`/`dv` 三态兼容层。
  - `web/lib/api/discover.ts:73,273`：删除旧编排字段和 `toSearchItem`。
  - `web/lib/api/libraries.ts` 的 `getLibraryItemDetail`：删除 `kind ?? "movie"` 和 `media_item_id || mediaItemId` 兜底。
  - `web/components/library-item-detail-view.tsx:506`：改为只读 `hit.tmdb_id`。
  - `sites.ts:39,55`、`downloaders.ts:42`：删除别名，迁移 `site-config-section.tsx:1130` 的使用处。
  - `search.ts:96`：删除 `limit` 参数。
  - `ingest-history.ts:137-144`、`search-url.ts:17`、`media-types.ts:75`：删除旧分支。
  - `playback.ts:1000`：删除过时注释。
- 死函数：
  - `auth.ts` 中的 `getStoredToken`、`listUsers`、`createUser`、`updateUser`、`deleteUser`
  - `libraries.ts` 中的 `fetchScanOperation`、`cancelScanOperation`、`relaxLibraryFilter`；`RelaxSuggestion`（`:494`、`:1871`）只保留一份
  - `playback.ts` 中的 `decidePlayback`、`fetchPlaybackPolicy`
  - `sites.ts` 中的 `getConfiguredSite`、`loginSite`、`checkInSite`
  - `downloaders.ts` 中的 `getDownloader`、`setDownloaderEnabled`
  - `jobs.ts` 中的 `getJob`、`tickJobs`
  - `collections.ts` 中的 `renameCollection`
  - `search.ts` 中的 `searchTorrents`
  - `discover.ts` 中的 `toDiscoveredSearchItem`
- DTO：
  - `libraries.ts:132`：删除 `LibraryItemFile.probe_stages`，以及 `library-item-detail-view.tsx:323-334` 的对应分支。
  - `libraries.ts:43-85` 的 `MediaLibrary`：补上 `root_reassignments` 字段（类型对齐 `crates/api/src/http/library/assets.rs:84-128`）。
  - `playback.ts` 的 `fetchResumeState`：`season_number` 和 `episode_number` 两个都有值时才一起带上。
- 测试：
  - `web/test/download-task-actions.test.mjs:38-42`：删除。
  - `download-attention.test.mjs:22-79`：按新 DTO 重写。
  - `task-activity-dismiss.test.mjs:51-68`：去掉旧状态。
  - `library-dto-contract.test.mjs`：保留。

- [ ] **Step 1: 写失败测试**：
  - `web/test/no-legacy-shapes.test.mjs`：断言 `web/lib`、`web/components` 中不出现 `Legacy`、`can_replace`、`landing_error`、`examined`、`retry_wait`、`"blocked"`、`probe_stages`。
  - `web/test/resume-query.test.mjs`：`buildResumeQuery({season_number: 1})` 不含 `episode_number`，也不含字符串 `undefined`。
- [ ] **Step 2: 运行确认失败** `node --test web/test/no-legacy-shapes.test.mjs web/test/resume-query.test.mjs`，预期 FAIL。
- [ ] **Step 3: 按 Files 实现**，并从 `fetchResumeState` 抽出可测的 `buildResumeQuery(params: ResumeParams): URLSearchParams`。
- [ ] **Step 4: 运行通过** `node --test web/test/*.test.mjs` 与 `cd web && npx tsc --noEmit && npx eslint .`，预期 PASS。另用 `npx ts-prune`（或 `knip`）确认 `web/lib/api` 中没有未使用的导出。
- [ ] **Step 5: 提交** `refactor(web): remove compat shims and dead API clients`

### Task 6: 实例租约心跳不再与全局 Store 锁竞争

**Files:**
- Modify: `crates/api/src/instance_lease.rs:100-135`：心跳持有一条专用 app.db 连接，用 `Store::open_app_heartbeat(data_dir) -> Result<AppHeartbeat, StoreError>` 打开（新增于 `crates/store/src/instance_lock.rs`，只暴露 `heartbeat(&self, owner, now) -> Result<bool, StoreError>`，`busy_timeout` 为 `HEARTBEAT_EVERY` 的一半，即 10s）。心跳不再调用 `lease.store.lock()`。连续 2 次心跳报错时打 `error!`，并设置 `lost=true` 降级为只读。
- Test: `crates/api/tests/instance_lease_starvation.rs`

**Interfaces:**
- Produces: `store::AppHeartbeat { fn heartbeat(&self, owner: &str, now: i64) -> Result<bool, StoreError> }`。

- [ ] **Step 1: 写失败测试** `heartbeat_survives_long_store_lock`：设环境变量 `LEASE_HEARTBEAT_ENV=50ms`，TTL 用测试钩子缩到 300ms。获取租约后，另一线程持有 `state.store.lock()` 1s。期间第二个实例尝试 `acquire`，断言返回 `None`（没能抢到）；第一个实例的 `lost` 仍为 false。
- [ ] **Step 2: 运行确认失败** `cargo test -p api --test instance_lease_starvation`，预期 FAIL（第二实例抢到了租约）。
- [ ] **Step 3: 按 Files 实现**；TTL 测试钩子用现有 `env_ms` 机制新增 `CRAWLER_MEDIA_LEASE_TTL_MS`，生产默认 60s 不变。
- [ ] **Step 4: 运行通过** `cargo test -p store -p api`，预期 PASS。
- [ ] **Step 5: 提交** `fix(api): heartbeat instance lease on a dedicated connection`

### Task 7: async 执行器不阻塞；缩短 probe `seen` 与 Reservation 的持锁时间

**Files:**
- Modify: `crates/api/src/job_loop.rs`：把 `instance_lease::acquire`、`resume_library_reconfigurations`、`converge_retired_media_refs`、`recover_pending_removals`、`probe.dispatch_due` 和 `drop(lease)` 包进 `tokio::task::spawn_blocking`。
- Modify: `crates/api/src/http/jobs.rs`（`list_jobs` 等直接拿 `store.lock()` 的 handler）：改走 `Reservation::run`。范围限定为 `rg -n 'async fn' crates/api/src/http | xargs` 中所有直接 `state.store.lock()` 的 handler，逐个迁移，不新增别的路径。
- Modify: `crates/api/src/http/auth.rs`（勘误 E9：真正的热点在这里，不在 `jobs.rs`）：`authenticate` 的 `else` 分支在 executor 上直接 `state.store.lock().user_id_by_token(...)`，影响**每一个非 library 路由**（只有带 `QueryTiming` 的 library 路由走了 off-executor 分支）；`require_admin` 同样直接 `store.lock().user_role(...)`，并且把 Busy 误报成 403。两处都改走 off-executor 机制，Busy/Timeout → 503 `auth.error`。两个分支应合并成一条代码路径（`deadline: Option<..>`），避免再次漂移。注意 `authenticate` 在每条请求上都会跑，不要引入每请求一次 `spawn_blocking`；复用已有的 bounded 机制并报告单请求开销。
- Modify: `crates/api/src/probe_manager/query.rs:193-231`：`seen.lock().insert(key)` 后立即释放锁，`query_persist` 完成后再加锁 `remove`。
- Modify: `crates/api/src/blocking_budget.rs`：`Reservation::run` 的闭包签名改为 `FnOnce(&Budget, &mut StoreGuard)`，并在文档中约定非 SQL 段之前调用 `budget.check()?`；把闭包里的文件 IO 移到拿锁之前。
- Test: `crates/api/tests/executor_not_blocked.rs`

- [ ] **Step 1: 写失败测试**：
  - `heartbeat_route_responds_while_store_locked`：`#[tokio::test(flavor = "current_thread")]`；另一 `std::thread` 持 Store 锁 2s 期间，`GET /api/v1/jobs` 应在 `Reservation` 截止时间内返回 503，而且执行器上同时跑的 `tokio::time::sleep(10ms)` 任务能按时完成（误差小于 200ms）。
  - `probe_seen_released_during_persist`：在 `query_persist` 中持 Store 锁时，另一线程 `seen.lock()` 能在 50ms 内拿到。
- [ ] **Step 2: 运行确认失败** `cargo test -p api --test executor_not_blocked`，预期 FAIL（sleep 任务被饿住 2s）。
- [ ] **Step 3: 按 Files 实现。**
- [ ] **Step 4: 运行通过** `cargo test -p api`，预期 PASS。
- [ ] **Step 5: 提交** `fix(api): keep blocking store work off async executor`

### Task 8: VACUUM 与 jobs 锁探测

**Files:**
- Modify: `crates/api/src/maintenance/database.rs`：VACUUM 之前在 Store 锁内设置写门禁 `state.write_gate.close()`（新增 `api/src/write_gate.rs`，`WriteGate { fn close(&self) -> GateGuard; fn admit(&self) -> Result<(), WriteClosed> }`）。播放进度、订阅、库写入口统一调用 `admit()`，门禁关闭时返回 503 + `Retry-After`。VACUUM 只在 `maintenance_window` 内执行，否则改跑 `PRAGMA incremental_vacuum(N)`。由于 `auto_vacuum=INCREMENTAL` 需要在建库时设置，这一项放进 Task 10 的 DDL。
- Modify: `crates/api/src/maintenance/runner.rs`：删除 `refuse_when_queue_lock_held`，改为 `run_maintenance(queue: &QueueGuard<'_>, …)`，用类型表达调用约定。
- Modify: `crates/jobs/src/queue.rs:323`：`Queue::tick` 先取出待执行的任务、释放锁，再运行 runner。签名改为 `pub fn tick(queue: &Mutex<Queue>, runner: &dyn Runner, now) -> TickReport`。
- Test: `crates/api/tests/maintenance_write_gate.rs`、`crates/jobs/tests/tick_reentrancy.rs`

- [ ] **Step 1: 写失败测试**：
  - `playback_progress_gets_503_during_vacuum`：注入一个可阻塞的 VACUUM 钩子，期间 `POST /api/v1/playback/progress` 返回 503 并带 `Retry-After`，不会等 5s 后才返回 500。
  - `tick_runner_can_lock_queue`：runner 内部 `queue.lock()` 不死锁，`recv_timeout(2s)` 能收到完成信号。
- [ ] **Step 2: 运行确认失败** `cargo test -p api --test maintenance_write_gate` 与 `cargo test -p jobs --test tick_reentrancy`，预期 FAIL 或超时。
- [ ] **Step 3: 按 Files 实现。**
- [ ] **Step 4: 运行通过** `cargo test -p api -p jobs`，预期 PASS。
- [ ] **Step 5: 提交** `fix(maintenance): gate writers during vacuum; release queue lock before tick`

### Task 9: 测试不再永久挂起

**Files:**
- Create: `crates/store/src/test_support/gate.rs`：`TimedGate::new(name) -> (GateA, GateB)`，`wait(&self)` 内部用 `mpsc` 加 `recv_timeout(Duration::from_secs(10))`，超时就 `panic!("gate {name} timed out")`。
- Modify: `crates/store/src/library_read/lease.rs:251-257`：`wait_gate` 改用 `TimedGate`。
- Modify: `crates/store/src/library_read/lease/tests.rs:62-258,365-446`：所有 Barrier 换成 `TimedGate`。`:133-147` 和 `:416-446` 改为先过门禁再设截止时间。`:365-410` 的 authorizer 门禁加 `hook_once`。
- Modify: `crates/store/src/library_query/facets.rs:~395`：空转循环加上限（`for _ in 0..1000`），超出就 `panic!`；`done_rx.recv()` 改为 `recv_timeout(10s)`。
- Modify: `crates/store/tests/write_transaction_contention.rs`：prune 线程用 `busy_handler` 回调发出"已进入等待"信号，主线程收到信号后再 COMMIT；`join` 改为带超时的 channel。
- Modify: `crates/api/tests/storage_contention.rs:50,144`：改为 `#[tokio::test(flavor = "multi_thread", worker_threads = 2)]`。
- Modify: `crates/store/src/library_read/context/tests.rs:96-128`：`finished.recv()` 改为 `recv_timeout(10s)`，预算从 3s 调到 10s。

- [ ] **Step 1: 写失败测试** `test_support/gate.rs::unpaired_gate_panics_instead_of_hanging`：只有一方调用 `wait()`，配合 `#[should_panic(expected = "timed out")]`，超时取 100ms（测试专用构造 `TimedGate::with_timeout`）。
- [ ] **Step 2: 运行确认失败** `cargo test -p store unpaired_gate_panics`，预期 FAIL（类型不存在）。
- [ ] **Step 3: 按 Files 实现。**
- [ ] **Step 4: 运行通过** 连续 20 次 `for i in $(seq 20); do cargo test -p store library_read -- --test-threads=16 || break; done`，然后跑 `cargo test -p api --test storage_contention`。预期全部 PASS，无挂起。
- [ ] **Step 5: 提交** `test: bound every cross-thread gate with a timeout`

### Task 10: 一次性 DDL 优化（library v3）

**Files:**
- Modify: `crates/store/src/schema/v1/library.sql`：
  - 新增 `CREATE INDEX operations_library_kind ON library_operations(library_id, kind, updated_at_ms DESC);`
  - 新增 `CREATE INDEX playback_sessions_media ON playback_sessions(media_id);`、`playback_logs_media ON playback_logs(media_id);`、`playback_metrics_media ON playback_metrics(media_id);`
  - 新增 `CREATE INDEX pending_deletions_due ON pending_file_deletions(next_retry_at);`，**并同时**把 `crates/store/src/pending_file_deletions.rs::list_due_file_deletions` 的 `ORDER BY rowid` 改成 `ORDER BY next_retry_at`（不排掉 rowid 排序，planner 永远不会选这个索引，那就是一个死索引；该查询无 LIMIT，集合与顺序无关）。
  - 新增 `CREATE INDEX metadata_inputs_media ON library_metadata_inputs(media_id);`
  - `files_library_media` **已经存在**（`library.sql:102`），不要再加。
  - **`items_protocol_default` 与 `items_protocol_default_explicit` 两个都保留，不要删任何一个**（勘误 E4：`library_query/protocol/page.rs:136-144` 的 `work_index_for_sort` + SQL 里的 `INDEXED BY {index}` 按名字硬引用；两者服务于不同的 ORDER BY，交叉使用会引入 1~3 个 TERM 的 TEMP B-TREE）。索引数因此是 73→79。在 `library_query_plans.rs` 里把四条计划都钉住，且不要改索引名。
  - 对 `library_item_terms`、`library_protocol_terms`、`library_protocol_people`、`user_library_items`、`library_file_units` 加上 `WITHOUT ROWID`（主键为复合键且行宽可接受，逐表量 `dbstat` 后再决定；`playback_units` 实测 PK b-tree 287 B/row > rowid 表 257 B/row，**跳过**）。
  - 文件头加 `PRAGMA auto_vacuum = INCREMENTAL;`，由 `open.rs` 在建库时、建表之前执行。
- Modify: `crates/store/src/schema/layout.rs`：`LIBRARY_USER_VERSION = 3`；新增 `StoreError::LayoutMismatch(String)`（在 `crates/store/src/lib.rs`），只从本文件的布局校验抛出，报错文案点名 `scripts/storage-cutover-rebuild.sh`。
- Modify: `crates/store/src/schema/v1/objects.rs`：`LIBRARY_INDEXES` 增删同步（`layout.rs:108` 用它做严格校验，不同步会导致**每次 `Store::open` 失败**）。
- Modify（P5 连带的测试面）：`crates/store/tests/schema_version_guard.rs`（版本号断言 2→3）、`crates/store/tests/query_model_integrity_barrier.rs`、`crates/store/tests/query_model_shared_barrier.rs`（`ORDER BY rowid` → 全列排序）、`crates/api/tests/storage_contention/harness.rs`（行身份从 `rowid` 改为 PK 元组）。
- Modify: `docs/database/library.md`、`docs/database/README.md`：同步表数、索引数，补上漏记的 `units_protocol_order`。
- Test: `crates/store/tests/library_query_plans.rs`（沿用 `cleanup_index_work.rs` 中 EXPLAIN 断言的写法）

- [ ] **Step 1: 写失败测试** `library_query_plans.rs`：
  - `integrity_lookup_uses_library_index`：浏览分页里的 integrity 子查询计划包含 `operations_library_kind`，且不含 `TEMP B-TREE`。
  - `delete_playback_for_media_uses_media_indexes`：三条 DELETE 的计划都是 `SEARCH … USING INDEX`。
  - `due_pending_deletions_use_index`：同上。
  - `protocol_default_and_explicit_indexes_each_avoid_a_sort`：四条计划（两个索引 × 两种排序方向交叉）各自无 TEMP B-TREE，且两个索引名都必须存在。
  - `v2_library_db_is_rejected`：用 `LIBRARY_USER_VERSION=2` 的空库文件 `Store::open`，返回 `StoreError::LayoutMismatch`，错误信息包含 `storage-cutover-rebuild.sh`（对应 Review Focus 第 5 条）。
- [ ] **Step 2: 运行确认失败** `cargo test -p store --test library_query_plans`，预期 FAIL。
- [ ] **Step 3: 按 Files 实现。**
- [ ] **Step 4: 运行通过** `cargo test -p store`，并跑 `cargo test -p store --test storage_work_bounds --release`（`seek_scale` / `library_integrity_scale` 都在这个 binary 里），确认耗时不回退。
- [ ] **Step 5: 提交** `perf(store): add missing indexes and compact library layout (v3)`

### Task 11: 读租约不再每次全量校验布局

**Files:**
- Modify: `crates/store/src/library_read/pool.rs`：连接加入池时执行一次 `validate_reader`，并记录 `PRAGMA schema_version` 和文件身份（dev, ino）。
- Modify: `crates/store/src/library_read/lease.rs`：`prepare` 和事后检查都只比对 `PRAGMA schema_version`（1 条查询）和缓存的文件身份，不一致才重新做全量校验；全量校验失败时关闭该连接，返回 `LeaseError::Layout`。
- Test: `crates/store/tests/library_read_pool.rs` 新增用例

**Interfaces:**
- Consumes: Task 10 的 v3 布局。

- [ ] **Step 1: 写失败测试**：
  - `steady_state_lease_runs_constant_validation_statements`：用 `Connection::authorizer`（已有测试钩子）统计一次租约中除业务查询以外执行的语句数，断言不超过 4。
  - `schema_change_forces_revalidation`：读池外的写连接执行 `CREATE TABLE zz(x)` 后，下一次租约返回 `LeaseError::Layout`。
- [ ] **Step 2: 运行确认失败** `cargo test -p store --test library_read_pool`，预期第 1 条 FAIL（实际约 250 条）。
- [ ] **Step 3: 按 Files 实现。**
- [ ] **Step 4: 运行通过** `cargo test -p store && cargo test -p api --test library_read_concurrency`，预期 PASS。
- [ ] **Step 5: 提交** `perf(store): validate read leases by schema_version instead of full layout`

### Task 12: 连接 pragma 调优

**Files:**
- Modify: `crates/store/src/open.rs` 的 `apply_connection_policy(conn, kind: DbKind)`：
  - 所有库：`cache_size = -16384`（16 MiB）、`temp_store = MEMORY`、`journal_size_limit = 67108864`、`mmap_size = 268435456`（256 MiB）。
  - `DbKind::Cache`：`synchronous = NORMAL`；其余库保持 `FULL`。
  - 打开时执行 `PRAGMA optimize = 0x10002`，关闭（Drop）时执行 `PRAGMA optimize`。
  - 读池连接（`library_read/pool.rs`）用同样的 cache/mmap/temp_store 设置。
- Test: `crates/store/tests/connection_policy.rs` 新增用例

- [ ] **Step 1: 写失败测试** `connection_policy_per_kind`：打开 Store 后逐库读取 pragma，断言 cache.db 的 `synchronous` 为 1、其余为 2，`temp_store` 为 2，`journal_size_limit` 为 67108864。
- [ ] **Step 2: 运行确认失败** `cargo test -p store --test connection_policy`，预期 FAIL。
- [ ] **Step 3: 按 Files 实现。**
- [ ] **Step 4: 运行通过** `cargo test -p store`，预期 PASS。
- [ ] **Step 5: 提交** `perf(store): tune per-database connection pragmas`

### Task 13: 操作类表的保留期

**Files:**
- Modify: `crates/api/src/maintenance/data_retention.rs`：新增保留类别 `operations`（默认 30 天），分批删除以下终态行：`library_operations`、`scan_operations`、`media_operations`，以及 `library_removals`（连带 roots/files 子表，靠外键 CASCADE 或显式删除）。终态集合直接引用各模块已有的 state 枚举，不硬编码字符串。
- Create: `crates/store/src/operations_retention.rs`：`Store::prune_terminal_operations(cutoff_ms: i64, batch: usize, dry_run: bool) -> Result<PruneCounts, StoreError>`，每批是一个 `write_tx`，单批不超过 `batch` 行。
- Test: `crates/store/tests/operations_retention.rs`

- [ ] **Step 1: 写失败测试**：
  - `prunes_only_terminal_operations_before_cutoff`：插入终态旧行、终态新行、运行中旧行，断言只删除第一类。
  - `dry_run_reports_without_deleting`：dry_run 只报数，不删行。
  - `integrity_lookup_ignores_pruned`：剪除后浏览页的 integrity 状态仍然正确（取最新的那一行）。
- [ ] **Step 2: 运行确认失败** `cargo test -p store --test operations_retention`，预期 FAIL。
- [ ] **Step 3: 按 Files 实现**，并在 `data_retention.rs` 中接入，打 `info!`（类别、删除数、耗时）。
- [ ] **Step 4: 运行通过** `cargo test -p store -p api`，预期 PASS。
- [ ] **Step 5: 提交** `feat(maintenance): retain terminal operation rows for 30 days`

### Task 14: 热点查询，以及前端浏览改用游标分页

**Files:**
- Modify: `crates/store/src/roots.rs:431`：`LIKE ?` 改为范围谓词 `input_key >= ?1 AND input_key < ?2`，其中 `?2` 是前缀末字符加 1 后的字符串（复用 `path_key` 模块里现有的前缀上界函数；没有就新增 `path_key::prefix_upper_bound(&str) -> String`）。同时把逐目录查询合并成一次查询，在 Rust 侧分组。
- Modify: `crates/api/src/http/library.rs:155`、`library_gallery.rs:49`：接受 `cursor` 查询参数，有 cursor 时走 `BrowsePage::Cursor`；`include_total` 只在第一页（没有 cursor）时生效。响应带上 `next_cursor`。
- Modify: `web/lib/api/libraries.ts` 与浏览列表组件：第二页起用 `next_cursor`，不再传 offset。`docs/api-contracts/self.md` 同步更新。删除 `offset` 参数，不做兼容。
- Test: `crates/api/tests/library_cursor_paging.rs`、`crates/store/tests/root_inputs_prefix.rs`、`web/test/library-paging.test.mjs`

- [ ] **Step 1: 写失败测试**：
  - `cursor_pages_cover_all_items_once`：250 个条目，每页 50 条，用 cursor 逐页翻完，断言每个 media 恰好出现一次，且只有第一页带 `total`。
  - `offset_param_is_rejected`：`?offset=50` 返回 400。
  - `prefix_inputs_match_like_semantics`：路径中包含 `%`、`_` 和非 ASCII 字符时，新的范围查询结果与按前缀过滤的结果集相同。
  - 前端：`buildBrowseQuery({cursor})` 不包含 `offset`。
- [ ] **Step 2: 运行确认失败** 运行上述三个测试文件，预期 FAIL。
- [ ] **Step 3: 按 Files 实现。**
- [ ] **Step 4: 运行通过** `cargo test -p store -p api && node --test web/test/*.test.mjs`，预期 PASS。
- [ ] **Step 5: 提交** `perf(library): cursor paging for browse; range scan for root inputs`

### Task 15: 拆分超过硬限制的文件

**Files:**
- `crates/store/src/media_operations/references.rs`（879 行）：按用途拆成 `references/{collect.rs,rewrite.rs,mod.rs}`。
- `crates/indexer/tests/browser.rs`（858 行）：按场景拆成 `tests/browser/{cookies.rs,render.rs,main.rs}`。
- `crates/api/tests/management/subscription_depth.rs`（805 行）：拆成两个文件。
- `crates/media-server/src/provider/query.rs`（801 行）：Task 3 删掉 `legacy_*` 后重新量行数，仍超过 800 才拆。
- `crates/api/src/scan_operations/tests.rs`（802 行，勘误 E8：计划漏列）：按场景拆分。
- `AGENTS.md` 中 "Already over the hard limit" 一节：更新为拆分后的状态（日期 2026-10-26）。

- [ ] **Step 1: 量行数** `git ls-files 'crates/**/*.rs' | xargs wc -l | awk '$1>800 && $2!="total"'`，实测（2026-10-26）列出 5 个文件（若已做 Task 3，`provider/query.rs` 可能降到 800 以下）。
- [ ] **Step 2: 按职责拆分**，`mod.rs` 只做模块声明和重导出；不改任何行为。
- [ ] **Step 3: 再量一次**，预期输出为空。
- [ ] **Step 4: 运行通过** `cargo test -p store -p indexer -p api -p media-server`，预期 PASS。
- [ ] **Step 5: 提交** `refactor: split files over the 800-line hard limit`

### Task 16: library.db 结构审计——删掉不再挣钱的东西（性能导向）

**背景：** 存储改造已多轮迭代，schema 里很可能残留"当前读路径已经不用"的表和索引，这些是纯写放大。
本计划明确允许推翻既有数据库设计，`main` 不作为权威。

**Files:**
- Create: `docs/audits/2026-10-26-library-schema-audit.md`
- Modify: `crates/store/src/schema/v1/library.sql`、`crates/store/src/schema/v1/objects.rs`、`crates/store/tests/`

**每张表必须回答（都要证据）：**
- 今天谁写、谁读（`git grep` 到 store fn + 调用点）。无生产读者的表 = 删除候选。
- 预期行数 vs 行宽。宽 JSON 列（`quality_json` / `fields_json` / `normalized_json` / `details_json` …）不得出现在 browse/detail/playback 热路径的 `SELECT` 里。
- 主键是否合理；`WITHOUT ROWID` 是否划算（复合主键、行窄、无 rowid 消费者）。
- 是否重复持有同一事实。重点审 `library_items`（摘要）vs `library_item_terms` vs `library_protocol_*` vs `library_query_count_rebuild` vs `library_units` vs `library_unit_terms`：谁是单一事实源、谁是派生缓存、派生缓存是否还有读者。

**每个索引必须回答：** 哪条真实查询用它（`EXPLAIN QUERY PLAN` 证据）；是否是另一个索引的冗余前缀；是否只有测试在用。

**两个横切问题要落结论：**
- 生产代码有 7 处用隐式 `rowid` 当排序键（`pending_file_deletions`、`probe_jobs`/`probe_job_units`、`scan_operations`、`playback_session_ids`、`unidentified`、`fingerprint_*`），这是脆弱契约且挡住 `WITHOUT ROWID`。逐表决定：加显式单调列，或声明该表永久 rowid-based。
- `library_operations` + `scan_operations` + `media_operations` + `library_removals` 这一族近乎同构的操作日志表，是否应合并成一张带 kind 判别列的表。

**Acceptance:** 审计文档给出逐表/逐索引裁决 + 按收益/风险排序的修复清单，且"保留"的项写明理由；
低风险高收益项（死表、死索引、冗余索引）直接落地，`cargo test -p store -p api` 全绿；
若需进一步 DDL 变更，与 Task 10 的版本 bump 合并（不要各自升版本）。

### Task 17: 性能基线 + 确定性回归门禁

**背景：** "性能优先"必须可量化，否则每个 Task 的"没回退"都只是感觉。

**Files:**
- Create: `crates/store/tests/perf_baseline.rs`（或 `crates/store/tests/storage_work_bounds/` 的新模块）
- Create: `docs/audits/2026-10-26-perf-baseline.md`

**复用已有资产（不要重造）：** `crates/store/tests/storage_work_bounds/`（含 `seek_scale.rs`、`library_integrity_scale.rs`）、
`StatementStatus::FullscanStep` 计数器、Task 10 的 `library_query_plans.rs`、`crates/api/tests/storage_contention/`。

- 确定性的种子 fixture（例如 3 个 Library / 20k media / 60k files / 10 facets / 5k episodes），全部经 store API 直接构造，无 sleep、无网络、无 Chromium。
- 每个热点操作报告：墙钟 p50/p95、SQL 语句数、全表扫描步数。至少覆盖：browse 首页、browse 深翻页（offset 与 cursor）、条目详情、每个 facet 维度、播放源选择、1000 文件扫描发布、integrity 状态查询。
- **硬断言只用确定性计数**（语句数、全表扫描步数），不得用墙钟阈值（慢 CI 必飘）；墙钟数字打印并写入基线文档。
- 文档里写清一键门禁命令（如 `cargo test -p store --release --test storage_work_bounds`），并要求每个 Task 收尾都跑一遍。

**Acceptance:** 默认 `cargo test` 增量的运行时间 < 60s（重活放 `--release` / `#[ignore]` 并在文档注明）；
基线文档记录首次数值；故意加一个全表扫描能让门禁失败（验证门禁真的有效）。

### Task 18–21（基线实测发现，新增）

Task 17 建好门禁后立刻量出四件计划没看过的事，各自独立成任务（共享任务板上 id 分别是 `task-18`…`task-21`）：

| Task | 问题 | 实测 | 要求 |
|---|---|---|---|
| 18 | `Store::pending_probe_dispatch_ids` 按 `j.created_at_ms, j.id, u.file_id` 排序，没有任何索引能满足 | 每次 refill 都物化并排序整个 queued 候选集；一次 N 条排空是 O(N²) 行访问（10 万条时是主要成本） | 先判定 FIFO 语义是否必须：必须 → 用索引满足顺序（或把 `created_at_ms` 并入 `probe_unit_dispatch`）；不必 → `ORDER BY u.rowid`。用 `EXPLAIN` + 确定性计数钉住 |
| 19 | **用户维度**的播放源选择比同一查询去掉 library 过滤慢 2000 倍 | 0.029 ms → **56.4 ms p50 / 59.1 ms p95** | 这是全库最大的延迟异常，且在播放起播路径上。查 `EXPLAIN QUERY PLAN` 两版（有/无 user 过滤）的 join 顺序与索引，用计划断言钉住；需要新索引则并入一次 v4 bump |
| 20 | 幂等重发布（内容未变）比首次插入更贵，且什么都没写 | insert p50 950.8 ms / 1000 文件 vs 重发布 p95 **2140.9 ms**（`unchanged = 1000`） | 查 `library_write/reduce_incremental` 是否在"发现没变"之前就做了摘要工作、是否每文件一个 `write_tx`（1000 次 fsync）。让未变文件在摘要工作之前短路，并批事务 |
| 21 | 详情/播放源/会话签发/扫描发布/进度写入/integrity 查询**没有**确定性计数器，只能靠墙钟 | 门禁对这类热点只能"记录"不能"判红" | 统一一个 `with_library_stats` 闭包式接缝（test-only 走 `test-support` feature），把这些行从"informational"升级为"hard" |

Task 19/20 都是**性能优先**这条主线上的直接收益，排在 Task 2–15 之后、Task 15 之前做。
Task 21 会碰 `crates/store/src/lib.rs` 与 `open.rs`，必须与 Task 4（`test-support` feature）、Task 12（pragma）排期错开。

### Task 22–28（执行过程中新发现，新增）

执行到中段时又暴露出七个问题，全部有实测支撑，各自独立成任务（板 id 见括号）：

| Task | 问题 | 实测 / 依据 | 处置 |
|---|---|---|---|
| 22（`task-22`） | Task 7 只清了最宽的两个点；**174 处 `async fn` 里仍直接 `state.store.lock()`**（72 个文件），另有 `ApiState::new` 起的后台驱动（`chapter_frame_service::worker_loop` 等）在 executor 上拿锁，连 `/health` 都会被饿住 | Task 7 的 `rg` 枚举 + 测试必须把 state 建在 runtime 外才能隔离 | 按爆炸半径分片迁移：先 playback / search，再 library 管理，再长尾；复用 Task 7 的 offload 接缝与 503 `runtime.busy` 语义 |
| 23（`task-23`） | 收尾要求 `clippy --workspace --all-targets -D warnings` 通过 | `cargo check --workspace --all-targets` 实测 **362 条 warning** | 先分诊（本会话碰过的文件必须为 0，其余机械修），禁止用 `#[allow]` 掩盖 |
| 25（`task-25`） | Jellyfin person 过滤是**每张卡片重扫整库 people 表**（`OR` 挡住两个索引） | 910 ms/1 卡、9.1 s/10 卡；同一谓词"身份只解析一次" → 20k 卡 54 ms。同结果同范围:1,307 ms → 8.3 ms = **157×** | 改成身份解析一次 + `IN` 驱动 join；用计划断言钉住;若 `protocol_people_wire/tmdb` 最终无用则后续轮次删掉 |
| 26（`task-26`） | Task 14 删掉 offset 后，A-Z 跳转与"续播位置"从 O(1) seek 退化为沿游标走 10–24 页 | web 组件实测 34 条 tsc 错误逼出的建模冲突 | 恢复 O(1):要么给游标加 anchor/before，要么 `item_index` 返回不透明游标。**不得把 offset 加回来** |
| 27（`task-27`） | Task 7 把 auth 移出 executor 后，长扫描期间**每个 member 路由都 503**（心跳也挂） | `storage_contention` 实测 `{200: 35, 503: 20}`,期望全 200 | 根因是架构而非超时:token→user 是 app.db 的**读**,WAL 本可并发。给 auth 一条专用读连接（沿用 Task 6 `AppHeartbeat` 与 Task 12 读池先例），而不是放宽 deadline 或把 503 合法化 |
| 28（`task-28`） | `LIBRARY_INDEXES` 本身不全（缺 `files_kind_path`、`terms_value`），且"多余索引"不拒绝 | Task 11 实测;严格校验一直是单边的 | 补齐 + 决定是否把"多余索引"也当作布局不匹配;与 v4 同文件，需排期 |

**并行与环境的教训（写进流程）**：同一 workspace 并发时，`cargo` 会串行等 target 锁，而**一个人清理 `target/` 会让所有在跑的构建报 `No such file or directory` 假失败**。结论：每个 agent 用 `/tmp/dsh-<task>` 作为 `CARGO_TARGET_DIR`，仓库内 `target/` 只保留主的；清理只在无构建时做。

### 收尾

- [ ] `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、`node --test web/test/*.test.mjs`、`cd web && npx tsc --noEmit`，全部通过。
- [ ] 跑 Task 17 的性能门禁，与基线对比，确认确定性计数（语句数、全表扫描步数）无回退。
- [ ] 用 `scripts/storage-cutover-rebuild.sh` 重建本地数据目录，启动后手动点一遍：浏览分页、播放 10 分钟（期间不应出现 404 或重建会话）、浏览器设置页。
- [ ] **codex 独立评审**（本计划的强制门禁）：

  ```bash
  CODEX=/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex
  git tag -f review-base f2d4f033          # 本次工作的起点，不是 main
  # 原生 CLI 实测：review 子命令不允许 --base 与 PROMPT 同用，且拒绝 --worktree。
  # 为保留完整上下文及独立 worktree，用 exec；范围由已提交简报明确限定。
  $CODEX exec --worktree -s read-only \
    -o /tmp/codex-review-out.md \
    - < docs/superpowers/plans/2026-10-26-codex-review-brief.md
  ```

  评审简报必须写明：范围只限 `review-base..HEAD`；意图是执行本计划；判据是本计划的证据与验收标准 + `AGENTS.md`/`CONTEXT.md` 的硬约束；重点按优先级
  ACL 归属 → 布局/版本拒绝 → 死锁饥饿 → 查询计划 → 测试是否被弱化 → 行数与形状 → 日志 → 公开图片路由；
  输出 BLOCKERS / SHOULD-FIX / PLAN DEVIATIONS / NITS(≤10) / 逐项 PASS-FAIL-NA 对照表。
  用 `--worktree` 让它在独立 worktree 里跑测试，不污染当前工作区。
- [ ] 按评审结论修 BLOCKERS，再复跑一次评审直到无 BLOCKER。

## 计划勘误（实测，2026-10-26）

执行时逐条实测计划里的 claim，以下与实测不符，**以本节为准**：

| # | 计划原文 | 实测 | 处理 |
|---|---|---|---|
| E1 | 隐含前提：HEAD 可编译、测试全绿 | HEAD 编译不过：4 个生产文件 + 3 个测试文件仍引用已删除的 `ledger`/`LedgerRow` | 新增 Task 0 前置 |
| E2 | 未提及探测入队/ack/backlog 的行为丢失 | 重构过程中丢了 5 处（缺失文件仍入队、窗口满即取消 durable 行、无条件 ack、指纹不 refill、硬编码采样时长） | 新增 Task 0b |
| E3 | `StoreError::LayoutMismatch`（Task 10、Task 4 的测试都断言它） | 该 variant 在 HEAD 和 `main` 都不存在 | 在 Task 10 里新增 variant，只从 `schema/layout.rs` 抛出 |
| E4 | Task 10 P4：`items_protocol_default` 与 `_explicit` 前三列相同 → 冗余，删一个 | **两者都必须保留**。`library_query/protocol/page.rs:136-144` 的 `work_index_for_sort` 与 SQL 里的 `INDEXED BY {index}` 按名字硬引用；`media_id` 无法穿过 explicit 索引里的 `title` 键。互相交叉使用会各自多出 1 个/3 个 TERM 的 TEMP B-TREE | 删除该条；索引数 73→**79**；在 plan 测试里钉住四条计划 |
| E5 | Task 10 P3：加 `pending_deletions_due(next_retry_at)` | 索引本身不会被选中，因为 `list_due_file_deletions` 里 `ORDER BY rowid` 让 planner 直接顺序扫描（无 LIMIT，集合与顺序无关） | 加索引 + 把该查询改成 `ORDER BY next_retry_at`（一并修注释） |
| E6 | Task 10 只列了 `library.sql` + `layout.rs` | 还必须改 `crates/store/src/schema/v1/objects.rs` 的 `LIBRARY_INDEXES`（`layout.rs:108` 用它做严格校验），否则删/加索引后**每次 `Store::open` 都失败** | 写入 Task 10 的必改文件 |
| E7 | Task 10 的 P5 只提到改 DDL | `WITHOUT ROWID` 会让 `ORDER BY rowid` 在 3 个测试里 prepare 失败，另 1 个 harness 用 `SELECT rowid, *` 做行身份 | 一并修 `query_model_{integrity,shared}_barrier.rs`、`api/tests/storage_contention/harness.rs`。生产代码对 6 张候选表的 rowid 引用实测为 **0**（其余 7 处 rowid 依赖都不在这些表上，已记入 Task 16） |
| E8 | Task 15 列了 4 个超限文件；`seek_scale.rs` 路径 | 实测超限文件是 5 个（多一个 `crates/api/src/scan_operations/tests.rs`，802 行）；`seek_scale.rs` 实际在 `crates/store/tests/storage_work_bounds/seek_scale.rs`（`storage_work_bounds` 这个 test binary 的模块） | 两处修正 |
| E9 | Task 7 的 Files 只列了 `http/jobs.rs` | 真正的热点是 `crates/api/src/http/auth.rs`：`authenticate` 的 else 分支与 `require_admin` 都在 executor 上直接 `store.lock()`，影响**每一个非 library 路由** | 纳入 Task 7（计划正文本来就写了"按 rg 逐个迁移"） |
| E10 | Task 10：`PRAGMA auto_vacuum = INCREMENTAL` 写在 `library.sql` 文件头 | **放在 DDL 批里是静默空操作**（WAL 模式下、以及事务内都被忽略，实测读到 0 而不是 2）。SQLite 只在"空库且非 WAL"这一刻接受它 | 改由 `open.rs::apply_auto_vacuum_on_create` 在刚打开的连接上、`is_uninitialized` 为真时执行（早于建表，也早于 `apply_connection_policy` 切 WAL）；`library.sql` 头部注释记录原因 |
| E11 | Task 5：删除前端 `examined` 字段 | `examined` **是活的 API**：`http/job_maintenance.rs:321`/`:332` 每类与聚合都会发，`docs/api-contracts/self.md:164` 记录它是 `data_retention` 唯一的"已检视"计数（那里 `scanned` 恒为 0）。真正的兼容残留只是 `examined ?? scanned` 兜底 | 前端恢复 `examined`（含 `classes[]`）并照常展示"检视"列，只删兜底；后端聚合里的 `"scanned": examined` 才是遗留别名 → 归 Task 3/4 的后端清理 |
| E12 | Task 10 的文件清单 | 版本号 bump 还会打断**三处版本字面量断言**：`crates/store/tests/schema_version_guard.rs`、`crates/store/tests/open.rs`（消息串里也带版本）、`crates/store/tests/query_model_layout.rs` | 一并改，属"版本 bump 强制项" |
| E13 | Task 10 只写"加 `pending_deletions_due`" | 这条索引**必须配一行调用方改动**才有用（`list_due_file_deletions` 的 `ORDER BY rowid` → `next_retry_at`）；只加索引会得到死索引 | 已并入 Task 10 正文（同 E5） |
| E14 | Task 10 未说明 `playback_units` 的处理 | 实测 PK b-tree 287 B/row > rowid 表 257 B/row，超出窄行预算，转换无收益 | 明确拒绝转换，测试钉住 `wr = 0`，docs 记录实测数字 |
| E15 | Task 9 的测试文件描述 | `crates/api/tests/storage_contention.rs:6-7` 的模块注释仍写仪器快照"含 `rowid`"，Task 10 已改为"rowid 表用 rowid、WITHOUT ROWID 表用 PK 元组" | 归 Task 9 收尾时顺手改注释 |
| E16 | Task 8："VACUUM 只在 `maintenance_window` 内执行" | **代码里不存在 `maintenance_window`**（`grep -rn maintenance_window crates/` 零命中），计划也没说它从哪来 | 用**已有的准入闸门**（`vacuum_enabled && !library_busy` + 冷却/空闲页阈值/磁盘余量）；不满足时跑 `PRAGMA incremental_vacuum(N)` 并记录跳过原因。**不新增时间窗配置面** |
| E17 | 隐含假设：所有库都由 `Store::open` 建库、因此都有 `auto_vacuum=INCREMENTAL` | `jobs.db` 由 `jobs::Queue` 打开，不走 `Store::open` 的建库路径，实际**不是** INCREMENTAL | 先读 `PRAGMA auto_vacuum`，不是 INCREMENTAL 就如实报"跳过"而不是假装回收；`jobs.db` 的 pragma 统一归 Task 12 / Task 16 处理 |
| E18 | Task 8：`run_maintenance(queue: &QueueGuard<'_>, …)`、`Queue::tick` 改自由函数、`Reservation::run` 的 `FnOnce(&Budget, &mut StoreGuard)` | `QueueGuard`/`Budget`/`StoreGuard` 这三个类型**在代码里都不存在**（`blocking_budget` 的 `Reservation` 只是一个信号量 permit） | 按语义落地：`QueueGuard` 在 `maintenance/runner.rs` 内定义并保留 `maintenance::run(state, job)` 一行构造器（`worker.rs` 不动）；`blocking_budget` 的 L4 约定改为文档化 + `try_lock_for` 有界等待，不臆造类型 |


## 后续单独立项（不在本计划内）

- **按库拆分 Store 锁（D2）**：把 `Arc<Mutex<Store>>` 拆成 `AppDb`、`LibraryDb`、`SubscribeDb`、`CacheDb` 四把锁，再把只读的详情和播放读迁到读连接池。这会改到 543 处调用点，需要先写 spec，明确跨库操作的固定加锁顺序（app → library → subscribe → cache）。
- **search_text 全文检索**：用 FTS5 或 trigram 替代 `instr` 扫描，等规模数据显示是瓶颈后再做。

## Self-Review 记录

- **规格覆盖**：单表浏览和同事务摘要已满足，没有任务；"不兼容旧版"由 Task 2–5 覆盖；"读隔离"由 Task 11 优化，行为不变。
- **类型一致**：`files_for_media_in_library`（Task 2，索引 `files_library_media` 已存在，实测无需新增）、`AppHeartbeat`（Task 6）、`TimedGate`（Task 9，同时被 Task 4 的 `test_support` cfg 隔离影响，路径都是 `store/src/test_support/`）、`LayoutMismatch`（Task 10 新增，Task 4 复用）、`LIBRARY_USER_VERSION=3`（Task 10，Task 11 依赖），前后命名一致。
- **执行顺序（2026-10-26 实际执行）**：
  1. **Task 0**（恢复可编译基线）—— 阻塞一切，必须先做。
  2. **Task 1 / Task 0b / Task 6 / Task 10** 可并行：前端、探测生命周期、租约心跳、DDL 四块写集互不重叠。
  3. **Task 7** 接 Task 6（同子系统），**Task 11** 接 Task 9 与 Task 10，**Task 12** 接 Task 11（共用 `library_read/pool.rs`）。
  4. **Task 2 → Task 3 → Task 4** 串行：三者都动 `crates/store/src`，且 Task 4 依赖 Task 2 的 `files/read.rs` 结果。
  5. **Task 5** 与前端并行，但 `web/lib/api/libraries.ts` 与 **Task 14** 冲突，必须错开。
  6. **Task 13 / Task 16 / Task 17** 可穿插：17 越早越好，后面每个 Task 都要拿它当"没回退"的判据。
  7. **Task 15** 最后（拆分行数，且要等 Task 3 删完 `legacy_*` 后重新量行数）。
  8. **收尾**：全量测试 + 性能门禁 + codex 独立评审。
- **并行约束**：同一 workspace 内多个 agent 并行时，`cargo` 会串行等锁（正常现象）；跨 agent 的编译错误要先确认是不是对端在写文件，不要据此判断"基线坏了"。写集重叠时以共享任务板上的 write scope 为准。
