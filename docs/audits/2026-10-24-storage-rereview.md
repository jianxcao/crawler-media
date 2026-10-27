# 数据库重构复审：cf7fb70

> 本文固定审查 `cf7fb70` 的修复前证据。2026-10-24 后续实施已落入同分支，当前状态见[执行记录](../superpowers/plans/2026-10-23-storage-review-remediation.md)及[新测量](../benchmarks/2026-10-24-storage-query-rereview.md)。下文“当前/尚未”只指所审查版本。已删除代码的历史原文可用 `git show cf7fb70:<路径>` 读取。


日期：2026-10-24。结论：**Request changes**。五文件及同库事实/摘要的方向合理，但优先修复尚未闭环，旧自有接口和配置回退也没有全部清除。本轮仅 review、隔离验证及维护文档，未修改 Rust、TS、DDL、测试或实例数据。

## 基线与证据范围

- 被审版本：`feat/storage-redesign@cf7fb70e20576e92d4df4a78ac2c57038400a516`，工作区起始干净。
- 本地 `main@86122d73a4a23439e1d83234590fdd98b388fc50` 是之前的设计/复审基线；fetch 后 `origin/main@14fdbb6855a85702d888310fa69a7253d6c283db` 是当前远端基线和 merge-base。两者之间只有 8 个设计文档提交，生产代码基线相同。相对本地 main 为 1129 文件、相对远端为 1127 文件；不能把两条 main 的 SHA 混写。
- 结合 main 差异、生产调用链和上一轮逐项对账；下列反例来自当前实现，不把旧审计的失败直接搬来。本页是当前发现入口，[唯一技术方案](../superpowers/plans/2026-10-23-storage-review-remediation.md)分配后续工作。
- 当前 Store 三份生产 DDL 在 SQLite 3.54.0 内存执行：app 为 10 表/50 字段/2 命名索引，library 为 47/553/57，subscribe 为 4/47/3；library 为 32 组 FK、0 触发器。加 cache/Jobs 后仍为 71 表/709 字段，没有本轮结构变更。

## 设计判断

保留 `app.db / library.db / subscribe.db / jobs.db / cache.db`。Media、Library、覆盖、Playback 与摘要同库，有利于一个事务维护不变量；cache 可重建，Job 定义和在途状态不可当缓存删除。显式 `library_id/root_id`、文件版本 CAS、冷详情、稀疏用户摘要和持久文件意图均有必要。

问题集中在边界没有收敛：Jobs 的建库仍跨多个自动提交；新增根的持久工作没有常驻恢复驱动；部分旧调用仍以路径推断归属或绕过新会话/重配置合同。继续增加兼容层会扩大状态空间。现阶段先收敛入口和调度，再按测量优化索引；没有证据要求改数据库产品或恢复旧浏览 journal。

## 当前发现与优先级

| ID | 级别 | 未完成项 | 证据 |
|---|---|---|---|
| S01 | P1 | Jobs 身份头与 DDL 非原子，首次初始化中断后无法重启；独立 Queue 仍走另一套建库规则 | 当前公开 seam 复现 |
| S02 | P1 | 根迁移遇冻结暂停后没有自动恢复触发，队首还会阻挡其他根；同步 drain 仍持锁耗尽队列 | 当前公开 seam + 调用链 |
| S03 | P1 | 前端仍调用旧 `/directory` 聚合写接口；直接换根破坏归属，Watch null 无法关闭 | 当前真实 Router 反例 |
| S04 | P1 | 无签发会话的旧进度事件被补上新源版本，污染替换后的恢复包 | 当前真实 Router 反例 |
| S05 | P2 | 默认 Downloader 仍读取旧 settings JSON | 当前公开选择函数回归 |
| S06 | P2 | 删除一个 Library 仍全实例扫描并访问无关库磁盘，整段 cleanup 持 Store | 当前公开 seam + 调用链 |
| S07 | P2 | Unidentified 移动行数有界，候选选择无索引 seek 工作量界限 | 当前 SQL/查询计划 |
| S08 | P2 | 同一 Media 批量文件发布重复重建协议 terms/search 索引 | 当前发布/归约调用链 |
| S09 | P2 | kind 未提前缩库，genre/filtered facets 工作量放大，count 还在 page 统计之外 | 当前查询链与隔离测量 |
| S10 | P2 | Unidentified 列表丢弃登记归属，TV 文件被报作默认 Movie 库且按 TV 过滤漏行 | 当前真实 Router 反例 |

