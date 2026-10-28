# Large-hit Query Read Model Implementation Plan

> 历史实施计划：A/B 已落地，固定版本验收见[结果记录](../../benchmarks/2026-10-25-large-hit-query-read-model.md)；当前剩余问题见[2026-10-28 复审](../../audits/2026-10-28-storage-main-rereview.md)。下列步骤不作为新待办。

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 在保持精确 total、ACL、locale 和分页合同的前提下，将大命中查询从整卡片/JSON 的重复处理改为事务维护的精确成员、边际计数和有序取页。

**Architecture:** 在现有 library.db 中完善读模型，复用 Web terms，新增精确协议成员及边际数量，沿原事实事务差量结算。查询先收窄合法成员，再计数/取页，只有页内条目读取源和详情。默认语言作为同库权威配置，通过持久 Library operation 冻结、分批重建和发布。

**Tech Stack:** Rust workspace、rusqlite/bundled SQLite、Axum、Tokio、TypeScript/React/Vite；现有依赖，无新增外部服务。

**Spec:** [大命中查询读模型与读取隔离技术方案](../specs/2026-10-25-large-hit-query-read-model.md)。执行前必须读 Spec；本计划只覆盖子项目 A，连接隔离见[子项目 B](2026-10-25-library-read-isolation.md)。

## Global Constraints

- 保留 SQLite 五文件边界；查询投影、成员和数量放在 library.db，domain 只放类型，不引入 IO。
- 不兼容旧数据库和旧自有 API：不做导入、迁移阶梯、字段别名、转发 shim 或双读双写。
- 不修改 synchronous/cache_size/mmap/temp_store/WITHOUT ROWID，不引入 Redis、外部搜索服务或位图依赖。
- 精确 total、跨库逻辑去重、ACL、locale、稳定排序、零桶与排除自身维度语义必须保留。
- 静态图片端点保持 public；Jellyfin 播放鉴权和签发绑定保持现有合同。
- Rust 文件硬上限 800 行，生产函数与测试函数硬上限 120 行；按职责拆分。
- 每项行为先 RED→GREEN；测试使用临时数据库与 fake，不依赖 TMDB、Downloader 或 Chromium。
- 每个触及 crate 运行 cargo test -p <crate>，最终运行 cargo test --workspace；Web 改动运行 pnpm test、pnpm typecheck、pnpm build。
- 仅在当前分支提交本任务文件；不推送、不部署，不清空或替换现有实例目录。

## Review Focus

1. 同 Media 跨库、不同语言与 ACL 子集：先按库过滤再去重，Root 不泄漏或重复计数；A4/A6 的公开查询回归。
2. 默认语言修改、冻结第 100/101 条后重启：无半更新投影、旧 WriteScope 失效、错误可查询；A3 的持久操作/HTTP 回归。
3. 最后文件消失、loser 直接 DELETE 与同一 Media 多文件：不漏扣或重复计数；Integrity的全写入口屏障、损坏counter独立修复及批间重开由A2回归。
4. Unicode/同折叠标题、多键方向、limit=0、深 offset：排序与 total 不变；A6 的协议分页回归。
5. 稀疏 User、Watch-only、未知genre空命中、非整数 rating、多 resolution 与零计数候选：facets 保留精确排除自身与 favorite 规则；A5/A7 的 oracle 回归。
6. 选页后源消失：A8成功响应严格满足完整页等式，错误响应单独验证，禁止短页200。

---

## 文件结构与接口约定

全部路径相对仓库根目录；Modify 的行号是 `1085a9a` 定位提示，执行时重新定位。

| 文件组 | 职责 |
|---|---|
| `crates/store/src/schema/v1/library.sql`、`schema/v1/objects.rs`、`schema/layout.rs`、`open.rs` | library version=2、唯一 DDL、只读拒绝旧版。 |
| `crates/store/src/query_projection/{mod,types,browse,protocol,people,counts,expected,repair}.rs`（新） | 原子投影结算、只读预期投影与冻结修复；effective protocol 与 Web 桶各自保留语义。 |
| `crates/store/src/library_write/publish_batch.rs`（新） | 从已 785 行的 publish.rs 移出批次调度，首次 capture/批末 settle。 |
| `crates/store/src/metadata_language.rs`、`library_operations/default_locale.rs`（新） | 默认语言的单库配置与持久重建。 |
| `crates/store/src/library_query/members/{mod,plan,facets,user}.rs`（新） | Web 成员计划、精确数量和稀疏用户集合。 |
| `crates/store/src/library_query/protocol/{request,membership,count,page,source}.rs`（新） | 从现有 sql/facts/predicates 拆分职责，保留公开 Store 接口。 |
| `crates/store/src/library_query/page_inputs.rs`（新） | 根据已选协议引用批量读取页内 owned 事实。 |
| API `http/metadata_language.rs`、`media_server_provider/selection/request.rs`（新） | 严格语言接口及不枚举台账的范围解析。 |
| `crates/store/tests/query_model_support/mod.rs`（新） | 临时事实夹具、独立 oracle、持久操作推进；不复制生产 SQL 构建器。 |

crate-private 类型统一使用以下名称，定义位置为 `query_projection/types.rs`：

```rust
struct ProjectionKey { library_id: LibraryId, media_id: MediaId }
struct CounterMember { surface: String, dimension: String, value: String }
struct ProjectionBefore { card_exists: bool, members: BTreeSet<CounterMember> }
struct ProjectionDelta { cards: u64, inserted: u64, deleted: u64, counters: u64 }
```

ProjectionKey派生Clone/Copy/Eq/Hash，CounterMember派生Clone/Eq/Ord；domain现有ID不实现Ord，不为本方案修改ID trait。这些不是 domain IO 类型，也不对 HTTP 暴露。Spec 第 4/5 节固定字段和计数口径，不另造别名。

