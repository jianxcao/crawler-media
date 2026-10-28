# 存储重构对 main 的审查基线（7030c1dd）

> 本文固定记录修复前的审查与反例。后续修复见[修复与验证记录](2026-10-28-storage-remediation-verification.md)，最新结论与未实施方案见[共享锁复审](2026-10-28-storage-lock-review.md)。下文问题、结构数字与门禁仅属于当时版本。

日期：2026-10-28。审查对象 `feat/storage-redesign@7030c1ddc8b6e28e457fdd309d00bbc5a21c09d6`；执行 `git fetch origin main` 后，本地 main、origin/main 与 merge base 均为 `756444f76bef53f5fabed6d0ef53e9f280e005e9`。开始时工作区干净。差异为 1,540 文件、187,136 行新增、65,496 行删除；这是重点调用链审查，不是全部差异逐行穷尽证明。

**当轮结论：暂不建议合并。确认两项 P1、四项 P2；当轮只审查、测量和整理文档，没有修改业务代码、测试或 DDL。** 后续 H1–H6 修复对账见[修复记录](2026-10-28-storage-remediation-verification.md)，原实施计划已完成撤下；当前剩余见[共享锁复审](2026-10-28-storage-lock-review.md)。原始临时探针源码与输出见[证据附录](2026-10-28-storage-head-review-evidence.md)。

## 架构与数据库判断

- 五文件按生命周期划分仍合理：app 保存配置/凭证，library 保存 Media、Library、Playback 权威事实及事务摘要，subscribe 保存策略/下载事实，jobs 保存执行事实，cache 保存可重建外部响应。无需因为表多而重拆数据库或换数据库。
- Media、文件覆盖、采用元数据、用户事实和查询摘要同库，比 main 的跨文件更新更容易保持一致。跨文件没有原子提交，必须依靠持久阶段、身份冻结与恢复；H1 证明目前并非所有后台发布入口都遵守屏障。
- library v8 的 53 表、594 字段、81 命名索引、37 组 FK，与[生产 DDL 字典](../database/README.md)一致。冷详情、覆盖摘要、协议成员和边际计数有明确读写用途。当前问题主要是入口校验和算法，不需要先改 DDL。
- 当时实现没有导入或双读写，但仍带有开发中间结构标记及切换提示；用户随后要求只保留最终当前定义。前端受审调用已切换到 Transfer revision CAS、Library operation、文件游标、版本化重新识别和签发 Playback session；但 H5 表明后端旧输入兼容尚未清零。

## H1 · P1：迟到指纹模型使 Media 合并恢复不能收敛

