# 存储重构再次复审与修复验收

> 历史阶段验收：当前 HEAD 的新发现与合并建议见[7030c1dd 审查](2026-10-28-storage-head-review.md)。本文关闭结论仅限当时已确认的问题。

日期：2026-10-28。审查代码：`feat/storage-redesign@9f6126d358c18afc532f6b551dadccdfa8ed0558`；比较基线 `main/origin/main@756444f76bef53f5fabed6d0ef53e9f280e005e9`，merge base 相同。开始时工作区干净。原始发现位置固定于该 HEAD；用户随后授权修复。完整只读报告可用 `git show b7992d7d:docs/audits/2026-10-28-storage-main-rereview.md` 读取。

R6–R9 已按公开行为回归修复，完整 crate/workspace 与 Web 门禁通过，本轮确认的合并阻断已关闭。修复覆盖写门禁、Media 身份屏障、执行器阻塞与旧输入兼容，证据见下文“本轮验证与边界”；本次不修改 DDL、不切换用户实例，也未实施 O1–O5 的独立测量/优化候选。

初始只读复审相对 main 共 1,527 文件、184,022 行新增、65,353 行删除，重点追踪存储、生命周期、查询及消费者链路，不声称逐行穷尽全部差异。R6 属于 main 已存在的缺口。

## 架构与数据库判断

- 保留 app.db（配置/凭证）、library.db（Media/Library/Playback 权威事实）、subscribe.db（策略/下载事实）、jobs.db（执行事实）、cache.db（外部响应）的生命周期分区。Media、文件、覆盖、采用元数据与摘要同库事务，比 main 的跨文件更新更合理，没有证据要求再次拆库或换数据库。
- 显式 Library/root 归属、文件版本 CAS、原始 Playback 与物理恢复包分离、冷详情、单位覆盖及增量摘要均有用途。跨文件没有 FK/原子提交，持久操作负责恢复；R7 补齐 Playback 写入口的冻结合同，以中断后重开恢复验证阶段收敛。
- 当前 library 布局 **v8：53 表、594 字段、81 个显式索引、37 组 FK、0 触发器**。五文件结构与唯一 DDL 见[数据库字典](../database/README.md)。表数/文件大小本身不是不合理设计的证据，热点工作量和写入代价才是优化依据。
- 旧数据库布局拒绝、不导入、不双读双写；旧 directory 聚合、Naming 拆解、Downloader JSON 回退、无签发 self Playback 已删除。当前 Web 未发现继续调用这些旧入口，静态图片仍在 public 路由。R9 删除旧 watch 别名与排序转换；查询只接受当前值。Jellyfin 协议映射及外部 Metadata source 合法失败回退按各自合同处理。

## R6–R9 修复实现

沿用既有 R1–R5 编号用于历史对账，本轮新增从 R6 开始。

| 编号 | 原优先级 | 已实施的修复 | 来源 |
|---|---|---|---|
| R6 | P1 | metadata、discovery、Playback 及 Jellyfin 写路由统一进入实例只读与维护门禁 | main 已存在，本轮确认；属于当前存储安全合同缺口 |
| R7 | P1 | Playback 写事务拒绝 merging/retired，签发后的迟到事件保持 IgnoredStale | 本次重构原冻结/恢复合同遗漏 |
| R8 | P1 | Media operation 移入有界 blocking；up-next 补图独立限流并按目标去重 | 原 R1 / Task22 剩余项 |
| R9 | P2 | 删除旧 watch 值、游标规范化及 release_date_asc 转换 | main 遗留逻辑，新游标曾延续别名 |

### R6：metadata 路由绕过写门禁

原反例：readonly/maintenance 下 `DELETE /ledger/{id}` 仍返回 200 并删除文件事实。现将 metadata、discovery、Playback 变更路由统一套用 write_gate；只读 POST 显式分组，不再按 `preview` 路径后缀豁免。GET/HEAD/OPTIONS 和真实计算预览继续可读，静态图片保持 public。GET 搜索在受保护期间仍返回结果，但跳过 best-effort history/snapshot 保存，并在拿到 Store 后重验准入。独立的 Jellyfin/Emby Router 同样接入门禁，唯一只读 POST Ping 显式保留；标记、会话、PlaybackInfo、偏好和登录等持久写请求均受准入控制。

实例只读优先返回 409 `instance.read_only`；维护返回 503 `runtime.busy` + Retry-After，检查发生于参数验证和数据库/磁盘副作用之前。回归用真实 Router 覆盖受保护写入口、文件字节/事实及安全读对照。原子 gate 是准入检查，并不代表已准入请求被取消或排空；后台维护仍依赖既有连接串行与 worker 生命周期。