测试支持模块的公共测试接口由 A2 一起建立：

```rust
struct Fixture { dir: TempDir, store: Store, library: LibraryId, media: Vec<MediaId>, user: UserId }
fn movies(count: usize) -> Fixture;
fn query(f: &Fixture) -> ProtocolItemQuery;
fn run_integrity(f: &Fixture, repair: bool) -> IntegrityReport;
fn reopen(f: &mut Fixture);
```

movies 用真实 Store 发布，标题 `Movie 000000` 起，年份 2024，genre 偶数 Drama/奇数 Comedy，每个 Media 一个 Whole 文件；用户是临时合法 User。run_integrity 通过 begin/advance/get operation 公开 seam，每批最多 100，使用有限上限防测试死循环。更大 benchmark 可直接批量填原事实，但必须运行公开的当前版 repair 构建投影，建库时间不算查询时间。

## 依赖与提交边界

```text
A1 → A2 → A3（语言）→ A4（协议成员）
       └→ A5（Web成员）
A4 + A5 → A6（协议分页）
A5 → A7（facets）
A3 + A6 → A8（HTTP页内读取）
A1..A8 → A9（验收）
```

语言权威与重建先于精确协议读路径切换，避免中间提交出现配置读新值、成员仍按旧值生成。A7只依赖A5，可与协议分页独立推进；每项提交必须独立可测试，不能留下新读路径读取尚未维护的投影。

### Task A1: 新布局原子初始化与明确拒绝旧库

**Files:**
- Modify: `crates/store/src/schema/{layout.rs,v1/library.sql,v1/objects.rs}`、`crates/store/src/open.rs`。
- Test: `crates/store/tests/query_model_layout.rs`（新）、`crates/store/tests/schema_version_guard.rs`。
- Modify: `docs/deploy.md` 的布局/新目录启动段；尚不把本轮性能写为已实现。

**Interfaces:**
- Consumes: `Store::open(impl AsRef<Path>) -> Result<Store, StoreError>`。
- Produces: `schema::layout::LIBRARY_USER_VERSION: i64 = 2`；Spec 第 4 节全部新列/表/索引与 query config 单例；其他文件的版本常量保持。

- [x] **Step 1: 写公开启动回归** `opening_library_v1_is_rejected_before_other_files_change` 与 `query_layout_fresh_open_and_reopen_preserve_rows`。

```rust
let dir = tempfile::tempdir().unwrap();
let store = Store::open(dir.path()).unwrap();
assert_eq!(store.schema_versions().into_iter().find(|v| v.0 == "library").unwrap().1, 2);
drop(store);
// 在副本中将 library user_version 设回 1，记录四个文件的字节摘要。
let error = match Store::open(old_copy.path()) {
    Err(error) => error,
    Ok(_) => panic!("old library layout must be refused"),
};
assert!(error.to_string().contains("library"));
assert_eq!(hashes_after, hashes_before); // 不仅检查报错。
```

`old_copy/hash` 是该测试文件的显式夹具变量，复制关闭的临时库；不要对 data/live 操作。另删一个新增索引，要求重开拒绝且零写；空文件初始化后单例恰好一行。

- [x] **Step 2: 运行** `cargo test -p store --test query_model_layout`；预期 RED 为 library 版本仍为 1 或缺少新布局。
- [x] **Step 3: 实现布局**。增加 Spec 的五张新表（包括 Integrity 持久计数暂存）、扩展 Web terms CHECK、卡片列和四个排序索引。新列初始默认值仅服务空库创建；A2 后所有可见卡片必须结算 revision。library 的两处 preflight/init 改用独立版本常量，错误指向保留旧目录与新目录启动，不提供自动 reset。
- [x] **Step 4: 运行** `cargo test -p store --test query_model_layout --test schema_version_guard --test storage_v1`；全部 PASS，旧版副本摘要不变。
- [x] **Step 5: 提交**上述 DDL/启动/回归/部署说明：`feat(store): define query read model layout`。

### Task A2: 统一事务结算、数量差量与 Integrity

**Files:**
- Create: `crates/store/src/query_projection/{mod,types,browse,protocol,people,counts,expected,repair}.rs`、`crates/store/src/library_write/publish_batch.rs`。
- Modify: `crates/store/src/lib.rs`；`library_write/{publish.rs,reduce/card.rs,reduce_incremental/mod.rs,delete.rs,reassign.rs,rename_durable.rs}`。
- Modify: `crates/store/src/{roots.rs,media_operations/apply.rs,metadata/adopt.rs,metadata/overrides.rs,metadata/nfo_input/reconcile.rs}`；相关Media身份、Collection展示字段及Playback/favorite写入口，按受影响Library核对屏障。
- Create: `crates/store/src/library_operations/integrity_guard.rs`；Modify `library_deletion/mod.rs`及其他操作begin/advance入口，双向互斥且失败owner保留。
- Modify: Store查询scope/lifecycle门；API `http/{library,library_scan}.rs`、`media_server_provider/selection/{mod,page}.rs`；media-server `provider/mod.rs`、`routes/{catalog,shows}.rs`及全部provider/fake，随Integrity冻结引入typed 503。
- Test: `crates/api/tests/integrity_read_barrier.rs`（新）、`crates/media-server/tests/query_error_contract.rs`（新）；Store `query_model_integrity_barrier.rs`（新）。
- Modify: `crates/store/src/library_operations/{mod.rs,integrity.rs}`；Create `crates/store/src/library_operations/integrity_query.rs`，从现有 probe_media/derived_snapshot 拆出查询投影检查，mod.rs只登记模块。
- Test: `crates/store/tests/query_model_projection.rs`、`query_model_mutations.rs`、`query_model_support/mod.rs`（新）；现有 `publication_batch_work.rs`、`library_integrity_facts.rs`。

