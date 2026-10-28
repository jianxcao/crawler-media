# 存储重构再次复审与剩余修复方案

日期：2026-10-28。比较基线 `main` / `origin/main`：`756444f76bef53f5fabed6d0ef53e9f280e005e9`；审查代码：`feat/storage-redesign@5730b0320c76992fac3b1517165567c2e6f1bbc6`。已 fetch 核实远端 main；merge base 即该 main。审查开始时工作区干净。

**结论：仍有问题，建议先修复下面 R1–R5 再合并。** 五文件及 library.db 内的事务边界合理；上一轮 F1–F8 大部分修复已经落地，但 F2 的执行器阻塞没有清完，F4 新增的独占路径缺少失败退出。另有已实测的 Integrity guard 历史扫描、分集查询放大，以及未绑定作用域的卡片游标。

本轮只审查、运行现有测试和临时复现、更新文档；不修改 Rust/TS/SQL/测试，不执行修复方案，不操作用户实例或重建数据库，不创建 issue。下文行号固定于上述代码 SHA。相对 main 的范围为 1,524 个文件，183,257 行新增、65,235 行删除；重点追踪存储与消费者链路，不声称逐行穷尽整个 diff。

## 架构与无兼容切换判断

- 保留 `app.db` 配置/凭证、`library.db` Media/Library/Playback 权威事实、`subscribe.db` 策略/下载事实、`jobs.db` 执行事实、`cache.db` 外部响应的分工。Media、文件、覆盖、采用元数据与摘要同库事务维护，比 main 的跨文件更新更合理；不建议再次拆库。
- 文件的显式 Library/root 归属、完整版本 CAS、冷详情、单位覆盖、稀疏用户摘要、协议成员/边际计数均有明确用途。不能为减少表数删掉恢复状态或必要摘要；永久删除根边界也不能按普通历史清理。
- 当前 library schema 为 **v7，53 表、594 字段、80 个显式索引、37 组 FK、0 触发器**。DDL 统计不等于业务完整性验收。权威源码及所有分区统计见[数据库入口](../database/README.md)。
- 上轮具体旧合同已删除：`library_name/name` 双字段、Library operation `state/phase` 回退、merge 缺失/旧 phase 回退；Library PATCH 拒绝旧字段。旧 `/directory`、旧 Naming 拆解、Downloader settings JSON 回读和无签发 self Playback 没有因本次修复恢复。当前 schema 拒绝旧布局，不导入、不双读双写；没有发现需保留的旧数据库兼容层。
- 本轮核查的存储相关前端消费者已使用当前合同，列出的旧字段/双形状未见残留；849 项测试、类型检查和构建通过，不代表逐条穷尽所有 API。仍存在的 `phase`、`library_name` 等词须按所属 DTO 判断；Jellyfin 协议映射、外部元数据失败回退、文件多版本和合法默认值不属于旧自有 API 兼容。

## 剩余发现

| 编号 | 优先级 | 触发与影响 | 与前轮关系 |
|---|---|---|---|
| R1 | P1 | Store 争用让 up-next、扫描协调及部分收尾占住 Tokio worker；无关定时器也延迟 | F2 / 更早 Task 22 未完整完成 |
| R2 | P2 | 首次 NFO 导出遇人工编辑后留下无法恢复的活跃 owner，新导出及 Library 配置一直被拒绝 | F4 原竞争已挡住，失败退出仍缺 |
| R3 | P2 | Integrity 写屏障扫描全部终态 operation 历史，影响播放进度等高频写入 | F8 修的是另一条查询，不能覆盖此项 |
| R4 | P2 | 分集状态批量读超出请求范围，另一分集接口仍逐集点查 | 前轮额外优化未完成 |
| R5 | P2 | 同一游标换库或换排序仍被接受，静默漏项/空页 | 前轮游标作用域建议未完成 |

### R1：同步 Store 等待仍在异步执行器上

关键位置：

- `crates/api/src/http/playback_views/up_next.rs:87`、`:93` 同步调用候选读取，`:121` 获取 Store；仅后续 artwork 阶段进入 blocking budget。
- `crates/api/src/scan_operations.rs:78`、`:94` 的 `trigger_next` 同步 claim；`:204` 的 `RunningGuard::drop` 从异步扫描任务调用它。
- `scan_operations.rs:325` 调 `acquire_library_work`，经 `library_lifecycle.rs:202` 的 Registry → `:209` Store 同步锁。
- `http/library_scan.rs:34` 的 scan/enqueue、`http/library_scan/operations.rs:57`、`:93` 的 cancel、`library_lifecycle.rs:91` 的删除异常收尾仍有相同问题。
- `http/library_scan.rs:521`、`:544` 的 item-index，以及 `http/library.rs:582`、`:598` 的分集查询也在 async handler 内拿 Store。

