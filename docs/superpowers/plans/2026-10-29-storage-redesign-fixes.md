# feat/storage-redesign 审核修复 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修掉审核发现的 3 个合并阻塞项（B1/B2/B3），再按优先级处理锁、索引、性能、零兼容和文档的建议修复项。

**Architecture:** 读写彻底分离。所有读走只读连接池（`StoreReaders`），所有写走唯一入口 `store_write`，准入检查放在锁内。写事务内不做 IO、不解析、不 `.await`。热点列表改为查投影表 `user_library_items`，配合部分索引和 keyset 分页，消除全库扫描和 N+1。

**Tech Stack:** Rust 2024，axum，rusqlite（SQLite WAL），parking_lot，tokio；前端 React + TS（pnpm）。

**Spec:** `docs/test-reports/2026-10-29-storage-redesign-review.md`（审核报告，下文简称「报告」；条目编号 B*/F*/S-DB*/P-*/Z*/D* 与报告一致）

## Global Constraints

- TDD：先写会失败的测试（red）再实现（green）。测试放在行为的 public seam 上，断言可观察的行为。不测私有 helper、SQL 文本或 mock 调用次数。query-plan 测试除外：只断言「没有 SCAN / TEMP B-TREE」。
- 零向后兼容：不写兼容层、迁移垫片或旧字段兜底。schema 改动直接改 `schema/v1/*.sql` 和布局清单，不写 ALTER。
- 锁纪律：读永不拿 writer 锁；写锁只包住真正的写；任何锁都不得跨 `.await`、网络或文件 IO 持有。
- 不可改的常量：`REMOTE_READ_LIMIT = 1`、`REMOTE_READ_COOLDOWN = 500ms`、`STORE_READERS = 8`。
- ADR-0012 的删除语义保持不变：先隐藏，取消任务，再删除，最后清理孤儿 Media（含 favorites 和进度）。
- 资源：同一时间只跑一个 cargo 进程；一律 `-j4`，测试一律 `-- --test-threads=4`；先跑定向测试，全量只在最后跑一次并带 `--no-fail-fast`。
- Git：只在 `feat/storage-redesign` 上提交；不 push main，不 force push，不开 PR；不提交 `docs/test-reports/`、`.e2e/`。
- 尺寸：`.rs` 文件硬上限 800 行；生产函数硬上限 120 行；`lib.rs` 只放模块树和 re-export。
- 术语以 `CONTEXT.md` 为准（Library、Media、Subscribe、Release…）。
- 提交信息结尾加 `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`。
- 性能验收以 `docs/testing/performance-slo.md` 为准（Task 0 产出）：每个阶段结束都要跑 `perfgate.py` 的 idle 和 busy 两种模式。相对上一阶段，任何接口的 p95 都不得变差超过 20%；阶段 C 结束时所有接口必须达标。测得的数字放在 `.e2e/perf/`，不提交。

## Review Focus

1. **读连接带着未结束的事务回池**（B3 一类）：任何在读连接上开 SAVEPOINT 或 BEGIN 的代码出错后，连接必须回到 autocommit；否则 WAL 被钉住，后续读一直是旧快照。归还时的断言写在 Task 1。
2. **维护/只读模式开启后，已经在排队的写**：开启之前就在等锁的心跳或标记，开启之后不得落库。测试写在 Task 2。
3. **B1 改成乐观校验后的并发删除**：prepare 之后、validate 之前，文件被删或被移到其他 Library，响应里不能出现幽灵条目，也不能 500。测试写在 Task 3。
4. **扫描持有 writer 时的交互读**：登录、`/UserViews`、`/Items/{id}`、`GET /jobs` 都必须在扫描持锁期间 < 1s 返回 200，不能 503。测试写在 Task 4。
5. **删除 Library 后绑定它的订阅**：转移不能报「目标 Library 不存在」后静默卡住，应回落到默认库。测试写在 Task 6。

---

## 阶段 0：性能基线（先于一切修复）

### Task 0: SLO 表、真实规模数据集、全路由压测闸门

**为什么先做：** 报告里的性能结论都来自静态分析和小库上的 EXPLAIN。SQLite 规划器依赖统计信息：`open.rs:370-403` 只在打开连接时执行一次 `PRAGMA optimize`，长时间运行、数据涨上来之后，统计信息可能已经过期。所以必须在真实规模、真实运行状态下实测。修复前测一次作为基线，之后每个阶段测一次。阶段 C 的任务顺序按实测缺口重新排。

**Files:**
- Create: `docs/testing/performance-slo.md`：SLO 表、数据集规格、测量条件和判定规则（内容见下）
- Create: `scripts/e2e/list-routes.py`：从源码提取全部路由，不再依赖已过期的 `/tmp/e2e/routes.txt`
- Create: `scripts/e2e/seed-activity.py`：通过 API 生成用户、播放进度、收藏、收藏夹和播放记录
- Create: `scripts/e2e/perf-cases.json`：每个 GET 路由对应一个用例（含 class、URL 模板、认证方式），或者一条豁免（含原因）
- Create: `scripts/e2e/perfgate.py`：执行用例，统计 p50/p95/p99，按 SLO 判定并与基线对比，不达标时退出码为 1
- Modify: `scripts/e2e/stress-up.sh`：改用 release 构建（`target/release/crawler-media`）；用 API 自动建 stress-movie 和 stress-tv 两个库，等待入库完成；最后调用 `seed-activity.py`
- Modify: `docs/testing/e2e-runbook.md`：新增「性能闸门」一节，指向上面的脚本和 SLO 文档

**`performance-slo.md` 必须包含的内容：**

测量环境：Mac mini，release 构建，基于 `stress-up.sh` 起的独立实例（:18766）。

数据集：
- `gen-stress.py` 默认规模：3000 部电影，300 部剧 × 70 集，共 24000 个 strm，带 NFO
- 50 个用户
- 每个用户 300 条播放进度：100 条看完，200 条看了一部分；剧集类的进度分布在 60 部剧上，用来产生 NextUp
- 每个用户 100 条收藏、5 个收藏夹（每个 50 项）、400 条播放记录（共约 2 万条）
- 1 个绑定 stress-tv 的订阅

