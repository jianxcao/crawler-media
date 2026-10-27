# Library Read Isolation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 使昂贵 Library 查询在有界只读连接上执行，减少其对扫描、Playback 和其他 writer 操作的 Store Mutex 阻塞。

**Architecture:** 短 Store 锁准备 owned 授权上下文，释放锁后由独占 read lease 在单一 library.db 快照执行，归还 lease 后短锁重验。沿用现有精确查询核心，连接池不初始化数据库、不获取第二个 writer 租约、不复制业务查询实现。

**Tech Stack:** Rust、rusqlite READ_ONLY/WAL/progress handler、parking_lot、Tokio blocking budget、Axum；现有依赖。

**Spec:** [大命中查询读模型与读取隔离技术方案](../specs/2026-10-25-large-hit-query-read-model.md)，特别是第7节。推荐在[子项目 A](2026-10-25-large-hit-query-read-model.md)完成后执行；B 的验收结果单独记录，不能用增加并发掩盖慢 SQL。

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

1. 排队后角色降级、账号禁用或凭据撤销：旧授权结果不能发布；B2/B4 的同步屏障回归。
2. 读期间 Library ACL/locale/freeze 改变：不得返回部分 total，最多一次重试；B2 的快照/指纹回归。
3. deadline 到达后 worker 仍运行：真实 worker 退出前 lease/permit 不释放；B1/B4 的超时复用回归。
4. 上一请求迟到取消、panic 或 SQL 错误：回调/事务清理，不中断下一租用；B1 的连接复用回归。
5. 停机、文件身份变化、旧快照阻塞checkpoint：安全排空且不删WAL/SHM；写者租约失效不破坏既有只读服务；B4/B5的生命周期与WAL观测。
6. writer先持锁：认证/设备凭据绑定/prepare/validate不阻塞Tokio线程，30秒从认证前开始且重试不重置；B3/B4的Router回归。

---

## 文件与类型边界

| Create/Modify | 职责 |
|---|---|
| Create `crates/store/src/library_read/{mod,context,pool,lease,query}.rs` | 准备/重验协议，readonly 连接及有界生命周期。 |
| Modify `crates/store/src/{lib.rs,open.rs}` | 只导出小接口；复用纯 UDF 注册，不调用写入连接 policy。 |
| Modify `crates/store/src/library_query/{items/page.rs,facets.rs,protocol/mod.rs,page_inputs.rs}` | 提取接收借用 Connection 的查询核心；Store 和 Reader 共用。 |
| Create `crates/api/src/library_read_work.rs` | 从认证前传入的deadline、有界Store锁等待、blocking调度、重验、错误与日志；B3先建立共享Store阶段执行器。 |
| Modify API `management/state.rs`、`http/library.rs`、`http/library_scan.rs`、`media_server_provider/selection/{mod,page}.rs` | 持有共享 pool，并将昂贵查询移出 writer Mutex。 |

所有新类型在 `library_read/mod.rs` re-export：

```rust
pub enum LibraryReadRequest {
    Browse(LibraryQuery), Facets(FacetRequest), Protocol(ProtocolItemQuery),
}
pub enum LibraryReadResult {
    Browse { page: ItemPage, facts: HashMap<MediaId, LibraryItemFacts>, stills: Vec<LibraryStill> },
    Facets(LibraryFacets),
    Protocol { page: ProtocolItemPage, inputs: ProtocolPageInputs, series: HashMap<MediaId, ProtocolSeriesScalar> },
}
pub enum LibraryReadError {
    Busy, Closed, Deadline, Cancelled, ContextChanged, Unauthorized,
    Store(StoreError),
}
pub struct LibraryReadContext { /* private，见 B2 */ }
pub struct ReadCredential { /* 私有user_id/token；不实现Debug/Serialize */ }
pub struct LibraryReadPool { /* private */ }
pub struct LibraryReadLease { /* private */ }
pub struct QueryBudget { pub deadline: Instant, pub cancelled: Arc<AtomicBool> }
```