**Interfaces:**
- Consumes: A1 DDL；现有 `reduce_library_media(tx: &Connection, LibraryId, MediaId)`、`BatchSettled`、WriteScope/FilePublication。
- Produces: `query_projection::capture(tx: &Connection, key: ProjectionKey) -> Result<ProjectionBefore, StoreError>`。
- Produces: `query_projection::settle(tx: &Connection, key: ProjectionKey, before: ProjectionBefore) -> Result<ProjectionDelta, StoreError>`，`remove` 同签名。
- Produces: `query_projection::expected_projection(tx: &Connection, key: ProjectionKey) -> Result<ExpectedProjection, StoreError>`；只读原始事实，不读实时counter作为期望。`ExpectedProjection`在expected.rs定义为私有owned结果，字段职责见Spec5.1，普通结算与修复共享派生规则。
- Produces: `query_projection::repair_projection(tx: &Connection, owner: &IntegrityOwner, key: ProjectionKey, expected: &ExpectedProjection) -> Result<(), StoreError>`；只替换派生行，不修改live counter。`IntegrityOwner`在integrity_guard.rs定义，续跑时在当前写事务核验operation ID/Library/冻结revision后产生。
- Produces: `ensure_integrity_writable(tx: &Connection, libraries: &[LibraryId]) -> Result<(), StoreError>`（crate-private）；全部受影响库在写前同事务核验，拒绝任一活动/失败未解冻的Integrity owner。普通写入口没有跳过检查的参数；repair专用入口须持有效owner。
- Produces: `StoreError::QueryUnavailable { library_id: LibraryId }`；media-server `QueryError::{Unauthorized,Unavailable,Internal(String)}`。`MediaServerProvider::query_visible_items(&self, user_id: UserId, query: ServerItemQuery) -> Result<ServerItemPage, QueryError>`改为必需实现，删除legacy默认转发，所有调用者/fake同批切换；A3复用，B3再携带原凭据。
- Produces: `IntegrityTotals.query_projection_mismatches: usize`；`IntegrityReport.query_projection_media: Vec<MediaId>`（最多100项）及 `query_count_mismatch: bool`。报告本身属于一个Library，库级counter不一致计入该类并置bool，不伪造Media ID。

- [x] **Step 1: 写持久行为回归** `integrity_detects_and_repairs_query_projection_corruption`。发布 4 张卡片后人为将 Drama 边际数量改为 999、删除一个协议成员、改错一个 query revision，执行公开 check/repair。

```rust
let f = movies(4);
corrupt_query_projection(&f); // 本测试私有注入，明确改上述三个持久事实。
assert!(run_integrity(&f, false).totals.query_projection_mismatches > 0);
run_integrity(&f, true); // repair报告可以保留本次发现，随后重新check确认已清除。
assert_eq!(run_integrity(&f, false).totals.query_projection_mismatches, 0);
assert_eq!(stored_browse_count(&f, "genre", "Drama"), 2);
```

补充 `integrity_repairs_undercount_with_extra_members`：分别注入counter=0、少计或缺失，配合多余genre成员；check须报告且不修改派生行，repair须完成并通过重开后的完整oracle。禁止仅覆盖999这种高计数。

补充 `integrity_blocks_changes_to_already_staged_media`：201个Media，完成第一批100个后，用公开Store入口对已扫描Media执行adopt/override/scraped display/NFO、Playback/favorite、Media merge/删除及相关展示更新；逐项断言Protected且事实/实时计数/暂存均零写。重开继续修复，最终计数符合原事实；解冻后重试变更成功且新计数正确。另测roots/locale/default_locale/删除/第二个Integrity不同开始顺序互斥（default_locale用例在A3补齐），失败操作不丢owner。

计数读取只用于验证持久修复结果，不断言 SQL 文本或 helper 调用。另写 `last_file_merge_and_cross_library_moves_conserve_counts`：两个 Media 合并成一个后 total=1；最后文件删除后 total=0；跨库移动 source=0、target=1。对应 Spec 覆盖清单每类入口至少一项公开行为。

- [x] **Step 2: 运行** `cargo test -p store --test query_model_projection --test query_model_mutations --test query_model_integrity_barrier`；预期 RED 为损坏计数不可修复、批间写入未被拒绝或新接口缺失。
- [x] **Step 3: 实现 capture/settle/remove**。先捕获 CounterMember 集合，再改事实，最后对 distinct 成员做差量，协议输入遵守 Spec 3.2。先从 publish.rs 移出批次职责再加 hook；每个 dirty key 首次修改前 capture，批末 settle 一次。Media merge 的 loser 必须在直接 DELETE 前 remove；FK 不代替扣减。被删卡片没有 query revision 可写，只有计数/成员清理。
- [x] **Step 4: 实现独占屏障及独立预期投影**。check/repair开始时单IMMEDIATE事务登记owner、递增config_revision并置reindexing；按Spec5.4检查全部写入口与操作双向互斥，不能只依赖文件WriteScope。`expected_projection`只读原始事实，check与repair不再通过正常reducer的savepoint扣减损坏counter。正常settle继续严格非负校验；repair凭owner只修派生行。
- [x] **Step 5: 实现分批累计与恢复**。从原始事实与反向孤儿枚举，逐Media比较预期投影；check和repair均将预期distinct成员累计到A1暂存表，与该批派生修复/游标推进同事务提交，崩溃重跑不重复累计。repair末批验证owner/revision后同事务替换计数、清暂存和恢复ready；check只比较/报告并清暂存解冻。执行失败保留owner与游标，接入startup/retry；末批故障注入证明替换、状态和暂存全部回滚后仍可恢复。扩展has_issues与真实总数，样本上限100，禁止每修1个Media全库GROUP BY。
- [x] **Step 6: 写并运行** `one_hundred_file_window_settles_one_card`、`unchanged_members_and_playback_do_not_rewrite_static_counts`。用 SQLite authorizer/trace 故障注入让 settle 失败，断言文件事实/成员/count/revision 全回滚；观测写放大仅因本 bug 使用 trace。`cargo test -p store --test query_model_projection --test query_model_mutations --test query_model_integrity_barrier --test publication_batch_work --test library_integrity_facts` 全 PASS。
- [x] **Step 7: 写并实现冻结读门回归**。真实Router对显式库/包含该库的Root返回typed503，未授权库不泄漏状态；check/repair期间不能读取部分投影，成功解冻后读恢复。`cargo test -p api --test integrity_read_barrier`、`cargo test -p media-server --test query_error_contract` RED→GREEN，并运行`cargo test -p store`、`cargo test -p api`、`cargo test -p media-server`。此门在A2交付，不等待A3。
- [x] **Step 8: 提交**本任务文件：`feat(store): settle query projections with library facts`。

