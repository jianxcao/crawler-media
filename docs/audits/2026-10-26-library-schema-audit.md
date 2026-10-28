# library.db 结构审计：删掉不再挣钱的东西（Task 16）

> 历史证据：本文数字仅适用其记录的版本/规模；当前状态与剩余方案见[2026-10-28 复审](2026-10-28-storage-main-rereview.md)。

日期：2026-10-26。分支：`feat/storage-redesign`。起点：`be7b6915`（layout v3）。
任务：计划 `2026-10-26-storage-redesign-review-fixes.md`（历史原文：`git show 5730b032:docs/superpowers/plans/2026-10-26-storage-redesign-review-fixes.md`） 的 Task 16。
配套测量：Task 17 的[性能基线](2026-10-26-perf-baseline.md)（同一夹具形状的确定性计数门禁）。

本分支以性能为第一裁决标准。**本审计的中心问题不是“这张表大不大”，而是“今天谁读它”**：
一张每次发布都要写、却没有任何生产读者的表，是这份 DB 里最贵的结构。

## 0. 方法与可复现命令

三条证据线，全部可重跑：

1. **谁写谁读**：把 `crates/store/src/**/*.rs` 里所有 SQL 字面量抽出来（1029 条，764 条能在
   schema 上 prepare），对每条跑 `EXPLAIN QUERY PLAN`，得到「索引/表 → 语句 → 文件」的反向表。
   再用 `git grep` 把每个 `CREATE TABLE`/`CREATE INDEX` 追到 store 函数，再从函数追到 `crates/api` 调用点。
2. **删掉会怎样**：对每个可疑索引，在**同一份**已播种库上 `DROP INDEX`，把 764 条语句重新规划一遍，
   逐条 diff。零 diff = 没有任何生产语句的计划会变（不是“我觉得没人用”）。这是本审计判“死索引”的判据。
   注意：生产从不跑 `ANALYZE`，所以 `sqlite_stat1` 不存在，规划器的估算与实际行数无关——
   空库上的计划与满库一致，这是可以放心用空库做计划断言的前提。
3. **空间与宽度**：`dbstat` 逐对象字节数 / 页数。

播种夹具（合成，`sqlite3 < library.sql` + 一段纯 SQL 播种，脚本见第 9 节）：
1 Library / 20,000 Media / 20,000 文件 / 20,000 卡片 / 180,000 `library_item_terms` 行 /
40,000 `library_protocol_terms` 行 / 200,000 `library_protocol_people` 行（每卡 10 人，TV 剧集的真实形状），
page_size 4096。**改前 117.9 MB，改后 105.9 MB。**

```bash
# 计划与索引反向表
python3 scripts/…（见第 9 节，一次性脚本，未入库）
# 空间
sqlite3 /tmp/pop.db "SELECT name,SUM(pgsize) b FROM dbstat GROUP BY name ORDER BY b DESC;"
# 回归
CARGO_TARGET_DIR=target/audit cargo test -p store
```

## 1. 结论摘要

| # | 结论 | 证据 |
|---|---|---|
| H1 | **`media_seasons` 是写-only 表**：每次元数据采用都写一行/季×locale，整个仓库没有一条生产 `SELECT … FROM media_seasons` | 第 2 节；`git grep media_seasons` 只有 INSERT/UPDATE/DELETE 与 merge 的 `COUNT(*)` |
| H2 | **协议人物过滤是 157× 的二次查询**：`membership.rs:21` 的相关子查询按「库内全部人物行」重扫，200k 人物行时 1 张卡 0.91 s、10 张卡 9.1 s | 第 4.2 节实测 |
| H3 | **`library_item_terms` 被存了 3 份**：WITHOUT ROWID 表 + `terms_media` + `terms_value` 三棵等宽 B 树，180k 行占 20.6 MB（DB 的 17%） | `dbstat`：6.89/6.81/6.89 MB |
| H4 | **两个索引没有任何语句能到达**：`protocol_terms_value`、`protocol_people_name`（本轮已删，−12.0 MB / −10.2%） | 第 3 节 drop-diff 零变化 |
| H5 | **`library_items` 一张表 14 棵 B 树**（表 + PK autoindex + 13 个索引），索引 15.6 MB = 表 5.3 MB 的 2.9 倍；13 个索引本轮逐条验证**都**有真实读者 | 第 3.1 节 |
| H6 | **`library_units.quality_json` 是常量 `'{}'`**：4 个写入点全写 `{}`，无任何读者 | `git grep quality_json` |
| H7 | **`library_items` 有 3 个写-only 列**：`aliases_json`（只有测试读）、`added_at_ns`（`added_at_sort` 的重复）、`has_nfo` | `git grep` |
| H8 | `library_query_counts` **不是**死表：它是唯一 O(1) 计数路径，被 6 处读；但 10 个 facet 维度里只有 `hdr` 真正走边际表 | 第 2/3 节 + Task 17 D6 |
| H9 | v3 的 `WITHOUT ROWID` 选表判据（窄行、复合主键、无 rowid 消费者）本轮**重复验证成立**，但 `probe_job_units`/`unidentified`/`playback_session_ids` 被 rowid 契约挡住 | 第 4 节 |

**v4 落地状态（2026-10-26 晚，Task 24）**：H4 的两个死索引、`protocol_people_tmdb` 改序、
H3（R1）、H1（R2）、H6（R4）、H7（R3）以及版本号 3→4 已经落在同一个 v4 轮次里；
R6（人物过滤 SQL）已作为 **Task 25** 单独落地（无 DDL，见第 6 节）；R9、R5、R7、R8、R11、R12 **没有**做，理由在各行。
v4 之后的结构：**53 表 / 592 字段 / 76 命名索引 / 37 组 FK**。

**一句话**：这份 schema 的问题不在“表太多”，而在**派生结构被写了很多次、读得很少**——
`media_seasons`（0 读者）、`library_item_terms` 的第三棵树、协议人物/术语上的 4 个索引、
`library_units.quality_json` 与 `library_items` 的 3 个列，都是纯写放大。

## 2. 逐表裁决（审计时 54 张；v4 之后 53 张）

判定口径：**KEEP** = 生产有读者且不可由更窄的结构替代；**KEEP(理由)** = 看着可疑但有确定读者；
**DEAD** = 生产无读者；**DERIVED** = 派生缓存，本轮判定仍有读者。

### 2.1 身份与采用元数据（7）