Browse 的 stills 仅在 require_image 请求中读取，供实际 Gallery 消费；Protocol 的 Series 标量单独保存，不假装成四类页内事实。两者都在同一 lease 快照内复用已有借用查询核心。

这里返回纯 owned 数据；不把 rusqlite Row/Transaction/Statement 送出 worker。`LibraryReadContext` 是 Store 产生的不透明 token，不允许调用方任意构造授权集合。

`ReadCredential::new(user_id: UserId, token: String) -> ReadCredential` 仅封装已有请求凭据；prepare必须重新核验它，构造本身不代表授权。实例CLI token已有登记User，不另外制造“无User即管理员”的身份分支。

### Task B1: 只读打开、有界 lease 与取消清理

**Files:**
- Create: `crates/store/src/library_read/{mod,pool,lease}.rs`。
- Modify: `crates/store/src/{lib.rs,open.rs}`。
- Test: `crates/store/tests/library_read_pool.rs`（新）；内部 fault seam 放 `library_read/lease/tests.rs`（新）。

**Interfaces:**
- Produces: `Store::open_library_read_pool(&self, size: NonZeroUsize) -> Result<LibraryReadPool, StoreError>`；实际路径由 Store 提供，生产使用 size=2。
- Produces: `LibraryReadPool::try_checkout(&self) -> Result<LibraryReadLease, LibraryReadError>`。
- Produces: `LibraryReadPool::close(&self)`、`LibraryReadPool::available(&self) -> usize`。
- Produces: lease 的私有 `with_connection<T>(&mut self, budget: &QueryBudget, work: impl FnOnce(&Connection) -> Result<T, StoreError>) -> Result<T, LibraryReadError>`，只能由 B2 的 typed execute 调用，不能对外暴露任意写 SQL 入口。

- [x] **Step 1: 写有界公开行为测试** `pool_exhaustion_is_busy_and_closed_pool_never_reopens_database`。

```rust
let pool = store.open_library_read_pool(NonZeroUsize::new(2).unwrap()).unwrap();
let a = pool.try_checkout().unwrap();
let b = pool.try_checkout().unwrap();
assert!(matches!(pool.try_checkout(), Err(LibraryReadError::Busy)));
drop(a);
assert_eq!(pool.available(), 1);
pool.close();
assert!(pool.try_checkout().is_err());
drop(b);
```

另在内部故障测试中通过同一 lease connection 尝试写 main/TEMP、改 schema，均被拒绝；故意失败/超时后下一次合法精确查询正常。删除文件/旧布局不能被 pool 自动创建或初始化，使用自定义 Store::open_at 路径也不猜 app.db。

- [x] **Step 2: 运行** `cargo test -p store --test library_read_pool`；预期 RED 为不存在只读 pool API。
- [x] **Step 3: 实现 pool/lease**。READ_ONLY、query_only、纯casefold；不设置持久 journal_mode，不调用seed/migrate/path alias刷新。固定连接总数，无临时溢出连接。请求 deadline 的 busy_timeout=min(剩余毫秒,5000)，progress每1000 VM检查截止/取消；worker持有lease至真正结束。Drop 清回调并rollback；无法确认连接干净则丢弃，在总连接上限内受控补建。
- [x] **Step 4: 写并运行取消竞态测试** `expired_lease_does_not_interrupt_the_next_request`、`panic_and_sql_error_leave_no_transaction_or_callback`。用Barrier/进度回调同步，不靠长sleep；确保迟到取消仅作用于本次QueryBudget，SQLite回调不持有ApiState。`cargo test -p store --test library_read_pool` 与相关内部tests全部PASS。
- [x] **Step 5: 提交**：`feat(store): add bounded read-only library leases`。

B1 局部验收：完整 Store **592 passed / 0 failed / 7 ignored**；15个新公开/内部行为测试覆盖取消、deadline、活跃关闭、布局变化与清理/容量竞态。独立代码审查及两轮定向修复复审通过，format/尺寸/whitespace通过。`Closed` 表示池或lease已关闭，B3/B4映射查询不可用503；`with_connection`已负责唯一Library读事务与返回前清理，B2复用其借用Connection，不再嵌套BEGIN。应用所有者的shutdown/close接线由B4完成。最终workspace门统一归B5记录。

