# feat/storage-redesign 合并审核（对比 `git diff main...HEAD`）

- 审核日期：2026-11-02；HEAD `a28249a3`；范围 123 commits / 2122 files / +257k −84k
- 方式：只读审核，未改动任何源码/文档、未运行 cargo。SQL 结论用 `sqlite3` 对 `crates/store/src/schema/v1/*.sql`、`crates/jobs/src/schema.sql` 建临时库跑 `EXPLAIN QUERY PLAN`（EQP），并用合成数据（10 万 items / 25k logs / 20 万 jobs 历史）计时；耗时绝对值仅供参考，复杂度阶数可靠。
- 按约定不计为问题：probe 并发 = 1、远程读冷却 500ms、ADR-0012 删库清理孤儿 Media 及其收藏/进度、library 读池容量 8。
- 本文件位于 `docs/test-reports/`（已 gitignore，不应提交）。

---

## 结论

**暂不可合并。** 阻塞项 4 个（B1–B4），全部修完即可合并；"建议修复"可在合并后紧随处理。

| # | 阻塞项 | 一句话 |
|---|---|---|
| B1 | 播放历史可见性过滤 O(L×V) | 相对 main 的性能回退，可见 Media 数千即秒级、数万即数十秒，占读池 |
| B2 | 观看统计同一问题 ×≈13 条查询 | 单请求可达数十秒 |
| B3 | `with_plan_write` 持 App writer 贯穿 Library 发布事务、并在 writer 下读 | 直接违反"读不拿写锁 / 写锁粒度最小 / 读写互不阻塞" |
| B4 | ADR-0008 及其引用的锁复审文档描述的是已删除的架构 | 本分支核心 ADR 失真，AGENTS 规定"本地文档优先"，会误导后续实现 |

总体评价：存储重构主干是对的——每库单 writer + `try_lock_for` 有界准入、`ReaderPool` query_only + deferred 快照读不碰 writer、fs/网络 IO 已分阶段移出临界区（observe → 短写 + CAS）、删除与 retention 分批、DB 布局只校验不迁移、无旧路由别名与 serde 兼容字段。问题集中在少数回退查询、一处跨库 writer 嵌套、jobs 锁、死代码与文档失真。

---

## 一、阻塞合并

### B1 播放历史可见性过滤 O(L×V)（性能回退）
- 位置：`crates/store/src/playback/log_scope/connection.rs:38`（page）、`:49-51`（hidden_count）
- 原因：`WITH request_visible_media AS (SELECT value AS id FROM json_each(?))` 未物化，在 `EXISTS / NOT EXISTS` 中对每条日志重扫一次 JSON 数组。main 上该处是 `CREATE TEMP TABLE request_visible_media (id TEXT PRIMARY KEY)`（`main:crates/store/src/playback/log_scope.rs:123`），本分支改写后退化。
- EQP（已复现）：
  ```
  SEARCH l USING INDEX logs_user_started (user_id=?)
  CORRELATED SCALAR SUBQUERY 2
    SCAN json_each VIRTUAL TABLE INDEX 1:
  ```
- 实测：25k logs、V=2000 可见 Media：**1535ms**；同 SQL 加 `MATERIALIZED`：**7ms**。子 agent 数据：V=20000 → 19.8s，外推 V=10 万 ≈ 100s，期间独占一个 library reader lease。
- 触发：普通用户（有可见库限制）打开 `GET /api/v1/playback/history`，库中 Media 数千以上。
- 修复（推荐顺序）：
  1. 不再传 id 列表，改为 `EXISTS (SELECT 1 FROM library_items i WHERE i.media_id = l.media_id AND i.library_id IN (<可见库>))`，走 `items_media(media_id, library_id)` 覆盖索引（实测 45.9ms），同时可删除 `api/http/playback_logs.rs:89` → `visible_scope.rs:12` → `store/ledger/connection.rs:172 media_ids_in_libraries` 的每请求 O(N) 全量 id 加载与 JSON 编码。
  2. 最小改动：CTE 改为 `AS MATERIALIZED`。
  3. 补一条回归测试：可见 Media 1 万条时请求在预算内完成（或断言 EQP 不含 `CORRELATED ... json_each`）。

