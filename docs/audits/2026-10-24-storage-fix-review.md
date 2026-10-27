# 存储修复复核：5bd696e

**历史审查：固定在 5bd696e。** 当前补修、验证与剩余项见[再次复核](2026-10-24-storage-fix-followup.md)；下文保留当时结论与独有反例，不作当前缺陷清单。

日期：2026-10-24。结论：**Request changes**。本轮只 review、运行隔离测试及维护文档，未修改 Rust、TS、DDL 或仓库测试，未操作真实实例。

## 基线与总体判断

- 本地 `main=86122d73a4a23439e1d83234590fdd98b388fc50`；被审 `HEAD=5bd696ef5282d3a49b902b61993cd7eb73422e74`，merge-base 等于 main。
- 本轮重点核对上次审查后的 13 个修复提交：`dcc1d81..5bd696e`，59 文件、+1346/-367。工作区开始时干净；没有把上一轮反例直接当作本轮结果。
- 五文件、Media/Library/Playback 同库、显式归属、事实与摘要同事务的设计仍合理。本次没有 DDL 变更，71 表/709 字段等字典统计不变。主要剩余问题是跨阶段版本与归属屏障、路径身份、输入校验和测试生命周期。
- 生产 Web Subscribe 字段切换、严格成功信封、默认根正常重开、原 NFO 批迁丢失和原 alias NFO 输入误删已有修复。不能据此宣称整轮完成；下列是当前源码或当前实测仍成立的问题。
- 当前执行入口只有[剩余修复技术方案](../superpowers/plans/2026-10-23-storage-review-remediation.md)。[上一轮审查](2026-10-23-storage-followup-review.md)保留固定 SHA 的历史证据。

## R01 / P1：Marker 与指纹缓存仍未消费计算前的 FileVersion（原 F04）

本次 `7a948fa` 将普通 tracks 的 expected 改为入队版本，但没有修改 Marker/指纹缓存发布链。