**已运行复现：** ApiState 在 runtime 外建立；另一 OS 线程持 Store 500ms，在 current-thread runtime 中并发执行真实 `GET /api/v1/playback/up-next` 与 10ms timer。请求返回 200，timer 为 **502.608ms**；直接调用异步扫描收尾所用的 `trigger_next`，timer 为 **513.872ms**。两个路径均链接当前 api/store 编译产物，无真实网络。不同运行有约 1ms 波动，不影响阻塞结论。

同一修改还把 `scan_operations.rs:290`–`:450` 的 `run_operation` 扩至 161 行，超过 AGENTS 的生产函数 120 行硬上限；准入、线程执行与异常收尾也确实是不同职责，应随此项拆分，不以文件未超 800 行代替函数检查。

这是执行器阻塞；没有据此证明 SQLite 永久锁环。多 worker 运行时也会被足够多并发请求占满。SQLite busy_timeout 不限制外层 parking_lot Mutex 等待。

**修复方案：** HTTP 读取复用 `store_read`；写入使用既有 `run_to_completion` 与有界等锁，不以外部 timeout 在写入仍执行时提前报告失败。扫描 `Drop` 只以非阻塞方式发送完成/唤醒通知，由单一协调器维护运行集合并在阻塞边界执行 claim，或使用专用协调线程。现有 claim 会持 running_ops 等 Store，不能只把原函数 offload 后让 Drop 再等同一协调锁；不得跨 Store 等待持有 async/Drop 路径也会获取的协调锁。若保留短锁，先释放再等 Store，并用 reservation/CAS 保证准入唯一性。继续遵守 Registry → Store 顺序，等待工作排空时不持这两把锁。扫描 walk 保持专用线程；不能让占满 blocking pool 的 walk 再等待同池 bookkeeping。item-index 的全库排序遍历应走现有 reader snapshot，执行后重验凭据/ACL/config，避免把整个索引生成放在 Store guard 内。

**验收：** 同样 500ms 受控争用下，10ms timer 在 200ms 内完成；覆盖 up-next、scan 准入、取消、`Drop` 通知、删除异常收尾、item-index 与分集。HTTP busy 使用已有 503/Retry-After；写请求等待真实结果。测试始终先释放持锁线程并 join，再做响应性断言；同时验证有限 blocking 容量、取消、panic 和重启，不只覆盖成功 handler。按准入、执行/汇合、终态处理拆分 run_operation，每个生产函数不超过 120 行，避免只提取参数透传包装。

### R2：NFO 失败后永久占有 sidecar

位置：`crates/store/src/metadata/export_driver.rs:463`–`:465` 写失败直接返回；`:505`–`:536` 的无基线恢复保持 pending；`metadata/export.rs:160`–`:166` 拒绝第二个 owner；`crates/api/src/scrape_metadata/export.rs:105`–`:123`、`:148`–`:154` 的错误处理只记日志。

**已运行的确定性交错：**

1. 文件存在、sidecar 不存在、没有旧导出基线，持久化 A 的 plan。
2. 用户创建有效 `<movie><title>Hand edited</title></movie>`；A 的写入因 digest 不符返回 Conflict，writer 已退出。
3. B/C 新 plan 均报 `nfo path ... owned by operation-a`；Library locale 变更报 `already has an operation`。
4. 重开 Store，恢复 A 连续两次均报 `refused to overwrite externally changed`，pending 仍为 true；D 新 plan 仍被拒绝。人工 NFO 字节保留。

活跃 Scrape 会阻止改根/语言/Integrity/删除（`library_operations/integrity_guard.rs:74`）。恢复在取得实例租约后的 startup work 执行（`api/src/job_loop.rs:183`、`:202` → `storage_operations.rs:234`）；此意图没有普通运行期周期重试。通用 retry 只接受 failed roots/locale/default_locale/integrity（`http/library_operations.rs:214`），没有可处理 planned Scrape 的取消/失败出口。

