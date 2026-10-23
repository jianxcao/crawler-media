# Storage Query Efficiency Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在结果、原子性和恢复合同不变的前提下，减少可证实的重复查询、归约及冷字段读取，以测量决定索引和物理布局调整。

**Architecture:** 先记录同数据/同SQLite版本的工作量，再分别优化反向索引、批内用户摘要、facet及卡片冷字段、协议分页。权威数据库耐久性不作为默认提速筹码，复杂排序/精确facet成本如实披露，不构建第二份通用全文件投影。

**Tech Stack:** bundled SQLite/rusqlite、现有 release测量与StatementStatus仪器、Rust公开行为测试；不预设新依赖或FTS服务。

**Spec:** [主规范](<../specs/2026-10-18-library-storage-redesign.md>) §§4.3/4.7/5/6；[核验报告](<../../audits/2026-10-23-storage-committed-head-verification.md>) O01–O07及撤回项。基线HEAD=`572d4fa8870271033068b9ede0cea71ecbdf922b`、main=`86122d73a4a23439e1d83234590fdd98b388fc50`。**当前只交付文档，未执行优化、EQP或benchmark。**

## Global Constraints

- “无旧数据导入、兼容视图、双读双写、旧版本回退。”未上线目标DDL不增加旧布局shim；未来已上线升级另走正式版本策略。
- “大表尽量不关联”；允许索引半连接、分页后主键水合、单Media范围聚合，不允许全表Vec/Rust JOIN。
- “未看查询必须包含缺失行”；用户私有Collection不写公共terms/facet；分页前授权、过滤、逻辑去重。
- “所有索引事实、覆盖、terms、卡片和用户摘要在同一 library.db `BEGIN IMMEDIATE` 事务内发布”；优化不能拆成对外可见的半事务。
- 规模验收沿主规范：“规模至少 Media=100k、文件=1M、Playback 单元=1M”；“普通请求 page_size=50”。这是后续测量目标，不是本轮已跑结果。
- 固定数据、SQLite版本、PRAGMA、存储介质，记录VM steps、实际读/写范围、CPU、锁等待/持有时长、p50/p95、JSON字节、WAL峰值及checkpoint结果；缺指标标Not measured，不能把None==None当通过。
- `.rs`≤800行/函数≤120行；正确性测试公开seam，性能仪器可以记录查询工作量，不能用私有SQL字符串代替结果断言。最终触及crate独立测试与workspace测试。
- 默认不降低app/library/subscribe/jobs的耐久性；WAL NORMAL不等于所有已确认提交断电不丢，journal_size_limit不等于WAL硬上限。

## Review Focus

1. metadata输入/root/用户删除与文件清理：索引优化不绕过复合FK、不删观看事实（Q1）。
2. 同batch同集多版本、空覆盖、仅NFO、presence变化：归约少做但结果必须等价full reduction（Q2）。
3. exclude-own-dimension、累计rating桶、零候选、无稀疏摘要：facet结果不因合并聚合改变（Q3）。
4. Jellyfin混合类型、同Media跨库、同名人物/genre、跨locale/NFO：分页与total不因late materialization改变（Q4）。
5. 相同时间戳cache游标、长reader阻checkpoint、10k集Media：布局/连接/分批政策不得牺牲顺序、耐久性或原子可见（Q5）。

---

## 文件责任、顺序与非目标

| 任务 | 生产职责 | 前置 |
|---|---|---|
| Q1 | 反向/清理索引与工作量基线 | 独立；DDL与运行时计划串行合并 |
| Q2 | 批内用户摘要finalize与固定热SQL缓存 | A3；A4固定事实/活动语义 |
| Q3 | 卡片studio标量与facet聚合 | A9 locale/采用政策；B1授权范围 |
| Q4 | 协议精确词条与分页后选源 | Q3共享采用事实；B2逻辑/物理目标规则 |
| Q5 | 长操作与物理布局/PRAGMA测量决策 | A6–A8持久屏障；前四项稳定后测 |

不复修BatchSettled、不重排当前terms主键、不把cache混合排序索引当重复删除、不因COUNT要扫描窄表就构造新缓存系统、不把 `%substring%` 换成普通B-tree后声称O(log N)。不独立扩大为FTS、多连接池或全仓结构重写。

### Task Q1: 按真实执行路径补反向/清理索引（O05）

