# 存储重构再次复审与剩余修复方案

日期：2026-10-28。审查代码：`feat/storage-redesign@9f6126d358c18afc532f6b551dadccdfa8ed0558`；比较基线 `main/origin/main@756444f76bef53f5fabed6d0ef53e9f280e005e9`，merge base 相同。开始时工作区干净。下文行号固定于该 HEAD；本文后续文档提交不属于被审查实现。

**结论：仍有 3 项 P1、1 项 P2，建议修复后再合并。** 五文件与 library.db 内的事务边界合理，上一轮 R2–R5 已有实质修复；R1 的主路径修好了，但执行器阻塞仍有遗漏。新增确认了写门禁绕过和 Media 冻结/退役后仍可写的问题。无兼容要求也尚未完全落实。

本轮只 review、运行现有测试及临时离线反例、更新文档；不修改 Rust/TS/SQL/测试，不部署、不改用户实例、不创建 issue。相对 main 共 1,527 文件、184,022 行新增、65,353 行删除；本轮重点追踪存储、生命周期、查询及消费者链路，不声称逐行穷尽全部差异。R6 明确属于 main 已存在的缺口，其他发现分别说明其来源。

## 架构与数据库判断

- 保留 app.db（配置/凭证）、library.db（Media/Library/Playback 权威事实）、subscribe.db（策略/下载事实）、jobs.db（执行事实）、cache.db（外部响应）的生命周期分区。Media、文件、覆盖、采用元数据与摘要同库事务，比 main 的跨文件更新更合理，没有证据要求再次拆库或换数据库。
- 显式 Library/root 归属、文件版本 CAS、原始 Playback 与物理恢复包分离、冷详情、单位覆盖及增量摘要均有用途。跨文件没有 FK/原子提交，持久操作负责恢复；R7 表明恢复正确性还依赖每个写入口遵守冻结合同，不能仅凭阶段表存在就验收。
- 当前 library 布局 **v8：53 表、594 字段、81 个显式索引、37 组 FK、0 触发器**。五文件结构与唯一 DDL 见[数据库字典](../database/README.md)。表数/文件大小本身不是不合理设计的证据，热点工作量和写入代价才是优化依据。
- 旧数据库布局拒绝、不导入、不双读双写；旧 directory 聚合、Naming 拆解、Downloader JSON 回退、无签发 self Playback 已删除。当前 Web 未发现继续调用这些旧入口，静态图片仍在 public 路由。仍有 R9 的旧输入别名/偏好转换；因此不能宣称“所有兼容已经清除”。Jellyfin 协议映射及外部 Metadata source 合法失败回退按各自合同处理。

## 剩余发现与技术方案

沿用既有 R1–R5 编号用于历史对账，本轮新增从 R6 开始。

| 编号 | 优先级 | 确认的问题 | 来源 |
|---|---|---|---|
| R6 | P1 | metadata 变更路由绕过实例只读与维护写门禁，受保护期间仍能删文件事实 | main 已存在，本轮确认；属于当前存储安全合同缺口 |
| R7 | P1 | Playback mark 可写 merging/retired Media；阶段恢复后可能永久卡在 retire | 当前重构冻结/恢复合同未闭环 |
| R8 | P1 | Media operation 等锁及 up-next 补图仍阻塞 Tokio worker | 原 R1 / Task22 剩余项 |
| R9 | P2 | 新查询保留旧 watch 别名，Web 仍转换旧排序偏好 | main 旧逻辑保留，最新游标又延续别名 |

### R6：metadata 路由绕过写门禁

位置：`crates/api/src/http/routes/mod.rs:73` 直接 `.merge(admin_metadata_routes())`；对照同文件的 `write_gate` 会先检查 `require_writable`，再检查 `require_write_admitted`。`routes/admin.rs:253` 注册的 `DELETE /ledger/{id}` 进入 `http/library_admin.rs:129`，handler 没有补这两道检查。

**已运行公开 HTTP 反例：** 临时 Store、有效管理员 token、发布文件事实，fake Fetcher/MemoryDownloader，无网络。