### Task A3: 默认语言同库权威与持久重建

**Files:**
- Create: `crates/store/src/metadata_language.rs`、`crates/store/src/library_operations/default_locale.rs`、`crates/api/src/http/metadata_language.rs`。
- Modify: `crates/store/src/{open.rs,library_operations/mod.rs,library_operations/locale.rs,library_query/details/metadata.rs,library_write/reduce.rs,query_projection/protocol.rs}`；统一借用library连接的语言读取，以及查询scope的lifecycle门。
- Modify: `crates/api/src/{http/settings.rs,http/mod.rs,http/routes/admin.rs,main.rs,settings_keys.rs,storage_operations.rs,scrape_metadata/work.rs}` 及 `lib.rs` 模块树。
- Modify: `crates/store/src/lib.rs`、`crates/api/src/media_server_provider.rs`、`media_server_provider/selection/{mod,page}.rs`、`crates/media-server/src/{provider/mod.rs,routes/catalog.rs,routes/shows.rs}`；复用A2已交付的typed查询不可用错误及provider传播链，补充语言重建场景。
- Create: `web/lib/api/metadata-language.ts`、`web/components/metadata-language-setting.tsx`；Modify `web/components/metadata-settings-section.tsx`、`web/lib/api/metadata.ts`。
- Test: `crates/store/tests/query_model_locale.rs`、`crates/api/tests/metadata_language_operation.rs`、`web/test/metadata-language-contract.test.mjs`（新）。

**Interfaces:**
- Produces: `MetadataLanguageState { language: String, revision: i64, operations: Vec<MetadataLanguageOperation> }`，operation 含 `library_id: LibraryId, operation_id: String, state: String, error: Option<String>`，定义在 metadata_language.rs 并由 Store re-export。
- Produces: `Store::metadata_language(&self) -> Result<MetadataLanguageState, StoreError>`。
- Produces: `Store::change_metadata_language(&self, expected_revision: i64, language: &str, operation_id: &str, now_ms: i64) -> Result<MetadataLanguageState, StoreError>`；子操作 ID 用 UUID5(parent operation id + LibraryId)，重试不重复登记。
- Consumes: A2 settle；现有 `advance_library_operation(&self, operation_id: &str, max_media: usize, now_ms: i64)`。
- Consumes: A2的 `StoreError::QueryUnavailable`、`QueryError::{Unauthorized,Unavailable,Internal(String)}` 和必需实现的typed `query_visible_items`；将同一503读门扩展到语言重建，B3后续增加原凭据与deadline。

- [x] **Step 1: 写重启/冲突/读屏障回归** `default_locale_change_freezes_all_libraries_atomically_and_resumes`。发布 201 个 Media，change 后只推进 100，reopen 后继续 100/1 到 ready，检查分类/语言及 total。旧 WriteScope 发布拒绝；显式 display_locale 不被改写。

```rust
let changed = f.store.change_metadata_language(0, "en-US", "locale-change", 1).unwrap();
assert_eq!(changed.revision, 1);
assert!(query_target_library(&f).is_err()); // API 层对应 503，不能返回旧/部分 total。
let target = changed.operations.iter().find(|op| op.library_id == f.library).unwrap().operation_id.clone();
advance_one(&f, &target, 100);
reopen(&mut f);
for op in &changed.operations { finish_bounded(&f, &op.operation_id); }
assert_eq!(f.store.metadata_language().unwrap().revision, 1);
```

其中advance_one/finish_bounded是该测试文件用现有advance API定义的有限夹具。实际测试先按f.library选出对应operation，不依赖Vec顺序；空默认库的子操作也必须推进完才能测试后续全局变更。再测任何Library已有操作时change返回Protected，单例/全部库revision均未改；无变化不新增子操作；空语言恢复未设置；新建库按当前revision正常写入。