| 表 | 写者 | 生产读者 | 判定 |
|---|---|---|---|
| `media` | `media/mod.rs::write_media`、`insert_media`、`media_operations/apply.rs` | 22 个 store 函数；`media_lookup_indexes.rs` 钉住 title 查重路径 | KEEP：身份唯一事实源，9 列窄行 |
| `media_aliases` | `alias.rs::add_media_alias`、`apply.rs::move_aliases` | `media/mod.rs::get_media_by_alias_kind`、`reduce.rs::media_identity` | KEEP |
| `media_metadata_sources` | `metadata/adopt.rs::upsert_source_snapshot`、`references.rs::merge_source_snapshots` | `library_query/items/hydration.rs::fill_studio`（browse 热路径）、`metadata/compose.rs::load_snapshots` | KEEP：4 个 JSON 列宽，但 browse 只读 studio 字段，不是宽读 |
| `media_metadata` | `metadata/effective.rs::write_work`、`references.rs::store_adopted` | `library_query/protocol/sql.rs`、`details/metadata.rs`、`protocol_index.rs::search_text` | KEEP：work 级已采用事实 |
| `media_seasons` | `metadata/effective.rs:153::write_season`（每次采用/刮削，每季×locale 一行） | **无**：`SELECT … FROM media_seasons` 全仓库 0 条；只有 merge 的 `SELECT 1`/`COUNT(*)` 自检 | **DEAD**：最高价值发现（H1） |
| `media_episodes` | `effective.rs::write_episode` | `details/episodes.rs::episode_metadata`、`protocol/predicates.rs::search_clause` | KEEP |
| `media_metadata_overrides` | `metadata/overrides.rs::set_metadata_overrides` | `metadata/compose.rs::load_overrides`、merge 预检 | KEEP：人工覆盖必须持久 |