| 条件 | DELETE /api/v1/ledger/{id} | 对照 POST /api/v1/libraries |
|---|---|---|
| `state.with_instance_writable(false)` | 200，deleted=true，数据库文件行确实消失 | 409 instance.read_only |
| 持有 `state.write_gate().close()` | 200，文件行确实消失 | 503 runtime.busy |

反例未指定 delete_file=true，视频字节保留；没有把未运行的磁盘删除也写成实测。本缺口在 main 已存在，不能归因于最新修复；本次数据库设计仍承诺只读实例和维护期间拒绝变更，因此需要闭环。

**方案：** metadata 路由组的变更统一经过现有 write_gated，避免每个 handler 自己补一份判断。逐项核对真正只读的 GET 和 preview，只有无持久副作用的 preview 才可豁免；不得按名称假定安全。维持实例只读 409 优先、维护 503+Retry-After 的现有合同，写门检查位于任何数据库/磁盘副作用之前。核对其余存储变更路由是否已经由组或 handler 负责同一准入，避免再次遗漏。

**验收：** 真实 Router 对 metadata 组每个变更入口覆盖两个关闭条件，分别断言 409/503+Retry-After、文件事实/摘要/操作行/磁盘字节不变；正常写入和安全读取作为对照。另测关闸前已准入请求与维护开始的交错，核对连接串行与预算结果；原子门禁标志本身不证明在途写已排空，不能仅检查响应码。

### R7：Media 冻结与退役写屏障遗漏 Playback mark

位置：`crates/store/src/playback/mutate.rs:324`–`:335` 仅检查 Integrity owner，随后直接 UPSERT playback_units；没有检查 Media identity_state。`http/playback/watch_state.rs:362`–`:377` 调用该入口，而 `http/playback/selection.rs:139` 对 whole 坐标直接放行。合并恢复 `media_operations/apply.rs:123` 按阶段继续，进入 subscribes/retire 后不再执行原 library 合并。

**已运行证据：**

1. Store 公共入口 begin_media_merge 冻结双方；mutate_playback(loser) 仍返回 Ok。完成合并后再写 retired loser 仍返回 Ok，新增事实重开 Store 后依然存在。
2. 真实 `POST /api/v1/playback/marks` 对 merging loser、retired loser 均返回 **200**，收藏事实确实写入。使用有效 User/token，不是绕过 HTTP 的内部 SQL 写入。
3. 临时库安装故障触发器，仅令 phase_cursor 向 retire 推进失败，使持久阶段停在 subscribes（模拟 library 事实提交后跨文件边界中断）；此时通过公共 mutate_playback 写 loser。撤掉故障后连续两次 advance_media_merge 均因 `references remain (playback_units=1)` 失败，游标停在 retire，两个身份保持冻结。初始故障已消失，恢复仍不能收敛。

这是持久业务状态阻塞，没有证据把它称为 SQLite 锁死。成功合并后允许对旧 ID 继续写也违反“retired 只作拒绝墓碑”的无兼容合同。

**方案：** 在 Playback 公共写入口的同一 library 写事务内复用身份写屏障，merging/retired 拒绝后零写；不能只在 HTTP 层先查状态。盘点 mark、轨道编辑、直接 unit 写入及签发事件的生产路径，保留已有旧会话 IgnoredStale 语义，并确保迟到事件不会制造可写 loser 引用。将 Protected 映射为明确冲突及当前 survivor 信息，不自动把旧 ID 写重定向给 survivor。合并驱动继续只按已提交阶段推进；不得用每次重跑 library phase 来掩盖新引用，否则可能重复归并用户事实。

**验收：** Store 和真实 self/Jellyfin mark seam 覆盖冻结、退役、正常身份；冻结/退役写入被拒绝且无用户行/摘要变化。固定上面的跨阶段中断，尝试晚到写入、重启后恢复，合并必须成功且 survivor 事实守恒；已签发会话、收藏、显式撤销与多 User 隔离全部保留。首次重现故障的 trigger 只用于 fixture，生产不加触发器兼容层。