### Task B2: 复用查询核心、同快照与授权重验上下文

**Files:**
- Create: `crates/store/src/library_read/{context,query}.rs`。
- Modify: `crates/store/src/library_query/{items/page.rs,items/hydration.rs,facets.rs,protocol/mod.rs,page_inputs.rs}`，以及需要提取的只读 Library/role 查询。
- Test: `crates/store/tests/library_read_snapshot.rs`（新）；`library_read/context/tests.rs`（新，连接预算策略恢复的内部故障回归）。

**Interfaces:**
- Produces: `Store::prepare_library_read(&self, credential: &ReadCredential, request: &LibraryReadRequest) -> Result<LibraryReadContext, LibraryReadError>`。
- Produces: `Store::validate_library_read(&self, context: &LibraryReadContext) -> Result<(), LibraryReadError>`。
- Produces: `Store::with_query_budget<T>(&self, budget: &QueryBudget, work: impl FnOnce(&Store) -> Result<T, LibraryReadError>) -> Result<T, LibraryReadError>`。同步执行owned结果的短Store阶段，不返回Row/Transaction/Guard；调用方须独占Store锁，回调不能await或再次获取Store锁。内部RAII暂设app/library连接的busy/progress，按Spec7.2在退出时恢复原策略；同一Store的嵌套预算调用在修改连接策略前拒绝；用于认证、绑定、prepare/validate与页内冷读取，不能用于lease或绕过写者租约。
- Produces: `LibraryReadLease::execute(&mut self, context: &LibraryReadContext, budget: &QueryBudget) -> Result<LibraryReadResult, LibraryReadError>`；请求从context取，不接受第二份可能范围不同的request。
- Consumes: A 的 `select_protocol(&Connection, &HashSet<LibraryId>, &ProtocolItemQuery)`、成员/facet 核心、页内批量输入。
- Context私有字段固定为：ReadCredential的owned副本、LibraryReadRequest的Clone副本、最新UserId/角色/启用状态、完整合法Library roster及各库 `(config_revision,lifecycle_state,access_mode,admin_visible,member_ids_json)` 指纹。prepare/validate通过原token查当前User并核对身份，不凭UserId推断凭据仍有效；validate重新解析roster以捕捉新增/删除的可见库。request内Some(user_id)必须与凭据一致；Browse的None仅表示无个性化筛选，不取消权限检查。Context同样不实现Debug/Serialize。

- [x] **Step 1: 写同快照与权限竞态回归** `count_and_page_keep_one_library_snapshot_during_a_concurrent_publish`。借reader已开始读取后，用另一个writer连接提交新文件，当前 count/page/facts 一致且下一次能见到新事实。

```rust
assert_eq!(current.page.total, Some(before_total));
assert_eq!(current.page.items, before_items);
assert_eq!(next.page.total, Some(before_total + 1));
```

另写 `revoked_role_token_or_library_acl_invalidates_context`：prepare后分别禁用User、撤销原token、取消库成员、开始locale重建或给Root新增可见库，validate必须拒绝旧context；普通Playback更新不应使context无效。

- [x] **Step 2: 运行** `cargo test -p store --test library_read_snapshot`；预期 RED 为上下文/execute API缺失。
- [x] **Step 3: 提取并复用查询核心**。Store wrapper 与lease调用相同接受借用Connection的函数，不创建第二套SQL。execute仅在一个library读事务内检查库版本、运行count/page/facets及页内批量事实。prepare/validate继续通过实际app路径的现有Store连接读取最新身份；绝不宣称跨文件原子快照。
- [x] **Step 4: 运行** `cargo test -p store --test library_read_snapshot --test query_work --test query_model_counts --test query_model_protocol --test query_model_facets --test query_model_paging`及内部预算恢复测试。短Store阶段超时/SQL错误/panic后下一次正常读写不受旧取消或busy策略影响；同一请求Store/Reader输出逐字段相同，observer可跨reader线程独立累计，lease归还前无live Transaction。
- [x] **Step 5: 提交**：`refactor(store): share snapshot queries with read leases`。