### B2 观看统计同一问题，乘以约 13 条查询
- 位置：`crates/store/src/playback/watch_aggregate.rs:18-21`（VISIBLE/HIDDEN 子句）、`:84-98`（`scoped_cte`）；查询在 `watch_aggregate/connection.rs:70,132,163,208,243,278,310`；handler `api/http/playback_stats.rs:23`
- 原因：与 B1 同一未物化 `json_each` CTE；每个窗口约 7 条 × 2 窗口 + hidden_titles。另 `(:user IS NULL OR l.user_id = :user)` 使 `logs_user_started` 用不上，只能走 `logs_started` 范围扫描。
- 实测：V=20000、30 天窗口，单条 totals 6483ms（物化后 16ms），整请求估计 > 80s。
- 触发：`GET /api/v1/playback/stats/watch`。
- 修复：同 B1；并在 Rust 端按"有/无 user"拼两种 SQL，去掉 `IS NULL OR`。

### B3 跨库 writer 嵌套：App writer 被持有贯穿 Library 发布事务
- 位置：`crates/api/src/worker/transfer/plan.rs:252-278`（`with_plan_write`：`writers().with_app(|app| { 读 transfer_settings/scrape 设置; store.library_write_with_error(...) })`）
- 调用方：`crates/api/src/management/subscribes/run.rs:584`（一个事务内循环本轮全部文件 + `queue_transfer_removals`）、`worker/transfer/run.rs:78`、`worker/transfer/recorder.rs:139`
- 原因：
  - 锁序 app→library 合法，但 App writer 持有时长被放大为整个 library 发布时长；
  - 在 writer 锁下做读（配置 fence），违反 AGENTS 硬性规则 3"读永不拿 writer 锁"与"写锁粒度最小"，也违反本次审核要求的"读写锁完全独立"。
- 触发：订阅轮次 / transfer 发布大量文件期间，登录 token 签发、设置保存、用户管理、站点编辑等 app.db 写入在 `STORE_WRITE_WAIT=1s` 后返回 503。
- 修复：
  1. 用 reader 读配置并带上 revision（或内容哈希），library 事务内只做 library 侧校验；
  2. 提交后用 reader 再短读一次 app revision，若变化则把本次发布标记为需重排/补偿（发布本身是幂等的），或将 revision 镜像进 library.db 在事务内 CAS；
  3. 立规则并加测试：任何 writer 闭包内不得再获取另一个库的 writer（见第五节锁设计）。`library_deletion/cleanup.rs:16 with_reference_write`（subscribe writer 下读 `subscribed_media` 再拿 library writer）同类，一并改为 revision fence（可列为建议修复）。

### B4 ADR-0008 与锁复审文档描述的是已删除的架构
- 位置：
  - `docs/adr/0008-sqlite-system-of-record.md:3`：把 `docs/audits/2026-10-28-storage-lock-review.md` 当作现状引用；
  - `:43-49`：`StoreReaders`、`crates/api/src/store_readers.rs`、`Store::open_reader`、`store_mutate`（5s）、`LIBRARY_READERS = 2`——均已不存在；
  - 实际：`ReaderPool` app/subscribe/library/cache = 4/2/8/2（`crates/store/src/open.rs:95-98`）、`STORE_WRITE_WAIT=1s`（`crates/api/src/http/store_wait.rs:28`）、`STORE_READ_WAIT=5s`（`:21`）、无 Store Mutex。
  - `docs/audits/2026-10-28-storage-lock-review.md` 全文描述 `StoreReaders` / `store_mutate` / `Arc<Mutex<Store>>` 与"目标结构（提案）"（已实现），被 8 处当现状引用：`docs/README.md:8`、`adr/0008:3`、`adr/0012:5`、`database/library.md:3`、`deploy.md:132`、`api-contracts/self.md:143,151`、`superpowers/specs/2026-10-18*:3`。
- 原因：ADR-0008 是本分支的核心决策文档，AGENTS 规定"本地文档优先于训练数据"，错误描述会直接误导后续 agent/人类实现（例如重新引入 Store Mutex 或按 5s 写等待设计）。
- 修复：重写 ADR-0008 的"读写分离"节为现状（每库 writer + rank 顺序、ReaderPool 容量、`store_write` 1s 准入/复查、`with_read_snapshot` 顺序、jobs_readers 容量 2）；锁复审文档标注为历史快照或删除，并更新上述 8 处引用。

---

## 二、建议修复

### 2.1 数据库设计