- [x] **Step 2: 运行** `cargo test -p store --test query_model_locale`；预期 RED 为默认语言无同库操作/无读屏障。
- [x] **Step 3: 实现 Store 配置与 default_locale 操作**。按Spec5.3单IMMEDIATE事务冻结与登记；ReconfigurationPlan增加 `default_config_revision: Option<i64>`，default_locale子操作固定目标revision，不修改display_locale。子ID用UUID5(NAMESPACE_URL, `default-locale:{operation_id}:{library_id}`)，GET按当前revision返回这些持久操作；纳入dispatch/retry/startup恢复和最终ready验证。普通显式locale重配置也必须满足新投影revision门；复用A2操作owner互斥，补齐default_locale与Integrity双向开始/失败重试回归。Root若目标范围含可见重建库则Protected；不要在query授权阶段提前过滤掉它而返回少算的total。
- [x] **Step 4: 写 HTTP/Web 合同 RED**：旧 `/settings/metadata` 含 language 和 API key 时400且key未写；新PATCH缺字段/null/未知字段400，revision409，变化202且能GET未完成状态。显式库与Root协议查询在目标库重建中返回503，不漏成200空页或500。实现strict DTO/路由、消费者与Web设置行；通用KV对metadata.language拒绝，main Catalog/详情链统一读新权威。将typed QueryUnavailable传播为QueryError::Unavailable，不用字符串前缀识别。环境固定credential行为保留。
- [x] **Step 5: 运行** `cargo test -p store --test query_model_locale --test library_reconfiguration`、`cargo test -p api --test metadata_language_operation --test storage_operations_runtime`、`cargo test -p media-server`，以及 `cd web && pnpm test && pnpm typecheck && pnpm build`；全部PASS。用rg确认旧key无生产读写消费者，只有拒绝逻辑和历史测试保留字面值。
- [x] **Step 6: 提交**：`feat(library): publish locale changes through durable rebuilds`。

### Task A4: 协议精确分类改用结构化成员

**Files:**
- Create: `crates/store/src/library_query/protocol/request.rs`、`membership.rs`。
- Modify: `crates/store/src/library_query/protocol/{mod.rs,facts.rs,predicates.rs,scope.rs,sql.rs}`、`crates/store/src/protocol_index.rs`、`crates/store/src/schema/v1/library.sql`；停止预筛维护后同步移除退役protocol term_kind，不保留新旧查询分支。
- Test: `crates/store/tests/query_model_protocol.rs`（新）；`crates/store/src/library_query/protocol/query_model_faults.rs`（新，内部SQLite故障注入）；`crates/api/tests/jellyfin_sql_page.rs`、`jellyfin_paged_metadata/scope.rs`。

**Interfaces:**
- Consumes: A2 的 library_protocol_terms/people 与规范值；当前 ProtocolItemQuery。
- Produces: `pub(super) fn normalize(request: &ProtocolItemQuery) -> Result<NormalizedProtocolRequest, StoreError>`；类型在 request.rs，包含原 request clone 及去重规范化 genre/studio/person keys，不修改搜索/排序意义。
- Produces: `pub(super) fn exact_predicates(request: &NormalizedProtocolRequest, scope: &Scope, bindings: &mut Bindings) -> Result<String, StoreError>`；只对有 SearchTerm 的分支请求相应冷字段。

- [x] **Step 1: 写公开结果与读取工作回归** `effective_locale_genre_does_not_read_each_candidate_metadata`。同 Media 的 zh-CN/en-US 分别只有 Drama/Comedy，NFO 与 adopted 独立选择；两个 Library 的不同 locale 结果分别断言，再组合 Root 断言过滤后去重。为 400 个 Media 填入 8KiB JSON，以 observer/trace 的完整读取统计确认 genre-only 页不会读取 metadata 表；不把 wall-clock 当单元门。

```rust
assert_eq!(ids(query_zh_drama), expected_zh_ids);
assert_eq!(ids(query_en_drama), expected_en_ids);
assert_eq!(root_page.total, expected_union.len() as u64);
assert_eq!(page.total, expected_total);
```

内部故障测试通过SQLite authorizer禁止读取media_metadata/library_metadata_inputs/media_episodes，再调用公开Store协议查询；genre/studio/person无SearchTerm仍须成功且结果正确。有SearchTerm的对照必须触发受控读取错误。authorizer仅为故障注入，不断言SQL文本，也不新增生产测试专用公开API。

另写 `scoped_person_aliases_preserve_name_or_tmdb_matching`：同名不同 TMDB、不同名同 TMDB、角色冲突和一个未授权库中的 ID 分别对照基线；Unit 分类仍 Work、Unit search 仍 Episode。

- [x] **Step 2: 运行** `cargo test -p store --test query_model_protocol`；预期 RED 为缺新 work 字段/当前冷读取不为 0，而非无关编译错误。
- [x] **Step 3: 切换精确谓词**。只从授权 Scope 内成员解析请求，删除 genre/studio/person 的 all-locale prefilter→逐条 JSON 精确匹配链。people 保留两步 alias 展开，不直接 wire_id 相等；search 专用路径保留现有逐字段 substring。保留 wire ID 工具函数，移除不再调用的预筛维护代码，其他调用者先 rg 核对。
- [x] **Step 4: 运行** `cargo test -p store --test query_model_protocol --test protocol_index` 与 `cargo test -p api --test jellyfin_sql_page --test jellyfin_paged_metadata`；全部 PASS。固定 1000 Movie、增加 TV 且都带同名 genre，工作量不随 TV 成员线性增长。
- [x] **Step 5: 提交**：`perf(store): query exact scoped protocol memberships`。

### Task A5: Web 精确成员计划与数量策略

**Files:**
- Create: `crates/store/src/library_query/members/{mod,plan,user}.rs`。
- Modify: `crates/store/src/library_query/{mod.rs,items/query.rs,items/page.rs}`。
- Test: `crates/store/tests/query_model_counts.rs`（新）、`query_work.rs`；复用并运行既有 `library_query_filters`。
- Reuse: `items/filters.rs` 的验证/精确谓词；组合与残余条件新增回归集中在 `query_model_counts.rs`，不重复修改既有过滤用例。