B2 局部验收：完整 Store **604 passed / 0 failed / 7 ignored**；7个公开与5个内部测试覆盖同快照、原凭据撤销、有效范围/删除冻结、权限与预算恢复。独立审查及定向修复复审通过，12文件format/尺寸/whitespace通过；仅测试文件格式调整后再次通过7个公开测试。HTTP接线与最终授权发布由B3/B4接入，最终workspace门统一归B5记录。

### Task B3: 认证前建立共享截止时间并保留原请求凭据

**Files:**
- Create: `crates/api/src/library_read_work.rs`中的有界Store阶段执行器；Modify `lib.rs`模块树。
- Modify: 实际挂载的 `http/{auth,mod}.rs`与相关路由装配、`media_server_provider.rs`及设备凭据绑定实现；认证前deadline层、认证Extension注入。未挂载的 `crates/api/src/auth.rs`不改。
- Modify: `crates/media-server/src/{provider/mod.rs,auth.rs,routes/catalog.rs,routes/shows.rs}`及路由装配；token查询、设备凭据绑定、provider实现与所有调用者/测试fake同批修改。
- Modify test: A2已创建的 `crates/media-server/tests/query_error_contract.rs`；Create test: `crates/api/tests/library_read_principal.rs`。

**Interfaces:**
- Produces: media-server provider中的 `QueryPrincipal { user_id: UserId, token: String, deadline: Instant }`，Clone、手写脱敏Debug，不Serialize。
- Consumes: A2已定义、A3复用的 `QueryError::{Unauthorized,Unavailable,Internal(String)}` 与401/503/500映射。
- Modify: `MediaServerProvider::query_visible_items(&self, principal: QueryPrincipal, query: ServerItemQuery) -> Result<ServerItemPage, QueryError>`，生产实现和fake全部切换，仅将A2/A3的UserId参数扩为带原凭据与deadline的principal。该方法保持必需实现，不恢复默认legacy转发/旧签名重载；其他NextUp/Resume接口保持。
- Produces: 覆盖查询的 GET/HEAD 在认证之前创建 `QueryTiming { started, deadline }`，deadline 为真实入口 started 加30秒，并注入Extension；Self API认证后保留该deadline与ReadCredential，Jellyfin从AuthUser的id/token及同一Extension构造QueryPrincipal。下游不得缺失后自建30秒预算，重试保留原值；实测elapsed使用started，不能由deadline倒推。短deadline测试通过仅覆盖层可提升的QueryTimingSeed注入。
- Modify: `MediaServerProvider::user_id_by_token(&self, token: &str, deadline: Option<Instant>) -> Result<Option<UserId>, QueryError>`；`bind_playback_credential`同样增加 `deadline: Option<Instant>` 参数并返回 `Result<(), QueryError>`。所有调用者/fake同批切换；本轮查询入口必须传Some，非本轮路由显式None并保留原有预算/绑定合同，不提供旧签名重载或默认转发。optional-auth遇到查询预算耗尽也返回503，不能降级匿名或401。
- Produces: 必需的 `get_user_for_query`、`list_visible_libraries_for_query` 接受同一QueryPrincipal；登录别名与早返回的Library目录头不能绕过原token和有界Store阶段。B3目录头在cover文件观察后重验token，B4补齐role/ACL/config/roster重验。
- Produces: `pub(crate) async fn run_store_query_stage<T: Send + 'static>(state: &ApiState, deadline: Instant, work: impl FnOnce(&Store) -> Result<T, LibraryReadError> + Send + 'static) -> Result<T, LibraryReadError>`。立即申请blocking permit，worker用 `try_lock_until(deadline)` 获取Store并在锁后重验预算，通过B2 `with_query_budget`执行；不持lease，不在async线程直接lock。worker结束前permit不释放，外层丢弃/超时设本阶段取消标记。
- Produces: covered Self items/facets/gallery与Jellyfin页内冷读取共用绝对预算；冷worker在闭包前后检查预算，元数据与自动probe每次Store获取有界，文件/.strm观察在锁外。incidental绑定/probe写入在实际变更guard内重验写者资格，只读服务保留合法读取。SQLite Busy/Locked沿typed错误传播为503，不使用字符串哨兵。