| # | 位置 | 问题 / 触发 | 修复 |
|---|---|---|---|
| D1 | `crates/store/src/schema/v1/subscribe.sql`（`subscribe_facts`/`subscribe_wanted`/`pending_downloads`）；`crates/store/src/subscribes.rs:205-219` | 同库父子表没有外键，`delete_subscribe` 手写 `for table in [...] DELETE`；`subscribes/connection.rs:27-72 upsert_pending_row` 不校验父行，`list_all_pending_routed`（`:252-256`）不 join 父表。删除订阅与下载回调/reconcile 交错时可产生孤儿 pending 行被持续轮询；新增子表易漏删。 | 三表 `subscribe_id REFERENCES subscribes(id) ON DELETE CASCADE`（PK 已以 subscribe_id 开头，无需新索引），`delete_subscribe` 只删父行。 |
| D2 | `crates/jobs/src/store.rs:207,225,335,348,361`（`WHERE payload = ?`）；`crates/jobs/src/schema.sql` | job 身份 = payload 字符串字节相等且不带 kind；`job_defs.payload` 无索引（EQP `SCAN job_defs`）、不唯一。现有 5 个不同 kind 的 def payload 都是 `'{}'`，按 `'{}'` 调 `set_def_enabled/set_def_schedule/delete_defs_for_payload` 会误伤所有全局任务；payload 由 `subscribe_schedule.rs:11`/`deletion.rs:219`（`format!`）与 `jobs_api.rs:59`（`json!`）各自拼接，键序/空白一变即静默匹配 0 行。 | `job_defs` 加结构化身份列 `(kind, scope_key)` + `UNIQUE(kind, scope_key)`，所有定位 API 改按 `(kind, scope_key)`；payload 只作参数。 |
| D3 | `crates/store/src/library_write/orphan_media.rs:40 purge_orphan_media_in_tx` 仅在文件删除路径被调用 | 最后一个文件删除时 Media 因订阅/收藏被保留；之后删除订阅（`api/http/subscriptions/deletion.rs:415`）或移除收藏条目（`collections/connection.rs:87-90,151`）不会再清理，无文件 Media 及其 aliases/metadata/playback 行永久残留，全库无周期清理。 | 删除订阅、移除收藏后对受影响 media_id 调同一 `purge_orphan_media_in_tx`（跨 subscribe→library 两步，按锁序分开写）。 |
| D4 | `crates/store/src/collections/connection.rs:50`、`:125` | `query_row("SELECT COALESCE(MAX(sort_order),0) ...").unwrap_or(0)` 吞掉 busy/IO 错误且无日志，排序错乱；违反 AGENTS"IO 路径禁止吞错/日志不能静默"。 | 改 `?` 传播（COALESCE 已保证非 NULL）。 |
| D5 | `subscribes.created_at/updated_at`（TEXT RFC3339，写于 `subscribes/admission.rs:114-115`、`subscribes/connection.rs:347`）；`collections.created_at`、`collection_items.added_at`（`library.sql:269-272`） | 其余表均为 INTEGER 毫秒/秒；`collection_items_order(collection_id, sort_order, added_at, media_id)` 用 TEXT 秒级做 keyset 次序键。 | 统一 INTEGER 毫秒，直接改 DDL（零兼容）。 |
| D6 | `app.sql` `playback_device_credentials` PK `(user_id, device_id, token)`，FK `token → user_tokens ON DELETE CASCADE` | 无 token 前导索引，EQP 实测每删一个 user_token 都 `SCAN playback_device_credentials`；且 `user_id` 与 `user_tokens.user_id` 冗余无一致性约束。 | 加 `INDEX(token)` 或 PK 改为 `(token, device_id)` 并去掉冗余 user_id。 |
| D7 | `crates/jobs/src/media_refs.rs:9,26,54` | 注释写"index acceleration"，实际 `payload LIKE '%'||?||'%'` 对 `job_defs` 全表扫（jobs 侧靠部分索引 `jobs_live_def_idx` 只扫活跃行，可接受）。 | 修正注释；随 D2 改为结构化 media_id 列定位。 |

### 2.2 锁