| Class | 典型接口 | idle p95 | idle p99 |
|---|---|---|---|
| W 交互写 | 心跳、进度上报、标记已看、收藏切换、搜索历史 | ≤ 50ms | ≤ 150ms |
| D 详情 | Self 条目详情、分集列表（70 集）、Jellyfin `/Items/{id}`、`PlaybackInfo` | ≤ 100ms | ≤ 250ms |
| L 列表 | Self items 列表（50 条，覆盖各种 sort/filter）、Jellyfin `Items`（50 条）、`Latest`、`NextUp`、`Resume`、up-next、favorites、collections、`/missing` | ≤ 150ms | ≤ 400ms |
| S 搜索 | `/search/library-items`（标题、前缀、演员）、Jellyfin `SearchTerm` | ≤ 200ms | ≤ 500ms |
| A 聚合/管理 | facets、`/playback/stats/watch`、`/jobs`、activity、sites | ≤ 300ms | ≤ 800ms |
| U 鉴权 | `/auth/me`、Jellyfin `/Users/{id}/Views`、`/System/Info` | ≤ 100ms | ≤ 250ms |
| U* 登录 | `/auth/login`、`/Users/AuthenticateByName`（含密码哈希） | ≤ 500ms | ≤ 1s |
| P 首屏 | 首页 / 媒体库页 / 详情页的全部 API 请求并发完成（墙钟） | ≤ 800ms | ≤ 1.5s |

判定规则：
- **busy 模式**（对 stress-tv 触发全量重扫，在扫描任务 running 期间测）：每个 class 的 p95 不超过 idle 目标的 2 倍；5xx（含 503）数量必须为 0；超时数量必须为 0。
- **冷启动**：重启进程后的第一次请求，不超过对应 class idle p99 目标的 2 倍。
- **统计信息过期**：在不重启的情况下先导入数据再测一次，再重启（触发 `PRAGMA optimize`）后测一次。两次 p95 相差超过 2 倍的用例，就是统计信息过期造成的性能问题，记录下来并修复。修复方式例如扫描轮结束后执行 `PRAGMA optimize`，属于写操作，必须走 `store_write`。
- 豁免：视频流（`stream`、`/Videos/*/stream*`）、SSE（`/jobs/stream`）、WebSocket、`/images/proxy`（依赖外网），在 cases 里注明原因。静态图片端点（`/posters/*` 等）不豁免，归入 D 类。

**Interfaces:**
- `list-routes.py`：扫描 `crates/api/src/http/**/*.rs` 和 `crates/media-server/src/routes*.rs` 中的 `.route("…", get|post|…)`，输出 JSON 列表 `[{"method": "get", "path": "/libraries/{id}/items", "surface": "self"|"jellyfin"}]`。
- `perfgate.py <idle|busy|cold> [--n 40] [--only <case 前缀>] [--dry-run] [--baseline .e2e/perf/<name>.json] [--out .e2e/perf/<name>.json]`：
  - 读 `perf-cases.json`，用 `list-routes.py` 的输出做覆盖检查：有 GET 路由既没有用例也没有豁免，就直接失败，并列出这些路由；
  - Self 接口用 `Authorization: Bearer <token>`；Jellyfin 接口先调 `/Users/AuthenticateByName` 拿 token，再用 `Authorization: MediaBrowser Token="…"`；
  - 每个用例从数据集里随机取真实 ID（库、条目、剧、用户），用多个用户轮流请求，避免单用户缓存让结果偏好；
  - P 类用例按 `web/` 页面实际发出的请求集合并发请求，请求集合用 `pages.mjs` 采集一次后写进 cases；
  - 输出每个用例的 p50/p95/p99/max 和状态码分布，附上 SLO 判定和相对基线的变化百分比。
- `seed-activity.py <base_url> <admin_token>`：全部通过公开 API 完成，不直接写 SQLite；可以重复运行（用户名固定，已存在则跳过）；并发数不超过 8。

- [ ] **Step 1: 先写闸门的自测**（`scripts/e2e/test_perfgate.py`，用 `python3 -m unittest` 运行，不访问网络）：
  - `test_uncovered_get_route_fails`：给一份 routes 输入，其中有一个 GET 路由既不在 cases 也不在豁免里。断言覆盖检查返回这个路由。
  - `test_p95_over_slo_fails`：给一组延迟样本（p95 = 180ms，class L）。断言判定为不达标。
  - `test_regression_over_20_percent_flagged`：基线 p95 = 100ms，本次 120.1ms。断言标记为退化。
  - `test_any_5xx_in_busy_fails`：busy 模式的样本里有一个 503。断言判定为失败。
- [ ] **Step 2: 运行，确认失败**：`python3 -m unittest scripts/e2e/test_perfgate.py`，预期 ImportError 或全部失败。
- [ ] **Step 3: 实现** `list-routes.py`、`perfgate.py` 的纯函数部分（覆盖检查、分位数、判定、对比），让 Step 1 的测试通过。
- [ ] **Step 4: 写 `perf-cases.json`**，直到 `python3 scripts/e2e/perfgate.py idle --dry-run` 的覆盖检查通过（`--dry-run` 只做覆盖检查，不发请求）。
- [ ] **Step 5: 写 `seed-activity.py` 和 `performance-slo.md`**；修改 `stress-up.sh`，构建一律用 `cargo build --release -j4 -p api`，构建期间不跑其他 cargo 进程。
- [ ] **Step 6: 在修复前的 HEAD（ec958b93）上测基线**：
  `zsh scripts/e2e/stress-up.sh /tmp/e2e-stress`
  `python3 scripts/e2e/perfgate.py idle --out .e2e/perf/baseline-idle.json`
  `python3 scripts/e2e/perfgate.py busy --out .e2e/perf/baseline-busy.json`
  重启实例后：`python3 scripts/e2e/perfgate.py cold --out .e2e/perf/baseline-cold.json`
  这一步预期会有大量不达标，这正是基线要记录的。
- [ ] **Step 7: 根据基线修订计划**：
  - 把实测不达标的用例逐条映射到报告条目（P-*、F*、B*）；
  - 找不到对应条目的不达标用例，在本计划阶段 C 末尾新增 Task 12.x，写法和其他任务一样：Files、失败测试、验证命令；
  - 阶段 C 的任务按「不达标幅度 × 用户触达频率」重新排序；
  - 统计信息过期造成的问题，并入 Task 5（改在扫描轮结束后执行 optimize）。
- [ ] **Step 8: 验证**：`python3 -m unittest scripts/e2e/test_perfgate.py` 通过；`perfgate.py idle --dry-run` 覆盖检查通过。
- [ ] **Step 9: 提交** `test(e2e): add performance SLOs and route-complete perf gate`。只提交脚本和 docs/testing 下的文件，不提交 `.e2e/perf/`。