- [x] **Step 1: 写路由错误与身份回归** `query_unavailable_is_503_and_principal_keeps_original_credential`。fake provider观察原始凭据及认证/绑定/查询相同deadline，返回Unavailable时catalog/shows查询503；Unauthorized401、Internal500。Debug不包含token，Self API保留同一token。另写 `writer_lock_before_auth_respects_deadline`：真实Router覆盖Self/Jellyfin及带DeviceId的绑定分支，writer先持锁，注入短deadline后请求在writer放锁前返回503，同时单线程Tokio上的另一任务仍能前进；有限超时只防挂死，不依赖长sleep。
- [x] **Step 2: 运行** `cargo test -p media-server --test query_error_contract` 与 `cargo test -p api --test library_read_principal`；预期RED为新类型/错误映射不存在。
- [x] **Step 3: 修改typed trait、认证层和所有调用者**。先装认证前deadline层，再通过run_store_query_stage执行token验证及设备凭据绑定；预算耗尽503、凭据无效401、内部失败500，不再统一映射401。正常provider暂在同一有界执行器内调用借用Store的查询核心，B4再改为lease；闭包直接调用Store方法，禁止调用会再次获取Store锁的现有provider/auth异步封装；此提交也不能在async线程等待Store。绑定保持原有幂等/写者租约合同，超时不宣称事务未提交。请求凭据不进日志/JSON，不保留旧签名或字符串前缀识别状态码。
- [x] **Step 4: 运行** `cargo test -p media-server`、`cargo test -p api --test library_read_principal --test jellyfin_sql_page`；全部PASS，普通合法客户端的协议响应不变。
- [x] **Step 5: 提交**：`refactor(auth): preserve query credentials and typed failures`。

B3 局部验收：media-server **42 passed / 0 failed**；API 完整测试目标 **1622 passed / 0 failed / 7 ignored**。完整 API 命令在会话中断前完成1407个测试，其余34个目标与doc在相同39文件源码hash下补跑，退出0；150个integration targets全部覆盖。原凭据、GET/HEAD、共享绝对预算、typed错误与只读incidental写入回归通过；独立审查和定向修复复审通过，39文件format/尺寸/whitespace通过。B4仍负责lease隔离及冷读取后的完整授权重验。

### Task B4: HTTP 准备/执行/重验与生命周期

**Files:**
- Modify: B3已创建的 `crates/api/src/library_read_work.rs`，增加lease执行及重验编排。
- Modify: `crates/api/src/{lib.rs,management/state.rs,http/library.rs,http/library_scan.rs}`、`media_server_provider/selection/{mod,page}.rs`、认证 Extension 的 owned principal 提取位置。
- Test: `crates/api/tests/library_read_concurrency.rs`、`library_read_auth_race.rs`（新）；`facets_budget.rs`、`query_page_materialization.rs`。