| # | 位置 | 问题 / 触发 | 修复 |
|---|---|---|---|
| L1 | 写入 `crates/store/src/playback/event.rs:57-73`（`with_library_write`）；调用 `api/http/playback/heartbeat.rs:87,301`、`media_server_provider/playback.rs:102`、`session.rs:156`、`watch_state.rs:368` | 高频、延迟敏感的播放心跳与扫描发布 / transfer 发布 / 删除批 / probe 结果 / VACUUM 共用 library 单 writer，准入只等 1s；`store_wait.rs:17-20` 注释自称"扫描一批 ≈1.3s，其中约 950ms 发布"已 > 1s。触发：大库扫描或订阅发布时播放，心跳 503、进度丢失。**若实测发布单事务 > 1s 应升级为阻塞。** | 心跳改内存 latest-wins 合并缓冲 + 后台 flusher 短事务批量落库（推荐），或拆独立 playback.db；同时把后台批量写单事务控制在 50–100ms 并实测更新该注释。 |
| L2 | `crates/api/src/jobs_writer.rs:18-24` `lock()` 无界；调用点 `http/jobs.rs:105,211,270,346`（`run_to_completion` 内，占 http_work permit）、`http/subscriptions/patch_jobs.rs:9`、`http/subscriptions.rs:525`（持 `subscribe_guard` 时）、`subscription_missing.rs:69`、`job_maintenance.rs:121`、`job_loop.rs:379,459,463`、`fs_watcher.rs:496,508`、`worker.rs:98`、`subscribe_create.rs:65` | jobs.db 维护（VACUUM/backup 页拷贝不响应 interrupt，`maintenance/database.rs:26,36-44`）持锁期间，所有 jobs HTTP 无限阻塞并占满 permit，进而让其它请求 503；订阅 PATCH 在 guard 内被卡住。 | 删除 `lock()`，统一 `try_lock_for(STORE_WRITE_WAIT)`，WriteBusy→503；后台路径超时退避重试。 |
| L3 | `crates/store/src/writer_rank.rs:26`（`debug_assert!`）、`:40`（`debug_assert_eq!`） | 锁序/同库重入检查只在 debug 生效；release 下 store writer 反转表现为 1s 后 WriteBusy（根因被掩盖），jobs writer 因 `lock()` 无界会永久死锁。 | release 也检查：返回 `StoreError::LockOrder` 并 `error!`（附持有栈与目标库），成本仅一次 thread-local 比较。 |

### 2.3 性能

| # | 端点 / 位置 | SQL / EQP | 复杂度 | 修复 |
|---|---|---|---|---|
| P1 | `GET /api/v1/libraries`、`GET/PUT /libraries/{id}`：`api/http/library.rs:172`、`library_statistics.rs:33` → `store/library_stats/connection.rs:5-28` | 每库 `COUNT(*),SUM(size_bytes) FROM library_items WHERE library_id=?`（EQP `SEARCH ... items_resolution (library_id=?)`，非覆盖）+ `library_files` 计数（`files_presence`） | 每库 O(items+files)，按库 N 次；8 万 items 库约 165ms/库/请求 | 写路径维护计数（并入 `library_query_counts` 或单独统计行）；最低限度建覆盖索引 `library_items(library_id, size_bytes)`、`library_files(library_id, presence_state, publication_state, path_key)` |
| P2 | `GET /libraries/{id}/items` 未看优先：`store/library_query/items/page.rs:84 WATCH_TIER_SQL` | `USE TEMP B-TREE FOR ORDER BY` | 每页 O(n log n)，8 万 items 122ms | 在 `user_library_items` 维护 `watch_tier` 并纳入排序索引，或按 tier 三段各走索引拼接 |
| P3 | `GET /libraries/{id}/item-index`：`items/page/connection.rs:43` | 全库扫描分桶 | O(n)，120–137ms | 维护首字母分桶计数，或 `(library_id, index_key)` 覆盖索引 + GROUP BY |
| P4 | `GET /subscriptions`：`api/http/subscriptions/snapshot.rs:96-105`；`views.rs:226 cached_show_body` → `store/catalog/connection.rs:43 list_catalog_cache` | 每订阅 `get_media`/facts/pending/times/progress 4–6 次查询；缓存未命中的开放季订阅对 6 张 cache 表 UNION ALL（含 body）全扫 + 排序，再 Rust 线性 `contains` | N+1；最坏 O(S×C×body) | facts/pending/times 改 `WHERE subscribe_id IN (...)` 批量；`cached_show_body` 改 `cache_key` 范围精确查找只取一行 |
| P5 | `GET /jobs`：`crates/jobs/src/read.rs:87 collect_last` | `ROW_NUMBER() OVER (PARTITION BY def_id ...)` 扫每个 def 全部终态历史 | O(历史)，20 万行 187ms | 每 def 相关子查询 `ORDER BY finished_at DESC, run_after DESC LIMIT 1` 命中 `jobs_last_def_idx`，实测 0.44ms |
| P6 | Jellyfin `Items` 不带 ParentId（多库）：`store/library_query/protocol/page.rs` | `ROW_NUMBER()` 窗口 + 两次 `USE TEMP B-TREE FOR ORDER BY` | 每页 O(n log n)，233ms | 按库各走排序索引后 k 路归并去重 |
| P7 | Jellyfin `StartIndex` 深翻页（单库） | `INDEXED BY items_protocol_* ... LIMIT ? OFFSET ?` | O(offset)，OFFSET 6 万 70ms | 协议只给 offset：缓存 offset→keyset 锚点 |
| P8 | `GET /libraries/{id}/facets`：`members/facets.rs:152-203` | 公共过滤为空时仍 `MATERIALIZE b`（整库）再 INTERSECT | O(n log n)，71.8ms→去掉后 52.1ms | 公共过滤为空时跳过 `b` |
| P9 | items `include_total` 仅残差过滤时 | `COUNT(*)` 全扫 | O(n)，36ms | 仅首页计算或改 has_more |
| P10 | Jellyfin `SearchTerm`：`protocol/predicates.rs search_clause`、`protocol/sql.rs` work_search | 词长 <3 无 FTS 预过滤，全库 `instr(casefold(...))`，每行 NFO 相关子查询 | O(n)+每行子查询 | 短词复用 `items_search_prefix`；NFO 子查询移到分页之后 |