**Interfaces:**
- Produces: `BrowseMemberPlan`，定义于 members/plan.rs，持有绑定参数、成员 SQL、残余条件以及 `CountStrategy::{Marginal,ExactMembers}`；不持 Connection。
- Produces: `plan_browse(library: LibraryId, user: Option<UserId>, filter: &BrowseFilter) -> Result<BrowseMemberPlan, StoreError>`。
- Produces: `count_browse(tx: &Connection, plan: &BrowseMemberPlan) -> Result<u64, StoreError>`。
- Consumes: A2 browse 成员与边际数；现有 ItemQuerySql/order/keyset 合同。

- [x] **Step 1: 写精确结果与增长回归** `unfiltered_and_single_genre_counts_use_marginals`，比较 400→4000 卡片的无筛选/Drama count，返回 400/200 与 4000/2000，VM 差 <2000；固定查询/页大小，不含 seed 工作。

```rust
assert_eq!(plain.total, Some(4000));
assert_eq!(drama.total, Some(2000));
assert!(large.vm_steps < small.vm_steps + 2000);
assert_eq!(combined.total, Some(reference_ids.len() as u64));
```

再测 OR genre 重叠不重复、genre+country 必须求交、min_rating=7.5 不近似、无 User 情况不读取用户摘要。fixture 不在 400→4000 之间改动已存在标题的排序口径。

- [x] **Step 2: 运行** `cargo test -p store --test query_model_counts`；预期 RED 为旧 exact count 工作量线性增长。
- [x] **Step 3: 实现成员/计数选择**。充分条件完全满足才读边际；否则精确集合求交。无 user 条件且无 LastPlayed/unwatched_first 排序时移除 user JOIN。将 item total/page 放入同一显式读事务；不得分别读取并声称同一快照。
- [x] **Step 4: 更新并运行** `cargo test -p store --test query_model_counts --test query_work --test library_query_filters --test library_query_v1`。替换旧“count 必须 > page 两倍”的性能反断言，改为增删事实后 total 与完整 request stats 均重新反映当前调用，保持 observer 隔离测试。
- [x] **Step 5: 提交**：`perf(store): count browse memberships from exact projections`。

### Task A6: 协议 count/page/source 分离

**Files:**
- Create: `crates/store/src/library_query/protocol/{count,page,source}.rs`。
- Modify: `crates/store/src/library_query/protocol/{mod,sql,scope,order,membership}.rs`。
- Test: `crates/store/tests/query_model_paging.rs`（新）、`crates/api/tests/jellyfin_sql_page.rs`、`protocol_paging_totals.rs`。

**Interfaces:**
- Consumes: A4 normalized request/exact membership、A5 CountStrategy、A1 排序列。
- Produces: `ProtocolReadPlan { count_strategy: CountStrategy, page_strategy: PageStrategy }`，`PageStrategy::{IndexedSingleLibrary,GeneralExact}`，定义于 protocol/page.rs。
- Produces: `pub(crate) fn select_protocol(tx: &Connection, authorized: &HashSet<LibraryId>, request: &ProtocolItemQuery) -> Result<ProtocolItemPage, StoreError>`，由 protocol/mod.rs 导出给后续 B 查询核心。
- 保留 `Store::protocol_item_page` 与 `protocol_item_page_with_stats` 的公开签名及返回类型。

- [x] **Step 1: 写公开分页与工作量 RED** `single_library_first_page_does_not_sort_or_window_the_entire_hitset`。400→4000 卡片，limit100、无筛选和100%命中 genre，total 精确且第一页与 oracle 相同；完整 VM 增量 <2000、排序工作不随命中线性增长。此任务新增 `QueryWorkStats.sort_operations: u64`，从SQLite StatementStatus::Sort累计，普通请求不安装trace。
- [x] **Step 2: 写排序/空页断言**。数据 `A/a/Ä/ä` 配不同 AddedAt；Name+DateCreated 必须先比较第二显式键，原串平局随后。默认排序、Name ASC/DESC、Mixed/Unit、limit0/offset越界逐项断言完整有序 ID；同一 Media 两库不同 genre 的 Root 先过滤后去重。

```rust
assert_eq!(page.items.iter().map(|r| r.item.clone()).collect::<Vec<_>>(), expected_ids);
assert_eq!(zero_limit.items.len(), 0);
assert_eq!(zero_limit.total, expected_total);
assert_eq!(past_end.total, expected_total);
assert_eq!(selected_source, newest_revision_then_smallest_file_id);
```

- [x] **Step 3: 运行** `cargo test -p store --test query_model_paging`；预期 RED 为当前全命中窗口/排序工作量。
- [x] **Step 4: 实现 select_protocol**。同一事务先 count，再 indexed/general page，再页内选源；单库不去重，多库在过滤后去重。不要将所有请求强塞常用索引；通用多键保持精确执行。删除 COUNT OVER 和空页重跑 count 分支，source_expr 原规则搬到 source.rs。
- [x] **Step 5: 运行** `cargo test -p store --test query_model_paging --test query_model_protocol --test query_work`、`cargo test -p api --test jellyfin_sql_page --test protocol_paging_totals --test jellyfin_paged_metadata`；全部 PASS。
- [x] **Step 6: 提交**：`perf(store): separate protocol cardinality from ordered pages`。

### Task A7: facets 边际读取与排除自身集合

**Files:**
- Create: `crates/store/src/library_query/members/facets.rs`。
- Modify: `crates/store/src/library_query/facets.rs`、`facets/matrix.rs`（删除退役全卡片计划）、`members/user.rs`。
- Test: `crates/store/tests/query_model_facets.rs`（新）、`crates/api/tests/facets_regression.rs`、`management/library_facets_and_missing.rs`。