**阶段闸门（之后每个阶段结束时执行）：**
```
zsh scripts/e2e/stress-up.sh /tmp/e2e-stress
python3 scripts/e2e/perfgate.py idle --baseline .e2e/perf/<上一阶段>-idle.json --out .e2e/perf/<本阶段>-idle.json
python3 scripts/e2e/perfgate.py busy --baseline .e2e/perf/<上一阶段>-busy.json --out .e2e/perf/<本阶段>-busy.json
kill $(cat /tmp/e2e-stress/pid); rm -rf /tmp/e2e-stress
```
- 阶段 A 结束：busy 模式下不能有任何 503；任何接口相对基线都不能退化超过 20%。
- 阶段 B 结束：busy 模式下各 class 的 p95 不超过 idle 目标的 2 倍。
- 阶段 C 结束：idle、busy、cold 三种模式全部达标。达标之后才能进入阶段 D。

---

## 阶段 A：合并阻塞项（必须先做，完成后可合并）

### Task 1: B3 观看统计 SAVEPOINT 回滚到正确的连接，读连接归还时自检

**Files:**
- Modify: `crates/store/src/playback/watch_aggregate.rs:134-146`
- Modify: `crates/api/src/store_readers.rs:104-115`（`impl Drop for StoreReader`）
- Modify: `crates/store/src/lib.rs`（新增 `Store::reset_if_in_transaction(&self) -> bool`；放进 `crates/store/src/open.rs` 或一个新文件，不要放在 lib.rs 本体）
- Test: `crates/store/tests/watch_aggregate_rollback.rs`（新建）
- Test: `crates/api/src/store_readers.rs` 内的 `mod tests`

**Interfaces:**
- Produces: `pub fn reset_if_in_transaction(&self) -> bool`。检查 4 个连接，任何一个 `!is_autocommit()` 就执行 `ROLLBACK`，返回 true 表示做过回滚。
- Produces（测试用）：`pub fn library_in_transaction(&self) -> bool`，加 `#[doc(hidden)]`。

- [ ] **Step 1: 写失败测试**：`watch_report_failure_leaves_library_connection_in_autocommit`。打开临时 Store，用测试专用的方式让聚合失败（例如 `DROP TABLE playback_logs` 后再调 `playback_watch_report(None, None, now, 30, 0)`），断言返回 `Err` 且 `!store.library_in_transaction()`；再调一次，断言错误类型不是 "cannot start a transaction"，SAVEPOINT 没有嵌套。
- [ ] **Step 2: 运行，确认失败**。命令：`cargo test -j4 -p store --test watch_aggregate_rollback -- --test-threads=4`。预期 FAIL，断言 library 仍在事务中。
- [ ] **Step 3: 实现**。`:143-145` 的 `self.subscribe` 改为 `self.library`。实现 `reset_if_in_transaction`，回滚时用 `tracing::error!(db = ..., "reader returned inside a transaction; rolled back")` 记录。
- [ ] **Step 4: 写失败测试**：`reader_returned_inside_transaction_is_rolled_back`。从 `StoreReaders::open(&writer, 1)` checkout 一个 reader，在 library 连接上执行 `BEGIN`，然后 drop；再次 checkout，断言 `!library_in_transaction()`。
- [ ] **Step 5: 实现**。`StoreReader::drop` 在 `idle.push(store)` 之前调 `store.reset_if_in_transaction()`。
- [ ] **Step 6: 验证**。依次运行：
  `cargo test -j4 -p store --test watch_aggregate_rollback -- --test-threads=4`
  `cargo test -j4 -p api --lib store_readers -- --test-threads=4`
  预期全部 PASS。
- [ ] **Step 7: 提交** `fix(store): roll back watch_aggregate savepoint on the library connection`。

### Task 2: B2 删除 `store_mutate`，所有写统一走 `store_write`（锁内准入检查）

**Files:**
- Modify: `crates/api/src/http/store_wait.rs`（删 `store_mutate`；更新模块文档）
- Modify：14 个调用点
  - `http/playback.rs:299,379,460`
  - `http/playback_activity.rs:181,296`
  - `http/playback_logs.rs:322`
  - `http/search/history.rs:132,191,353,430,458`
  - `http/watch_state.rs:362`
  - `http/session.rs:152`
  - `http/playback/heartbeat.rs:135`
- Test: `crates/api/tests/write_admission.rs`（新建；如果已有同类测试文件，就并入）

**Interfaces:**
- Consumes：现有的 `store_write<T, F>(state: &ApiState, work: F) -> Result<T, Response>`，签名不变。
- 调用点改写模式：`store_mutate(&state, f).await.map_err(StoreCallError::response)` 改为 `store_write(&state, f).await`。

- [ ] **Step 1: 写失败测试**：`heartbeat_is_refused_in_read_only_mode`。启动测试 app，建一个播放会话，把服务切到只读模式（使用现有的 maintenance/read-only 切换接口），再 POST 心跳。断言状态码是 503，错误码与 `require_writable` 的一致；并断言 DB 里该会话的 `last_report_at_ms` 没有变化。
- [ ] **Step 2: 写失败测试**：`queued_write_does_not_land_after_maintenance_starts`。在测试线程里持有 writer 锁，发起一个搜索历史写（`POST` 对应 `search/history.rs:132` 的路由），然后开启维护模式，再释放锁。断言响应不是 2xx，并且历史表里没有新行。
- [ ] **Step 3: 运行，确认失败**。命令：`cargo test -j4 -p api --test write_admission -- --test-threads=4`。预期 2 个 FAIL，返回的是 200。
- [ ] **Step 4: 实现**。把 14 处 `store_mutate` 都换成 `store_write`，然后删除 `store_mutate`。模块文档里删掉「late heartbeat better than 503」一段，改写成「写一律 1s 预算 + 锁内准入」。
- [ ] **Step 5: 验证**。命令：`cargo test -j4 -p api --test write_admission -- --test-threads=4`，再跑 `cargo test -j4 -p api --test playback -- --test-threads=4`（播放相关的现有测试）。预期全部 PASS。
- [ ] **Step 6: 提交** `fix(api): route every request write through store_write admission`。

### Task 3: B1 Library 读路径（prepare / validate / hydrate）改走读池

**Files:**
- Modify: `crates/api/src/library_read_work.rs`：删掉 `:69` 的 `try_lock_until` 路径；writer 参数换成 `Arc<StoreReaders>`
- Modify: `crates/api/src/media_server_provider/selection/response.rs:148,150,188,189,202`
- Modify: `crates/api/src/media_server_provider/selection/mod.rs:285-326`（`heavy_db_facts` 改为整页一次批量读）
- Modify: `crates/api/src/media_server_provider/query_lookups.rs:14,75`、`crates/api/src/probe_manager/query.rs:26`
- Modify: `crates/store/src/library_query/page_inputs.rs:41-95`（validate 改为批量）
- Create: `crates/store/src/library_query/page_validate.rs`（批量校验 SQL，保持私有）
- Test: `crates/api/tests/library_reads_off_writer.rs`（新建）
- Test: `crates/store/tests/page_validate.rs`（新建）