已确认良好：自有搜索 `GET /search/library-items`（FTS5 trigram，1.2ms）、items 标题/添加时间/评分 keyset 分页（约 0.1ms，O(log n)）、收藏按 favorited_at（0.22ms）、`GET /jobs/{id}`（O(log n)）、collect_live（0.1ms）。按 kind 过滤为残差谓词，单库单 kind 设计下无需索引。

### 2.4 零兼容 / 遗留代码（待删除）

后端：
- `crates/api/src/management/state.rs:97-98` `#[allow(dead_code)] library_root`（单根目录模型残留，构造 `:209-363`、`:288 create_dir_all`）——删字段、参数与目录创建。
- `crates/api/src/user_removal.rs:13-22` `#[serde(default = "default_version")] version`（本分支新增的缺字段兜底，从不校验）——删除或改为严格校验拒绝。
- 本分支新增且全仓零引用：`store/src/library_write/rename_intent.rs:133 begin_file_reconciliation`、`library_write/rename.rs:56 check_file_rename`、`library_query/continue_watching/home.rs:64 home_continue_query_plans`、`library_query/details/metadata.rs:23 instance_default_locale`、`marker/src/redirect.rs:104 invalidate_remote`、`domain/src/metadata_fields.rs:24 is_value`、`api/src/storage_operations.rs:177 library_operation`/`:207 write_verdict`、`store/src/metadata/export_driver.rs:160 resume_nfo_export`、`domain/src/library_storage.rs:235 season_or_one`、`api/src/user_removal.rs:54 start_removal`。
- 本分支删掉调用方后成孤儿：`api/src/playback_events.rs:37 classify_event`、`playback_events/clock.rs:18 clamp_elapsed`、`management/state.rs:548-605` browse_page 计数器整套（`#[allow(dead_code)]`）、`store/src/users.rs:43 update_user_and_password`。
- 其它 `#[allow(dead_code)]`：`api/src/http/notify.rs:64 send`、`http/playback_logs.rs:278 _err_shape`、`store/src/library_deletion/mod.rs:42 terminal_operation_predicate`、`management/state.rs:149 statistics_role_lookups`。
- `crates/api/src/http/downloaders/task_listing.rs:328`、`:236-238` 固定输出 `"sources": []`、`error_message/site_name/resolution: null` 旧前端占位字段——删除或真实填充，前端同步。
- `crates/api/src/worker.rs:98` `let _ = state.jobs.lock().delete_defs_for_payload(...)` 静默吞错——至少 `warn!`（并随 L2 改有界锁）。
- `crates/store/src/lib.rs:100-102` `pub use probe_dispatch::PENDING_DISPATCH_SQL` 把 SQL 文本放进公开 API（仅测试使用）；`Store::delete_media`（`store/src/media/mod.rs:286`）无生产调用者且不处理引用——降为 crate 内/测试钩子。

前端（`web/`）：
- `web/lib/api/subscriptions.ts:754-799` 按旧形状补默认值：后端 `subscription_depth/removal_preview.rs:68` 不返回 `hit_and_run_count`/`recycle_retention_days`/`retained_cross_season`。**用户可见 bug**：`:795` 用 `?? 0` 填充，使 `components/subscription-cancel-dialog.tsx:89` 的 `?? 7` 永不生效，弹窗提示"保留 0 天"；`season-cleanup-dialog.tsx:82,109` 依赖字段恒为空。——按真实契约收紧类型，后端补齐或删 UI。
- `web/lib/api/downloaders.ts:283-352` + `components/downloader-config-section.tsx:706-751` + `lib/downloader-limits-capabilities.ts`：`alt_speed_enabled/queue_enabled/max_active_*` 后端不返回不接受，UI 被永久关闭；注释引用 Python 时代 schema——删除。
- `web/lib/api/downloaders.ts:80-123` + `task-center-view.tsx:112,239,456`：`purpose/upgrade_keep_old/units[].replaced/content_missing/enters_library/task_count` 后端不返回，分支不可达——删除。
- `web/lib/api/playback.ts:565-610` + `components/player/video-player.tsx:3583`：`PlaybackDiagnostics/ArtifactUpload` 无后端端点，调用处写死 `null`——删除。