**Interfaces:**
- Produces: `ApiState.library_reads: Arc<LibraryReadPool>`；Store/Jobs完整初始化后再open，测试可注入size=1。
- Produces: `pub(crate) async fn run_library_read(state: &ApiState, credential: ReadCredential, request: LibraryReadRequest, deadline: Instant) -> Result<LibraryReadResult, LibraryReadError>`；消费B3认证前建立的绝对deadline，ContextChanged最多重试1次且不重置预算。
- Consumes: B2 prepare/execute/validate和with_query_budget，B3 run_store_query_stage及现有BlockingBudget；认证principal不能丢掉原凭据或deadline。
- Produces: Self items/gallery/facets共用有界 `run_self_library_read` 可见性预检；B2先验证原凭据，再判断已验证roster。不存在或初始不可见的Library保留404，原凭据失效401、SQL失败500、冻结/预算503；成功后复用原read helper及同一deadline。
- Produces: 必需的typed `MediaServerProvider::query_latest_items(QueryPrincipal, ServerItemQuery) -> Result<ServerItemPage, QueryError>`。Latest两段由同一响应尝试组合，保留未看完优先、分段offset、显式IsPlayed及数组响应；全部context/引用在冷读取后一起验证，变化时丢弃整个组合且最多重试一次。
- Produces: 共享协议响应attempt在轻量范围解析后保留已识别的显式Library，重试不能把撤权的UUID改解释成Genre/Studio；初始未知父级仍按原空页合同。`Store::validate_protocol_references(&[LibraryReadContext], &[ProtocolItemRef])`在一个新短Library事务中先验证所有opaque context，再复核选中源/归属/root/coverage/lifecycle，并在提交前重验原凭据。Root权限变化保留ContextChanged，显式撤权终止，版本/目录错误500、冻结503。
- Refinement: opaque context必须留在共享内部编排中直到cold finisher与最终验证结束；四参数read helper不能先返回已验证结果再追加冷读取。Store锁和lease不重叠，取消后worker实际结束才释放permit/连接。显式shutdown和最后状态owner关闭池，单个clone释放或写者租约丢失不关闭只读服务。

- [x] **Step 1: 写真实 Router 并发 RED** `slow_facet_reader_does_not_hold_writer_store_mutex`。在read progress同步点暂停reader，另一路公开发布/合法绑定Playback写入必须在放行reader之前完成；用有限recv_timeout仅防测试挂死，不把固定毫秒当性能结论。
- [x] **Step 2: 写真实 Router 权限与预算回归**。查询已获scope后撤销原token/role/ACL，放行SQL后响应不能包含旧敏感结果。池满503、deadline503、查询失败500；不会返回空数组200掩盖失败。context变一次重试成功，连续变化两次503；第二次尝试只能消费剩余时间，metadata reindexing保留A的503语义。另在prepare与validate各自获取Store前用Barrier让writer占锁，断言共享deadline到期503且Tokio其他任务前进；覆盖页内冷读取与超时后连接策略恢复。
- [x] **Step 3: 运行** `cargo test -p api --test library_read_concurrency --test library_read_auth_race`；预期 RED 为当前昂贵查询仍阻塞 Store，或新read-work接口缺失。
- [x] **Step 4: 实现 run_library_read 与消费者**。prepare/validate及剩余页内冷读取均通过B3 run_store_query_stage（permit准入→blocking worker→try_lock_until→B2连接预算RAII）。短锁prepare→释放→blocking worker独占lease execute→关闭快照/归还lease→必要页内冷读取→短锁validate。每阶段与一次重试共用传入的绝对deadline，超时结果不发布。不能持Store等待checkout，也不能持lease等待Store重验。pool Busy/Deadline映射现有工作预算503，身份失败401，连续ContextChanged503；Jellyfin沿B3的typed QueryError传播。返回前再执行最终身份/ACL重验，页内冷详情不得在重验后把未授权事实重新拼入响应。
- [x] **Step 5: 接入生命周期**。pool不持ApiState强引用；关闭/文件身份变化时停止接收、设置取消、worker结束后释放连接。仅丢失写者租约时保留现有只读服务与写入409合同，测试覆盖read-only ApiState。数据库替换只允许停机关闭所有连接后的运维操作，运行期间不热替换。现有静态图片/流路由不迁移到该查询预算。
- [x] **Step 6: 运行** `cargo test -p api --test library_read_concurrency --test library_read_auth_race --test facets_budget --test query_page_materialization --test fixture_lifecycle`；全部PASS，ApiState释放后池/预算Weak可回收。
- [x] **Step 7: 提交**：`perf(api): isolate library query work from writers`。

B4 局部验收：完整 API **1656 passed / 0 failed / 7 ignored**，Store **604 / 0 / 7**，media-server **43 / 0 / 0**；最终相关 API 回归94/0/1。独立审查与两轮定向复审通过，32个源码hash/format/尺寸/whitespace核对通过。真实Router覆盖SQL暂停时写入可完成、原凭据撤销、整响应重试、Latest组合、显式库撤权、404合同、prepare/validate等待、池满、取消/关闭、只读读取与写入409。并发实测及最终workspace门结果见B5。