### R8：执行器阻塞仍有两个生产分支

- `http/media_operations.rs:132`、`:159`：async POST merge / GET operation 直接等待 Store Mutex。
- `http/playback_views/up_next.rs:491`–`:493`：`tokio::spawn(async {...})` 内同步调用 attach_backdrop。后者在 `poster_fetch.rs:605`、`:612` 获取路径租约/Store，并在 `:636` 调同步 Catalog，还可能做文件/HTTP IO。async 包装不会把同步工作移出 Tokio worker。

**已运行真实 HTTP/current-thread 反例：** Media operation GET 遇外线程持 Store 500ms，10ms timer 延迟 **506.163ms**（请求 404）；up-next 有真实候选且缺 fanart，fake Catalog 耗时 500ms，10ms timer 延迟 **510.400ms**（请求 200）。无网络；第一个 holder 独立到期并 join 后才断言，测试不会因缺陷永久等锁。现有空 up-next 回归没有覆盖补图分支。

**方案：** operation 读复用 store_read；写复用有界准入与 run_to_completion，响应真实完成结果，避免超时返回后写入仍继续却宣称未应用。背景图片补全放入有界 blocking 执行并按目标去重；保留 Registry→Store 顺序，磁盘/网络 IO 锁外执行。扫描已有专用线程，不再把它与补图混进无界同步 async 任务。

**验收：** 上述两个公开 HTTP 场景下 10ms timer 在 200ms 内完成；覆盖非空候选/缺图、Store 争用、busy、取消、panic 及副作用最终结果。先安全释放并 join holder，再断言响应性。当前未确认新的 SQLite 永久锁环或测试永久等待；这项是执行器饥饿，busy_timeout 不限制外层 Mutex 等待。

### R9：无兼容切换尚有旧值转换

位置：`store/src/library_query/items/filters.rs:84`–`:85`、`:150`–`:158` 接受 watched/unplayed/unseen；新 `items/cursor.rs:84`–`:88` 将这些值转换为 played/unwatched。它们来自 main 的旧过滤器；当前 Web 没有这些拼法的调用。

**已运行 HTTP：** items/facets/item-index 对三种旧 watch 值的 **9 个请求均 200**，对照 nonsense 为 400。`seen` 是当前首页“最近观看”的独立业务语义，不能随旧别名删除。

另有 `web/lib/home-rows.ts:253`–`:265` 明确保留旧 release_date_asc 偏好转换，`web/test/home-rows.test.mjs:190` 起仍验收此兼容行为。当前部分 Web/API 的排序查询也实际使用 release_date_asc，所以若统一排序表示，必须同批迁移消费者，不能只删后端枚举。

**方案：** 只接受当前 watch 五值 unwatched/watching/played/favorite/seen，删除旧验证、SQL 分支与游标规范化。删除旧偏好专项转换，非法偏好按普通默认值处理。若统一旧排序表达，采用 release_date+order=asc，批量更新 Web enum、query builder、后端 parser 与测试，只保留一种表达，不做双格式读取。先明确当前合同再删除，不能把合法 Jellyfin 值当旧自有 API。

**验收：** 旧 watch 值在所有共享查询入口（含 gallery）返回 400，五个当前值保持正确结果；新游标 scope 回归继续通过。旧偏好不再得到专用映射，当前排序往返/首页顺序通过。用真实 HTTP/adapter 行为断言，不仅做源码字符串扫描。 `store/tests/probe_stage_state.rs:270` 的 v6_database_migrates_without_losing_probe_jobs 实际只创建并重开当前 v8，名称/注释仍声称迁移；下次清理应改为当前布局重开语义，配合既有旧布局拒绝门，不能把这个绿色测试当作迁移存在的证据。

## 上轮修复逐项对账

原 R1–R5 针对 `5730b032`，修复提交为 `9f6126d3`。旧完整反例与数字可从 `git show 9f6126d3:docs/audits/2026-10-28-storage-main-rereview.md` 读取，不再重复维护已完成实施步骤。