已确认无问题：前端约 150 处调用全部能在路由表找到，无调用已删除端点；DB 布局闸门只校验 + 拒绝（`store/src/schema/{layout,definitions}.rs`、`jobs/src/schema.rs`、`media/src/response_cache`），无迁移代码；`password.rs:35-47` 明文行 fail closed；无旧路由别名（media-server `routes/system.rs:16,18` 的 `alias` 属 Jellyfin 协议）。

### 2.5 文档

需删除：
- `docs/benchmarks/2026-10-28-storage-lock-work.md`：基于已删除的 Store Mutex（`state.store().lock()`），含本机绝对路径，已被 `crates/store/tests/storage_lock_work.rs` 取代；同步删 `docs/README.md:8`、`docs/database/library.md:3` 的链接。
- `docs/audits/2026-10-28-storage-lock-review.md`：见 B4（删除或明确标为历史快照）。

需更新：
- `docs/adr/0008-*.md`：见 B4。
- `docs/adr/0012-library-removal.md:5`：仍把锁复审当现状引用。
- `docs/database/library.md:40`、`:910`：说"最后一个文件删除时随孤儿 Media 清理 collection 成员"，与代码 `orphan_media.rs:52-55` 及 ADR-0012:17（收藏条目保留 Media）矛盾。`:52,535` 写 v4（实际 user_version=1）；`:17` 说 `LedgerRow` 已删除但 `store::LedgerRow` 仍导出并被 `http/library_admin.rs:74` 使用。
- `docs/deploy.md`：`:66,95,96,144` 及 `docs/adr/0007-*.md:51` 的环境变量 `CRAWLER_MEDIA_OBSCURA_INSTALL` 代码不读取（实际是设置项 `obscura.install`，`api/src/settings_keys.rs:35`、`runtime_browser.rs:32`）；`:132,136,140` 仍描述两条只读连接与 Store 锁；`:79` 说 compose 起 qB/TR，实际 `docker-compose.yml` 只有 crawler-media；环境变量表缺 `CRAWLER_MEDIA_SLOW_REQUEST_MS`、`FFMPEG_LOG_LEVEL`、`LEASE_*`、`ALLOW_MULTI_INSTANCE`。
- `docs/api-contracts/self.md:143,151`：item-index 说依赖共享 Store，实际 `store_read(&[Library, App])`（`library_scan.rs:408`）。
- `docs/database/subscribe.md:60`（通知接收者实为最小 admin id，`users.rs:432-440`）、`:61`（删除用户更新 jobs.db 定义未实现）、`:124`（漏 `rejected` 状态，`subscribes/connection.rs:141-148`）。
- `docs/database/app.md:66-73`：漏 finalizing 阶段与 `user_removal.result.<op>`、`user_removal.done.<uid>` 键（`user_removal.rs:148,154,159,273`）。
- `docs/README.md:8`：标签过时；索引缺 `testing/performance-slo.md`、`superpowers/plans/2026-10-29-storage-redesign-fixes.md`、`superpowers/plans/2026-11-02-storage-redesign-final-acceptance.md`。
- `docs/superpowers/plans/2026-10-29-storage-redesign-fixes.md`：117 个复选框未勾但已完成；引用 StoreReaders（:7,143,146,159,315）、`app_reader.rs`（:243）、`Arc<Mutex<Store>>`（:206）。
- `docs/superpowers/specs/2026-10-18*:3,22`：引用锁复审并把全局串行化当未解决问题。
- `docs/audits/2026-10-26-perf-baseline.md`：:35（1/144→实际 1/50）、~:77（1000 发布槽→`fixture.rs:78` 为 2400）、:82（v4→1）、§4.1（23→25 实际 28，`gate.rs:149`）、§5 引用不存在的 `objects.rs`。
- `docs/agents/domain.md:11`：指向已删除的 `2026-10-28-storage-main-rereview.md`。
- `crates/api/src/http/store_wait.rs:17-20` 注释（"扫描一批 ≈1.3s"）需按实测更新（见 L1）。