**Interfaces:**
- Produces（store）：
  - `pub struct PageSnapshot { pub config_revision: i64, pub files: Vec<(FileId, i64 /*file_revision*/, MediaId, LibraryId)> }`
  - `pub fn validate_page_snapshot(&self, snapshot: &PageSnapshot) -> Result<PageValidation, StoreError>`
  - `pub enum PageValidation { Fresh, Stale { gone: Vec<FileId> } }`。实现方式：在一个读事务内，用 `json_each(?1)` JOIN `library_files` 一次查完，再比对 `library_query_config.revision`。
  - `pub fn page_hydration(&self, files: &[FileId]) -> Result<HashMap<FileId, HydratedFile>, StoreError>`。file_meta、chapters、markers、library_for_file 各用一条 `IN`/`json_each` 查询。`HydratedFile` 字段取现有 `heavy_db_facts` 的返回内容，不新增字段。
- Produces（api）：`library_read_work` 的 prepare/validate/hydrate 签名中，`store: &Arc<Mutex<Store>>` 一律改为 `readers: &Arc<StoreReaders>`。Task 4、5 复用这个模式。
- 规则：validate 返回 `Stale` 时最多重做一次 prepare；第二次仍是 Stale，就过滤掉 `gone` 再返回，不报错。

- [ ] **Step 1: 写失败测试**（store）：`validate_reports_files_removed_after_prepare`。插入 3 个文件，拿 snapshot，删掉 1 个，断言结果是 `Stale { gone: [那个 id] }`；不做任何修改时断言 `Fresh`。
- [ ] **Step 2: 写失败测试**（store）：`page_validate_and_hydration_have_no_full_scan`。对 `validate_page_snapshot` 和 `page_hydration` 的 SQL 做 query-plan 断言，计划里不允许出现 `SCAN library_files` 或 `SCAN library_items`。沿用 `crates/store/tests/media_lookup_indexes.rs` 的做法。
- [ ] **Step 3: 写失败测试**（api）：`jellyfin_items_page_is_served_while_writer_is_held`。测试 app 有 1 个库、60 个 item。测试线程拿住 `state.store().lock()` 并保持 3s，同时 GET `/Users/{id}/Items?ParentId=…&Limit=50` 和 `/Users/{id}/Items/Latest`。断言 1s 内都返回 200，且 Items 页含 50 条。
- [ ] **Step 4: 写失败测试**（api）：`items_page_drops_file_deleted_between_prepare_and_validate`。用测试钩子（`#[cfg(test)]` 的 prepare 后回调）在 prepare 之后删掉一个文件。断言响应是 200，且不含该 item。
- [ ] **Step 5: 运行，确认失败**。依次运行：
  `cargo test -j4 -p store --test page_validate -- --test-threads=4`
  `cargo test -j4 -p api --test library_reads_off_writer -- --test-threads=4`
  预期 FAIL：store 侧找不到函数，api 侧超时或 503。
- [ ] **Step 6: 实现 store 侧**：`PageSnapshot`、`validate_page_snapshot`、`page_hydration`，各自在一个 `BEGIN DEFERRED` 读事务内完成。
- [ ] **Step 7: 实现 api 侧**：替换参数类型；删掉 `try_lock_until` 及其 deadline 逻辑；`heavy_db_facts` 改为整页调用一次 `page_hydration`；`query_lookups.rs` 和 `probe_manager/query.rs` 改走 `store_read`。
- [ ] **Step 8: 验证**。运行 Step 5 的两条命令，再跑 `cargo test -j4 -p api --test media_server -- --test-threads=4`（Jellyfin 相关的现有测试；目标名以 `ls crates/api/tests` 为准）。预期全部 PASS。
- [ ] **Step 9: 提交** `fix(api): serve Library reads from the reader pool with optimistic validation`。

> 阶段 A 收尾：`cargo fmt --all -- --check`、`cargo clippy -j4 --all-targets -- -D warnings`，再跑一次全量 `cargo test -j4 --workspace --no-fail-fast -- --test-threads=4`。然后执行 Task 0 的阶段闸门（产物命名为 `phaseA-*`）。测试全绿、且 busy 模式下没有 503，才算满足合并条件。

---

## 阶段 B：锁的建议修复

### Task 4: F4 / F5 剩余的纯读移出 writer

**Files:**
- Modify：
  - `http/subscriptions.rs:331`
  - `http/subscription_depth/activity.rs:56`
  - `http/library_artwork.rs:61`
  - `library_cover_service.rs:219`
  - `marker_resolver.rs:278`
  - `http/password_login.rs:79`
  - `http/jobs.rs:49-55`、`http/jobs_read.rs:278,290`
  - `tmdb_http.rs:25`
  - `media_server_provider.rs:291-330`
  - `media_server_provider/items.rs:104,129,312`
  - `worker/transfer/plan.rs:84`、`worker/transfer/delivery.rs:207`
- Modify: `crates/api/src/app_reader.rs`：删除，鉴权改走 `store_read`（F5）
- Test: `crates/api/tests/reads_during_scan.rs`（新建）

**Interfaces:**
- Consumes：`store_read(&ApiState, F)`；sync 后台代码用 `state.store_readers.checkout(STORE_READ_WAIT)`。
- 规则：`/UserViews` 先在读连接上拷出 `Vec<(LibraryId, PathBuf)>`，`is_file()` 放在锁和连接都释放之后做。
- `GET /jobs` 的 `get_subscribe` N+1 改为一次 `SELECT id FROM subscribes WHERE user_id=?1` 取集合，在内存里过滤。

- [ ] **Step 1: 写失败测试**：`interactive_reads_succeed_while_writer_is_held`。持有 writer 锁 3s，期间并发请求：`POST /api/v1/session/login`（密码登录）、`GET /Users/{id}/Views`、`GET /Users/{id}/Items/{item}`、`GET /api/v1/jobs`、`GET /api/v1/subscriptions`。断言每个请求都 < 1s 返回 200。
- [ ] **Step 2: 运行，确认失败**：`cargo test -j4 -p api --test reads_during_scan -- --test-threads=4`。
- [ ] **Step 3: 实现**：按 Files 列表逐点改为读池；删除 `AppReader` 及其构造。
- [ ] **Step 4: 验证**：Step 2 的命令，以及 `cargo test -j4 -p api --test auth -- --test-threads=4`（现有鉴权测试；目标名以实际文件为准）。PASS。
- [ ] **Step 5: 提交** `fix(api): move remaining pure reads off the writer lock`。

### Task 5: F3 probe_manager 不在 tokio worker 上无界拿 writer