### Task B5: 并发、WAL 与完整门禁

**Files:**
- Create: `crates/api/tests/library_read_perf.rs`、`docs/benchmarks/2026-10-25-library-read-isolation.md`。
- Modify: `crates/api/src/library_read_work.rs`的完整指标；`docs/deploy.md`、ADR-0008和本计划状态。

**Interfaces:**
- Produces: 每次请求日志字段 `auth_lock_wait_ms,auth_ms,credential_binding_ms,queue_wait_ms,prepare_lock_wait_ms,prepare_lock_held_ms,sql_ms,validation_lock_wait_ms,validation_lock_held_ms,elapsed_ms,retry_count,returned,total,vm_steps,sort_operations`；elapsed_ms从认证前开始；超时日志注明阶段与剩余预算，不记录token、绑定值或原始SQL。
- Consumes: Spec 9.2并发矩阵，A的同形状性能数据。

- [x] **Step 1: 运行隔离并发测量** `cargo test --release -p api --test library_read_perf -- --ignored --nocapture`。固定2reader+1writer，writer每批100文件；记录读写p50/p95/max、池拒绝/超时、Store锁阶段、WAL字节/checkpoint忙碌和RSS。SQL测量与HTTP测量分别记录，不混用trace开关和缓存状态。
- [x] **Step 2: 验证真实执行边界**。挂起reader时writer仍可提交；被取消查询实际退出后checkpoint可回收。若慢盘IO尚在进行，连接不能归池，报告停止耗时而非宣称瞬时取消；不强制unlink WAL通过测试。
- [x] **Step 3: 完整门** `cargo test -p store`、`cargo test -p api`、`cargo test -p media-server`，最终`cargo test --workspace`；format/尺寸/文档链接检查。无Web改动不重复Web测试；有错误展示变更则跑完整Web门。
- [x] **Step 4: 更新实测与限制并提交** `perf(storage): verify bounded library read isolation`。标明NAS/真实设备、1M组合、掉电场景是否已跑；不删除A的查询工作量预算，也不声称增加读池提升了单条SQL本身的速度。

B5 验收：[实测记录](../../benchmarks/2026-10-25-library-read-isolation.md)保留 5k/100k、两类真实 Router pair、first + 5 warmup + 50 timed、独立 trace-off HTTP / trace-on leased SQL、WAL/RSS 及源/日志 hashes。两读者已建立快照时 100-File/100-Media writer 可提交；取消后等待实际 worker 清理才归还 lease/permit 并 checkpoint。请求指标、Latest known-total、超时后实际日志与重建错误码回归通过。独立任务、整体审查及一次集中修复复审均通过，0 遗留问题。2026-10-26 最终完整门：Store **607 passed / 0 failed / 7 ignored**，API **1668 passed / 0 failed / 8 ignored**，media-server **43 passed / 0 failed / 0 ignored**，workspace **2907 passed / 0 failed / 19 ignored**；format/尺寸/文档链接检查通过。[验证元数据](../../benchmarks/2026-10-26-library-read-isolation-verification.json)保留真实命令、退出码和日志 hashes。最终 Latest/日志/错误码修正未重新计时；原实测源码可从受版本控制的补丁与 manifest 重建。

## 自审与执行交接

- B1/B2只涉及Store，B3建立认证前deadline/有界Store执行器并规范provider接口，B4接入lease，B5执行并发门；每项均可单独审阅并用临时库验收。2026-10-25 review 新增认证前/prepare/validate 锁等待回归已由 B3–B4 实施并通过；完整局部门和独立审查记录在各任务下。
- reader pool与writer锁不可嵌套，跨文件权限重验的线性化点已明确；不使用全局可变path alias作为快照的一部分。
- B1–B5 已实施并通过独立审查、最终 crate/workspace 门；A9 Step5 与 B5 Step3/4 已完成。并发实测与明确未跑的容量/现场项见[隔离记录](../../benchmarks/2026-10-25-library-read-isolation.md)。本轮未执行实例切换。