**修复方案：** 在驱动确认原 writer 已退出后，把“源已被外部修改且无安全基线”等不可重放冲突，以当前 operation_id/owner CAS 转成终态 failed，记录原因并保留人工字节；允许后续请求重新观察。重新观察须按既有 Library-scoped NFO 输入/采用规则保留有效人工字段后再渲染，不能只更新 digest 后覆盖，也不能把失败计划伪造成已完成的导出基线。临时 IO 错误如需保留 pending，必须有真实、有界的重试调度和明确取消出口。终态转换和路径释放必须由拥有该次执行的驱动负责，不能为解锁删除仍可能写盘的意图。继续保留 v7 路径唯一性，不恢复“新意图覆盖旧意图”。

**验收：** 首次导出无基线/外部编辑、不可读或无效 NFO、临时 IO、写盘成功后登记前崩溃分别有公开 driver 回归。不可重放冲突退出后，人工字节不变、旧行 failed、新计划可创建、Library 配置可继续；后续成功导出仍保留人工字段，或明确停在可操作的待人工解决状态，不能只验证旧失败时未覆盖。重启行为一致。可重放失败必须能实际收敛。路径竞争回归继续通过。

### R3：Integrity 写屏障随不相关操作历史线性增长

位置：`crates/store/src/library_operations/integrity_guard.rs:55`–`:65`：先从 `library_operations o` 选 `kind='integrity' AND state<>'succeeded'`，再按 Media 存在性检查；当前计划为 `SCAN o`。调用包括 `playback/event.rs:68`、`playback/mutate.rs:325`、`metadata/adopt.rs:34`、`library_write/publish.rs:285`，因此不限于少量管理员操作。

**当前 bundled SQLite 3.50.2 实测：** 临时生产 schema；固定一个合法、无文件 Media，仅增加无关 succeeded Scrape 历史，执行 ANALYZE 并重开 Store，以载入统计。测公开 `Store::update_media` 的不变更新来隔离 guard 成本。没有重开、还用旧统计的连接可选出不同计划，不能省掉这一复现条件。

| 无关终态历史 | 当前公开调用 VM / Fullscan / SQL数 | 仅临时库加候选索引后 |
|---:|---|---|
| 1,000 | 3,082 / 999 / 6 | 75 / 0 / 6 |
| 20,000 | 60,082 / 19,999 / 6 | 75 / 0 / 6 |

隔离 guard SQL 本身为 3,009 → 60,009 VM，候选后均为 8；Sort 均为 0。候选仅用于临时数据库 A/B，不是已实施变更或生产延迟承诺。

**修复方案：** 优先验证 `library_operations(library_id) WHERE kind='integrity' AND state<>'succeeded'` 的部分索引，同时覆盖按库与按 Media 的两种 guard。也可改查询从有关 Library 开始，但必须保留当前冻结保护语义，特别是 failed Integrity 仍可恢复且仍应拦写。索引变更同步提升布局版本、对象声明、字典与旧布局拒绝测试，不增加迁移；不能用缩短保留期代替消除热点扫描。

**验收：** 真实 Store 写入口与签发会话的进度事件均覆盖；固定有关事实，将无关终态历史 1k → 20k，记录完整调用 SQL数/VM/Fullscan/Sort。加入活跃 Integrity 正确拒绝、无关库不拒绝、failed 继续冻结的对照。量插入/完成 operation 的写入与页成本后再保留候选索引；当前数据只证明读收益。

### R4：分集状态读取未按请求范围收敛

两个不同问题，不能混写为二次方扫描：

1. Playback 的 `http/playback/episode_listing.rs:41` 先取整部 coverage；`:57`–`:60` 的 `effective_unit_states` 虽只做两条状态查询，但 `store/src/playback/assertions.rs:127`、`:174` 的 SQL 只限定 User/Media，加载整部的断言和状态。请求一季仍受其他季文件/历史数量影响。
2. Library 的 `http/library.rs:598`–`:610` 按每集调用 `effective_watch_state` → `playback.rs:74` 的 Media 查询，加 own/whole/season/episode 点查，约 **5 条 SQL/集**。这一路是重复点查，不是每集加载全量快照。

**当前 bundled 引擎的实际状态 SQL 工作量：** 固定请求 S01E01/E02，只增加 S02 的用户断言/行。

| 无关分集状态行 | 两条 SQL 解码行数 | 两条 SQL VM | Fullscan / Sort |
|---:|---|---|---|
| 100 | 100 + 100 | 1,012 + 2,012 | 0 / 0 |
| 10,000 | 10,000 + 10,000 | 100,012 + 200,012 | 0 / 0 |