**Files:**
- Modify：`crates/api/src/probe_manager/` 下：
  - `probe.rs:156,275,318,406,444`
  - `markers.rs:64,87,210,224,360,508`
  - `season.rs:175,184,251,343,452`
  - `stages.rs:47`
  - `marker_jobs.rs:39,72,130,155,239`
  - `refresh.rs:55`
- Modify: `crates/store/src/ledger.rs:276`：新增分批读 `list_file_coverage_page(library: LibraryId, after: Option<FileId>, limit: usize) -> Result<Vec<FileCoverage>, StoreError>`
- Test: `crates/api/tests/probe_manager_lock.rs`（新建）

**Interfaces:**
- 纯读：`store_read` / reader checkout。
- 写：`store_blocking` 改为 `tokio::task::spawn_blocking` + `try_lock_for(STORE_WRITE_WAIT)`，等不到就下一轮再试，不无界等待。
- `list_file_coverage` 只保留给离线维护；探测路径改用 `list_file_coverage_page`，按 `(library_id, file_id)` keyset 分页，每页 ≤ 200。

- [ ] **Step 1: 写失败测试**：`probe_round_does_not_stall_runtime_while_writer_is_held`。用 `current_thread` runtime，拿住 writer 3s，期间驱动一轮 probe manager `step()`，同一 runtime 上再 spawn 一个 `tokio::time::sleep(10ms)` 任务。断言该任务 < 200ms 完成。
- [ ] **Step 2: 写失败测试**（store）：`file_coverage_page_uses_index`，做 query-plan 断言，计划里没有 `SCAN library_files`。
- [ ] **Step 3: 运行，确认失败**：
  `cargo test -j4 -p api --test probe_manager_lock -- --test-threads=4`
  `cargo test -j4 -p store file_coverage_page -- --test-threads=4`
- [ ] **Step 4: 实现**。注意：`REMOTE_READ_LIMIT` 和 `REMOTE_READ_COOLDOWN` 不动。
- [ ] **Step 5: 验证**：Step 3 的命令，加 `cargo test -j4 -p api --test probe -- --test-threads=4`。PASS。
- [ ] **Step 6: 提交** `fix(api): keep probe manager store access off tokio workers`。

### Task 6: S-DB1 / S-DB2 / S-DB3 删除路径的索引，以及订阅悬空

**Files:**
- Modify: `crates/store/src/schema/v1/library.sql`：新增 3 个索引
  - `playback_sessions_media ON playback_sessions(media_id)`
  - `playback_session_ids_media ON playback_session_ids(media_id)`
  - `probe_unit_file ON probe_job_units(file_id, job_id)`
- Modify: 布局清单（`schema/definitions.rs`；如果 Task 13 还没删 `objects.rs`，也同步它）
- Modify: `crates/store/src/library_write/orphan_media.rs:19-38`：OWNED 去掉 `user_library_items`、`library_item_terms`、`library_protocol_terms`、`library_protocol_people`
- Modify: `crates/store/src/library_deletion/cleanup.rs`（finalize 之后）和 `crates/api/src/library_deletion/*`：调用新函数
- Create: `crates/store/src/subscribe_detach.rs`：`pub fn detach_subscribes_from_library(&self, library: LibraryId) -> Result<usize, StoreError>`，在 subscribe 连接上单独开 IMMEDIATE 事务
- Test: `crates/store/tests/library_removal_plans.rs`（新建）、`crates/api/tests/library_removal_subscribes.rs`（新建）

**Interfaces:**
- Produces: `detach_subscribes_from_library`，返回置 NULL 的行数，并记 `info!(library_id, detached)`。
- 顺序：library 事务提交之后再调用，不在 library 事务内跨库写。

- [ ] **Step 1: 写失败测试**：`orphan_purge_and_probe_unit_retirement_use_indexes`。对孤儿清理里每条 DELETE，以及 `probe_unit_retirement.rs:18-25` 的 SELECT，做 EXPLAIN 断言，计划里没有 `SCAN`。
- [ ] **Step 2: 写失败测试**：`orphan_purge_still_removes_projection_rows`。确认去掉显式删除后，依赖级联依然能把 4 张投影表清干净（ADR-0012 行为不变）。
- [ ] **Step 3: 写失败测试**：`subscribe_bound_to_deleted_library_falls_back_to_default`。订阅绑定库 L，删除 L 后断言 `subscribes.library_id IS NULL`，并且 transfer plan 能解析到同 kind 的默认库，不再报错。
- [ ] **Step 4: 运行，确认失败**：
  `cargo test -j4 -p store --test library_removal_plans -- --test-threads=4`
  `cargo test -j4 -p api --test library_removal_subscribes -- --test-threads=4`
- [ ] **Step 5: 实现**。如果 `worker/transfer/plan.rs:85-90` 遇到 `library_id = NULL` 时不会走默认库，在这里补上。
- [ ] **Step 6: 验证**：Step 4 的命令，加 `cargo test -j4 -p store --test schema_layout -- --test-threads=4`（布局校验；目标名以实际文件为准）。PASS。
- [ ] **Step 7: 提交** `perf(store): index Library removal paths and detach orphaned subscribes`。

### Task 7: F8 / F6 / F7 每个库一个 writer，统一读池，jobs 读写分离

**Files:**
- Modify: `crates/store/src/lib.rs`、`crates/store/src/open.rs`：`Store` 拆为 `Writers { app, library, subscribe, cache }`，每个是 `Mutex<Connection>`
- Modify: `crates/api/src/store_readers.rs`：每个库一个 async FIFO 池，容量 library 8、app 4、subscribe 2、cache 2、jobs 2，连接设 `query_only=1`；删除 `LibraryReadPool`、heartbeat 专用连接；sync checkout 统一走 FIFO（F11）
- Modify: `crates/api/src/job_loop.rs:362-382,434`：`active_job_attempts` 与 jobs 锁不再嵌套
- Modify: `crates/api/src/http/subscriptions.rs:331`、`worker/transfer/*`：subscribe_guard 只包住决策，下载器 IO 移到 guard 外
- Test: `crates/api/tests/writer_isolation.rs`（新建）

**Interfaces:**
- Produces（store）：`pub fn with_writer<T>(&self, db: Db, work: impl FnOnce(&Connection) -> Result<T, StoreError>) -> Result<T, StoreError>`，其中 `pub enum Db { App, Library, Subscribe, Cache }`。现有的 Store 方法内部改为只锁自己需要的那个库。
- 顺序常量（写进 store 的模块文档并加 debug 断言）：app → subscribe → library → cache → jobs。
- 这是最大的一个任务。如果单个 PR 过大，可以拆成 7a（store 内拆 writer）和 7b（读池 + jobs）。