**Interfaces:**
- Consumes: A5 BrowseMemberPlan；现有 FacetRequest/LibraryFacets、Dimension 候选/label/rank。
- Produces: `pub(crate) fn read_facets(tx: &Connection, request: &FacetRequest) -> Result<LibraryFacets, StoreError>`；单个外层读事务覆盖全部 SQL。

- [x] **Step 1: 写独立小数据 oracle** `facets_match_reference_for_every_selected_dimension`。固定 12 张事实卡片：genre多值、国家、多 resolution、rating=7.4/7.5/8、无语言、missing/unscraped、稀疏收藏/seen/played；测试层从显式期望事实按 Spec 公式穷举筛选，逐桶比较 primary/all。

```rust
assert_eq!(actual.total, expected.total);
assert_eq!(actual.groups, expected.groups);
assert_eq!(bucket(&actual, "decades", "1990s").count, 0);
assert!(actual.groups["watch"].iter().any(|v| v.value == "favorite" && v.count == 0));
```

favorite 断言使用“库内有收藏、被其他条件排除”的夹具；无任何收藏的对照必须不返回该候选。

增加 `watch_only_filters_static_buckets`：Drama/Comedy各一条且只收藏Drama，watch=favorite的total=1、Drama=1、Comedy=0；Watch自身桶仍按排除本维规则计算。played/unwatched及无用户也逐项对照oracle。增加 `empty_selected_genre_preserves_own_dimension_counts`：选择不存在的genre后total=0、其他维计数为0，genre自身Drama/Comedy仍各1；共同条件为空时所有计数为0，但来自未过滤U的动态候选仍保留。

- [x] **Step 2: 写工作量 RED** `unfiltered_facets_read_counts_not_all_cards`：400→4000 卡片、固定相同桶，VM 增量 <2000；filtered 100%/50%/空集均通过冷表authorizer故障门。运行 `cargo test -p store --test query_model_facets`，旧全量 cards 计划必须失败工作量门。
- [x] **Step 3: 实现 read_facets**。只有无共同条件且全部维度（含Watch）无筛选才直接读静态边际；Watch-only也走集合交集，静态桶保留Watch条件，Watch自身排除它。有筛选共享前缀/后缀集合，候选全集独立；单维空命中不短路该维自身桶。任意rating留作精确残余条件，不得将marginal相乘推导组合数量。
- [x] **Step 4: 运行** `cargo test -p store --test query_model_facets --test query_work --test library_query_filters` 与 `cargo test -p api --test facets_regression --test management --test facets_budget`。用并发第二连接与同步屏障证明多条 SQL 同快照；删除 `statements == 1` 绑定，保留真实工作量与结果检查。
- [x] **Step 5: 提交**：`perf(store): compute facets from scoped member sets`。

### Task A8: HTTP 协议真正先取页后读事实

**Files:**
- Create: `crates/store/src/library_query/page_inputs.rs`、`crates/api/src/media_server_provider/selection/request.rs`。
- Modify: `crates/store/src/library_query/mod.rs`；API `media_server_provider/selection/{mod,page,scope,light}.rs`、`read_snapshot.rs`。
- Test: `crates/api/tests/query_page_materialization.rs`（新）、`jellyfin_paged_metadata/{paged,scope}.rs`、`storage_query_scale.rs`。

**Interfaces:**
- Produces: `pub(crate) fn resolve_page_request(store: &Store, user: UserId, query: &ServerItemQuery) -> Result<Option<ProtocolItemQuery>, String>`；无页外 coverage 枚举。
- Produces: `Store::protocol_page_inputs(&self, user: UserId, references: &[ProtocolItemRef]) -> Result<ProtocolPageInputs, StoreError>`。
- `ProtocolPageInputs` 定义在 page_inputs.rs：`files: HashMap<LedgerId, LibraryFile>`、`media: HashMap<MediaId, Media>`、`coverage: Vec<FileCoverage>`、`user_items: Vec<PlaybackUnitSnapshot>`；只含请求引用必需的源/逻辑单元。Series 的总量另从既有 summary 读聚合标量，不把所有 episode 加入该 Vec。
- Consumes: A6 ProtocolItemPage/physical source revision，现有 heavy_snapshot 页内展示合同。
- Produces: 此任务增加 `QueryWorkStats.hydrated_rows: u64`，累计实际页内事实materialize行数；计数范围在测试中同时包含源、coverage、Media和User状态，不能直接设为limit。

- [x] **Step 1: 写真实 Router 回归** `http_page_does_not_load_unselected_ledger_or_metadata`：1000与10000个 Media，limit10，观察已有 candidate/heavy 计数；页内事实数随10个引用而不是库大小增长。页外坏JSON/坏路径不导致本页失败；权限、Folder、未知父级行为保留。
- [x] **Step 2: 写故障回归** `selected_reference_failure_never_returns_a_short_success_page`。在选页后通过同步 seam 替换/删除选中源，结果为有界重试后完整页或明确错误，不能跳过条目后200。正常 `items.len() < total` 是分页，不应产生错误日志；成功响应严格断言 `items.len() == min(limit, total.saturating_sub(offset))`（同一数值类型），不能使用允许空页通过的 `<=`；重试后用最终响应的total，明确错误响应另行断言。覆盖limit=0、offset越界、末页及非末页少一条。
- [x] **Step 3: 运行** `cargo test -p api --test query_page_materialization`；预期 RED 为当前 scope先读取完整库/选中坏引用被continue。
- [x] **Step 4: 实现轻量解析与页内批量读取**。库/根/typed ID/genre/studio 只做必要分类查询；SQL选页后按固定源 ID/逻辑键读取事实。正常分页不调用 full ledger candidates/read_scope；NextUp/Resume 显式保留其独立入口。FileVersion 和 Logical Item 必须与 reference 对齐，不能回退任意当前文件。
- [x] **Step 5: 运行** `cargo test -p api --test query_page_materialization --test jellyfin_paged_metadata --test storage_query_scale --test jellyfin_sql_page`；全部 PASS，日志不会因 total大于limit误报。
- [x] **Step 6: 提交**：`perf(api): materialize only selected protocol page facts`。