| 原项 | 当前判断 | 本轮证据/剩余 |
|---|---|---|
| R1 / F2 / Task22 执行器 | 原点名的扫描协调、主 up-next DB 阶段、分集与 item-index 已修；整体**部分完成** | executor_not_blocked 5 passed；残余生产分支见 R8 |
| R2 NFO 失败 owner | 原冲突/失败退出已修 | nfo_export_ownership 15 passed；精确 plan CAS、每路径执行 lease、失败终态释放及重启回归；不把代码存在扩大成真实设备验收 |
| R3 Integrity guard 历史扫描 | 已修 | v8 部分索引，公开 update_media/progress 对无关历史 1k→20k 工作量稳定，含 failed 继续冻结对照及索引写成本 |
| R4 分集超范围与逐集点查 | 原缺陷已修 | UnitKey+必要祖先、400 键窗口、同一读快照；状态/coverage 规模回归通过。完整 HTTP 跨线程 SQL/解码汇总尚未完成，见后续 O1 |
| R5 游标无 scope | 已修 | 唯一 version/scope/键类型校验，跨库/User/filter/order/direction 拒绝；无旧游标解码回退 |

更早 F1/F3/F5/F6/F7/F8 与 R1–R10/T9 的关闭映射保留在同一 Git 历史和各固定版本审计；本轮未把当年的反例重新报成当前缺陷。F6 当时列出的字段已删，并不意味着未列出的所有旧别名都已清除（R9）。

## 查询性能与真正未完成的优化

本轮复跑现有公开 Store 工作量门，bundled SQLite **3.50.2**。下表是当前代码的 SQL 工作量；debug 与 workspace 并发运行，不以墙钟冒充 release/NAS 延迟。

| 数据变化/固定请求 | 当前结果 |
|---|---|
| 无关终态 operation 1k→20k | update_media 均 7 SQL / 109 VM / 0 Fullscan；签发会话 progress 均 27 SQL / 893 VM / 2 Fullscan |
| 无关分集状态 100→10k，仅请求两集 | 均 3 SQL / 77 VM / 0 解码无关行 |
| 其他季文件 100→10k，仅请求固定季两文件 | 均 1 SQL / 95 VM / 2 解码行 |
| Query matrix 400→4000，固定页100 | 普通页 3184→3184 VM；Drama 页 6666→6666；协议 Drama 页 17459→17459 |
| 同矩阵 facets | 无筛 567→567 VM；全命中 All 584→584；50% Drama 9146→84746，线性而非二次 |
| Movie1000 固定、无关 TV400→4000 | kind_library 均10121 VM；kind_root 均172891 VM，total均1000 |

类型（movie/TV）查询已先收窄，genre/person 成员投影、静态边际和页内水合已工作。当前证据支持这些改动，没有依据恢复旧 projection、盲目加索引或把精确 total 改成近似值。过滤后的 facets 仍需精确交集，大命中下并非常量成本。100k 历史 release/2-reader+writer 延迟只引用[原 A 报告](../benchmarks/2026-10-25-large-hit-query-read-model.md)及[原 B 报告](../benchmarks/2026-10-25-library-read-isolation.md)，不当作本轮重测。

| 后续项 | 状态与技术验收 |
|---|---|
| O1 分集完整 HTTP 工作量 | **未完成验收**。Store 范围与快照已修；对 Library/Playback 两个实际 HTTP 请求汇总跨线程 SQL、VM、解码行及 coverage/归约，固定返回两集、其他季100→10k，工作量应稳定。只验证SQL条数不足。 |
| O2 容量/设备 | 独立100k Media/1M Files/1M Playback组合、NAS、真实播放设备、OS冷缓存、掉电仍未执行。按单独环境验收，不把这些列成已发现回归。 |
| O3 schema 身份加固 | Store/cache 的头与对象名不等于列/FK/CHECK/索引定义完整一致。可按 Jobs 单一DDL规范化比较加固，坏布局零写拒绝；不增加旧库迁移。 |
| O4 连接策略 | 独立 ResponseCache 与 Store cache 策略尚未统一测量。先测fsync/争用/RSS与WAL再决定，权威库FULL不变；无证据不改耐久性换数字。 |
| O5 schema 精简候选 | 旧结构审计真正未实施：R5指纹重复列、R7协议terms合并、R8根索引收窄、R12 Probe显式派发序后WITHOUT ROWID。先核对所有读写及页/写成本；R7涉及facet语义，不能机械合表。 |