- [ ] **Step 1: 写失败测试**：`cache_write_does_not_block_library_heartbeat`。拿住 cache writer 2s，期间心跳写（library）断言 < 200ms 成功。
- [ ] **Step 2: 写失败测试**：`job_claim_and_job_list_do_not_serialize`。持有 jobs 写事务期间 `GET /api/v1/jobs`，断言 < 200ms。
- [ ] **Step 3: 写失败测试**：`transfer_releases_subscribe_guard_during_downloader_io`。用 fake downloader 阻塞 2s，期间对同一订阅 `GET /api/v1/subscriptions/{id}`，断言 < 200ms。
- [ ] **Step 4: 运行，确认失败**：`cargo test -j4 -p api --test writer_isolation -- --test-threads=4`。
- [ ] **Step 5: 实现**。
- [ ] **Step 6: 验证**：先跑 store 和 api 两个 crate 的全部测试，`cargo test -j4 -p store -- --test-threads=4`，再 `cargo test -j4 -p api -- --test-threads=4`。PASS。
- [ ] **Step 7: 提交** `refactor(store): one writer per database and unified reader pools`。

---

> 阶段 B 收尾：执行 Task 0 的阶段闸门（`phaseB-*`）。busy 模式下各 class 的 p95 不超过 idle 目标的 2 倍。

## 阶段 C：性能

> 每个任务的通用模式：先写 query-plan 测试（断言计划里没有 `SCAN <大表>` 和 `USE TEMP B-TREE`），再写行为测试（分页、顺序、可见性），然后实现。
>
> 任务顺序以 Task 0 Step 7 重排后的结果为准。每个任务提交前，单独跑一次与它相关用例的 `perfgate.py idle`（用 `--only <case 前缀>` 过滤），确认目标用例已达标。阶段 C 结束时执行完整的阶段闸门（`phaseC-*`），idle、busy、cold 三种模式必须全部达标。

### Task 8: P-J1 + P-S1 NextUp / Resume / up-next 改查投影表

**Files:**
- Modify: `library.sql`：新增部分索引
  - `user_items_next ON user_library_items(user_id, last_played_at DESC, media_id) WHERE next_season IS NOT NULL`
  - `user_items_resume ON user_library_items(user_id, last_played_at DESC, media_id) WHERE resume_season IS NOT NULL`
- Create: `crates/store/src/library_query/continue_watching.rs`：
  - `pub fn next_up_page(&self, user: UserId, libraries: &[LibraryId], limit: usize, offset: usize) -> Result<Vec<ContinueEntry>, StoreError>`
  - `pub fn resume_page(...)`，签名同上
  - `pub struct ContinueEntry { pub library_id, pub media_id, pub season: i64, pub episode: i64, pub last_played_at: i64 }`
- Modify: `media_server_provider/selection/mod.rs:156-165`、`selection/resume.rs:55`、`http/playback_views/up_next.rs:76,149,178,385`：改调上面两个函数，再对页内结果批量水合（用 Task 3 的 `page_hydration`）
- Test: `crates/store/tests/continue_watching.rs`、`crates/api/tests/next_up_resume.rs`

- [ ] **Step 1: 写失败测试**：`next_up_and_resume_plans_use_partial_indexes`，EXPLAIN 断言走 `user_items_next` / `user_items_resume`，没有 TEMP B-TREE。
- [ ] **Step 2: 写失败测试**：`next_up_orders_by_last_played_and_respects_visible_libraries`。3 个库中用户只可见 2 个，断言结果只来自可见库，并按 `last_played_at DESC, media_id` 排序。
- [ ] **Step 3: 写失败测试**：`up_next_statement_count_is_constant`。用 `Store::with_stats` 统计语句数，分别在 10 条和 200 条历史下请求 `GET /api/v1/playback/up-next?limit=20`，断言语句数相同（证明没有 N+1）。
- [ ] **Step 4: 运行，确认失败；实现；验证**：
  `cargo test -j4 -p store --test continue_watching -- --test-threads=4`
  `cargo test -j4 -p api --test next_up_resume -- --test-threads=4`
- [ ] **Step 5: 提交** `perf: serve NextUp, Resume and up-next from user_library_items`。

### Task 9: P-S2 / P-S3 收藏与 LastPlayed 排序

**Files:**
- Modify: `library.sql`：`user_items_favorite ON user_library_items(user_id, favorite_at DESC, media_id) WHERE favorite = 1`。如果没有 `favorite_at` 列，就在 v1 schema 里新增，写入时机与 favorite 标记一致。
- Modify: `crates/store/src/library_query/playback_views.rs:45-92`：从收藏集合出发，按 PK 回表 `library_items`，keyset 游标为 `(favorite_at, media_id)`，total 只在首页计算
- Modify: `crates/store/src/library_query/items/page.rs:61,72`：LastPlayed 分两段，先用 `user_items_activity` 取已播放的，再走 items 排序索引取未播放的并排除前一段；游标里编码当前所在段
- Test: `crates/store/tests/favorites_page.rs`、`crates/store/tests/last_played_page.rs`

- [ ] **Step 1: 写失败测试**：`favorites_page_plan_has_no_scan`；`favorites_keyset_pages_are_disjoint_and_complete`（25 条收藏、页大小 10，三页的并集等于全集，没有重复）。
- [ ] **Step 2: 写失败测试**：`last_played_sort_has_no_temp_btree`；`last_played_pages_cross_from_played_to_unplayed`（跨段翻页时既不漏也不重复）。
- [ ] **Step 3: 运行，确认失败；实现；验证**：`cargo test -j4 -p store --test favorites_page --test last_played_page -- --test-threads=4`。
- [ ] **Step 4: 提交** `perf(store): keyset favorites and segmented LastPlayed paging`。

### Task 10: P-S4 /missing、P-S6 收藏夹、P-S7 /jobs 的 N+1

**Files:**
- Modify: `library.sql`：
  - `files_missing ON library_files(library_id, missing_at) WHERE missing_at IS NOT NULL`
  - `collection_items_order ON collection_items(collection_id, sort_order, added_at)`
- Modify: `crates/store/src/library.rs:263`：`missing_at_by_path` 改为 `missing_files_page(library: LibraryId, after: Option<(i64, FileId)>, limit: usize) -> Result<Vec<MissingFile>, StoreError>`，一条 SQL 完成，JOIN media
- Modify: `crates/api/src/http/library_scan.rs:404-421`
- Modify: `crates/store/src/collections/reads.rs:67-80,124,175`：用一条 JOIN 查询代替嵌套循环；items 改为 keyset 分页
- Test: `crates/store/tests/missing_files.rs`、`crates/store/tests/collections_reads.rs`