这测的是生产 SQL，不是整个 HTTP 的耗时；Fullscan=0 仍可能扫描大量索引区间。现有批量函数的 SQL 数常量不等于请求工作量常量。

**修复方案：** 批量接口只读取请求 UnitKey 集合及其必要 whole/season 祖先，覆盖读取也按 Library/Media/季收窄；统一有效断言归约。Library 分集复用这个受限批量入口；不能直接换成现有整部批量函数再宣称完成。若键数超过 SQLite 绑定预算，按固定键窗口读取，并保证同一读快照。同步按 R1 移出执行器/长 Store guard。

**验收：** 两个真实分集 HTTP 入口对相同可见逻辑单元返回一致的有效观看状态，并分别保留各自的列表语义和响应形状；较新 whole/season/episode、同时间 specificity、用户隔离全部保留。固定返回两集，把其他季状态/文件 100 → 10k，完整工作量不随无关数据增长；增加返回集数时，SQL 依固定键窗口增长，避免逐集查询。记录解码行和 VM，不能只断言 Fullscan=0 或 SQL 数=2。

### R5：卡片游标不验证查询作用域

位置：`crates/store/src/library_query/items/page.rs:51`–`:54` 的 PageCursor 仅有 keys/media_id；`:344`–`:350` 解码后构造 seek，未验证 Library/User/filter/order/direction 或键类型。对照 `library_query/details/episodes.rs:79`–`:87`，UnitCursor 已有 scope 校验。

**Store 公共 API 已复现：** A 库 `[M,Z]` 第一页得到 M 的标题游标；B 库正常结果 `[A,B,X]`，复用该游标被接受并仅返回 `[X]`。将同一标题游标用于 Rating 排序也被接受，返回 `[]`。这是错误上下文被静默接受，当前 ACL 仍限定目标 Library，没有证明跨库数据泄漏，也未证明正常 UI 一定会发出该请求。

**修复方案：** 当前唯一游标格式加入版本和规范化 scope 指纹，至少覆盖 Library、User 状态作用域、filter、order、direction、排序键类型；索引桶与分页共用同一编码/校验。换作用域或类型不符返回 Invalid/HTTP 400；删除无 scope 旧格式，不做解码回退。无需为此引入持久游标表或兼容转换。

**验收：** 同 scope 连续分页稳定；换 Library/User/filter/order/direction 均拒绝；title 游标投 Rating、键类型损坏、缺失版本/scope 均 400；item-index 签发的桶游标仍可用于对应列表。数据自然变化的 seek 语义与 scope 不匹配分别测试。

## 上轮修复逐项对账

前一轮审查代码为 `eb79bacd`，F1–F8 修复提交为 `5730b032`。原修复方案的 unchecked 不再用作当前待办。

| 旧编号 | 当前核对 | 证据/剩余 |
|---|---|---|
| F1 有效观看状态 | 功能修复已落地 | `storage_review_playback` HTTP 覆盖整季/整部、取消、同时间、用户/重开；性能另见 R4 |
| F2 executor 等锁 | **部分完成** | Transfer 等已 offload；R1 的真实生产路径仍阻塞 |
| F3 failed Scrape 阻止准入 | 已修 | admission 复用 owning unfinished predicate；`terminal_operation_admission` |
| F4 新意图替换旧 writer | 原交错已阻止，**失败退出未闭环** | v7 唯一路径 owner、`nfo_export_ownership`；R2 是新失败边界 |
| F5 Barrier 错误路径挂死 | 已修该反例 | channel deadline/取消；`fingerprint_source_safety/rendezvous.rs` 有失败释放/无信号/掉线回归 |
| F6 旧字段/双形状/phase 回退 | 已修列出的缺口 | 严格 PATCH、单字段 Web 消费、无效 merge phase 拒绝；当前 Web 三门通过 |
| F7 NFO 秒写毫秒 | 已修 | `playback_clock.now_ms`；真实 export glue 的 retention 回归 |
| F8 pending Scrape 扫终态历史 | 已修该查询 | `nfo_export_pending_age`；`nfo_export_work` 覆盖 pending 0/1/20、history 1k/20k；R3 是不同 SQL |

更早 `710ac083` 的 R1–R10 经 `eb79bacd` 修复，当前代码已有完整 Presence 版本验证、多单元标记显式坐标、pending 发布顺序、毫秒时钟、完整 SDR 清 HDR、严格 Library 创建、Presence 窗口和去重归约、锁外 Subscribe 快照，以及旧死文件删除。相关回归仍在测试树；旧静态反例不能直接重报为当前缺陷。其 T9 的文件清理、存活 session、终态保留索引已进 v6，`storage_cleanup_work` 对 100/1k/10k 历史验证工作量；删除根墓碑未纳入 retention。

