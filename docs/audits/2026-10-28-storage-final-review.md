# 存储重构对 main 的再复审与修复验收

> 历史阶段验收：以下记录f569c3c7审查及后续修复，当前7030c1dd仍发现新问题；以[当前审查](2026-10-28-storage-head-review.md)及其方案为准，不沿用本页合并结论。

审查基线：`feat/storage-redesign@f569c3c71ee0c1423f7e491d47d1d65d345a7fa7`，本地 `main` 与 `origin/main` 均为 `756444f76bef53f5fabed6d0ef53e9f280e005e9`，merge base 相同。工作区开始时干净。初始阶段仅审查及整理文档，用户随后授权修复；未运行真实实例。已确认问题已修复，本次范围内没有剩余合并阻断；O1–O5及现场边界按下文保留，不算已验收。差异达 1536 文件，本报告是针对存储、查询、API 切换及锁边界的重点审查，不宣称逐行穷尽。

**修复完成，最终 workspace 全量通过。以下原始反例固定于审查基线。** 本轮修复运行时锁等待、页内覆盖读取及性能门禁，测试命名同步清理；最终结果见下方修复验收。前次 R1–R9 修复与原证据见[上一轮验收](2026-10-28-storage-main-rereview.md)，该报告的“合并阻断已关闭”仅针对当时确认的 R6–R9，不能覆盖本轮新发现。

## P1：异步请求直接等待同步 Store 锁