**位置：** [模型事务](../../crates/store/src/fingerprint_models.rs#L27)、[后台发布](../../crates/api/src/probe_manager/season.rs#L315)、[发布前校验](../../crates/api/src/probe_manager/season.rs#L339)。

`persist_season_model` 保留 `pipeline.media_id`。最终校验重新读取文件当前归属，只比较 source_version 并对当前行调用 `ensure_source_recorded`，没有比较当前 Media 与 pipeline Media。合并转移文件时 source_version 不变，因此旧 pipeline 可以通过检查。`put_fingerprint_model` 又没有在同一写事务拒绝 merging/retired 或验证成员 Media。

公开 Store 探针在 library 阶段提交、subscribes→retire 边界注入一次失败。文件已经归 survivor、source_version 未变；此时向 loser 发布模型返回 `Ok(())`。去掉失败注入、关闭重开 Store 后，恢复报 `references remain (fingerprint_season_models=1)`，停在 retire/failed。已有模型的移动发生于已提交阶段，恢复不会重新移动晚到模型。生产合并链没有保证先排空此 pipeline。

**影响与来源：** 新合并冻结/恢复合同遗漏后台写入口；可能持续冻结操作，需人工介入。这是持久状态不收敛，不是 SQLite 锁死。探针覆盖公开 Store 阶段交错；没有运行真实指纹提取进程。

**方案：** 保留 `Store::put_fingerprint_model` 接口，在其 IMMEDIATE 事务内复用 `library_write::publish::ensure_media_writable`，验证所有成员当前 Media、source_version 与有效发布/Library 状态；失败零写。API 最终比较 pipeline Media，过期结果丢弃并记录原因，不自动重定向 survivor。不以重跑已提交 merge 阶段掩盖晚到写。

**验收：** 正常模型可发布；merging/retired、改挂、换源成员全部拒绝且模型/成员零变更；跨阶段失败→迟到发布被拒→重开→合并 succeeded。还需验证正常模型更新失败时原模型和成员完整保留。

## H2 · P1：剩余真实接口仍在 Tokio worker 等同步锁

**位置：** [Library 列表](../../crates/api/src/http/library.rs#L128)、[统计](../../crates/api/src/library_statistics.rs#L40)、[Collection 列表](../../crates/api/src/http/collections.rs#L90)、[重复文件列表](../../crates/api/src/http/library_duplicates.rs#L75)。Collection 其他 CRUD/gallery 同样直接加锁；gallery 还在锁内做图片存在性检查。

本轮使用有效 User token、真实 Router、current-thread runtime，让独立线程自主持 Store 500ms，同时运行 10ms timer：

| 请求 | HTTP | timer 实际耗时 |
|---|---:|---:|
| GET /api/v1/collections | 200 | 507ms |
| GET /api/v1/libraries | 200 | 506ms |
| GET /api/v1/libraries/{id}/duplicates | 200 | 502ms |

duplicates 还在同一 guard 下进行全库覆盖、逐文件单元查询、实体 metadata 和响应 size metadata。慢磁盘会占住全局 Store，且阻塞执行器。SQLite busy_timeout 不限制 parking_lot Mutex；仅把磁盘 IO 搬走也不能解决同步等锁。

**来源：** 这些同步锁在 main 已存在，本分支修改后仍保留；不能全部归因于重构新引入。上一轮修复详情、续页和 provider 的结论成立，但不能推广到所有接口。

**方案：** owned SQL 阶段整体移入现有 off-executor Store 机制，读采用 `store_read`，写采用有界 `http_work.run_to_completion`、`with_bounded_store` 并在拿锁后重验写准入。磁盘/网络观察用现有有界预算在 Store 锁外运行；不持 Store 跨 await，不叠加 reader lease 和 Store。先覆盖本项点名路径，再按实际 async 调用链审计剩余直接锁；不能靠搜索命中数量宣称全部安全。

**验收：** 三个真实 Router 在争用时 timer <200ms；长争用返回合同规定的 busy；慢文件观察期间独立 Store 读仍可完成。Collection 写保留 owner/ACL、实例准入和真实结果；取消/panic 后预算最终归还，测试 holder 必须由外部线程自主释放。

## H3 · P2：Series 分集完成数在无统计信息时二次增长

**位置：** [相关子查询](../../crates/store/src/library_query/series_scalar.rs#L76)，[实际协议查询调用](../../crates/store/src/library_read/query.rs#L151)。

每个 owned episode 都执行三坐标 OR 查询；无统计信息时 SQLite 选择仅按 Media/User 定位，再扫描该 Media 的全部 Playback 行和排序。单 Media 拥有 E 集且同 User 每集有断言时，工作量约 O(E²)。正常协议页每个 TV Work 都计算该 scalar，`Limit=1` 也不能消除它。

公开 `Store::protocol_series_scalar` 探针使用正常 Store 建库、合法 UUID、FK=ON，返回 episodes/completed 均为 E；同库同参数原生产 SQL 的 VM 如下（bundled SQLite 3.50.2）：

| E | 无 ANALYZE VM | ANALYZE 后 VM | 公开调用耗时：无/有统计 |
|---:|---:|---:|---:|
| 100 | 169,891 | 14,583 | 2 / 3ms |
| 1,000 | 16,098,091 | 145,083 | 185 / 8ms |
| 5,000 | 400,490,091 | 725,083 | 4,879 / 34ms |

耗时在本机 debug/workspace 并发下仅供参考；VM 是该单条 SQL，不冒充完整 HTTP。10 倍集数对应约 95 倍 VM，明确不是线性。新库在空库时执行 optimize 后持续加入数据、只读池不执行写型 optimize，允许无统计状态出现；本项不声称生产从不 ANALYZE。

**方案：** 三个坐标分别精确主键 seek，以 UNION ALL 合并最多三行，沿原 `played_changed_at_ms DESC, season DESC, episode DESC` 规则选断言，再保留 own fallback。无需新增索引，不以强制 ANALYZE 作为唯一修复。

**验收：** 100/1,000/5,000 集、统计前后均近线性 VM；whole/season/episode、显式 false、无断言 own 值与同时间 tie-break 不变。补真实协议 `Limit=1` 的完整 leased SQL 工作量门。本项属于本分支新读模型。

## H4 · P2：重复文件列表丢弃已读覆盖后 N+1，且去重二次增长

**位置：** [覆盖被丢弃](../../crates/api/src/http/library/assets.rs#L7)、[逐文件重读](../../crates/api/src/http/library_duplicates.rs#L81)、[线性查重循环](../../crates/api/src/http/library_duplicates.rs#L59)。

`files_in_library` 已通过 `coverages_in_library` 读取完整 FileCoverage，却只保留 file；duplicates 随后为每个文件重新 `get_file_units`。此外同一逻辑单元有 N 个不同物理实体时，Vec 去重执行 N(N−1)/2 次比较。这是源码可推导的 O(N²)，不是指数增长；本轮未做该循环的 HTTP 规模耗时实测。

**来源：** Vec 去重来自 main；先读覆盖再丢弃、逐文件重读是当前模型接线问题。锁与 IO 影响归 H2，避免将同一问题重复计数。

**方案：** 直接捕获 FileCoverage，批量取得 Media。锁外每 file 只观察一次实体和 size 并复用结果，HashSet 去重或找到第二实体即可停止“是否重复”的判定；返回文件清单仍完整。保持 hardlink 不是独立副本、未知覆盖不算 whole、多单元副本不能凭部分覆盖删除。

**验收：** 100→10,000 文件不增加逐文件覆盖 SQL；独立副本、同 inode hardlink、多单元 pack、缺失文件输出正确；删除仍执行覆盖包含与版本重验，不放宽安全条件。

## H5 · P2：Filter API 仍转换三个旧输入别名

**位置：** [旧 kind 分支](../../crates/api/src/http/rule_sets.rs#L76)。

真实 Router 的 POST `/api/v1/rule-sets` 分别提交 `platforms_block`、`hdr_block`、`release_group_block`，均返回 201 且持久化成 `title_match`；当前 `hdr` 返回 `hdr`，未知 kind 返回 400。当前 Web 发送标准 `title_match`/`hdr` + exclude，没有这三个旧 kind 的消费者。`hdr_block` 还将 HDR 属性错误变成标题匹配。

**来源与方案：** main 遗留；违反本轮明确要求的完全无兼容。删除三个分支及误导注释，POST/PATCH 对旧值一律 400、零写，保留标准 atom 的正常往返；不提供转换、别名或新迁移。无需改表。

## H6 · P2：NextUp/Resume 的多单元文件被按单元重复复制

**位置：** [逐单元创建候选](../../crates/api/src/media_server_provider/selection/light.rs#L334)、[每候选克隆完整覆盖](../../crates/api/src/media_server_provider/selection/light.rs#L415)、[NextUp 先物化后分页](../../crates/api/src/media_server_provider/selection/mod.rs#L170)、[Resume 共用构建器](../../crates/api/src/media_server_provider/selection/resume.rs#L109)。

一个文件覆盖 U 集时生成 U 个 LightItem，每个 item 又 clone 含 U 个 UnitKey 的 FileCoverage，静态可推导出 U² 个 UnitKey 副本。NextUp Limit=1 仍先构造全部候选，构建发生在 Store guard 内。这是当前多单元模型引入的复制代价；本轮没有跑公开 NextUp/Resume 的内存或耗时探针，不能将推导写成实测。

**方案与验收：** 每个文件只建立一次共享的 `Arc<FileCoverage>`，轻候选持共享句柄和自身 UnitKey，选页后水合；owned scope 捕获后释放 Store 再做纯计算。用公开 provider 的 NextUp 和 Resume，单文件 U=100/1,000/5,000、Limit=1，验证覆盖存储/分配线性及相同逻辑结果，包含不同 played/favorite、多版本与 ACL；不能只数最终 heavy snapshot。

## 类型查询、接口规模与优化优先级

movie/TV 的当前查询先收窄授权 Library/kind；成员投影、边际计数和页内选源有价值。此前固定 Movie 数、扩大无关 TV 时工作量稳定的结果属于[历史版本测量](2026-10-28-storage-main-rereview.md#查询性能与真正未完成的优化)，本轮不冒充重测。筛选后精确 facets、搜索匹配、深 offset 随命中/跳过数量增长并不自动等于缺陷；精确 total 不改成近似值。

目前没有指数复杂度证据，但 H3/H4/H6 已足以否定“所有接口随数据增长都没有超线性风险”。不能用 Fullscan=0 或 SQL 条数少证明 CPU/内存有界。后续按接口固定响应页，分别扩大无关数据、同 Media 集数、文件版本、User 状态和终态操作历史；记录 SQL/VM/解码行、纯计算/复制、锁等待、p50/p95、busy、RSS/WAL。HTTP 指标需汇总跨线程各阶段。

以下是独立候选或未测边界，不与六项确认问题混成已实施任务：

- **O1**：完整分集 HTTP 跨线程 SQL/VM/解码与归约仍未验收；Store 单元/文件覆盖的局部回归已修，不重复报旧缺陷。
- **O2**：100k Media/1M Files/1M Playback 组合、NAS、真实设备、OS 冷缓存和掉电未执行。
- **O3**：Store/cache 的对象名与头校验不等于完整列/FK/CHECK/索引定义校验；可参考 Jobs 单一 DDL 比对，坏布局零写拒绝，不增加旧布局兼容。
- **O4**：独立 ResponseCache 与 Store cache 连接策略先测 fsync/争用/RSS 再统一；权威库 FULL 保持。
- **O5**：指纹重复列、协议 terms 合并、root 索引收窄、Probe WITHOUT ROWID 只属需 A/B 的结构候选；不盲目加索引或合表。
- **O6**：Web `getLibraryItemDetail` 自动收齐所有文件页，Probe 每5秒重拉完整详情；加载是 O(F)，慢请求可重叠。后续可先展示首屏、按展开范围加载、单个在途轮询并支持取消；本轮未做 UI 运行测量，不当作旧 API 残留。

## 锁、测试与验证边界

H2 是已复现的执行器饥饿，H1 是已复现的持久恢复不能收敛。未确认 SQLite 永久锁环。抽查 LibraryRead 的 TimedGate/recv_timeout、artwork gate 的超时/Drop、独立到期 holder，未发现这些测试的永久等待；这不构成全部锁交错安全证明。

- 本轮 `CARGO_TARGET_DIR=/tmp/crawler-review-20261028-target cargo test --workspace --no-fail-fast` **退出0：3,198 passed / 0 failed / 24 ignored，480个结果段**。日志 `/tmp/storage-head7030-review-workspace.log`；ignored不计通过。未新增或修改正式测试，H1–H6回归尚未纳入现有绿灯。
- Web `pnpm test` **850 passed / 0 failed / 0 skipped**；`pnpm typecheck`退出0。日志 `/tmp/storage-review-api-cutover-{web-test,typecheck}.log`。本轮无Web改动，未重跑build或浏览器。
- H1/H2/H3/H5的公开Store/真实Router探针已由父任务重新运行，结果保存于证据附录；探针链接本次HEAD的构建产物，无仓库源码变更。H3的SQL工作量与公开返回值分别测量，未声称完整HTTP耗时。
- Python SQLite仅用于核对生产DDL对象统计，确认library的53表/594字段/81命名索引/37组FK；性能探针使用workspace bundled SQLite 3.50.2，二者未混用。
- `git diff --check`与198个修改文档本地链接检查通过；Rust源码未改，未对全部函数的尺寸或所有接口锁交错作穷尽证明。
- 未访问真实TMDB/Downloader/Chromium、用户实例、NAS、设备或掉电环境；容量及O1–O6边界仍见上文。

## 文档收敛

当前入口、ADR-0008、主 Spec、数据库总览和 API 当前方案指向本报告；前两轮报告保留固定版本证据并标为历史。实施方案只列未实施任务，避免把历史 RED/GREEN 步骤当当前待办。

删除 `2026-10-27-codex-review-report.md`：它的全局无阻塞/无漏页概括已被后续具体反例推翻，不继续作为独立审查入口。独有有效结论保留如下：`f71bfe36` 将 `library_removals/library_removal_roots` 排除终态历史清理，成功删除根仍是永久边界；现行源码见 `crates/store/src/operations_retention.rs` 与 `library_deletion`。原全文可从 `git show 7030c1dd:docs/audits/2026-10-27-codex-review-report.md` 取回。其他历史审计、实测数据、设计取舍不按日期删除。

本轮仅提交审查/方案/证据和文档导航，不执行修复、不合并、不部署或操作用户实例。