### Task A9: 完整测量、容量门与文档切换

**Files:**
- Modify: `crates/store/src/library_query/items/stats.rs`、`stats/trace.rs`、`crates/store/tests/{query_work.rs,query_work_perf.rs,query_work/support.rs}`。
- Create: `crates/store/tests/query_model_perf.rs`；`crates/api/tests/query_http_perf.rs`；`docs/benchmarks/2026-10-25-large-hit-query-read-model.md`。
- Modify: `docs/database/{README,library}.md`、`docs/api-contracts/self.md`、`docs/deploy.md`、ADR-0008、本计划状态；保留历史测量版本。

**Interfaces:**
- Consumes: 全部现有 Store 查询和 with_stats；A2 Integrity 修复；A8 真 HTTP 路径。
- Consumes: A6 的 `QueryWorkStats.sort_operations: u64`；A8首次使用前增加 `hydrated_rows: u64`，仅累计实际materialize的页内事实行，不能估算成limit或total。VM/statement/sort取SQLite真实状态，实际逻辑命中数来自精确total；不伪造SQLite内部候选行/冷表物理访问计数。普通请求无observer时不安装trace。

- [x] **Step 1: 固定同形状基线与期望**。在独立临时 checkout/目录运行1085a9a的原 harness，固定确定性 ID、事实生成 seed、SQLite版本、机器和构建；A版同样原始事实、通过官方当前版repair生成投影。不在基线库上改旧布局继续跑A。本任务扩展矩阵，统计字段在A6/A8首次使用时随该任务实现，不能让上游等待未来接口。
- [x] **Step 2: 运行独立 oracle 与错误隔离**。扩展 query_work 的跨线程/嵌套/错误清理，确认 page/count/辅助读取全部累计，新的counter快速路径没有漏统计。普通查询结果与with_stats完全相同。
- [x] **Step 3: 执行 release 门** `PERF_SIZES=5000,20000,50000,100000 cargo test --release -p store --test query_model_perf -- --ignored --nocapture`；每形状5预热+50计时，输出p50/p95/max/首轮、完整工作量。按 Spec 9.1 目标判定，不用减少字段/近似total过门。
- [x] **Step 4: 执行 HTTP/写入门** `cargo test --release -p api --test query_http_perf -- --ignored --nocapture`，fake Catalog/Probe/Downloader、不启动Chromium；分别报告Store、选页、页内facts、冷详情、序列化和端到端。保留100文件/100actor写放大回归，记录DB/WAL大小和发布耗时；容量组合与NAS未跑的项写Not run。
- [x] **Step 5: 执行完整门** `cargo test -p store`、`cargo test -p api`、`cargo test -p media-server`、`cargo test --workspace`、`cd web && pnpm test && pnpm typecheck && pnpm build`；新增Rust文件/函数尺寸与format检查，确认所有当前消费者切换且无legacy路径。失败先定位，不反复无依据重跑。
- [x] **Step 6: 更新文档并提交**：只有实际完成才勾选任务/写实测。更新唯一DDL字典及新API/503/语言操作说明，旧报告加后续链接，不覆盖历史数据。提交 `perf(storage): verify large-hit query read models`。

实施中先发现 A7 filtered facets 的 materialized ID 集合嵌套扫描；精确 `INTERSECT` 与共享分组后，100k Drama/三维仍未达到 §9.1。最终按[Spec §6.3](../specs/2026-10-25-large-hit-query-read-model.md#63-facets)的全集/空桶集合恒等式，仅对可证明的桶读取边际或精确 eligible 数量，部分桶继续精确交集。最终同形状四尺寸 release 的命名目标全部通过；失败迭代和完整数值保留在[测量记录](../../benchmarks/2026-10-25-large-hit-query-read-model.md)。独立容量与现场项未跑，不纳入本机总门通过的验收声明。

## 自审与执行交接

- 需求 R01–R15 与任务映射见 Spec 第11节；R10 的连接隔离明确由 B 承担，A 完成不声称无锁竞争。
- 本计划所有新增签名在上游 Interfaces 定义；A6/A8 的观察字段随首次使用实现，A9再扩展容量矩阵。
- Review Focus 的六类条件均有明确归属回归；功能 oracle 不复用生产筛选/计数代码。2026-10-25 review 新增的 A2 屏障/损坏计数、A7 Watch-only/空集合、A8 严格页完整性回归已由 A 实施；B 的认证前 deadline/读隔离已由 B1–B4 实施并通过独立审查与局部门；B5 指标、WAL/取消及实测进度见[隔离记录](../../benchmarks/2026-10-25-library-read-isolation.md)，最终共享总门与 B5 完整门均已于 2026-10-26 通过。
- A1–A9 与 B1–B5 均已实现、独立审查并完成最终本地验证（2026-10-26）。共享完整门：Store **607 passed / 0 failed / 7 ignored**，API **1668 passed / 0 failed / 8 ignored**，media-server **43 passed / 0 failed / 0 ignored**，workspace **2907 passed / 0 failed / 19 ignored**。format/尺寸/文档链接检查通过；Web 自 A3 后无改动，沿用 848 测试/typecheck/build 通过记录。完整门与日志 hashes 见[最终验证](../../benchmarks/2026-10-26-library-read-isolation-verification.json)；容量/现场未跑项继续按实测报告列明。本轮未执行实例切换。