**Files:** Modify [library.sql](<../../../crates/store/src/schema/v1/library.sql>)、[app.sql](<../../../crates/store/src/schema/v1/app.sql>)、[objects.rs](<../../../crates/store/src/schema/v1/objects.rs>)；Test [schema_redundant_indexes.rs](<../../../crates/store/tests/schema_redundant_indexes.rs>)、[ledger_cleanup.rs](<../../../crates/store/tests/ledger_cleanup.rs>)、[data_retention.rs](<../../../crates/store/tests/data_retention.rs>)；Create `crates/store/tests/cleanup_index_work.rs`。

**Interfaces:** 不变更业务API。目标索引候选：`fingerprint_attempts_file(file_id,attempt_id)`、`probe_stage_active_job(active_job_id) WHERE active_job_id IS NOT NULL`、`metadata_inputs_media(media_id,library_id)`、`unidentified_root(root_id,library_id)`、`media_survivor(survivor_id) WHERE survivor_id IS NOT NULL`、`library_operations_scope(library_id,kind,updated_at_ms DESC,operation_id)`。Playback logs/metrics 的 media_id/user_id、search_snapshots 的 vertical/created_at 作为单独候选测量；不能以整包“高优索引”无差别加入。

- [ ] **Step 1 — 写测试/基线**：有大量无关attempts/jobs/input行，公开删除一个file、prune一个终态probe job、merge两个Media、删除用户，核对仅预期关联事实改变；用bundled SQLite采集相关语句EQP/VM和写放大。下列是业务断言，不以索引名字代替功能：
  ```rust
  assert_eq!(remaining_file_ids, expected_unrelated_ids);
  assert_eq!(raw_playback_after_file_delete, raw_playback_before);
  assert!(integrity_report.has_issues() == false);
  ```
- [ ] **Step 2 — RED/基线**：`cargo test -p store --test cleanup_index_work -- --nocapture`；结果可能已正确，缺索引的失败应是新增独立工作量预算，而非伪造业务失败。预算设为针对被查反向键的扫描行数不随无关行增长，不用固定硬件毫秒。
- [ ] **Step 3 — 实现**：为实测全扫且频繁的候选建前缀索引，更新objects真实清单；partial index谓词必须被调用SQL蕴含，否则不强求planner使用。files现有(library_id,media_id,file_id)保留，不能误删复合FK target UNIQUE。library_operations状态结果tie时operation_id稳定；若正常状态无需查错误operation，用CASE守卫且保留API已有返回语义。
- [ ] **Step 4 — GREEN**：上述target、`cargo test -p store --test storage_v1 --test schema_redundant_indexes --test ledger_cleanup --test data_retention`、`cargo test -p store`。逐索引A/B相同数据，记录查询收益与写入/DB体积代价；无收益或重复者不加入DDL。
- [ ] **Step 5 — Commit**：`git commit -m "perf(storage): index measured reverse-reference cleanup paths"`；测量写入现有benchmarks文档，注明请求参数、版本与未测项目。

### Task Q2: batch 末尾去重用户摘要，缓存固定热点语句（O01）