结构审计的 R6（人物过滤）、R9（静态 facets 走边际）、R11（死模块删除）已落地，不能再列待办；本轮已纠正该文。`PRAGMA optimize` 已可能采样 ANALYZE，旧“生产没有 sqlite_stat1、空库计划永远代表满库”的前提已失效。后续索引 A/B 应覆盖统计前后和 bundled 引擎。

建议实施顺序：R6→R7→R8→R9；每项先公开 seam 固定 red，再实现 green，触及 crate 单独测试，末尾 workspace，Web变更跑typecheck/test/build。O1在下次声称分集性能完整验收前完成；O3–O5均需新证据再决定是否实施。本轮没有执行这些方案。

## 本轮验证与边界

- Web：typecheck、build exit 0；测试 **849 passed / 0 failed / 0 skipped**，有既有大 chunk 提示。
- Store聚焦：`cargo test -p store --test episode_state_work --test integrity_guard_work --test library_query_filters --test library_query_plans -- --nocapture`，**31 passed / 0 failed**。
- 额外现有工作量程序：`PERF_SIZES=400,4000` 执行 query_model_perf 的 ignored query_matrix，**1 passed**；这是主动执行该测量，不代表其余ignored项通过。
- API executor_not_blocked **5 passed**；NFO owner 回归 **15 passed**。均是已有测试，未修改测试代码。
- Rust：`cargo test --workspace --no-fail-fast` 完整结束，**3,161 passed / 0 failed / 24 ignored**，474 个结果段。ignored 不计为已验证；额外主动执行的 query_matrix 单独记录，不并入此总数。
- 临时反例通过表示当前缺陷成立，不能当作修复通过。R6/R7/R8/R9 均有实际生产公共入口证据。
- 未确认新的 SQLite 永久锁环或测试永久等待；R7是持久状态不能收敛、R8是执行器停顿。正常测试绿色不能证明所有故障交错安全。
- 未运行真实 TMDB/Downloader/Chromium、NAS/设备/掉电/实例切换；未操作真实数据。Rust文件大小只作仓库约束检查，不代替生命周期正确性。

临时产物不属于项目依赖，/tmp不保证长期存在。日志：`/tmp/crawler-storage-review-20261028-workspace.log`、`/tmp/schema-query-focused.log`、`/tmp/schema-query-matrix-current.log`；Web为`/tmp/crawler-review-web-{typecheck,test,build}.log`。反例源码：`crawler-review-admin-write-gate.rs`、`crawler-review-http-retired-marks-clean.rs`、`crawler-review-retired-marks.rs`、`crawler-review-merge-late-mark.rs`、`crawler-review-remaining-locks.rs`、`crawler-review-legacy-http.rs`（均在/tmp）。

可移植复现条件与断言已按R6–R9保留在正文；正式回归应复用现有fixture并按行为拆分，不把这些临时程序整份塞进生产树。

## 文档收敛记录

- 用本版当前结论替换原报告中“顶部写已修、正文继续要求R1–R5修复后合并”的冲突状态；移除已完成步骤的重复方案，保留逐ID对账、当前未完成项和Git取回方式。
- 同步 docs入口、数据库字典、ADR-0008、主Spec及API游标合同；删除NFO失败退出和分集范围仍为原缺陷的过时描述。
- 纠正10-26结构审计中已落地R6/R9/R11和统计前提；保留当时测量，不重写历史速度/空间数字。
- 之前已删除的10-26修复执行稿、Codex brief和两份10-27计划仍从`git show 5730b032:docs/superpowers/plans/<原路径>`取回。其独有finding与测量见旧报告Git版本、固定版本审计及benchmark。本轮不为清理再删除唯一证据，也不恢复多份“唯一当前方案”。