历史 pending Scrape 查询曾在 bundled SQLite 下随 1k/20k 终态历史使用 7,012/140,012 VM、999/19,999 Fullscan、1 次排序；前轮候选为 9 VM、0/0。此为前轮数据，保留用于解释 F8，不能当本轮重新跑过的结果。

## 还值得做但没有证据要求立即改设计的优化

| 项目 | 当前判断 | 下一步验收 |
|---|---|---|
| 类型/genre/person/精确 total/facets | 成员投影、边际计数、kind 收窄与页内选源已存在；没有证据要求恢复旧 projection 或换数据库 | 保留现有工作量门禁。历史 [A 验收](../benchmarks/2026-10-25-large-hit-query-read-model.md)、[B 隔离](../benchmarks/2026-10-25-library-read-isolation.md) 的版本/规模不得冒充本轮实测 |
| item-index 全库排序 | 桶计数本身可随命中规模增长；目前在 Store guard 内，首先按 R1 迁出 | 固定当前库、增加无关库；测 VM/解码/Store 持锁时间与 reader snapshot 生命周期 |
| Store/cache schema 身份 | header/对象名校验不等于列、FK、CHECK、索引定义一致；Jobs 校验更严格 | 独立加固可比较生产 DDL 的规范化定义并对缺约束布局零写拒绝；当前不是发现自动迁移旧库 |
| 独立 ResponseCache 连接策略 | Store 的 cache 为 NORMAL 并调 cache/mmap/temp；独立 media ResponseCache 只显式 WAL/timeout/FK | 先测写入 fsync、争用和 RSS，再决定共用策略。权威四库保持 FULL |
| 进一步合并索引 / WITHOUT ROWID | v4/v5 已移除明确冗余；`playback_units` 不转换有历史空间证据。旧 schema audit 的 R5（指纹重复列）、R7（协议 terms 合并）、R8（根索引收窄）、R12（显式派发序后转换）仍待评估，不算已完成 | 先列全部真实读写路径和页成本；R7 涉及 facet 语义，须另评审，不能作为机械性能改动。历史空间数字不承诺当前收益 |
| 完整容量与设备 | 100k Media / 1M Files / 1M Playback 组合、NAS/真机、OS 冷缓存、掉电尚未本轮验证 | 单独环境验收，不把未运行项写为通过；普通测试不接真实网络/Downloader/Chromium |

R1 → R2 → R3 → R4 → R5 为建议实施顺序；R1 与 R4、游标与 item-index 有共同文件，避免同时改写。每项先在公开 seam 固定反例 red，再实现 green；每个触及 crate 单独 `cargo test -p <crate>`，末尾 workspace 一次；Web 有改动再跑三门。改 DDL 同步版本/对象校验/字典；任何任务均不添加旧布局或旧 DTO 兼容。

## 本轮验证与边界

- Web：`npm --prefix web run typecheck` exit 0；`npm --prefix web test` **849 passed / 0 failed / 0 skipped**；`npm --prefix web run build` exit 0，存在原有大 chunk 提示。
- Rust：`cargo test --workspace --no-fail-fast` exit 0，**3,127 passed / 0 failed / 24 ignored**，472 个测试程序/文档测试结果段；ignored 不计为已验证。本轮只改文档，没有触及需要另跑单 crate 门的源码。
- 临时反例：真实 HTTP/current-thread 响应性、NFO 公共 driver 重启阻塞、Store 游标跨上下文、生产状态 SQL 规模、Integrity guard 公共写入口 A/B 均已运行。反例断言通过意味着缺陷成立，不意味着修复通过。
- 测量引擎为项目 rusqlite bundled SQLite **3.50.2**；DDL 字典统计用 Python SQLite **3.54.0** 的内存库，仅作结构统计，不作运行性能证据。
- Rust 文件行数检查未发现超过 800 行的 .rs；已确认 run_operation 为 161 行，R1 要求修复此函数硬上限，未声称全仓每个函数均已逐一通过大小检查。
- 生产 SQLite 永久锁环和新的测试永久等待没有得到确认；R1 是执行器停顿，R2 是持久业务状态阻塞，不混称数据库死锁。workspace 正常路径通过也不能证明所有故障交错安全。
- 没有运行真实 TMDB/Downloader/Chromium、真实浏览器或 Jellyfin 客户端、NAS/掉电/实例切换。没有修改运行代码，也没有应用上面的候选索引。