### R7：Media 冻结与退役写屏障遗漏 Playback mark

原反例：公开 mark 可写 merging/retired Media；library 阶段提交后插入 loser 的 Playback 引用，会使恢复永久卡在 retire。这是持久状态不能收敛，没有证据称为 SQLite 锁死。

在 Playback 的同一写事务内复用身份屏障，覆盖 marks、直接单元、轨道、会话及日志引用写入；冻结/退役操作拒绝且零写。self/Jellyfin marks 对可见冻结项返回明确冲突；退役 self ID 的错误保留 survivor 提示，Jellyfin 退役项不可见时返回 404，均不自动重定向。已签发的迟到播放事件继续返回 IgnoredStale，不创建或关闭事实。

回归在 library→subscribes 跨文件边界注入一次失败，尝试晚到写入后重开 Store，再推进至 succeeded；检查 survivor 事实、多 User 隔离、显式 false/favorite 与旧会话行为。生产不加兼容触发器，也不重跑已提交的 library 阶段掩盖新引用。身份拒绝直接读取 Media 的 state/survivor_id；新增 1k→20k 终态 merge 历史工作量回归，拒绝写入的 VM/Fullscan 不随无关历史增长，不为错误提示扫描操作历史。

### R8：执行器阻塞仍有两个生产分支

原 current-thread HTTP 反例：外线程持 Store 500ms 时 operation GET 的 10ms timer 延迟 506ms；非空 up-next 缺图且 fake Catalog 延迟 500ms 时 timer 延迟 510ms。问题是执行器饥饿，busy_timeout 不限制外层 Mutex 等待。

operation 读使用 store_read，写使用有界准入与 run_to_completion，响应实际结果。up-next 图片补全使用独立有界 blocking 调度并按目标路径去重，permit 与目标租约跟随实际 worker 结束释放；保持 Registry→Store 顺序及网络/磁盘 IO 在 Store 锁外执行。

正式回归覆盖 GET/POST 等锁、非空 up-next 补图的 <200ms timer，以及容量、去重、请求取消、panic 后预算恢复；持锁线程独立到期并安全 join，不靠可能被阻塞的 timer 解锁。

### R9：无兼容切换尚有旧值转换

删除 Store 验证/SQL/游标中的 watched、unplayed、unseen，保留当前五值 unwatched/watching/played/favorite/seen。真实 items/gallery/facets/item-index 请求拒绝旧值并接受当前值；`seen` 仍是首页的独立语义。

排序统一使用 release_date + 独立 order，删除 Web enum/方向表/展示表及后端 parser 中的 release_date_asc。非法查询排序返回 400；非法偏好按普通默认值处理，不再专项转换。首页当前排序及偏好往返测试保留。

`probe_stage_state` 的误导性 v6 migration 测试改名为 current_database_reopens_without_losing_probe_jobs，明确仅验证当前布局重开，既有旧布局拒绝合同保持。没有新增迁移或旧值回退。

## 上轮修复逐项对账

原 R1–R5 针对 `5730b032`，修复提交为 `9f6126d3`。旧完整反例与数字可从 `git show 9f6126d3:docs/audits/2026-10-28-storage-main-rereview.md` 读取，不再重复维护已完成实施步骤。

| 原项 | 当前判断 | 本轮证据/剩余 |
|---|---|---|
| R1 / F2 / Task22 执行器 | 原点名主路径与本轮 R8 残余分支均已修 | 原 executor_not_blocked 与新增非空补图/operation 回归覆盖 |
| R2 NFO 失败 owner | 原冲突/失败退出已修 | nfo_export_ownership 15 passed；精确 plan CAS、每路径执行 lease、失败终态释放及重启回归；不把代码存在扩大成真实设备验收 |
| R3 Integrity guard 历史扫描 | 已修 | v8 部分索引，公开 update_media/progress 对无关历史 1k→20k 工作量稳定，含 failed 继续冻结对照及索引写成本 |
| R4 分集超范围与逐集点查 | 原缺陷已修 | UnitKey+必要祖先、400 键窗口、同一读快照；状态/coverage 规模回归通过。完整 HTTP 跨线程 SQL/解码汇总尚未完成，见后续 O1 |
| R5 游标无 scope | 已修 | 唯一 version/scope/键类型校验，跨库/User/filter/order/direction 拒绝；无旧游标解码回退 |