`media_seasons` 的成本：`effective.rs:153` 在每次 adopt 里对每个季×locale 写
`title/overview/air_date/poster_url/details_json/field_origins_json/revision`。一部 10 季的剧、2 个 locale
= 20 行宽 JSON 写入，每次刮削一次。**没有任何界面读过它**：剧集列表走 `library_units` 的
(`library_seasons`/`unit_season_exists`），分集信息走 `media_episodes`，播放走 `library_files`。
它只被 merge 的“loser 还有没有引用”检查数到行数。删掉它需要同时删
`metadata/effective.rs::write_season`、`objects.rs` 的一条、`apply.rs:529` 的引用表清单。

### 2.2 Library 与文件台账（5）

| 表 | 写者 | 生产读者 | 判定 |
|---|---|---|---|
| `libraries` | `libraries.rs`、`library_operations/*` | 61 个函数（每次查询都校验 lifecycle/config_revision/ACL） | KEEP：ACL 与操作栅栏的唯一来源 |
| `library_roots` | `roots.rs`、`libraries/root_config.rs` | 23 个函数 | KEEP |
| `library_files` | `library_write/publish.rs::upsert_file_row` 等 30 处 | 126 处（browse/详情/播放/发布/reduce 全路径） | KEEP：权威物理文件台账，34 列但热路径只取窄列 |
| `file_details` | `publish.rs::upsert_file_details`、`probe.rs` | `files/read.rs::file_details_batch`、`playback/source.rs` | KEEP，但见 R5：`fingerprint_json`/`outro_fingerprint_json` 与 `fingerprint_cache` 重复 |
| `library_file_units` | `publish.rs::sync_coverage` | 29 处（覆盖事实） | KEEP：v3 的 WITHOUT ROWID 转换正确 |

### 2.3 派生摘要（browse / 单元 / 倒排 / 计数）（8）

| 表 | 写者 | 生产读者 | 判定 |
|---|---|---|---|
| `library_units` | `reduce/card.rs::rebuild_units`、`reduce_incremental/units.rs` | `library_seasons`、`unit_rows_cte`、`played_clause`、`units_protocol_order` | DERIVED，KEEP；但 `quality_json` 是常量 `'{}'` → R4 |
| `library_metadata_inputs` | `metadata/nfo_input/record.rs`、`export_driver.rs` | 21 处；Task 17 实测：删掉 `metadata_inputs_media` 会让发布从 112 → 410 ms/100 文件 | KEEP：NFO 是发布期真实输入 |
| `library_items` | `reduce.rs`、`reduce_incremental/mod.rs`、`query_projection/mod.rs::settle` | 27 处（browse/详情/协议/统计/完整性） | KEEP：卡片摘要单一事实源；3 个写-only 列 → R3 |
| `library_item_terms` | `query_projection/browse.rs::sync_terms`、`reduce_incremental/terms.rs` | 8 处（facet 过滤、成员集、locales 批） | DERIVED，KEEP；三棵树 → R1 |
| `library_protocol_terms` | `query_projection/mod.rs:67` | **1 处**：`library_query/protocol/membership.rs:17` | DERIVED，KEEP(理由) → R7 |
| `library_protocol_people` | `query_projection/people.rs::sync` | **2 处**：`people.rs::read`（PK 前缀）、`membership.rs:21` | DERIVED，KEEP(理由) → R6 |
| `library_query_counts` | `query_projection/counts.rs::apply`（每次 settle 增量维护） | 6 处：`omit_full_library_terms`、`facets::query_counts`、`count_browse`、`exact_count_sql`、`protocol/count.rs` | DERIVED，**KEEP**：唯一 O(1) 计数 |
| `library_query_config` | `metadata_language.rs`、`default_locale.rs` | 8 处（locale 解析、`mark_current`） | KEEP：单行配置 |

**关于 “`library_query_counts` 到底有没有被用上”**（Task 17 发现 facet 是 O(cards)×10）：
本轮追到 6 个读者，其中 `facets.rs::omit_full_library_terms`（两处查询）与
`protocol/count.rs` 的 `CountStrategy::Marginal` 是纯边际读取。
Task 17 看到的 9/10 维度全表扫描，是因为 `bucket_count_sql` 只在
`marginal=0` 或 `marginal=all_total` 时才短路，不是“没人读”。**结论：不是死表，
但边际表的价值被 `library_item_terms` 的 CROSS JOIN 盖住了——这是 R9（读放大），不是删除项。**
另外每轮 settle 的写成本是「每卡每个 (dimension,value) 一行 upsert」，与卡片数线性，不随库规模爆炸。

### 2.4 查询投影里被逐条问过的“重复事实”（Task 描述点名的几组）

| 谁 | 谁是单一事实源 | 谁是派生 | 派生还有读者吗 |
|---|---|---|---|
| `library_items` vs `library_item_terms` | `library_items`（卡片行） | `library_item_terms`（多值倒排） | 有：facet 过滤只能靠倒排，卡片行没有 genre 列 |
| `library_item_terms` vs `library_protocol_terms` | 都不完整：前者 genre 来自已采用 `genres_json`，后者 = 前者 ∪ NFO/adopted genres ∪ studio | 后者 | 有，但只有 1 条语句（协议 genre/studio 过滤）→ R7 |
| `library_units` vs `library_file_units` | `library_file_units`（覆盖事实） | `library_units`（单元聚合 + preferred） | 有：单元粒度协议分页/季列表/`played` 决议 |
| `library_query_counts` vs `library_query_count_rebuild` | 前者是现值 | 后者是 integrity 校验的暂存（每 operation 一行/dimension-value，跑完即删） | 有：`integrity_query::finish_counts` 在跨事务分批里需要持久暂存 |
| `library_units.quality_json` | `library_files` 的质量列 | 无（常量 `{}`） | **没有** → R4 |

⚠️ `library_item_terms` 的 `term_kind` CHECK 允许 `'studio'`，但没有任何写入者产生它
（`raw_terms` 只产 genre/country/resolution/collection，`buckets` 只产 decade/runtime/rating/language/hdr/stock）
——studio 只在 `library_protocol_terms` 里。这是 R7 的一个附带事实。

### 2.5 播放与用户事实（7）

| 表 | 写者 | 生产读者 | 判定 |
|---|---|---|---|
| `playback_units` | `playback/units.rs`、`event.rs` | 27 处（进度/收藏/继续观看） | KEEP：用户观看事实唯一来源；20 列宽但按 PK 读 |
| `user_library_items` | `playback/user_summary/mod.rs`、`reduce/card.rs` | 10 处（browse 的 user_join、favorites、facets） | DERIVED，KEEP：v3 转 WITHOUT ROWID 后 PK 读直取 |
| `playback_session_ids` | `playback/session_ids.rs::issue_play_session` | 5 处（事件校验、绑定） | KEEP；rowid 契约 → 4.1 |
| `playback_sessions` | `playback/sessions.rs::upsert_session`、`event.rs` | 7 处（活动会话、设备列表） | KEEP |
| `playback_logs` | `event.rs::close_live_session` | 6 处（历史、保留策略、watch 聚合） | KEEP |
| `playback_metrics` | `playback/logs.rs::insert_metric` | 1 处（`data_retention::prune_qoe`）但写入方是每次 QoE 上报 | KEEP(理由)：唯一读者是保留策略，但表窄（13 列）且按 `recorded_at` 有界清理 |
| `collections` / `collection_items` | `collections.rs` | 6 处（公共合集进 terms，私有合集走 API） | KEEP：公共/私有可见性有 CHECK |

### 2.6 操作日志族（**横切问题 B**）（6）

| 表 | 写者 | 生产读者 | 判定 |
|---|---|---|---|
| `library_operations` | `library_operations/mod.rs` | 21 处；`operations_library_kind` 是 v3 为 browse/detail 加的 | KEEP |
| `media_operations` | `media_operations/preflight.rs::begin_merge` | 6 处（合并推进、`ensure_no_operation`） | KEEP |
| `library_removals` | `library_deletion/mod.rs::begin_library_removal` | 18 处（阻断、快照游标、清理推进） | KEEP |
| `scan_operations` | `scan_operations.rs::create_scan_operation` | 7 处（claim、取消、恢复、API 状态） | KEEP |
| `library_removal_roots` | `begin_library_removal` | 2 处（`path_key_in_removal_scope`、`removal_roots`） | KEEP：删除作用域的唯一记录 |
| `library_removal_files` | `snapshot.rs::capture_batch` | 4 处（分批清理游标） | KEEP |

**合并成一张带 kind 判别列的表？裁决：不合并。** 理由（都被证据支撑）：

1. 行数与库规模无关：它们是“在飞操作”的载体，不是历史表。夹具里这 6 张表各 1 页（4 KiB），
   合并省下的空间在生产规模下不可见。
2. 索引形状互斥：`library_operation_dispatch(state,updated_at_ms)`（派发）、
   `operations_library_kind(library_id,kind,updated_at_ms DESC)`（browse/detail 的完整性状态子查询）、
   `scan_library_status(library_id,status)`、`library_removals.library_id UNIQUE`。
   合并后要么保留全部四组列（表变宽，`state`/`kind` 语义冲突），要么让每个热查询多一层判别过滤。
3. 生命周期与栅栏不同：`integrity_guard` 用 `EXISTS(... kind='integrity' AND state<>'succeeded')` 做写栅栏，
   合并会让三种操作互相污染这条 EXISTS 的计划与语义。
4. `scan_operations` 是**热轮询**表（API 每批查状态），另外三张是**写栅栏**表；把热轮询表和栅栏表合并
   是把不同访问模式塞进同一棵 B 树。

## 3. 逐索引裁决（审计时 79 → v4 之后 76）

方法见第 0 节第 2 条。下表「读证据」列给出**语句位置**，`E` = 已用 `EXPLAIN QUERY PLAN` 在本夹具上确认
计划里出现该索引名（或对应 PK）。

### 3.1 `library_items`（13 + PK autoindex，15.6 MB 索引 vs 5.3 MB 表）

每个排序键都由 `library_query/items/page.rs:72-80` 的 `BrowseOrder → expr` 映射生成，
`library_schema_audit.rs::every_browse_sort_has_its_own_index` 逐个钉住（ASC/DESC 双向、无 TEMP B-TREE）：

| 索引 | 读证据 | 判定 |
|---|---|---|
| `items_title` | `BrowseOrder::Title` → `ORDER BY i.title_sort` | KEEP E |
| `items_added` | `BrowseOrder::AddedAt` | KEEP E |
| `items_release` | `BrowseOrder::ReleaseDate` | KEEP E |
| `items_rating` | `BrowseOrder::Rating` | KEEP E |
| `items_runtime` | `BrowseOrder::Runtime` | KEEP E |
| `items_size` | `BrowseOrder::Size`；`library_stats::library_roster_stats` 的 `SUM(size_bytes)` 覆盖读 | KEEP E |
| `items_resolution` | `BrowseOrder::Resolution`；`ledger.rs`/`reduce_incremental` 取 `resolution_rank` | KEEP E |
| `items_shuffle` | `BrowseOrder::Random`（`stable_shuffle_key`） | KEEP E |
| `items_media` | `media/mod.rs`、`apply.rs`、`user_summary` 的按 media 反查 | KEEP E |
| `items_protocol_name_asc` | `protocol/page.rs:146::work_index_for_sort` → `INDEXED BY` | KEEP E（v3 未覆盖，本轮补） |
| `items_protocol_name_desc` | 同上（Name DESC） | KEEP E |
| `items_protocol_default` | v3 测试钉住（默认 DateCreated 序） | KEEP E |
| `items_protocol_default_explicit` | v3 测试钉住（显式 Name DESC 序） | KEEP E |

`BrowseOrder::LastPlayed` 的 `ORDER BY COALESCE(u.last_played_at,0)` **无法**走索引（表达式列），
计划里是 `SEARCH u USING PRIMARY KEY` + `TEMP B-TREE`。这不是漏建索引：`user_items_activity`
在投影里存在，但 COALESCE 让它不可用。判定：接受该序有排序成本，**不为此加索引**（详见 R10）。

### 3.2 `library_files`（6 + 3 个 UNIQUE autoindex）

| 索引 | 读证据 | 判定 |
|---|---|---|
| `files_library_file` | `library_deletion/cleanup.rs`、`publish.rs` 的 `(library_id,file_id)` 存在性检查 | KEEP E |
| `files_media_library` | 16 个文件；`ledger.rs`、`library/markers/source_guard.rs` 按 media 取文件 | KEEP E |
| `files_library_media` | 18 条语句；`reduce/card.rs`、`reduce_incremental/mod.rs` 的 `(library_id,media_id)` 取文件 | KEEP E |
| `files_root_path` | `roots.rs:287::prune_unreferenced_roots` 的 `EXISTS(... WHERE root_id=?)`（实测 `SEARCH library_files USING COVERING INDEX files_root_path (root_id=?)`）+ `(root_id,library_id)` 形状的归属/子查询 | KEEP，但**列序浪费** → R8 |
| `files_kind_path` | `roots.rs::SELECT DISTINCT library_id FROM library_files WHERE kind=?1` | KEEP E |
| `files_presence` | `files/read.rs::missing_paths_for_library`、`library_stats` | KEEP E |

`files_library_media` vs `files_media_library` 不是冗余前缀（前者以 library 开头，后者以 media 开头），
两者分别服务“库内取某片”和“跨库取某片”，与 v3 保留 `items_protocol_default` 的论证同构。

### 3.3 `library_item_terms` / `library_protocol_terms` / `library_protocol_people`

| 索引 | 读证据 | 判定 |
|---|---|---|
| `terms_media` | `members/facets.rs:220` 显式 `CROSS JOIN … INDEXED BY terms_media`；`browse.rs::read_terms` | KEEP E（但它是一棵**整表副本** → R1） |
| `terms_value` | `members/plan.rs:135`、`members/facets.rs:134` 的 `(term_kind,term_value)` → media 集 | KEEP E |
| `protocol_terms_media` | `query_projection/browse.rs::read_terms`（`WHERE library_id=? AND media_id=?`） | KEEP E |
| `protocol_terms_value` | **无**：唯一的术语谓词 `membership.rs:17` 带 `library_id`+`media_id` 等值，直接 seek PK | **删除（本轮已落地）** |
| `protocol_people_wire` | 本人实测：`WHERE wire_id IN (…)` → `SEARCH … USING COVERING INDEX protocol_people_wire` | KEEP（是 H2 修复的前提） |
| `protocol_people_name` | **无**：唯一人物谓词以 `wire_id`/`tmdb_id` 起手，`name_key` 只作别名行后缀比较 | **删除（本轮已落地）** |
| `protocol_people_tmdb` | 原列序 `(role,tmdb_id,…)` 下 `tmdb_id IN (…)` 只能 **SCAN** 整棵索引；改序为 `(tmdb_id,role,…)` 后才是 seek | **改列序（本轮已落地）** |

### 3.4 其余 50 个索引

全部用「抽语句 + EXPLAIN + drop-diff」确认有读者。按表列出（`E` = 计划里出现过）：

- `media`：`media_title`（`media/mod.rs:230,246,408` 的 title 查重/`ORDER BY m.title`）、
  `media_survivor`（`media/retired.rs`）。
- `media_aliases`：`alias_media`（6 个文件）、`alias_target`（7 个文件）。
- `libraries`：`library_default`（`libraries.rs::default_library` 等 4 处）。
- `library_roots`：`roots_library`（8 个文件）。
- `library_file_units`：`coverage_media`（`apply.rs` 改归属）。
- `library_units`：`units_media_scope`（`apply.rs`、`user_summary`）、`units_protocol_order`
  （`protocol/page.rs:191` 显式 `INDEXED BY`）。
- `library_metadata_inputs`：`metadata_input_path`（`page_inputs.rs::file_path_key` 的
  `(root_id,path_key)`、`roots.rs:287` 的 `root_id`）、`metadata_inputs_media`（v3 加的 FK 子查询）。
- `library_query_counts`：主键即 (library_id,surface,dimension,value)，无额外索引；6 处读都走它。
- `collections`/`collection_items`：`collections_owner`、`collection_media`。
- `playback_units`：`playback_unit_media`（5 个文件）、`playback_activity`（`units.rs`/`mutate.rs`/
  `subscribes.rs` 的 `ORDER BY watch_activity_at DESC`）。
- `user_library_items`：`user_items_media`（完整性、user_summary）、`user_items_activity`（facets、units）。
- `playback_session_ids`：`session_id_expiry`（`session_ids.rs:78` 的过期批）、`session_generation`
  （`MAX(generation)`）。
- `playback_sessions`：`playback_session_file`（`reassign.rs`、`library_deletion/sessions.rs`）。
- `playback_logs`：`logs_user_started`（按用户列表）、`logs_started`（管理员全量列表，
  `SCAN … USING COVERING INDEX logs_started` 后有序输出）、`logs_ended`（`playback/retention.rs` 批量过期）。
  **`playback_logs_media` 已于 v5 删除**（同下表注）。
- `playback_metrics`：`metrics_recorded`（`data_retention.rs:158` 游标）。
- `library_operations`：`library_operation_dispatch`（派发 claim）、`operations_library_kind`（v3）。
- `pending_file_deletions`：`pending_deletions_due`（v3 + E13 的 `ORDER BY next_retry_at`）。
- `pending_root_reassignments`：`root_reassignment_dispatch`（`roots.rs` 公平轮转）。
- `root_reassignment_state`：`root_reassignment_library`（`roots/status.rs`）。
- `pending_file_renames`：`pending_file_renames_due`（`rename_durable.rs`）。
- `unidentified`：`unidentified_identity`（`consume` 按 id）、`unidentified_path_seek`（seek 窗口）、
  `unidentified_root`（`roots.rs:289`）。
- `scan_operations`：`scan_library_status`（`claim_next_queued_scan`、`MAX(generation)`）。
- `probe_jobs`：`probe_active_scope`（部分索引）、`probe_media_created`、`probe_terminal`、
  `probe_scope_terminal`。
- `probe_job_units`：`probe_unit_active`、`probe_unit_dispatch`（`status='queued'` 部分索引，
  `probe_dispatch.rs`）、`probe_unit_status`（4 个文件）。
- `file_probe_obligations`：`probe_obligation_dispatch`（`scan_probe_obligations.rs`）。
- `probe_stage_state`：`probe_stage_active_job`、`probe_retry`（部分索引，`claim_due_probe_stages`）。
- `chapter_frame_tasks`：`chapter_dispatch`。
- `fingerprint_samples`：`sample_lookup`、`sample_job`。
- `fingerprint_season_models`：`season_model_lookup`。
- `fingerprint_model_members`：`model_file`。
- `fingerprint_attempts`：`attempt_job_file`。

**v5 追加（2026-10-26）**：`playback_{sessions,logs,metrics}_media` 三个索引已删除。本审计当时把它们
记为「v3 新增、有真实语句」——那句话对 v3 成立，对今天不成立：它们唯一的消费者
`Store::delete_playback_for_media` 已移入 `src/test_support.rs`，生产没有任何按 `media_id` 删除播放行的路径
（Media 退役不删除；生产删除按 `file_id`/`user_id`/`id`）。这正是本审计的判据反过来适用于 v3 自己的新增项：
**“当时有人读”不等于“今天有人读”**。同轮把 `LIBRARY_INDEXES` 的两个漏项（`files_kind_path`、`terms_value`）
补齐，并把索引侧的布局校验改成双向——多余索引现在会被拒绝，理由同上：它是无人读取的写放大。

**没有一个索引的唯一用户是测试**（`protocol_terms_value` / `protocol_people_name` 是唯一两个零读者，
已删；`schema_redundant_indexes.rs` 里那两个名字是历史删除项的守卫）。

## 4. 横切问题

### 4.1 隐式 `rowid` 当排序键（9 处，逐表裁决）

生产代码里 24 处 `rowid` 引用，去重后落在 8 张表。裁决分三档：
**REWRITE**（改写即可，无 DDL）、**DECLARE**（声明永久 rowid 表）、**V4**（需要显式单调列，归 v4）。

| 表 | 现用法 | 裁决 |
|---|---|---|
| `pending_file_deletions` | `list_pending_file_deletions`/`list_stuck_file_deletions` 的 `ORDER BY rowid` | **DECLARE**：`path` 是 PK，入队顺序就是重试顺序，语义上 `rowid` 正是“FIFO 位置”；加显式列只为排序要付一列 + 一棵索引，不值得。`list_due_file_deletions` 已在 v3 改成 `next_retry_at` |
| `probe_jobs` | `latest_probe_job_for_scope`/`latest_marker_refresh_for_season` 的 `created_at_ms DESC, rowid DESC` | **REWRITE**：`id` 已是唯一列，用它做 tie-break 同样确定（只要求稳定，不要求单调） |
| `probe_job_units` | `probe_job_units()` 的 `ORDER BY rowid`；两个有界窗口查询的 `ORDER BY u.rowid LIMIT ?` | **V4**：这是“派发顺序”的真实语义，且是一张每文件一行的热表，值得 `enqueued_seq INTEGER NOT NULL`（单调）后转 WITHOUT ROWID。本轮不动 |
| `scan_operations` | `created_at_ms DESC, rowid DESC` / `created_at_ms ASC, rowid ASC` | **REWRITE**：`id` tie-break |
| `playback_session_ids` | `WHERE rowid IN (SELECT rowid … ORDER BY issued_at ASC LIMIT 500)` | **REWRITE**：改成选中 PK 三元组 `(user_id,device_id,play_session_id)`，同时解开 WITHOUT ROWID |
| `unidentified` | `DELETE … WHERE rowid IN (SELECT rowid … LIMIT 100)` | **REWRITE**：同上，用 `(library_id,path_key)` |
| `fingerprint_samples` | `ORDER BY window_start_ms ASC, rowid DESC` | **REWRITE**：tie-break 用 `sample_id` |
| `fingerprint_season_models` | `ORDER BY created_at_ms ASC, rowid ASC` | **REWRITE**：tie-break 用 `model_id` |

补充：`data_retention.rs::chunk_delete_rowids` 的 `DELETE … WHERE rowid IN` 只服务
`catalog.rs::CACHE_TABLES`（**cache.db**，不是 library.db），不在本审计范围。
`probe_backpressure.rs::newer.rowid > u.rowid` 见 R11（整模块已死）。

### 4.2 协议人物过滤（H2）：不是索引问题，是 SQL 形状问题

`library_query/protocol/membership.rs:21` 的相关子查询：

```sql
EXISTS (SELECT 1 FROM library_protocol_people p
        WHERE p.library_id=s.library_id AND p.media_id=s.media_id
          AND EXISTS (SELECT 1 FROM library_protocol_people a
                      JOIN scoped seed ON seed.library_id=a.library_id AND seed.media_id=a.media_id
                      WHERE (a.wire_id IN (…) OR a.tmdb_id IN (…))
                        AND a.role=p.role
                        AND (p.name_key=trim(a.name_key) OR (…))))
```

`EXPLAIN QUERY PLAN`：`SEARCH p EXISTS USING PRIMARY KEY (library_id=? AND media_id=?)`
→ `CORRELATED SCALAR SUBQUERY 2` → **`SEARCH a USING PRIMARY KEY (library_id=?)`**：
`a` 以 `library_id` 前缀进入，即**每张卡都重扫整库的人物行**（OR 谓词让两个人物索引都无法驱动）。

夹具实测（200k 人物行 / 20k 卡片）：

| 形状 | 1 张卡 | 10 张卡 | 20k 卡片 | 同一 3 卡作用域（结果相同=3） |
|---|---|---|---|---|
| 生产形状 | 910 ms | 9,142 ms | —（外推 ≈ 30 min） | 1,307 ms |
| 身份集先解析一次（`wire_id IN` 走 `protocol_people_wire`） | 9 ms | 9 ms | **54 ms** | **8.3 ms** |

**157×（同结果）/ 1000×+（大规模）。** 修法是把人物身份解析提到作用域之外
（一条 `MATERIALIZED` CTE 走 `protocol_people_wire`/`protocol_people_tmdb`，再与 `scoped` 求交），
**不需要新索引**——这正是本轮保留这两个索引的理由（R6）。改动在 `crates/store/src/library_query/protocol/`，
不在本任务写集内，已上报 Lead。

## 5. 已落地（v4 轮次，`LIBRARY_USER_VERSION` 3 → 4）

一个 DDL 轮次一个版本号：v3 期间落地的两项索引删除与 v4 新增的 R1/R2/R3/R4 合并在
同一次 bump 里，避免“隔一天再来一次切库”。

| 改动 | 证据 | 空间效果（合成夹具） |
|---|---|---|
| 删 `protocol_terms_value` | drop-diff：764 条语句计划**零变化**；唯一术语谓词 seek PK；投影读走 `protocol_terms_media` | −1.63 MB |
| 删 `protocol_people_name` | drop-diff 零变化；谓词以 `wire_id`/`tmdb_id` 起手，`(role,name_key)` 前缀无法服务 | −10.31 MB |
| `protocol_people_tmdb` 列序 `(role,tmdb_id,…)` → `(tmdb_id,role,…)` | 原序列下 `tmdb_id IN (…)` 是整索引 `SCAN`；新序列是 seek（`library_schema_audit.rs` 钉住） | ±0（同字节，可用） |
| **R1** `library_item_terms` 主键改序 `(library_id,media_id,term_kind,term_value)` + 删 `terms_media` | 每次卡 settle 的 term 读/删/FK 子查询变成键前缀；`facets.rs` 的 `INDEXED BY terms_media` 提示同步删除，11 个 facet 维度的 `statements/sort/fullscan/vm` 与改前**逐项相同** | −6.81 MB（表 + 索引的 25.9 MB → 19.1 MB） |
| **R2** 删 `media_seasons` 整表 | 生产零 `SELECT`；`write_season` 与 merge 的 DELETE/UPDATE 一并删除，`write_effective` 对 Season scope 直接返回 | 夹具未播种该表（0 行），实际收益 = 季×locale×宽 JSON/每次刮削 |
| **R3** 删 `library_items.aliases_json` / `added_at_ns` / `has_nfo` | 三者生产只写不读；`aliases_json` 还让 `reduce::media_identity` 每卡多跑 **5 条按来源查询**，这些查询随之删除 | −0.32 MB（含 3 列 + 那 5 条查询的消失） |
| **R4** 删 `library_units.quality_json` | 4 个写入点全写常量 `'{}'`，零读者 | −0.03 MB |
| 版本号 3 → 4 | `schema/layout.rs`；v3 文件按 rebuild-on-cutover 拒绝（`v3_library_db_is_rejected` 用真实 v3 对象播种） | — |

合计 **117.9 MB → 98.0 MB（−19.9 MB / −16.9%）**，表 **54 → 53**，索引 **79 → 76**，字段 **606 → 592**。

落地文件：
- `crates/store/src/schema/v1/library.sql`（逐条“为什么删/为什么改序”的注释）
- `crates/store/src/schema/v1/objects.rs`（`LIBRARY_INDEXES`/`LIBRARY_TABLES` 同步，否则每次 `Store::open` 都会红）
- `crates/store/src/schema/layout.rs`（版本号）
- 写入点：`metadata/effective.rs`、`media_operations/{references,apply}.rs`、`library_write/reduce.rs`、
  `library_write/reduce/card.rs`、`library_write/reduce_incremental/{mod,units}.rs`、
  `query_projection/expected.rs`、`library_query/members/facets.rs`
- 测试：`library_schema_audit.rs`（新增：死索引不得回归、术语谓词 seek PK、人物身份覆盖 seek、
  8 个 browse 排序各自命中索引、11 个 facet 维度的工作量上限、v3 拒绝）、`library_query_plans.rs`、
  `query_model_layout.rs`、`media_alias_updates.rs`、`merge_support`、`media_merge_{recovery,snapshots}.rs`、
  `storage_work_bounds/seek_scale.rs`、`crates/api/tests/storage_query_scale.rs`（夹具列清单）

**没有做的**（都是“读侧”或跨 crate，见第 6 节）：R6、R9、R5、R7、R8、R11、R12。

## 6. 按收益/风险排序的修复清单（给 Lead 决策）

收益口径：写放大（每次发布/探测要维护多少结构）与空间；风险口径：需要改动多少 crate、能否被现有测试钉住。

| 序 | 项 | 收益 | 风险 | 需要什么 | 建议 |
|---|---|---|---|---|---|
| R1 | `library_item_terms` PK 改序为 `(library_id,media_id,term_kind,term_value)` 并删 `terms_media` | **−6.81 MB（−6.4%）**；表从 3 棵树降到 2 棵，每次 terms 同步少一棵索引写 | 中：`facets.rs:220` 的 `INDEXED BY terms_media` 提示必须去掉（store/src 改动），否则 prepare 失败 | DDL + `facets.rs` 一行 + 计划断言 | **已落地（v4）** |
| R2 | 删 `media_seasons`（含 `write_season` 调用点） | 去掉每次 adopt 的季×locale 宽 JSON 写；H1 的头号发现 | 中：DDL + `metadata/effective.rs` + `objects.rs` + `apply.rs:529` 引用表 + merge 快照测试 | DDL + store/src | **已落地（v4）** |
| R3 | 删 `library_items.aliases_json` / `added_at_ns` / `has_nfo` | 每张卡每行少 3 个 NOT NULL 列（一个宽 JSON） | 低-中：`reduce.rs`、`expected.rs`、`reduce_incremental` 写入点；`media_alias_updates.rs` 改为断言 `media_aliases` 行集（`aliases_display_json` 是“目录译名”，与来源别名是两件不同的事，不是同一个投影） | DDL + store/src + 测试改写 | **已落地（v4）** |
| R4 | 删 `library_units.quality_json` | 每单元每行少一个常量 JSON；去掉 `json_valid` CHECK 开销 | 低：4 个写入点都写 `'{}'`，零读者 | DDL + `card.rs`/`units.rs`/`expected.rs` | **已落地（v4）** |
| R5 | 删 `file_details.fingerprint_json` / `outro_fingerprint_json` | 每次探测/发布少两列 JSON 往返；真实事实在 `fingerprint_cache` | 中：`files/types.rs`、`publish.rs`、`probe.rs`、`api/probe_manager/probe.rs:492` 都在搬这两列 | DDL + 2 个 crate | 建议 v4 后段处理（先确认 `probe.rs` 的 `None` 不是临时状态） |
| R6 | **修协议人物过滤的 SQL 形状**（身份集物化一次） | 同结果下 **1,310× 更少 VM 步**、**1,196×–5,921× 更快**；两个 `protocol_people_*` 索引从“0 读者”变成真有读者 | 低：只改 `library_query/protocol/{membership,mod}.rs` + 测试，**无 DDL** | store/src + 计划断言 + 工作上限断言 | **已落地（Task 25）** |
| R7 | 把 studio 并进 `library_item_terms`，删 `library_protocol_terms` | −3.31 MB（表 + `protocol_terms_media`），少一张每卡同步的表 | 高：会让 web facet 的 genre/studio 取值集合变化（NFO 独有的 genre 进入公共倒排），属语义变更 | DDL + 投影 + facet 语义评审 | 只在确定“web facet 可以接受 NFO genre”后才做 |
| R8 | `files_root_path(root_id,path_key)` → `(root_id)` 或 `(root_id,library_id)` | 少复制一份最宽的 `path_key` 文本（1.88 MB → ~0.2 MB 量级） | 低-中：需要 FK 子查询仍可 seek（`(root_id,library_id)` 满足 FK，`EXISTS(root_id)` 满足 prune） | DDL + 计划断言 | 可并入 v4；先补一条 `DELETE FROM library_roots` 的 FK 子查询断言 |
| R9 | facet 计数改走 `library_query_counts`（9/10 维度） | 读放大从 O(cards)×10 降到 O(边际行) | 中：`bucket_count_sql` 的短路条件需重写，一致性依赖 `counts::apply` | store/src + Task 17 门禁 | 与 R6 并列的读侧收益；不入本轮 DDL |
| R10 | `BrowseOrder::LastPlayed` 的 `COALESCE` 排序 | 每次该序都 TEMP B-TREE | — | 需要 `user_library_items` 上的表达式索引或改 SQL | **不建议**：一个排序序换一棵索引不划算，记录在案 |
| R11 | 删死模块 `ledger_presence/probe_work.rs` + `schema/probe_backpressure.rs` | 无写放大收益，但它们引用**已删除的表**（`ledger`、`ledger_probe_obligations`），一旦被接线就是运行时错误 | 低（无调用点，`git grep` 已确认） | store/src | 建议任何人顺手删；不影响本任务的验收 |
| R12 | `probe_job_units` 加 `enqueued_seq` 后转 WITHOUT ROWID | 少一棵 PK autoindex；派发顺序从隐式 rowid 变显式列 | 中：`probe_tasks.rs`/`probe_dispatch.rs` 4 处 `ORDER BY rowid` | DDL + store/src | v4 之后再说（先做 R6/R1 这类高收益项） |

### 6.1 R6 落地实测（Task 25，2026-10-26）

改法（`crates/store/src/library_query/protocol/membership.rs`）：把请求里的身份解析成
`person_wanted AS MATERIALIZED`（一次），每张卡只对自己的 person 行做探针，不再在 per-card
子查询里带 `wire_id IN (…) OR tmdb_id IN (…)`。两个细节都是结论的一部分：

- **作用域检查必须是相关 `EXISTS`，不能是 `JOIN scoped seed`**。用 JOIN 时规划器把 `a` 当内表，
  计划仍是 `SEARCH a USING PRIMARY KEY (library_id=?)`（整库人物前缀扫描）；换成 `EXISTS` 后
  析取留在最外层，SQLite 用 **`MULTI-INDEX OR`** 同时 seek `protocol_people_wire` 和
  `protocol_people_tmdb`。**因此这两个索引不是“为将来的任务保留”，而是本修复真正在用。**
- **`DISTINCT` 是修复的一部分**：一个演员在 N 张卡上就有 N 行 person 记录；不去重，物化集合会
  随库增长，per-card 探针又变成 O(库)。去重后集合大小由请求决定（本例：66 行 → 1 个身份）。

夹具：`library.db` v4 schema，纯 SQL 播种，每卡 10 个人物行，`people = cards × 10`。

| 规模 | OLD（改前形状） | NEW（改后） | 同答案 |
|---|---|---|---|
| 500 卡 / 5,000 人物行 | 785 ms / **149,982,907** VM 步 | 0.66 ms / **114,504** VM 步（1,310× 更少） | ✓ 均返回 1 |
| 2,000 卡 / 20,000 人物行 | 14,645 ms | 2.47 ms（5,921×） | ✓ 均返回 6 |
| 20,000 卡 / 200,000 人物行 | 未跑（按实测二次律 ≈24 min） | 30.6 ms（热）/ 112.8 ms（首次） | — |

二次律的证据：卡数 ×4（500→2,000）时 OLD 时间 ×18.6，即 O(卡 × 库内人物)，与 H2 的诊断一致；
NEW 在同区间 ×3.7，即 O(卡 × 自己的 person 行)。**`fullscan_steps` 两个形状都是 0**
（前缀扫描不计入 fullscan），所以门禁钉的是 `vm_steps`，不是全表扫描步数。

门禁（都已提交）：
- `crates/store/tests/library_query_plans.rs::person_filter_resolves_identities_once_and_seeks_both_person_indexes`
  —— 计划必须出现 `MATERIALIZE person_wanted`、`protocol_people_wire`、`protocol_people_tmdb`，
  且**不得**出现 `SEARCH a USING PRIMARY KEY (library_id=?)`。
- `crates/store/tests/query_model_protocol.rs::person_filter_work_is_bounded_by_scoped_people_not_library_people`
  —— 40 张命中卡 + 400 张填充卡（各 10 人）共 4,040 行人物，`vm_steps ≤ 400,000`（实测 164,559；
  改前形状按 O(卡×库内人物) ≈1.78M 次访问，无法通过）。
- 语义等价由既有 `query_model_protocol.rs::scoped_person_aliases_preserve_name_or_tmdb_matching` 覆盖
  （tmdb 身份、wire 身份、姓名 + Director 角色、未授权身份不得扩展姓名别名、Scope 外身份不得播种别名），
  改前改后同绿。

**不建议做的**：合并操作日志族（第 2.6 节四条理由）；给 `library_query_counts` 加索引
（PK 已覆盖）；把 `playback_units` 转 WITHOUT ROWID（v3 已实测 287 B/row > 257 B/row，拒绝）。

## 7. 保留，以及为什么（本轮明确不动的高风险/高价值项）

- **`library_items` 的 13 个索引全部保留**：逐个都能指到一条真实语句（3.1 节），
  8 个 browse 排序 + 2 个协议 Name 序 + 2 个协议默认序 + 1 个 media 反查。
  索引总量 15.6 MB 确实等于表的 2.9 倍，但删任何一个都会让对应排序退化为 TEMP B-TREE，
  而 `library_schema_audit.rs` 现在会判红。
- **`items_protocol_default` 与 `_explicit` 都保留**：v3 已用交叉实验钉死（E4），本轮不重新论证。
- **`protocol_people_wire` / `protocol_people_tmdb` 保留**：今天是 0 读者，但它们是 R6 唯一便宜的修法
  （实测 `wire_id` seek 0.0 ms）。**这是本审计里唯一“按未来读者保留”的索引，理由已量化**；
  若 Lead 决定不做 R6，则这两个索引应连同 `library_protocol_people` 的协议过滤一起删。
- **`library_query_counts` 保留**：6 处生产读，唯一 O(1) 计数路径（第 2.3 节）。
- **`library_query_count_rebuild` 保留**：integrity 的跨事务暂存，行数是
  `surface × dimension × value`（与卡片数无关），跑完即删。
- **`library_units` / `user_library_items` 保留**：前者是单元粒度协议分页的唯一来源，
  后者是 browse 的 user_join 目标；v3 已把后者转 WITHOUT ROWID。
- **`media_metadata_sources` 保留**：4 个 JSON 列宽，但 browse 只取 studio 一个字段
  （`hydration.rs::fill_studio`），不是宽读；删它会破坏来源合成与合并快照。
- **`fingerprint_*` 六张表全部保留**：`get_fingerprint_cache` 在 `crates/api` 有 13 处调用，
  `find_covering_fingerprint_samples` 4 处；不是死表（本轮逐个核过）。
- **`chapter_frame_tasks`、`probe_season_comparison_history` 保留**：外部调用点各 1 处
  （章节帧生成、marker refresh 的比较摘要），窄表、行数有界。

## 8. 建议但没有做（以及为什么）

审计当时（v3）建议但没有做的项，v4 轮次里 R1/R2/R3/R4 已落地、版本号已 bump、两份
`docs/database/*.md` 已按 v4 统计更新。**仍未做**的如下：

| 项 | 为什么没做 |
|---|---|
| R6（人物过滤 SQL） | 这是**读侧**修复（`library_query/protocol/membership.rs`），不动 DDL；已给出实测数字（157×/1000×+）与修法，Lead 已决定单独立项 |
| R9（facet 走边际表） | 需要重写 `bucket_count_sql` 的短路语义，且它是“读放大”，不是“结构多余”；建议在 Task 17 门禁基线之上单独做 |
| R5（`file_details` 两个指纹列） | 跨 `store` + `api` 两个 crate（`api/probe_manager/probe.rs:492` 一直写 `None`），需先确认那不是临时状态 |
| R7（studio 并入 terms 后删 `library_protocol_terms`） | 会让 web facet 的 genre/studio 取值集合变化，属语义变更，需要单独评审 |
| R8（`files_root_path` 收窄为 `(root_id,library_id)`） | 纯 DDL，可并入下个轮次；本轮不追加 DDL（一个版本号一次 DDL 轮次） |
| R11（死模块 `ledger_presence/probe_work.rs`、`schema/probe_backpressure.rs`） | 已由 Lead 归入死代码任务，不在本任务 |
| R12（`probe_job_units` 加 `enqueued_seq` 后转 WITHOUT ROWID） | 需要同时改 4 处 `ORDER BY rowid`，属下一轮 |
| 用真实 20k/61k 夹具重测 | Task 17 的夹具缓存键包含 `schema/**/*.sql` 哈希，v4 的 DDL 改动会让它自动重播种；让 Task 17 跑完整门禁比在 debug 下重测更可信（本审计的百分比来自同一形状的合成夹具 + `dbstat`，与 Task 17 的 `dbstat` 口径一致） |

## 9. 复现

```bash
# 1) 只有 schema 的空库（计划/索引反向表的基底）
rm -f /tmp/audit.db
sqlite3 /tmp/audit.db < crates/store/src/schema/v1/library.sql

# 2) 播种夹具（纯 SQL，DB 无关；1.2 s）
#    media/library_files/library_file_units/library_units/library_items 各 20k
#    library_item_terms 180k（9 种 term_kind × 20k 卡）
#    library_protocol_terms 40k、library_protocol_people 200k（每卡 10 人）
sqlite3 /tmp/pop.db < /tmp/populate.sql     # 脚本见 git 之外的临时文件；形状如第 0 节所述

# 3) 空间
sqlite3 /tmp/pop.db "SELECT name,SUM(pgsize) b FROM dbstat GROUP BY name ORDER BY b DESC LIMIT 20;"

# 4) 索引反向表 + drop-diff
#    1029 条 SQL 字面量 → EXPLAIN QUERY PLAN → 索引→语句表；
#    对每个候选索引在 /tmp/audit.db 的副本上 DROP INDEX 后重规划 764 条语句并 diff。
#    （一次性 python 脚本，未入库；判据与输出摘要已写进第 3 节）

# 5) 人物过滤 A/B
sqlite3 /tmp/pop.db "EXPLAIN QUERY PLAN <production shape>";   # → SEARCH a USING PRIMARY KEY (library_id=?)
#    计时对比见第 4.2 节表格

# 6) 回归
CARGO_TARGET_DIR=target/audit cargo test -p store
```

## 10. 与 v3、与 Task 17 的边界

- 本审计**不重新论证** v3 已定的项：6 个新索引、5 张 WITHOUT ROWID 的选表判据、
  `items_protocol_default(_explicit)` 必须并存、`playback_units` 拒绝转换、`auto_vacuum` 只能建库时设。
  这些在 `docs/database/*.md`、`tests/library_query_plans.rs`、计划勘误 E4/E5/E7/E12-E15 里已有结论。
- 本审计新增的是 v3 **没有看**的部分：73 个既有索引的逐条可达性（v3 只覆盖了自己新增的 6 个）、
  54 张表的“今天谁读”、rowid 契约逐表裁决、操作日志族是否合并、以及派生缓存的写放大清单。
- Task 17 的 `dbstat` 数字（61k 文件夹具）与本审计（20k 卡片 / 200k 人物）口径一致、侧重点不同：
  Task 17 看**读写墙钟与确定性计数**，本审计看**每个结构是否有读者**。两者结论不冲突：
  Task 17 明确说“不要因为夹具没测到读者就删索引”，本审计对每个删除项给的是**计划零 diff**，不是“没测到”。