临时源码/日志用于此次复核，不是项目依赖，`/tmp` 不保证长期存在：`crawler_review_executor.rs`、`crawler_review_nfo_orphan.rs`、`crawler_review_integrity_work.rs`、`crawler-review-cursor.rs` 及对应日志；主测试日志 `/tmp/crawler-storage-rereview-workspace.log`。下面保留可移植重现场景，永久测试按项目 fixture 拆分，不把临时程序整份塞进生产代码。

### 可移植复现步骤

**R1：** 在 runtime 外用临时 Store、fake Fetcher/MemoryDownloader 创建 ApiState 和 Router；外线程持 `state.store().lock()`，用 channel 确认拿锁后保持 500ms。在 current-thread Tokio 中 `join!(GET up-next, sleep(10ms))`，记录 timer，join 持锁线程后断言 `<200ms`；当前约 503ms。以同样步骤调用 `scan_coordinator.trigger_next`；始终让 holder 独立于 runtime 到期释放。

**R2：** 临时 Movie Library、Media、一个发布文件；依次 `nfo_export_target`、`plan_nfo_export`（sidecar 不存在）→ 写有效人工 NFO → `write_planned_nfo` 返回 Conflict → 新 operation 计划应能在旧 driver 明确退出/终态后建立，当前失败。重开 Store，再 `resume_nfo_export_phased(|| &store, &old_request, now_ms)`，当前重复失败且 `nfo_export_pending(old_id)==true`。同时验证 `begin_library_reconfiguration` 当前被阻止，以及人工 NFO 原字节不变。

**R3：** `Store::open(temp)`、`insert_media`；通过临时 SQLite 连接事务插入 1k/20k succeeded Scrape 历史（所有 v7 必填列、非空 resource_key；可使用已删除 Library 的历史 ID），`ANALYZE` 后关闭/重开 Store。`with_stats(StatsScope::Library, || update_media(&same_media))` 记录上表；再仅在临时库创建候选索引，重复公共调用。量 SQL 时使用当前 `StatementStatus::{VmStep,FullscanStep,Sort}`，不是机器自带 sqlite3。

**R4/R5：** 使用固定标签生成 Media 和 `publish_fixture_file`。分别建 A `[M,Z]`、B `[A,B,X]`，`LibraryQuery` 的 Title cursor 页签发 M 游标；将原游标不改直接用于 B，再用于 Rating，当前均成功，修复后应 Invalid。状态规模实验向同一 User/Media 的另一季写 100/10k 断言，仅请求固定两集；对 `assertions.rs` 两条原 SQL 记录解码行及 statement status。完整 HTTP 门还须覆盖 coverage 读取和归约，不能以这两条 SQL 的数字代替整条请求。

## 文档收敛记录

本页是当前审查与剩余方案入口。移除以下已被实现/新复审替代的执行稿，避免照旧 unchecked 重做或相信旧“全部完成”：

- `docs/superpowers/plans/2026-10-27-storage-redesign-review-plan.md`：原 R1–R10/T9 对账见上；实现提交 `eb79bacd`。
- `docs/superpowers/plans/2026-10-27-storage-redesign-main-review-v2.md`：F1–F8 对账、未完成额外优化、历史 pending 查询数字见上；修复提交 `5730b032`。
- `docs/superpowers/plans/2026-10-26-storage-redesign-review-fixes.md`：旧 v2→v5 步骤和相互冲突的完成状态撤下；独有逐表/索引决策与测量保留在 [schema audit](2026-10-26-library-schema-audit.md)、[perf baseline](2026-10-26-perf-baseline.md)、[Task22 锁清单](2026-10-26-task22-on-executor-store-locks.md)。Task22 余项仍由 R1 跟进，不自动关闭。
- `docs/superpowers/plans/2026-10-26-codex-review-brief.md`：旧 review-base/命令提示稿已过期，不再作当前审查入口；对应独立报告标为历史。

原文均可 `git show 5730b032:<上述路径>` 取回，包括旧复现程序、任务勘误与审批上下文。较早审计/benchmark 的独有数据不删除、不改成新版本结果；10-23 执行记录和 10-25 A/B 计划明确为历史。数据库入口、ADR-0008、主 Spec 和 API/部署的当前指针同步到本页；DDL 仍只在生产源码维护。