- [ ] **Step 1: 写失败测试**：`missing_files_is_library_scoped_and_indexed`。两个库各有缺失文件，断言只返回目标库的，并且计划走 `files_missing`。
- [ ] **Step 2: 写失败测试**：`missing_entry_matches_by_path_key`。用大小写或尾随斜杠不同的路径复现报告里怀疑的 path_key/path 不匹配。如果测试本来就通过，说明不存在这个问题，在提交说明里记录。
- [ ] **Step 3: 写失败测试**：`collection_list_statement_count_is_constant`。用 `with_stats` 统计 5 个和 50 个 item 时的语句数，断言二者相同。
- [ ] **Step 4: 运行，确认失败；实现；验证**：`cargo test -j4 -p store --test missing_files --test collections_reads -- --test-threads=4`。
- [ ] **Step 5: 提交** `perf(store): index missing files and batch collection reads`。

### Task 11: P-S5 Library 搜索改用 FTS5 trigram

**Files:**
- Modify: `library.sql`：`CREATE VIRTUAL TABLE library_search_fts USING fts5(search_text, people, content='', tokenize='trigram')`，rowid 对应 `library_items` 的 rowid
- Modify: `crates/store/src/query_projection/*`：投影刷新时同步维护 fts 行（与 `library_items` 在同一个写事务里）
- Modify: `crates/store/src/library_search.rs:45-106`：`MATCH` 查询，游标改为行值比较 `(title_sort, media_id) > (?, ?)`
- Test: `crates/store/tests/library_search.rs`

- [ ] **Step 1: 写失败测试**：`search_matches_title_and_people_substrings`（中文 3 字子串、演员名都能命中）；`search_plan_uses_fts`；`search_respects_library_visibility`。
- [ ] **Step 2: 写失败测试**：`search_shorter_than_three_chars_falls_back_to_prefix`。trigram 不支持少于 3 个字符的查询；这种查询走 `title_sort` 前缀范围扫描，并断言计划里没有 SCAN。
- [ ] **Step 3: 运行，确认失败；实现；验证**：`cargo test -j4 -p store --test library_search -- --test-threads=4`。
- [ ] **Step 4: 提交** `perf(store): FTS5 trigram index for Library search`。

### Task 12: P-J2 / P-J4 Jellyfin 计数与剧集排序

**Files:**
- Modify: `crates/store/src/library_query/protocol/count.rs`、`protocol/predicates.rs:234-277`：
  - Work 粒度只数存在的那一支，不再把 CTE 展开两次；
  - played/favorite 谓词改为查 `user_library_items` 的 EXISTS；
  - Latest 用 `LIMIT n+1` 代替精确 total。
- Modify: `crates/store/src/library_query/protocol/page.rs:186-189`：`scope.pinned_media` 有值时用 `INDEXED BY units_media_scope`
- Test: `crates/store/tests/protocol_plans.rs`（如果已有 query-plan 测试文件，就并入）

- [ ] **Step 1: 写失败测试**：`episodes_of_pinned_series_do_not_skip_scan`；`played_filter_count_expands_cte_once`（EXPLAIN 中 CTE 只出现一次）；`latest_skips_exact_count`（`with_stats` 统计 COUNT 语句数为 0）。
- [ ] **Step 2: 写行为回归测试**：`played_and_favorite_counts_match_page_totals`。计数与逐页翻完的条数一致，覆盖 IsPlayed=true/false 和 IsFavorite 三种组合。
- [ ] **Step 3: 运行，确认失败；实现；验证**：`cargo test -j4 -p store --test protocol_plans -- --test-threads=4`。
- [ ] **Step 4: 提交** `perf(store): single-pass protocol counts and pinned episode index`。

---

## 阶段 D：约束、零兼容、文档、尺寸

### Task 13: S-DB4 / S-DB5 状态列 CHECK 与 app.db 外键

**Files:**
- Modify: `crates/store/src/schema/v1/{library,subscribe,app}.sql`、`crates/jobs/src/schema.sql`：给报告 S-DB5 列出的每一列加 `CHECK (col IN (…))`，取值逐个对齐 Rust enum 的 `as_str()`；app.sql 增加 `user_tokens.user_id` 和 `playback_device_credentials.token` 的级联 FK，以及 `user_tokens_user` 索引
- Delete: `crates/store/src/schema/v1/objects.rs`（Z5），同时删 `schema.rs:13-14` 的 `pub mod v1`
- Modify: `crates/store/src/users.rs:375-387`（`finish_user_removal`）：app 事务提交之后，在 library 写里把该用户从各 Library 的成员集合移除
- Test: `crates/store/tests/status_checks.rs`、`crates/store/tests/user_removal.rs`

**Interfaces:**
- 每个状态 enum 提供 `pub const ALL: &[Self]`，测试遍历它，断言每个值都能插入，`"bogus"` 插入失败。

- [ ] **Step 1: 写失败测试**：`every_status_variant_is_accepted_and_unknown_is_rejected`（逐列）。
- [ ] **Step 2: 写失败测试**：`removing_user_drops_tokens_credentials_and_memberships`；`rotating_token_drops_its_device_credential`。
- [ ] **Step 3: 运行，确认失败；实现；验证**：`cargo test -j4 -p store --test status_checks --test user_removal -- --test-threads=4`，然后 `cargo test -j4 -p jobs -- --test-threads=4`。
- [ ] **Step 4: 提交** `feat(store): CHECK status columns and cascade app.db ownership`。

### Task 14: 零兼容清理（报告第 5 节）

**Files:**
- Modify: `crates/api/src/http/subscriptions/types.rs:30-32`：删掉 `tracking_state`（Z1）
- Modify: `web/lib/api/subscriptions.ts:252-289,997-1110`、`web/lib/subscription-ui.ts:10-20`、`web/components/rule-sets-panel.tsx`：删除旧 RuleSetSpec 适配层，面板直接编辑 atoms（Z2）。后端 `crates/api/src/http/rule_sets.rs:43-75` 如果还没有表达「H&R 未知也排」的 atom 值，就加一个 atom 参数 `unknown: "exclude" | "allow"`，并在过滤原子中生效（`crates/filter`）。
- Modify: `web/lib/image-proxy.ts:42-50`、`web/components/library-view.tsx:588`（Z3）
- Delete：Z4 列出的 6 个死函数；`library_removals.removed_ledger_count`（Z6，前后端一起改）；`media_metadata_sources.preferences_json`（Z7）
- Modify：报告 5.2 中的「可选优化」项，作为同一提交里的独立 commit，按需处理
- Test: `crates/api/tests/subscriptions_create.rs`、`crates/filter/tests/hr_unknown.rs`、`web/test/rule-sets.test.ts`