**Files:** Modify [publish.rs:77–161](<../../../crates/store/src/library_write/publish.rs#L77-L161>)、[reduce_incremental/mod.rs:141–214](<../../../crates/store/src/library_write/reduce_incremental/mod.rs#L141-L214>)、[reduce/card.rs:56–94](<../../../crates/store/src/library_write/reduce/card.rs#L56-L94>)；Create `crates/store/src/library_write/reduce_incremental/finalize.rs`；Test [file_publication.rs](<../../../crates/store/tests/file_publication.rs>)、[storage_invariants.rs](<../../../crates/store/tests/storage_invariants.rs>)、[storage_work_bounds/scan_windows.rs](<../../../crates/store/tests/storage_work_bounds/scan_windows.rs>)。

**Interfaces:** 新内部 `SummaryDirtySet = BTreeSet<(LibraryId,MediaId)>`；批内增量入口增加 `dirty:&mut SummaryDirtySet`，保持现有 `BatchSettled` 对units/media的防重复机制；`finalize_user_summaries(tx:&Connection, dirty:&SummaryDirtySet)->Result<(),StoreError>` 事务提交前每key只调用一次现有refresh。单文件入口创建本地dirty并立即finalize，不能漏掉单文件调用者。

- [ ] **Step 1 — 写等价测试**：50个文件同Media、两个版本同new unit、删最后文件、unknown coverage、同文件多集、仅NFO、present↔missing、pending→published、source升级；每组比较incremental与同事实full reduction，含当前用户和其他有事实用户摘要。保留既有 `existing_card_batch_two_versions_of_new_episode`，不宣称它当前仍红。
  ```rust
  assert_eq!(incremental.items, full.items);
  assert_eq!(incremental.units, full.units);
  assert_eq!(incremental.user_items, full.user_items);
  assert_eq!(incremental.raw_playback, before_raw_playback);
  ```
- [ ] **Step 2 — RED/基线**：`cargo test -p store --test file_publication existing_card_batch_two_versions_of_new_episode` 现有非回归；新批量工作量case先记录重复summary读算成本，再使“同key不随本批文件数重复finalize”预算失败，不能伪称原计数有错。
- [ ] **Step 3 — 实现**：逐文件保持计数/覆盖/首选源 delta 与BatchSettled，用户摘要刷新移到所有文件/units稳定后的事务末尾；full-rebuild分支也将summary刷新纳入同一个dirty流程，不能一次full又一次finalize。第一版保守对所有相关key刷新一次，不按units相等跳过，避免漏presence/source相关行为。只对测得重复prepare的固定SQL用prepare_cached，statement在写/commit前结束作用域；动态IN arity不无界塞cache。media_identity五次别名查合并一条SELECT，按refresh_target等现有身份政策选择，不改成任意一条别名。
- [ ] **Step 4 — GREEN**：file_publication/storage_invariants、`cargo test -p store --lib incremental_reduction_matches_full_recompute`、`cargo test -p store`；release scan-window记录相关user/Media数、VM/事务锁时长；RowOutcome::Unchanged不是全请求无副作用，不提前return跳NFO/probe义务。
- [ ] **Step 5 — Commit**：`git commit -m "perf(storage): finalize affected user summaries once per publication batch"`；statement缓存可独立第二个已绿提交，不把两个收益混为一个百分比。

### Task Q3a: studio 轻量投影（O02）

**Files:** Modify [library.sql](<../../../crates/store/src/schema/v1/library.sql>)、[reduce.rs](<../../../crates/store/src/library_write/reduce.rs>)、[reduce/card.rs](<../../../crates/store/src/library_write/reduce/card.rs>)、[items/hydration.rs](<../../../crates/store/src/library_query/items/hydration.rs>)、[items/page.rs](<../../../crates/store/src/library_query/items/page.rs>)；Test [adopted_filter_facts.rs](<../../../crates/store/tests/adopted_filter_facts.rs>)。

**Interfaces:** library_items新增 nullable `studio TEXT, studio_logo TEXT`，Facts/SELECT/mapper带两个Option<String>；名字映射到现有Web字段。只存卡片标量，不复制演职员/完整JSON。消费A9统一locale政策，Q4消费同一采用事实。

- [ ] **Step 1 — 写失败测试/成本基线**：多source/locale、NFO/override、刷新、merge、repair、语言切换后卡片studio/logo与有效metadata一致；清cache不改值。数据结果原本可能正确，新成本门禁要求普通页不读取source.normalized_json/details_json。
  ```rust
  assert_eq!(card.studio, expected_adopted_studio);
  assert_eq!(card.studio_logo, expected_adopted_logo);
  assert_eq!(card_after_clear_cache, card_before_clear_cache);
  ```
- [ ] **Step 2 — RED/基线**：`cargo test -p store --test adopted_filter_facts`，新投影字段/页冷数据预算在旧实现失败；不能把已有结果正确的测试假称红。
- [ ] **Step 3 — 实现**：写路径从锁外已准备的typed有效字段按相同locale/采用优先级生成标量，发布前校验捕获metadata/config/input版本；不新增事务内冷JSON解析。覆盖来源采用/NFO/人工锁/locale/merge/repair，checker纳入新字段。删除fill_studio读所有source JSON路径；favorite保留分页后有界标量补读，已有必要用户join时才投影，不为省一条SQL强制全库LEFT JOIN。
- [ ] **Step 4 — GREEN**：上述target、`cargo test -p store --test library_query_v1`、store/api独立套件；相同50项page记录JSON字节、VM与锁时长，正确性和成本分别验收。
- [ ] **Step 5 — Commit**：`git commit -m "perf(library): materialize adopted card studio fields"`。

### Task Q3b: 保持facet合同的合并聚合（O03）

**Files:** Modify [facets.rs](<../../../crates/store/src/library_query/facets.rs>)；Test [library_query_filters.rs](<../../../crates/store/tests/library_query_filters.rs>)、[facets_regression.rs](<../../../crates/api/tests/facets_regression.rs>)。

**Interfaces:** `library_facets`公开签名不变；closed-bucket每维一个多列SUM/候选存在聚合；bucket值、累计阈值、exclude-own合同不变。与Q3a无代码接口依赖，可独立审查。

- [ ] **Step 1 — 写等价测试**：每维exclude-own、无摘要默认未看、favorite仅库内有候选时出现、累计rating、空库/零候选结果固定，结果与参考fixture逐维比较；记录当前All26/Primary12条结构SQL及实际VM。
  ```rust
  assert_eq!(new_facets, reference_facets);
  assert_eq!(new_facets.total, matching_item_count);
  assert_eq!(zero_count_bucket_visibility, expected_zero_count_visibility);
  ```
- [ ] **Step 2 — RED/基线**：`cargo test -p store --test library_query_filters`；`cargo test -p api --test facets_regression`；业务结果可已绿，工作量门禁单列，不伪造结果缺陷。
- [ ] **Step 3 — 实现**：closed维每桶合为一条SQL；同without-own谓词可共扫，但候选是否出现、过滤后count、零桶保留分别表达，不简单删除外层condition。保持私有Collection用户授权；精确count允许扫描本库窄行。
- [ ] **Step 4 — GREEN**：上述targets、store/api独立套件；全部total/每桶结果等价后对同fixture比较EQP/VM/锁时长。查询条数减少不保证扫描/耗时下降，无实益保留原方案并记录测量。
- [ ] **Step 5 — Commit**：`git commit -m "perf(query): aggregate facet buckets with equivalent predicates"`。

### Task Q4: Jellyfin 词条过滤与可证实的 late materialization（O04）

**Files:** Modify [protocol/sql.rs](<../../../crates/store/src/library_query/protocol/sql.rs>)、[protocol/facts.rs](<../../../crates/store/src/library_query/protocol/facts.rs>)、[protocol/predicates.rs](<../../../crates/store/src/library_query/protocol/predicates.rs>)、[reduce/card.rs](<../../../crates/store/src/library_write/reduce/card.rs>)、[reduce_incremental/terms.rs](<../../../crates/store/src/library_write/reduce_incremental/terms.rs>)；Create `crates/store/src/library_write/reduce/protocol_terms.rs`；Test [jellyfin_sql_page.rs](<../../../crates/api/tests/jellyfin_sql_page.rs>)、[jellyfin_paged_metadata.rs](<../../../crates/api/tests/jellyfin_paged_metadata.rs>)。

**Interfaces:** `PreparedProtocolTerms { expected_metadata_revision:i64, expected_library_revision:i64, input_revisions:Vec<(String,i64)>, terms:Vec<(String,String)> }` 在锁外从已采用typed字段与同locale NFO决议准备；`sync_protocol_terms(tx:&Connection,library:LibraryId,media:MediaId,prepared:&PreparedProtocolTerms)->Result<(),StoreError>` 在写事实前验证捕获版本，再随事实同事务写现有CHECK允许的 jellyfin_genre/studio/person_id/person_name/person_tmdb_id。terms内容按本次拟发布的有效字段计算，不在sync内部解析冷JSON；并发变化则重新准备，不能发布旧词条。不新增同义字段或第二套metadata事实；协议公开接口与ID编码不变。

- [ ] **Step 1 — 写基线/结果测试**：PersonIds、GenreIds、Studio、Unicode大小写/同名但不同外部ID、库NFO覆盖与跨locale、同Media跨可见/隐藏库、mixed movie/series/season/episode、不可用源。记录精确total与全分页去重集合；同请求捕获bundled SQLite EQP/VM与source lookup范围。
  ```rust
  assert_eq!(paged_ids, expected_logical_ids);
  assert_eq!(total, expected_logical_ids.len());
  assert!(!source_ids.contains(&hidden_file));
  assert_eq!(visible_person_ids, expected_effective_person_ids);
  ```
- [ ] **Step 2 — RED/基线**：`cargo test -p api --test jellyfin_sql_page --test jellyfin_paged_metadata -- --nocapture`；将前置person/genre JSON扫描成本单独记录，不能只看page_sql。CTE未强制materialized，未测不写“每候选必执行4查询”。
- [ ] **Step 3 — 实现**：写terms与消费者同提交切换，明确逻辑person ID/genre ID生成与casefold规则、NFO/人工锁优先级，repair/full/incremental一致；过滤/授权/逻辑去重均在LIMIT前。只有不参与过滤/排序/去重的source字段可后移到页内批读，优先用有效unit.preferred_file_id但仍核对授权/版本；同一读快照避免total/page漂移。whole IsPlayed仅在与当前分集完成归约严格等价的分支使用用户摘要，不能以浏览seen口径替换协议played。
- [ ] **Step 4 — GREEN**：上述targets、`cargo test -p store --test adopted_filter_facts --test library_query_v1`、store/api独立套件；对比全部分页/total/选源ID一致。索引不能加速任意 `%substring%` 到O(logN)，overview/别名子串搜索保留必要本地谓词并披露成本，不把整简介塞热terms求假收益。
- [ ] **Step 5 — Commit**：精确词条与分页后水合可分两个已绿提交；`perf(protocol): filter adopted indexed facts before page hydration`。若VM未下降或结果不同，不采用该late物化改写，保留测量结论。

### Task Q5: 长操作与物理存储政策的有界测量决策（O06/O07）

**Files:** Modify [open.rs](<../../../crates/store/src/open.rs>)、[response_cache/mod.rs](<../../../crates/media/src/response_cache/mod.rs>)、[response_cache/queries.rs](<../../../crates/media/src/response_cache/queries.rs>)、[library_deletion/mod.rs](<../../../crates/store/src/library_deletion/mod.rs>)、[data_retention.rs](<../../../crates/store/src/data_retention.rs>)；Test [connection_policy.rs](<../../../crates/store/tests/connection_policy.rs>)、[media connection_policy.rs](<../../../crates/media/tests/connection_policy.rs>)、[library_removal_recovery.rs](<../../../crates/store/tests/library_removal_recovery.rs>)、[storage_contention.rs](<../../../crates/api/tests/storage_contention.rs>)。

**Interfaces:** 权威库显式 synchronous=FULL；cache可单独实验NORMAL并记录丢失可重抓响应的政策。不固定mmap/cache/temp_store全实例值：先按已有连接总数与进程预算测量。现有持久operation继续保存cursor/phase，删除与保留窗口使用最多100个输入实体的批次，最后校验再succeeded；单Media事实/摘要仍同事务。

- [ ] **Step 1 — 写边界测试/测量**：持长reader同时大量cache写、观察WAL与checkpoint busy；同秒大量cache entry分页/保留清理跨source cursor不漏不重；删库每批崩溃重启、10k集Media、多用户状态不丢，期间普通读不返回混合归属成功页。WITHOUT ROWID A/B仅窄terms/file_units候选，并把rowid测量工具改为真实PK后才比较。
  ```rust
  assert_eq!(all_cache_entry_ids, expected_ids_in_contract_order);
  assert_eq!(after_restart.raw_playback, before.raw_playback);
  assert!(recovered_operation.is_succeeded());
  assert_eq!(authoritative_synchronous, 2); // SQLite FULL
  ```
- [ ] **Step 2 — RED/基线**：`cargo test -p store --test connection_policy --test library_removal_recovery --test data_retention`；`cargo test -p media --test source_cache --test connection_policy`；`cargo test -p api --release --test storage_contention -- --nocapture`。性能仪器read_scope若None标Not measured，不以None相等过门禁。
- [ ] **Step 3 — 实现有界操作**：删除/retention保护检查改集合查询，probe_stage的exists走Q1索引，最多窗口内候选批量判定。大删除以持久cursor+freeze分批，cursor与本批删除同事务；无需把media merge的规范原子library阶段拆成可见半合并。慢磁盘观察依A7/A8移锁外，不只增busy_timeout。
- [ ] **Step 4 — 决策与GREEN**：FULL固定权威库；cache NORMAL/cache_size/temp_store/mmap按独立A/B单变量选择，没有净收益不改默认。WITHOUT ROWID只在体积/写成本/查询均可接受且无rowid依赖的表启用；cache两个混合方向索引暂保留，若合并必须先改并验证recent/retention同时间tie-break，不能直接删一棵。journal_size_limit仅checkpoint后保留体积目标，监控WAL峰值/长reader。运行上述targets及store/media/api独立套件。
- [ ] **Step 5 — Commit**：长操作与每项经测量采纳的物理调整分别提交；未采纳项写明证据/理由。消息 `perf(storage): bound durable cleanup and document measured connection policy`；不把性能指标改善冒称durability验证。

## 自查与交付门禁

- O01–O07各有任务；Q1不给重复索引清单，Q2保留BatchSettled，Q3区分分页后合法轻水合和冷JSON成本，Q4避免从CTE语法推断实际次数，Q5保留权威耐久性。
- 新索引/字段/type都有具体owner；增量与full/repair共享相同字段采用，private Collection不进入public terms。所有DDL/tasks串行集成，先跑schema清单测试。
- Review Focus均有对应结果断言；性能预算单独归档，主规范规模未跑时记Not run，不伪报1M验证。
- 每个实际实现变更先RED/基线后GREEN；所有crate独立与最终workspace、Web（如接口受影响）门禁结果归档。当前仅文档，所有任务保持未执行。