P1 优先于吞吐调优。P2 中 S05 是本次“无旧版兼容”的交付门，不能因优先级较低而保留。下文每项均为后续修复建议，本轮没有执行。

### S01：Jobs 初始化应由一个入口原子完成

[API open_jobs](../../crates/api/src/management/state.rs#L666)先调用 [initialize_jobs_layout](../../crates/store/src/schema/layout.rs#L69)单独提交 application_id 和 user_version，再另开连接执行 [Queue::open](../../crates/jobs/src/queue.rs#L30)的 DDL。进程在这些步骤之间退出，留下带当前版本头、没有业务表的文件；下一次布局检查必然拒绝，不能正常重试初始化。

隔离探针对新文件调用正式 `initialize_jobs_layout` 后关连接，模拟停在 Queue 建表之前；第二次正式调用返回 `missing required table job_defs`。另用正式 `Queue::open` 创建新文件，API 预检返回 `expected … user_version 1, found … 0`。这不是旧实例导入问题，而是当前两个公开入口对同一新布局的定义不一致。没有杀真实进程或注入实际断电。

方案：让 Jobs crate 拥有唯一布局标识、DDL、只读预检和完整初始化；空库在一个 IMMEDIATE 事务中创建表、索引、触发器及单例，再写身份头并提交。已存在当前库只验证，未知/旧布局拒绝；删除 `DROP INDEX IF EXISTS jobs_live_def` 等旧升级步骤。API 和测试使用同一入口，不为已写坏的头增加兼容认领。故障注入覆盖建表、索引、触发器、身份头每个阶段及重试，旧/未来库拒绝且字节/对象不变。

### S02：根迁移缺少活性保证，不能只归为性能剩余项

[roots.rs](../../crates/store/src/roots.rs#L176)总是只处理按根深度排序的首条 pending，冻结即返回 None。生产 drain 调用仅出现在 create/update/insert-root 和 [启动循环](../../crates/api/src/management/state.rs#L332)；[重配置 driver](../../crates/api/src/storage_operations.rs#L426)完成并解冻后没有再次调度根迁移。

公开 Store seam：发布父库嵌套目录中的文件 → 开始父库 locale 重配置使其冻结 → 创建嵌套 Library，pending=1 → 将 locale operation 推到 succeeded。父库已 ready，文件仍属于父库，pending 仍为 1；只有显式 drain 才转入嵌套库。探针没有测试 batch budget/failpoint。生产自动恢复缺口由调用链确认；没有把手动推进 Store 的探针写成真实 HTTP 自动恢复测试。

方案：在现有 Library 工作体系增加根迁移 driver，启动、提交新根、解冻/删除终态通知并定时兜底；每次一个有限窗口，批间释放 Store、检查停机、yield。暂停根持久保留并公平轮转，不阻塞其他 ready 根。新增根返回可追踪的 operation，收敛前不能伪装整体 ready；沿用当前最深根/旧 scope 屏障。根身份和部分 Watch 的 canonicalize 仍需锁外准备。验收必须经真实 ApiState 完成“冻结→新增根→解冻→自动收敛”，禁止测试里手动 drain 或靠重启过关。

### S03：删除旧目录聚合写入口，并同步迁移 Web

`web/lib/api/directory.ts:34`（`cf7fb70`）的 `setTransferMode/setWatchIntake` 先 GET 全部配置，再把 root、Naming template、Scrape 等一起 PUT；[transfer-mode-card](../../web/components/transfer-mode-card.tsx#L10)仍消费它。`crates/api/src/http/directory.rs:68`（`cf7fb70`）写 app 设置后直接 `crates/api/src/http/directory.rs:133`（`cf7fb70`），返回同步 200，没有新 Library 重配置的 revision/operation 合同，还调用 [apply_legacy_naming](../../crates/api/src/scrape_store.rs#L71)反拆旧组合字符串。

真实 Router 探针在默认 Movie Library 登记文件后 PUT 新根：200，config_revision 仍 0→0，原文件保留却在新声明根之外，strict owner 为 None。另一真实请求先保存 Watch intake，再传 null，两次 200，最终旧路径仍在，因为 handler 只在 Some 时写值。过期 GET 整份回写还会携带过期根；跨 app/library 提交间退出也不能原子恢复。

方案：移除旧聚合 PUT 及 `/directory/roots` 写入口、旧 Naming template 拆解和专用消费者。实例 Transfer/Watch 设置使用仅携目标字段的严格 DTO，在 app 事务 CAS 写入，明确 missing/null/值的语义；Naming template 统一当前 Scrape 配置形状；根只走 `/libraries/{id}` 的 202 operation。前后端同批迁移，旧请求拒绝，不能做 alias/转发 shim。回归验证改 Transfer mode/Watch 绝不改变根、命名或 Scrape；并发根更新不被过期设置请求覆盖，清空 Watch 后运行配置确实停用。

### S04：自有 Playback 不再接收无会话旧写入

[真实 Router 回归](../../crates/api/tests/playback_event_identity/legacy_no_id_identity.rs#L44)要求无 `play_session_id` 的 `/api/v1/playback/progress` 返回 200、position=3000 且新增 `playback_units`；这证明兼容行为仍在运行。分类器 [classify_legacy](../../crates/api/src/playback_events.rs#L151)、[heartbeat](../../crates/api/src/http/playback/heartbeat.rs#L400)和 [stage_unbound_event](../../crates/store/src/playback/event.rs#L146)仍支持无签发绑定的事实写入。Web 的 progress 类型仍把会话 ID 设为可选。

更强的当前 Router 反例：r1 时准备无 ID progress body，通过正式 `publish_files(expected=r1)` 替换为 r2，再提交旧 body，返回 200；position=12345，恢复包 revision=2，而原事件准备时 revision=1。handler 补当前文件版本使旧事件被接纳，不只是保留一个无实际影响的兼容类型。

方案：自有 API 强制已有签发 session、合法事件 sequence 与固定 target/source generation；缺失/格式错误 400 零写，过期绑定按当前 stale 合同不写。移除自有入口的无 ID 分类、临时签发及 Store unbound 写分支，同步 Web 必填类型、生产者及测试；共享 classify_legacy 中仍属 Jellyfin 协议需要的规则下沉至该 adapter。Jellyfin adapter 按其协议在规范入口建立绑定，不因清理自有旧协议破坏合法客户端；保留 typed Item/MediaSource 分离和协议鉴权。验收迟到旧事件、跨设备/目标、换源、终止/重复 Stop，以及新 Web 完整播放链。

### S05：只从当前 Downloader 配置选取

[choose_downloader](../../crates/api/src/runtime_downloader.rs#L85)没有默认 Downloader 行时仍读取 `settings['downloader']`，通过 `SavedDownloader/from_saved` 连接旧 JSON 中的配置；当前测试还要求该回退。

方案：保留明确支持的环境变量覆盖和 `downloaders` 行，删除旧 JSON 类型/读取/重复转换逻辑；没有合法当前配置时报明确错误。旧 key 存在不能改变结果，也不自动导入成新行。更新测试为“仅旧 key → 拒绝”，覆盖环境覆盖、有效/缺失默认行、显式 Downloader 选择。

### S06：Library 删除仍依赖全实例旧归属推断

[begin_library_removal](../../crates/store/src/library_deletion/mod.rs#L128)读取全实例 `list_file_coverage`，逐文件 get_media/realpath，再用根路径重新判定；[freeze](../../crates/api/src/library_lifecycle.rs#L320)同时持 registry 和 Store。公开 seam 中 A 是空 Library，B 有同 kind 文件；仅将 B 的文件换成自引用 symlink，删除 A 返回 `FilesystemLoop`，修复 B 后删除 A 才成功。无关库的磁盘错误实际阻止目标删除。

此外 [drain_removal](../../crates/api/src/library_lifecycle.rs#L164)持一把 Store 调完整 `clean_library_removal`；SQL 每批 COMMIT 不代表其他请求能获得 Store。关闭会话还有 nullable file_id 推断路径，而当前 `playback_sessions.file_id` 已 NOT NULL。

方案：先短事务登记 freeze/operation，再按登记 `library_id/root_id/path_key/file_id` 的目标索引事实分批捕获快照/清理，持久游标推进，API 每批实际放锁。所需磁盘观察仅针对目标且锁外执行，重验版本与生命周期；会话按登记 file_id 有界关闭，删除不可能出现的旧空 ID 分支。保留末库、嵌套根、Subscribe 绑定、用户事实与字节保留合同。验收无关库慢/坏路径不影响 A，百万无关文件下候选/锁时间不随其数量增长，目标分批重开仍守恒。

### S10：Unidentified 必须消费已登记归属

[list_unidentified](../../crates/store/src/library.rs#L341)只返回 path/confidence，丢弃表里现有 library_id/root_id；[HTTP 列表](../../crates/api/src/http/library_admin.rs#L238)先按 Movie 再按 TV 调带 default fallback 的路径 resolver。Movie 未命中真实根也返回默认 Movie Library，因而 TV 分支到不了。

隔离 Router 中 `stored_is_tv=true`，GET `/api/v1/unidentified` 200 却 `response_is_tv=false`；GET `?library_id=<TV>` 返回 200、count=0。应定义携登记 library_id/root_id 的当前 DTO，SQL 按归属过滤并保留 ACL，Claim/交付等消费者固定待认领来源的登记身份，删除读取阶段的猜测/默认补缺；合法 IdentifyAndTransfer 仍可按明确目标生成新的 Library 归属。验收不同 kind、嵌套根、迁移与 Claim，不能通过把 TV fixture 改为 Movie 规避。

### S07–S09：查询与写入成本

Unidentified 的 [reassign_batch](../../crates/store/src/roots/unidentified.rs#L23)在 WHERE/关联子查询里反复 `norm_path_key(u.path)`，逐候选求最深根，之后才 LIMIT 100；没有持久 path_key 范围索引。SQL normalizer 已是纯计算，不再把它误报成 SQL 内 realpath；但更新 100 行仍可能扫描很多无关行或重复已正确归属的历史。

协议索引已正确从请求期全库 JSON 推导迁到写入期；问题是 [增量 terms](../../crates/store/src/library_write/reduce_incremental/terms.rs#L110)仍逐文件调用 [sync_card](../../crates/store/src/protocol_index.rs#L31)，重复 DELETE/INSERT 同一 Media 的 person/genre 与 search_text。公开 `publish_files` 合成探针：已有 TV 卡片，直接写入 100 名演员采用元数据、批前尚无 person_id 索引，一批发布 100 个新文件，person_id INSERT=10000、DELETE=9900，最终只有 100 行。计数用临时数据库 trigger，未改生产 DDL；不是耗时 A/B。用户摘要已做批末去重，不能据此认为协议索引也已合并。

genre/person 预筛使用当前 `terms_value`，不能重复提出“补一个已有索引”。大命中 genre 仍需要命中集解析、精确 locale 过滤、去重/排序及 total；keyset 只减少翻页开销，不消除精确 count。

本轮已将 Media kind、genre 和已筛选 facets 分开测量，详见[当前测量](../benchmarks/2026-10-24-storage-query-rereview.md)：固定 1000 Movie、无关 TV 从 4k 到 49k，Root Movie 页中位从 8.320ms 到 72.928ms，指定 Movie Library 基本稳定；100k Media 的无筛选 facets 中位 1217.737ms，选 Drama 后 3798.064ms。前者在授权 roster 阶段未按 kind 缩小，后者逐 bucket 重复展开过滤谓词。所有数字来自 debug 合成探针，不是 release SLA；旧 50k genre=221ms 及约 70k 超 300ms 外推不冒充当前结果。

还发现观测缺口：items 的 QueryWorkStats 只统计 page statement，不含 count；100k Drama 带 total 从 1.387ms 增至 129.221ms，暴露的 page VM 仍同为 13723。后续验收必须汇总 page/count/facets 和外层锁时间，不能用“小页零 fullscan”证明整个请求工作量有界。

## 死锁与测试结论

本轮没有确认新的生产锁环；运行复现了恢复驱动缺失和无关磁盘错误拒删；源码确认整段持 Store/registry、锁内磁盘观察及同步耗尽工作，按阻塞、饥饿、活性风险处理。本轮未计时复现慢 NAS 的长持锁。SQLite WAL/FULL/busy_timeout 不能消除外层 Mutex 阻塞。

当前 `fixture_lifecycle` 12 项、`rename_startup` 1 项、`root_unidentified` 3 项、`root_reassign_batched` 4 项均通过。旧的 handler/owner join 环、panic 丢失、慢 peer 退出反例已有修复证据，不能继续作为当前缺陷。现有测试中保留无 ID 旧协议是验收方向错误，绿色不代表满足本轮“无兼容”要求。

## 文档处理与验证

本轮删除当前字典中“元数据合成/人工锁冲突未实现”“R12 未按时钟决议”等过时结论，更新会话绑定、NFO 和根迁移的实际范围；ADR-0008 收敛为现行五文件决策。上一轮补修报告固定为 `cf7fb70` 历史证据，保留独有反例/红绿结果；不继续作为“只剩性能”的当前结论。没有删除仍有效的 Spec、验收合同或历史测量。

| 本轮验证 | 结果 |
|---|---|
| `cargo test --offline --workspace` | exit 0；2714 passed / 0 failed / 12 ignored，392 个进程结果汇总，包含 doc tests |
| `cd web && pnpm test` | exit 0；842 passed / 0 failed |
| `cd web && pnpm typecheck` | exit 0 |
| 并发定向现有 binary | fixture_lifecycle/rename_startup/root_unidentified/root_reassign_batched 共 20 passed；与 workspace 重复，不再累计 |
| 隔离公开 seam / Router 探针 | S01/S02/S03/S04/S06/S10 复现；现有 Downloader 测试确认 S05 回退仍有效 |
| 当前生产 DDL 内存核对 | 表/字段/索引/FK 数与字典一致 |

本轮只触及 docs，没有受影响 crate 的代码改动，因此不重复跑单 crate；Web build 未重跑。workspace 的忽略项不计通过，不表示真实环境/规模验收完成。绿色测试仍覆盖部分旧兼容要求，不能据此宣称完成本轮切换。

临时探针与原始日志位于 `/tmp/storage-rereview-cf7fb70/`、`/tmp/storage-concurrency-review-20261024/`、`/tmp/storage-api-review-probe/`、`/tmp/storage-review-query-20261024/`；本页及测量报告保留关键结果，临时目录不是长期证据仓库。16 份变更文档的 189 个相对本地链接及源码行号范围检查通过，`git diff --check` 通过。

真实 NAS、浏览器/Jellyfin 设备、100k Media/1M 文件/1M Playback 的组合验收、连接参数单变量 A/B：**Not run**。自动化通过不能替代这些场景；本轮没有部署或重建实例。