[Library 详情](../../crates/api/src/http/library.rs#L288)及[文件续页](../../crates/api/src/http/library.rs#L408)在 async handler 中直接调用 `state.store.lock()` 并运行同步 SQLite；[Jellyfin provider](../../crates/api/src/media_server_provider.rs#L269) 的 `get_user`、`is_device_revoked`、`set_user_marks`、`issue_play_session` 等也直接获取同一锁，协议路由直接 await 它们。其他线程持锁或慢查询时会阻塞 Tokio worker；单线程执行器不能调度 timer 或其他请求，多线程负载下可耗尽 worker。busy_timeout 不约束外层 mutex 等待。独立复核已通过公开 `ApiServerProvider::get_user` seam 复现：外线程持锁 500ms，current-thread runtime 的 10ms timer 实际约 513ms，用户查询正常返回。协议 `routes/users.rs::user_profile` 直接 await 该方法；本次探针没有运行整个 HTTP Router。源码探针位于 `/tmp/storage-review-lock-probe.rs`，仅使用当前构建产物和临时目录。这是执行器饥饿，尚无证据证明永久锁环或 SQLite 死锁。

**与 main 的关系：** `get_user`、`set_user_marks` 及 Library 详情同步锁在 main 已存在；新的文件续页路径仍沿用同一问题。本项是当前安全整改未覆盖的范围，不应全部归因于本次重构新引入。

**方案与验收：** 把整个同步 Store 阶段移入现有有界 blocking 调度，包含 Jellyfin 认证/mark/session 与详情/续页调用链；保留身份、ACL、实例写门禁及操作响应，禁止持锁跨 await 或把文件/网络 IO 搬进锁内。分页读使用同一快照或明确重验授权和文件引用，避免两个锁阶段之间状态改变导致不一致。分别通过真实 Router 和 Jellyfin 路由在 current-thread runtime 注入外线程持锁，验证短 timer 和其他请求可运行；验证请求取消、容量耗尽、panic 后预算释放、写操作没有迟到副作用，以及正常响应合同。测试的持锁线程必须由独立线程释放，不能依赖可能被阻塞的 runtime timer。

## P2：文件分页仍读取整个 Media 的覆盖

[Store::file_units](../../crates/store/src/library_query/details/mod.rs#L193) SQL 只按 Library 和 Media 过滤，把该 Media 所有 `library_file_units` 解码到 map，之后才筛选所请求文件。[详情首页](../../crates/store/src/library_query/details/mod.rs#L90)和[文件续页](../../crates/api/src/http/library.rs#L447)均调用它。固定 50 或 100 条文件的页面，成本仍随该 Media 的总覆盖数增长；逐页加载可能反复读取相同覆盖。它还延长上述 Store 持锁时间。独立使用仓库 bundled rusqlite 执行相同生产 SELECT，并保留覆盖表的主键、唯一键及索引：同一 Media 覆盖从 100 增至 10,000，解码行从 100 增至 10,000，VM 从 611 增至 60,011；两者 Fullscan 均为 0。说明索引命中不能证明页内工作量有界。该隔离 SQL 探针不含完整 FK 父表及 HTTP 流程，不是公开 Store/HTTP 性能验收或设备耗时；源码 `/tmp/storage-review-coverage-probe.rs`。

**方案与验收：** 用本页文件 ID 的有界集合做索引 seek 或批量 join，保持 Library/Media 限定、文件顺序、多集覆盖和 whole 单元语义；空页不查询。先检查现有索引与实际 query plan，避免盲目加索引。公开 Store 与真实详情/续页回归固定页面文件及覆盖，扩展同一 Media 的其他文件与覆盖（例如 100→10,000），统计 SQL VM、解码行、实际返回内容和完整 HTTP 工作量；覆盖跨页、多覆盖、空页、删除并发和游标。成本只随选中页面及其覆盖增长，测试应同时计算写入和索引代价。

## P2：性能门禁负对照在新夹具失败、缓存夹具通过

[gate.rs:362–397](../../crates/store/tests/perf_baseline/gate.rs#L362) 假定删除 `items_title` 必然产生 Fullscan，并要求据此拒绝；browse 的通用预算不约束 VM。本轮 workspace 失败：语句数保持 6，VM 从 698 增至 2612，但 Fullscan 为 0，门禁无违规。单独用缓存夹具重跑通过（Fullscan 105）；再设 `PERF_FIXTURE_CACHE=off` 独立运行同一测试稳定重现失败（VM 2613，Fullscan 0）。不能把缓存命中后的绿灯计作修复。

`fixture.rs::build` 新建后直接使用 seeder Store，缓存命中则 `Store::open` 重开；后者执行连接优化策略。现有证据证明夹具两条路径的查询计划结果不同，统计/优化的精确因果还需单变量验证，不能未经验证称为随机竞争或死锁。

**方案与验收：** 明确新建/缓存重开及统计前后的夹具状态，按同一生产语义验证；把“索引存在性检查”和“查询工作量门禁”分开。对固定规模 browse 引入有依据的 VM/排序/解码预算，选择能稳定触发预算超限的负对照，不能强求优化器必须全表扫描，也不能删掉断言掩盖退化。无缓存、缓存命中及完整 workspace 都应通过，并保持重复查询、全表扫描、ledger 扫描负对照可拒绝。

复现：`PERF_FIXTURE_CACHE=off cargo test -p store --test perf_baseline gate::a_lost_index_fails_loudly_instead_of_scanning -- --exact --nocapture`；缓存路径同命令去掉环境覆盖。本轮已使用相同构建的 test binary 执行两条路径，日志 `/tmp/storage-review-perf-gate-{fresh,repeat}.log`。源码未修改。

## P3：测试名称误称旧布局迁移

[playback_session_migration.rs](../../crates/api/tests/playback_session_migration.rs#L13) 的测试名称称旧主键已迁移，但实际仅新建当前库并检查当前主键及记录，没有构造旧库或调用迁移。这不是生产迁移的证据，也不能证明旧库拒绝。建议改名描述当前布局行为，并保留独立的旧布局零写拒绝测试；不增加迁移。

## 设计、切换与后续优化

五文件按生命周期划分、Media/Library/Playback 事实及摘要同库事务、跨库持久恢复的方向合理；目前没有证据要求重拆数据库或改用其他数据库。跨库仍无原子提交，必须维持恢复与身份屏障。复查旧 directory、旧 watch 值和旧排序的相关调用与现有 Web 契约测试，未发现已确认的新兼容残留；受审查范围限制，这不是全仓无旧逻辑的证明。Jellyfin 协议映射不是旧自有 API 兼容。静态图片保持 public。

类型查询先按授权 Library/kind 收窄；既有实测及筛选后精确 facets 随命中规模增长的限制见[上一轮](2026-10-28-storage-main-rereview.md#查询性能与真正未完成的优化)。原 O1 完整分集 HTTP 工作量、O2 大规模/设备、O3 schema 完整定义校验、O4 连接策略测量、O5 schema 精简候选仍未完成；它们分别是验收缺口或待测候选，不是已证实的新增回归。优先完成上述两个问题及 O1 后再据测量决定索引、表结构或连接策略；保持精确 total 与权威库 FULL，不引入旧库迁移、旧 API 回退或双读写。

## 验证与边界

- Web `pnpm test`：850 passed，0 failed；`pnpm typecheck` 成功。未进行真实浏览器、NAS、设备、掉电和完整容量测试。
- `CARGO_TARGET_DIR=/tmp/crawler-review-20261028-target cargo test --workspace --no-fail-fast` 完整重试退出 101：3184 passed / 1 failed / 24 ignored，478 个结果段。唯一失败为上述 `perf_baseline` 负对照；无缓存单独复现失败，缓存命中重跑通过不抵消失败。首次运行在环境切换时中断，不计入数字。日志 `/tmp/crawler-review-20261028-workspace-retry.log`。
- 上述 3184/1/24 为修复前复核结果，不代表修复后验证。
- 未针对所有锁路径或所有 API 做穷尽证明；未确认永久死锁。现有测试通过不能证明大数据工作量或所有锁交错安全。初始只读阶段未修改源码和测试。

## 文档整理与复核范围

当前入口、ADR、主 Spec 与数据库总览改指向本报告；上一轮验收标为历史阶段，删除“异步边界已经全部补齐”的当前时态。历史审计和 benchmark 含独有证据，不物理删除；旧执行稿此前已撤下，不为清理重复删除。两个临时探针未修改仓库源码或测试。上一模型的两项主要结论成立，但来源区分、实测证据和完整性边界不足，本报告已补正；前后端审查未穷尽，不能据此宣称所有兼容代码均已清零。

## 授权修复与最终验证

- Library 详情/文件续页使用 off-executor Store 读取，续页选页、覆盖与冷详情同一持锁阶段；新增真实 Router timer 竞争回归，修复前延迟约 506ms，修复后通过 <200ms 门。
- Jellyfin 同步阶段使用现有有界 blocking 调度，等锁受预算限制，持久写在拿锁后重验实例准入；图片文件读取保持锁外。8项公开 provider 回归覆盖 user/device/cover/credential/marks/session、取消容量恢复及 marks 饱和返回503；其他既有 String 错误通道维持原协议错误合同，未扩大改造。
- Store 覆盖读取按页内 file_id 强制现有主键 seek，每400键一窗；跨窗口同一快照，保留每文件所有单元。公开回归固定50文件覆盖，同 Media 总覆盖100→10,000，在 ANALYZE 前后均 1 SQL / 914 VM / 50 SQL rows / 0 Fullscan / 0 sort；空页零SQL、多覆盖/whole/顺序/作用域及501文件跨窗通过。不新增 DDL 或旧协议兼容。
- 性能门禁验证新建与缓存路径，使用实际 VM 工作量预算检测缺失索引退化；迁移误称改为当前 session 结构测试。
- Store 完整 `cargo test -p store --no-fail-fast`：754 passed / 0 failed / 11 ignored；Media Server 完整 `cargo test -p media-server`：40 passed / 0 failed。Web 850 passed，typecheck exit 0。
- 单独 API 全量 `cargo test -p api --no-fail-fast`：1802 passed / 1 failed / 9 ignored；唯一失败是 `scan_operations::committed_chunk_prefix_survives_a_later_chunk_failure` 在并行验证中未于夹具约15秒等待内进入终态。同一用例在最终 workspace 的完整11项 scan_operations 中通过（8.10秒），单独 `cargo test -p api --test scan_operations committed_chunk_prefix_survives_a_later_chunk_failure -- --exact` 也通过（7.82秒）。未修改该测试时限/断言；不将单独API失败报告为全绿。
- 最终 `cargo test --workspace --no-fail-fast` 退出0：**3198 passed / 0 failed / 24 ignored**，480个结果段；包含最终 Jellyfin 8项、文件覆盖3项及新建/ANALYZE性能门禁，均通过。前一轮中途源码改动的运行已停止，不计作最终验收。日志为 `/tmp/storage-fix-{store-final,media-server-full,api-full,scan-recheck,workspace-final}.log`、`/tmp/storage-fix-web-{test,typecheck}.log`。
- 修改Rust文件 rustfmt（skip_children=true）、文件/函数尺寸、diff空白和131个本地文档链接通过；子模块既存排版差异没有纳入本次修改。