- [marker_jobs.rs:63](../../crates/api/src/probe_manager/marker_jobs.rs#L63)在比对完成后调用 [source_observations:162](../../crates/api/src/probe_manager/markers.rs#L162)，重新取得当前 snapshot；成员还只来自 `chapter_updates`，没有章节更新时不能覆盖参与比对的全部文件。
- [ensure_source_recorded:15](../../crates/api/src/fingerprint_job/source.rs#L15)只比较路径、Media 和覆盖，未比较原 FileVersion。[probe.rs:662](../../crates/api/src/probe_manager/probe.rs#L662)仍在这个检查后直接写指纹缓存。
- **当前生产链实测：** 使用真实 `ProbeManager::enqueue_marker_refresh`、8 个本地 STRM 文件和假 FingerprintEngine；在 `find_common_segment` 接收旧样本时，正式 `publish_files` 将 E01 同路径换为 r2/新 source。刷新最终 `succeeded`，却为 E01 写入旧样本产生的 `10s..60s` intro 标记。本例没有章节缓存，不声称实测写了旧章节。
- 独立采集场景在假引擎 `extract_at` 中换源，最终也 `succeeded`，r2 文件上重新出现 r1 cache key。当前读取校验仍能跳过该旧 cache；这不能使旧任务覆盖新源后的缓存成为正确发布。

方案：计算/采集开始固定全部成员的 file_id、FileVersion、Library/root/config revision；锁外记录实际磁盘 source token；最终短事务验证原 expected，拒绝旧工作并重新派发。Marker 的验证成员集合独立于章节更新集合。tracks 还需覆盖真实探测前后磁盘 token 变化，不能只测 Store helper。

## R02 / P1：根迁移完成后，旧父库 scope 仍可写回错误归属（原 F03）

冻结目标时不推进 cursor 的原反例已修，但完整归属屏障未落地。

[roots.rs:137](../../crates/store/src/roots.rs#L137)仍只登记 kind/root/cursor；创建嵌套根不使父 Library 的旧 config revision 失效。[ensure_root_owns_path:14](../../crates/store/src/library_write/root_ownership.rs#L14)只验证声明的根包含路径，没有验证当前最长根归属。

**公开 Store seam 实测：** 捕获父 LibraryFile 与 revision=0 的 WriteScope；创建嵌套 Library 并完成迁移；用旧父 scope 发布此前未记录的嵌套路径：`inserted=1`，文件持久归属父库，严格路径解析归属嵌套库，父 revision 仍 0，pending=0。正常完成迁移后仍可被已准备的扫描结果污染，不存在后续队列保证修复。

方案：根变更纳入持久 operation，固定参与根/Library/revision，源与目标建立写入屏障并使旧 scope 失效；最终发布校验该路径的当前声明归属。目标/源冻结、删除、并发根变更或配置变化时保留可恢复状态。补旧 scope、新文件落在 cursor 前、每批重启和并发 locale/removal 的行为回归。

## R03 / P1：marks 仍接受改变目标的坐标和未知字段（原 F05）

- [coordinate:148](../../crates/api/src/http/playback/watch_state.rs#L148)用 `i32::try_from(value).unwrap_or(i32::MAX)`；真实 Router 对 `4294967295` 返回 200，将两个坐标均写成 `2147483647`。新测试仅测 `4294967296`，漏掉原反例及整个 `i32::MAX+1..u32::MAX` 区间。
- [MarkInput:159](../../crates/api/src/http/playback/watch_state.rs#L159)不拒未知字段；POST 只手动拒 `season/episode`。真实 Router 对 `seasonNumber/episodeNumber + played:true` 返回 200，持久写入 whole `(-1,-1)`。
- [GET:81](../../crates/api/src/http/playback/watch_state.rs#L81)仍用 HashMap，旧 `season/episode` 被忽略，返回 whole 状态。GET 本身没有写入。

方案：GET/POST 共享严格字段及目标合同，按实际 i32 存储范围 checked conversion，越界直接 400；拒未知/旧键、重复查询坐标与错误类型，坏请求零写；保留合法 whole、整季、S00E00。严格化时同步处理 [Web:943](../../web/lib/api/playback.ts#L943)仍发送而 MarkInput 未声明/未使用的 `device_id`，不能只给 DTO 加属性便破坏当前调用。

## R04 / P1：alias 路径下共享图片被当作独占图片移动（F10 后续）

本轮 Store 全套在 `file_rename_shared_artwork::shared_artwork_is_copied_and_kept_for_remaining_media` 失败：[源图片读取:123](../../crates/store/tests/file_rename_shared_artwork.rs#L123)得到 NotFound。

[sibling_files:265](../../crates/store/src/library_write/rename_sidecars.rs#L265)只对视频的 display 父目录做 lexical normalize，再与 canonical `library_files.path_key` 比较。alias 与 real 拼写不同就漏掉仍在同目录的 E02，`classify_shared_sidecars` 选择 Move，E01 迁移后源 `poster.jpg` 消失。`/var` 与 `/private/var` 是当前环境的触发条件。另在canonical临时目录内构造显式real/alias根，对照仅改变根拼写：canonical通过、alias源图片消失，证明不依赖平台路径特例。图片没有被销毁，而是被错误移走，剩余文件失去共享图片。

方案：rename 意图锁外准备 canonical 文件/目录及 sidecar 身份；共享判定和引用判定使用固定身份键，display 仅用于执行。不能通过 canonical TMPDIR 或修改测试期望规避。验收显式 symlink alias、源/目标不同拼写、共享/独占、崩溃恢复和竞争写入，保证源图片在仍有使用者时保留。

## R05 / P2：新建嵌套库不再迁移 Unidentified

[reassign_files_under_root:137](../../crates/store/src/roots.rs#L137)删除了原有 Unidentified 迁移；新 batch 只处理 library_files 和 NFO 输入，没有对应 Unidentified 阶段。

**公开 seam 实测：** 父库登记嵌套路径的 Unidentified，再 `create_library` 建嵌套库，完成 drain 后 `owner_is_parent=true, owner_is_nested=false, pending=0`。正式 reconfiguration 的 R19 测试覆盖另一条入口，不能证明新建嵌套库正确。

方案：将该事实纳入同一根 operation 的独立有界阶段/cursor，按最终最长根与生命周期屏障更新归属，失败可恢复；不能恢复无界全表 UPDATE。补 create/insert-root 与正式 reconfiguration 的实际公开入口回归。

## R06 / P2：seed 失败可留下永久无根默认库（F09 后续）

正常重开不复活已删除默认根的修复成立；初始化失败原子性仍缺。

[schema.rs:79](../../crates/store/src/schema.rs#L79)插 Library 和 [:85](../../crates/store/src/schema.rs#L85)插 root 分别自动提交。探针在临时已初始化库移除空 TV kind 配置后，直接调用公开 `schema::seed_defaults`，用拒绝 root INSERT 的 trigger 注入失败：seed 报错后留下 `libraries=1, roots=0`；移除故障，Store 重开成功，但该 kind 的 root_paths 仍为空，因为“已有 Library”直接跳过。

方案：全新 kind 的 Library/root 初始化放在一个 IMMEDIATE 事务中，原子提交；失败回滚整组新事实，重试正常完成。已有配置不补旧默认根，不增加兼容修复 shim。临时 trigger 仅为故障注入，不修改生产 DDL。此探针验证 absent-kind seed 与真实重开的组合，未复现首次空目录 `Store::open` 故障全链。

## R07 / P2：fixture 仍丢 panic、保留 join 环并无总退出期限（原 F06）

- [lifecycle.rs:141](../../crates/api/tests/support/lifecycle.rs#L141)用 retain 丢掉已结束线程的 JoinHandle。实测先 handler panic，再发第二个正常请求，Drop 成功，panic 仍未传播。
- MockQbit/MockTransmission owner 结构未变。handler upgrade Weak 后释放最后 Arc，仍可能出现 connection→accept→connection join 环。本机实际表现为系统拒 join 的 `Resource deadlock avoided` panic；探针主进程仍 exit 0，未把它描述成本机永久死锁。
- 新增单次写 timeout 不等于总期限；Drop 没关闭活跃 socket。慢 header 每 200ms 发一字节，5.6s 后 Drop 未结束；64MiB 响应的 peer 不读，13s 后 Drop 仍未结束，关闭 peer 才退出。
- `resume_join` 遇首个 panic 立即 unwind，其余 handle 未全部回收。新增仓库测试只覆盖 Drop 时仍保留 handle 的 handler panic。

方案：handler state 与 server owner 分离；reap/join 所有完成线程，全部回收后统一传播失败；shutdown 主动取消 socket，并限制整个请求/退出时间。用外部子进程 watchdog 覆盖最后 owner、已完成 handler panic、慢 header、peer 不读和多个失败。

## R08 / P2：路径归一化新增锁内磁盘 IO，完整拆锁仍未完成（原 F07）

- [SQL scalar:174](../../crates/store/src/path_key.rs#L174)现在调用 `canonical_existing_key`→`fs::canonicalize`；所有相关查询/写事务都可能访问磁盘。
- [checkpoint_fresh:65](../../crates/api/src/http/library_scan/runner/checkpoint.rs#L65)持 Store 锁调用两个批量查询；[scan_seen.rs:131](../../crates/store/src/scan_seen.rs#L131)、140、171、194 各重新计算路径。每路径四次 realpath，“纯 DB”注释失实。
- 新增 `library_owning_key` 也再次 canonicalize；`Store::observe_nfo_inputs` 仍同步读/解析 NFO，claim/watch 调用仍持外层 guard。`alias_snapshot` 返回共享 Arc，不能将其注释当作不可变快照保证。
- 启动 root driver 已逐 batch 释放 guard；create/update/insert-root 入口仍整段 drain。housekeeping、rename begin/complete/finish、启动 rename 恢复、season/markers 仍有持 Store 的 stat/读文件/磁盘执行或全量循环。

以上为当前源码证据，支持慢 NAS 导致全局阻塞/饥饿风险；本轮没有确认新的生产锁环，也没有实测 NAS 延迟。

方案：锁外一次准备 owned 路径/源观察与固定根快照；SQL normalizer 保持纯内存，锁内只消费 key 并校验 revision。完整磁盘阶段进入已有预算池，恢复 driver 每次有限窗口、批间释放并让出调度。门控实际 realpath/NFO/rename 阶段验证并行读和心跳；现有 stamp 观察测试不能覆盖新增 realpath。

## R09 / P2：共享测试夹具仍用旧合同，当前门禁失败

- [management/common.rs:110](../../crates/api/tests/management/common.rs#L110)仍发送旧 inline `filter`，而 create 已 deny_unknown_fields。定向 `subscribes::list_subscribes_includes_media_title_and_tv_coverage` 实测 400≠201，多个测试复用该 helper。
- 当前 workspace 停在 `jellyfin_sql_page` 两个排序测试的 FK787。两测试用名称 A/a 生成物理 A.mkv/a.mkv；当前 macOS 文件系统将它们视为同一文件，新 canonical lookup 复用同一 file_id，却要求另一个 Media。失败发生在fixture发布，未进入排序断言。外部副本仅将物理文件名（含对应NFO）改为独立Media ID前缀，保留A/a名称和排序断言，两项均通过；未编译精确旧提交，不声称证明其引入版本。

方案：Subscribe fixture 先持久化当前 Filter，再传 filter_id，或明确使用默认 Filter；不恢复旧 inline 接口。排序 fixture 使用独立 UUID 文件名，Media 名称仍保持 A/a 来测试大小写排序；不能放松真实物理文件身份约束。修复后重跑完整 API/workspace。

另外，触及的 `nfo_scoped_inputs.rs=853`、`metadata_adoption.rs=846` 行超过 AGENTS 的 800 行硬上限，需按职责拆测试模块。其他触及 Rust 文件未超过 800 行；文件行数合格不等于函数长度或功能正确。

## 上次问题逐项结论

| 原项 | 当前复核 |
|---|---|
| F01 | 生产 Web coverage/filter_id 与 create 严格拒旧已修；旧测试 fixture 见 R09。 |
| F02 | 输入改随文件 batch 复制/迁移，原跨目录丢失反例已有回归通过；不代表全部根 operation 验收完成。 |
| F03 | 冻结目标暂停 cursor 已修；完整归属/旧 scope 屏障仍开，见 R02/R05。 |
| F04 | tracks 绑定 queued version 有改进；真实 Marker/指纹生产链仍复现 R01。 |
| F05 | 显式旧 POST 键、坏类型已拒；越界和未知字段/GET 尚开，见 R03。 |
| F06 | 新增写 timeout 和部分 join 传播；R07 仍复现。 |
| F07 | 启动 root batch 释放 guard 已修；完整 IO 边界仍开且新增 realpath，见 R08。 |
| F08 | 实际 HTTP adapter 拒旧 success/裸 payload/缺 data，catalog/health 及消费 data 的组件已迁移。生产问题关闭；仓库仍应保存这些拒绝行为回归。 |
| F09 | 正常重开不复活旧根已修；全新初始化故障原子性见 R06。 |
| F10 | 原 NFO alias 首次发布/详情已修，当前 NFO suite 全绿；不能外推所有路径消费者正确，rename 见 R04。 |
| Jobs FULL | OFF→实际 policy→FULL=2 的连接级回归通过，关闭此耐久性门。独立 media ResponseCache 连接策略须另行区分。 |

## 本轮验证与边界

| 验证 | 结果 |
|---|---|
| `cargo test --offline --workspace` | **失败**；已完成 binary 合计 456 passed / 2 failed / 2 ignored；停于 API jellyfin_sql_page，后续 suite 未运行。 |
| `cargo test --offline -p store` | **失败**；已完成 binary 合计 122 passed / 1 failed / 1 ignored；停于 shared artwork，后续 suite 未运行。 |
| `cargo test --offline -p jobs` | **通过**；55 passed / 1 ignored（含 doc tests 汇总）。 |
| Store 5 个定向 suite | 30 passed：library_reconfiguration_roots 5、nfo_scoped_inputs 8、root_reassign_batched 3、scan_operations 11、scan_probe_obligations 3。 |
| API 定向 management | marks、coverage PATCH、upgrade 换 Filter 各 1 passed；旧 Subscribe fixture 场景 1 failed。 |
| Web `npm test` / `npm run typecheck` | 842 passed / 0 failed；typecheck 通过。 |
| 隔离生产 seam 探针 | R01/R02/R03/R05/R06/R07 复现；没有实际 TMDB、Downloader、Chromium 或用户数据库。 |

临时证据：`/tmp/storage-fix-review-20261024/`（workspace/store/jobs 日志、marker-race 源码和两模式日志、locking 探针）；`/tmp/api-cutover-review-fixes-probe/`；本机 TMPDIR 的 `schema-review-current-dr0m9nk8`、`rename-artwork-review-f9dp9dlz`、`sorting-fixture-review-03zcvqr9`。持久结论及关键输出已记于本页，不依赖临时目录长期存在。

没有重新测性能；genre 命中集线性成本、同 Media 大覆盖写放大与最大 Store 持锁时间仍需测量。原 5k/20k/50k 数据是历史数字，约 70k 超 300ms 只是外推。NAS、真实浏览器/Jellyfin 设备、完整 100k Media/1M 文件/1M Playback、物理参数 A/B 均 Not run。先关闭上述正确性与门禁，再决定新增索引/读模型；不新增兼容层。