更早 F1/F3/F5/F6/F7/F8 与 R1–R10/T9 的关闭映射保留在同一 Git 历史和各固定版本审计；本轮未把当年的反例重新报成当前缺陷。F6 当时列出的字段已删；本轮 R9 补充删除复审发现的 watch 别名与旧排序表示。

## 查询性能与真正未完成的优化

修复前的只读复审在 `9f6126d3` 复跑公开 Store 工作量门，bundled SQLite **3.50.2**。下表保留该次 SQL 工作量，不能作为新增身份检查后的精确 SQL 数字；debug 与 workspace 并发运行，不以墙钟冒充 release/NAS 延迟。

| 数据变化/固定请求 | 修复前复审结果（9f6126d3） |
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

R6–R9 为本次授权修复范围。O1 在下次声称分集性能完整验收前完成；O3–O5 均需新证据再决定是否实施，本次不扩大实施范围。

## 本轮验证与边界

| 项目 | 正式行为回归 |
|---|---|
| R6 | [Router 门禁与协议写入](../../crates/api/tests/metadata_write_gate.rs)、[受保护搜索仍返回结果](../../crates/api/tests/metadata_write_gate/search_reads.rs) |
| R7 | [Store 身份屏障、恢复与拒绝工作量](../../crates/store/tests/playback_identity_barrier.rs)、[self/Jellyfin marks](../../crates/api/tests/playback_identity_barrier.rs) |
| R8 | [执行器响应与入口](../../crates/api/tests/executor_media_artwork.rs)、[背景图限流/去重/取消/panic](../../crates/api/tests/executor_media_artwork/background.rs)、[合并 busy 与无迟到写](../../crates/api/tests/executor_media_artwork/operations.rs) |
| R9 | [真实 browse 请求](../../crates/api/tests/library_paging_validation.rs)、[Store 过滤与游标](../../crates/store/tests/library_query_filters/filters.rs)、[首页偏好](../../web/test/home-rows.test.mjs) |

- 完整 Store：`cargo test -p store`，750 passed / 0 failed / 11 ignored；Media Server：`cargo test -p media-server`，40 passed / 0 failed。
- 完整 API：`cargo test -p api --no-fail-fast`，1,794 passed / 0 failed / 9 ignored。
- 完整 workspace：`cargo test --workspace --no-fail-fast`，3,185 passed / 0 failed / 24 ignored，478 个结果段；忽略项不计作通过。

- 修改文件的 rustfmt、Rust 文件/函数尺寸、diff 空白与 219 个本地文档链接通过；全仓 fmt 检查另有未触及文件的既存格式漂移，本次只格式化修改文件。
- 新增回归先确认 red：旧 watch 在 Store/真实 Router 仍被接受；旧排序偏好被专门转换；metadata 删除和 Jellyfin 标记在受保护模式仍成功；Playback 冻结写及 executor 反例均可复现。
- Web：850 passed / 0 failed / 0 skipped；typecheck 与 build exit 0，保留既有大 chunk 提示。
- 没有访问真实 TMDB/Downloader/Chromium、NAS/设备/掉电或用户实例。O1–O5 与 ignored 的独立现场门不计作已验证。
- 一次 API 全量检查在 lib 的 219 项通过后遇到仓库 target 目录消失，后续二进制未执行；该次不计验收通过。检查所见 API 测试递归删除均指向各自临时夹具，未确认源码清理越界。最终 API/workspace 使用独立 `CARGO_TARGET_DIR=/tmp/crawler-fix-20261028-target` 重跑。
- 最终日志：`/tmp/crawler-fix-{store,api,media-server,workspace}-full.log`；Web 为 `/tmp/crawler-fix-web-{test,typecheck,build}.log`。临时路径不保证长期保留，正式回归源码与命令见本节。
- 只读阶段历史结果为 3,161 passed / 0 failed / 24 ignored；完整反例、日志和当时测量见 `b7992d7d` 版本，不能计入本次修复验收。

## 文档收敛记录

- R6–R9 正文改为最终行为与回归覆盖；原始反例/修复方案固定于 `b7992d7d`，不并存两套当前状态。
- 同步 docs入口、数据库字典、ADR-0008、主Spec及API游标合同；删除NFO失败退出和分集范围仍为原缺陷的过时描述。
- 纠正10-26结构审计中已落地R6/R9/R11和统计前提；保留当时测量，不重写历史速度/空间数字。
- 之前已删除的10-26修复执行稿、Codex brief和两份10-27计划仍从`git show 5730b032:docs/superpowers/plans/<原路径>`取回。其独有finding与测量见旧报告Git版本、固定版本审计及benchmark。本轮不为清理再删除唯一证据，也不恢复多份“唯一当前方案”。