---

## 三、可选优化

数据库 / 索引：
- `library.sql:185-199` `library_items` 15 个二级索引 + 1 唯一索引，近似重复两组：`items_protocol_default` vs `items_protocol_default_explicit`、`items_search_prefix` vs `items_protocol_name_asc`——确认 `title`/`title_sort` 语义可否合并，各留一个，降低投影重建写放大。
- `library.sql:499` `fingerprint_samples.sample_lookup` 与 UNIQUE 前 4 列同集合，planner 选 autoindex——删除。
- `playback_metrics` 无 `user_id` 索引，用户删除按 user_id 删除为全表扫（低频，可接受或加索引）。
- cache.db 6 张表 `INTEGER PRIMARY KEY AUTOINCREMENT`（`crates/media/src/response_cache/mod.rs:13-71`）无需"不复用"语义，改 `INTEGER PRIMARY KEY`；`idx_*_fetched` 与 `idx_*_retention` 仅方向不同，可合并为一个；`CREATE TABLE IF NOT EXISTS` 多余。
- `library.sql:243-247` 注释称 `protocol_people_name` 已删，`:533` 又建同名索引——改注释或改名。
- 只写不读列：`library_removals.total_files`（`library.sql:398`）、jobs `retention_cursors.cycle_started_at`（`schema.sql:52`）；无用 DEFAULT：`user_tokens.created_at DEFAULT 0`、`pending_downloads.submitted_at DEFAULT 0`。
- `items/filters.rs:110` `LOWER(i.language) IN (...)` 无法用索引——写入时归一化小写。

模块边界：
- `crates/api/src/maintenance/database.rs:322,364,465` 的 `PRAGMA optimize`/`VACUUM`/`incremental_vacuum` 与 `pragma_i64(...)` 下沉到 store/jobs 维护接口。
- `open.rs:330-345 connection_pragmas()` 为诊断拿 writer 锁——改走 reader。
- `BrowsePage::Offset` 仍被支持（`items/page.rs:303`、`last_played.rs:158`、`details/episodes/connection.rs:285`）——零兼容下只保留 keyset。

锁：
- WAL checkpoint：writer 使用默认 autocheckpoint（在提交线程、writer 临界区内同步执行，叠加 `synchronous=FULL` 造成提交尾延迟尖刺）。建议 writer `wal_autocheckpoint=0`，每库后台独立连接 PASSIVE checkpointer，维护窗口 TRUNCATE；reader lease 设最长持有时间防 WAL 无法 reset。
- `crates/store/src/writers.rs:89` admission deadline 是整个 `store_write` 作用域的总预算，闭包内先算后取 writer、或依次取多个 writer 会被挤占——文档写明，或改为按次获取独立 wait。
- `library_read/pool.rs:260-280 check_identity` 每次 checkout/归还 stat 文件——缓存 identity 定时/事件失效。
- 其它小项：`library_facts`（`api/http/library/assets.rs:49`）每库调 `removal_for_library`/`root_reassignments_for_library`；facets marginal count 每值一次查询（`members/plan.rs:156`、`facets.rs:36`）可合并 GROUP BY；收藏未看优先排序 O(F log F)、`playback_favorite_position` 用 OFFSET。

前端 / 后端死代码（main 已有）：`user_agent.rs:23`、`fingerprint_job.rs:168`、`poster_fetch.rs:51,503,551`、`http_agent.rs:112`、`catalog.rs:627,679`、`probe_manager/policy.rs:37`、`obscura_manager.rs:191`、`probe_manager.rs:212`、`http/library_scan.rs:81`、`probe_manager/stages.rs:89`、`media/src/douban.rs:119`、`state.rs:408,709`、`indexer/src/browser.rs:192`、`scrape_config.rs:141`；兼容表述注释 `scrape_config.rs:1-5`（"zero-migration promise"）、`downloader/src/client.rs:25-27`、`file_stat_read.rs:31`；前端 `web/lib/api/auth.ts:15-29,50-77`（`nickname/avatar_url/capabilities` 旧会话形状）、`discover.ts:21-23` 过时注释、旧书签页 `web/app/(app)/discover/movie/{top250,high-score}/page.tsx`、旧名 `MediaLibrary`/`JobView` 与 `schemas.subscription.*` 引用；无前端/Jellyfin 调用的端点（如 `routes/admin.rs:133,137` integrity-checks/repairs、`:234,266,268`、`member.rs:25,74,78,143,147,238,245,255`、`admin.rs:34-35,59-67,89,116,120,262,263`）逐个确认是否运维 API，否则删除。