- [ ] **Step 1: 写失败测试**（api）：`create_subscribe_rejects_tracking_state_field`，断言 400。
- [ ] **Step 2: 写失败测试**（filter）：`hr_atom_with_unknown_exclude_drops_unknown_hr_release`。
- [ ] **Step 3: 写失败测试**（web）：`strict_hr_toggle_survives_save_and_reload`。atoms 序列化后再解析，断言严格开关的状态不变；平台和制作组往返后保持原样，不再塌缩成 `title_match`。
- [ ] **Step 4: 运行，确认失败**：
  `cargo test -j4 -p api --test subscriptions_create -- --test-threads=4`
  `cargo test -j4 -p filter -- --test-threads=4`
  `cd web && pnpm test`
- [ ] **Step 5: 实现**。完成后执行 `cd web && pnpm lint && pnpm typecheck && pnpm build`。
- [ ] **Step 6: 验证**：Step 4 的命令全部 PASS。
- [ ] **Step 7: 提交**：分 3 个 commit。
  - `fix(web): edit rule-set atoms directly and keep strict H&R`
  - `refactor(api): drop dead compat fields and functions`
  - `refactor(store): drop removed_ledger_count and preferences_json`

### Task 15: 文档更新（报告第 6 节）

**Files:**
- 修改：报告 D1–D11 涉及的文档
- 删除：D12 的 3 个桩文件（先改 4 处源码注释的指向），D13 视 D4 的决定而定
- 删除：前端约 77 个文件中的 `docs/design/*.md` 引用（D8）

- [ ] **Step 1**：`grep -rn 'docs/design/' web | wc -l` 记录基线数量；改完后再跑一次，结果应为 0。
- [ ] **Step 2**：更新 AGENTS.md 的依赖方向和关键位置（D1/D2），内容要反映阶段 A–B 之后的真实入口（`store_write`、每个库一个 writer、统一读池）。
- [ ] **Step 3**：更新 `docs/database/*.md` 的列数、索引和 CHECK（D6/D7），用 `sqlite3 <新建的空库> '.schema'` 核对。
- [ ] **Step 4**：执行 `cd web && pnpm lint`（只改注释，确认没破坏任何东西）。
- [ ] **Step 5: 提交** `docs: align AGENTS, database docs and ADRs with storage redesign`。

### Task 16: 函数与 lib.rs 尺寸（S-MOD2 / S-MOD3 / S-MOD1）

**Files:**
- Modify：S-MOD2 列出的 4 个已核实超过 120 行的函数，以及启发式名单中经核实确实超限的函数
- Modify：`store`、`domain`、`library`、`release`、`downloader` 的 `lib.rs`：把实现搬到具名模块，lib.rs 只留 `mod` 和 `pub use`
- Modify: `crates/store/Cargo.toml:18`：删掉 `release`；把 `QualityFact`、`SubscribeFacts` 移到 `domain`
- Test：不新增行为测试，现有测试即为回归保障

- [ ] **Step 1**：用 AGENTS.md 的尺寸检查命令，以及一个能统计函数行数的脚本（放在 `$TMPDIR`，用完删除），列出超限清单。
- [ ] **Step 2**：按职责拆分，每个 crate 一个 commit，提交前跑该 crate 的测试 `cargo test -j4 -p <crate> -- --test-threads=4`。
- [ ] **Step 3: 收尾**：`cargo fmt --all -- --check`、`cargo clippy -j4 --all-targets -- -D warnings`、`cargo test -j4 --workspace --no-fail-fast -- --test-threads=4`，再加 `cd web && pnpm lint && pnpm typecheck && pnpm test && pnpm build`。
- [ ] **Step 4: 提交** `refactor: keep lib.rs as module trees and split oversized functions`。

---

## Self-Review

1. **报告覆盖**：
   - 阻塞项：B1 → Task 3，B2 → Task 2，B3 → Task 1。
   - 锁：F3 → Task 5；F4、F5 → Task 4；F6、F7、F8、F11 → Task 7。
   - DB 与模块：S-DB1/2/3 → Task 6，S-DB4/5 → Task 13，S-MOD1/2/3 → Task 16。
   - 性能：P-S1、P-J1 → Task 8；P-S2/3 → Task 9；P-S4/6/7 → Task 10（P-S7 的 N+1 已在 Task 4 一并处理）；P-S5 → Task 11；P-J2/4 → Task 12；P-J5 → Task 4。
   - 零兼容：Z1–Z7 → Task 14。文档：D1–D13 → Task 15。
   - 未单独建任务的项：F9、F10、F12、F13、P-J3（GeneralExact 的 k 路归并）、O-DB1/2/3（时间列统一、jobs FK、`library_members` 表）、报告 3.2 / 4.2 的可选优化。这些都是可选优化，等阶段 C 完成后用基线数据重新评估，不进入本计划。
2. **步骤检查**：每个测试步骤都给了测试名和断言要点；每个实现步骤都给了文件和签名。Task 7 规模较大，已注明可拆成 7a/7b。测试目标名以 `ls crates/*/tests` 为准的地方都已标注，没有凭空写名字。
3. **类型一致**：`PageSnapshot`、`PageValidation`、`page_hydration` 在 Task 3 定义，Task 8 复用；`ContinueEntry` 只在 Task 8 使用；`Db` 和 `with_writer` 只在 Task 7 定义，之后的任务不依赖它们，因此 Task 7 可以推迟。
4. **Review Focus**：5 条都已落到对应任务的测试里（Task 1 / 2 / 3 / 4 / 6）。
5. **篇幅**：只在算法会影响结果的地方给出 SQL 和顺序，其余只写签名和断言。
6. **性能可验收**：Task 0 定义了 SLO 表，数据集规格写进了文档，闸门脚本做全路由覆盖检查（漏一个 GET 路由就失败）。阶段 A、B、C 结束时各有一次阶段闸门。修复前先测一次基线，用基线校正阶段 C 的任务顺序：有不达标用例而报告里没有对应条目的，新增任务补上。目前覆盖不到的只剩视频流、SSE、WebSocket、外网图片代理四类，已在豁免里写明原因。

## Execution Handoff

Task 0 必须最先做，并且基线要在修复前的 HEAD 上测，否则就没有可对比的数据。Task 0 只改脚本和文档，不改 crate 代码。之后的阶段 A（Task 1–3）是合并前必须完成的，三者互相独立，可以按 1 → 2 → 3 的顺序实施。阶段 B 起，Task 7 会改动 Store 的公共形态，后续任务都应在它之后开始，或者在它之前完成。