文档：`database/cache.md:41-42` 未提 `definitions::validate`；`audits/2026-10-26-library-schema-audit.md:85,352,371` 引用不存在的 `objects.rs`；`adr/0009` 未写 jobs_readers（容量 2）与 `concurrency_key`/`timeout_secs`；`testing/system-regression-cases.md:251` 行号漂移；`agents/domain.md:23` 格式。

---

## 四、锁架构现状与目标设计

现状（已核实）：

```
HTTP handler (tokio)
 ├─ store_read ── spawn_blocking + 5s ──► ReaderPool app4 | sub2 | lib8 | cache2
 │                                        (query_only, deferred BEGIN 快照, 不碰 writer)
 ├─ jobs 读 ──► jobs_readers ReaderPool(2, 只读) ──► jobs::QueueRead
 └─ store_write ── http_work permit ── admission(deadline=now+1s, 实例可写, WriteGate)
       └─ Writers(Arc, 每库一把)  rank: app → subscribe → library → cache → jobs  [仅 debug_assert ← L3]
            app.db / subscribe.db / library.db / cache.db : Mutex<Conn> + try_lock_for → BEGIN IMMEDIATE
            library.db 同时承载 扫描发布/transfer/删除批/probe/播放心跳/VACUUM  ← L1
            jobs.db : JobsWriter Mutex<Queue>  lock() 无界 ← L2
 应用级：subscribe_guard(每订阅) → jobs.lock()  ← L2
         with_plan_write: APP writer ─持有贯穿─► LIBRARY 写事务  ← B3
         with_reference_write: SUBSCRIBE writer ─► LIBRARY writer（writer 下读 fence）
 SQLite：WAL, synchronous FULL(cache NORMAL), busy_timeout 5000(维护 250), journal_size_limit 64MiB,
         默认 autocheckpoint 在提交路径
```

做得好的部分：读写分离已成立（读不触 writer Mutex、只读连接 query_only）；fs/网络在锁外分阶段执行（`pending_file_deletions.rs:58/85/124-154`、`metadata/export_driver.rs:221-258`、`metadata/nfo_input/reconcile.rs:35-67`、`roots.rs:44`、`scan_seen.rs:153,173`、`library_write/rename_intent.rs:42`；订阅轮次的 downloader add 与 ffprobe 在 `subscribe_guard` 外）；删除每批 100、retention 每批 500 且有游标；`jobs::Queue::tick` 执行 job 时释放锁；HTTP 访问 store 经 `spawn_blocking` + 预算，`try_acquire` 失败即 503 不排队；VACUUM 期间维护会关闭 WriteGate（`maintenance/runner.rs:260`）。

目标设计（在现有骨架上收紧，不推翻）：
1. **单写者不变量**：每库一把 writer，统一 `try_lock_for`，删除一切无界 `lock()`（L2）。
2. **禁止跨库 writer 嵌套**：writer 闭包内不得获取其它库 writer；跨库一致性一律"reader 读 revision → 本库写事务内 CAS / 提交后复核 + 幂等重排"（B3、`with_reference_write`）。rank 检查在 release 下也生效，违规即报错（L3）。
3. **writer 下不读**：fence 读走 reader；writer 闭包内只允许本次写所需的 SQL。
4. **写事务时长预算**：后台批量写（扫描/发布/删除/retention/probe）按 50–100ms 切 chunk，chunk 间释放 writer；记录每次持有时长，> 500ms `warn!`，便于 `logstats.py` 发现。
5. **前台热写隔离**：播放心跳内存合并 + 后台 flusher（或独立 playback.db），使前台交互写只剩低频小写，1s 准入预算充足（L1）。
6. **checkpoint 移出临界区**：writer `wal_autocheckpoint=0`，后台 PASSIVE checkpointer 用独立连接；维护窗口 TRUNCATE；reader lease 设最长持有。
7. **预算语义**：admission deadline 明确为写作用域总预算，闭包内只放 SQL。

---

## 五、建议的修复顺序

1. B1/B2（同一修复，改 EXISTS + `items_media`，删 `visible_media_ids` 链路）+ 回归测试。
2. B3 去掉 `with_plan_write` 的跨库嵌套，加"writer 闭包内不得取其它 writer"测试。
3. B4 重写 ADR-0008 读写分离节，处理锁复审文档及 8 处引用。
4. 合并后：L1（先实测发布单事务时长）、L2、L3、D1–D4、P1、P4、P5，前端 `recycle_retention_days` 用户可见 bug，其余遗留代码与文档清理。
