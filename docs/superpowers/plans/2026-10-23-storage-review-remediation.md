# 存储重构剩余修复技术方案

状态：**部分修复，仍需整改**（2026-10-24，源码 `5bd696e`）。以[本轮修复复核](../../audits/2026-10-24-storage-fix-review.md)为当前依据，比较本地 `main@86122d7`。本页删除已完成步骤，只列仍需实施或验收的任务；逐项关闭证据和历史 F01–F10 映射留在审查报告。上一轮完整方案可从 `git show dcc1d81:docs/superpowers/plans/2026-10-23-storage-review-remediation.md` 读取。

## 约束与顺序

保留五文件、显式归属、事实/摘要同事务、冷详情和持久意图。无旧数据导入、兼容表/视图、双读双写、旧字段别名或旧格式回退；未上线布局直接修改目标合同与全部消费者，不补旧 JSON/数据库升级 shim。静态图片保持 public，Jellyfin 视频按协议鉴权；合法 whole/整季/S00E00 不属于兼容。

建议顺序：T1 共享图片 → T2 源版本 → T3 根归属与初始化 → T4 marks → T5 fixture 退出 → T6 完整锁边界 → T7 门禁与测量。独立项可并行。每项先建立能区分原错误的公开行为 RED，再实施和 GREEN；不能换一个原本就能拒绝的输入冒充原反例已修。

本轮只 review/文档，没有实施下述方案。修复时遵守 AGENTS：Rust 文件≤800行、函数≤120行，关键失败结构化记录，按职责拆分，crate 测试后 workspace；不操作真实实例或自动部署。

## T1：rename 全链统一路径身份（R04 / P1）

**范围：** rename sidecar 观察/意图、`classify_shared_sidecars/sibling_files/referenced_elsewhere`、路径观察合同及共享图片回归。

**设计：** 在锁外以固定根快照准备 canonical 视频、目录、sidecar 身份；持久比较使用身份键，display 仅供磁盘执行。共享判定同时覆盖同目录文件与显式引用，不能用 display 目录范围查询 canonical path_key；最终执行前仍须重验共享关系和竞争目标。保留现有 no-clobber、不同目标优先、Unknown 不删除和崩溃恢复语义。

**验收：** 两分集共享图片，real/alias 两种根及源/目标不同拼写，移动一集后源图片仍在且目标有副本；独占图片才移动。显式软链与 canonical 对照、崩溃重启、竞争目标、后出现的共享者全部经过生产 rename seam。默认 macOS TMPDIR 失败必须修生产逻辑，不改 TMPDIR 掩盖。

原 F10 NFO 初次采用/重扫和 5bd696e caller spelling 查询已通过，保留其回归，不重做已完成实现。

## T2：计算输入版本贯穿 Marker/指纹发布（R01 / P1）

**范围：** `probe_manager/{markers,marker_jobs,probe,season}.rs`、`fingerprint_job/source.rs`、Store Marker/cache 发布事务。

**设计：** 计算前固定所有样本的 file_id、FileVersion、Library/root/config revision及完整成员集合，锁外捕获实际 source token。短 IMMEDIATE 事务比较原 expected；Marker 验证成员独立于 chapter_updates，无章节也必须校验。缓存发布同样 CAS，旧任务拒绝后重新派发当前版本；禁止发布前刷新 expected。

**验收：** 真实 ProbeManager 刷新在 comparison/capture 门内同路径替换 r2 后放行 r1：旧 Marker、章节、cache 零发布，任务不谎报成功，新任务正常完成。覆盖成员变更、冻结、Unknown、删除/改挂；tracks 保留 queued version 修复，另测真实磁盘探测前后 token 改变。Store helper 的旧 snapshot 测试不足以替代生产链。

## T3：完整根 operation、Unidentified 与 seed 原子性（R02 / P1；R05/R06 / P2）

**范围：** create/update/insert-root、`roots.rs`、Library operation/lifecycle、文件发布归属验证、Unidentified、`schema.rs::seed_libraries_and_roots`。

**设计：** 根变更固定参与 Library/root/revision、范围、phase/state/error/cursor，源/目标建立写屏障并使旧 WriteScope 失效；最终发布校验当前最长根归属。冻结/删除/另一根操作冲突时明确等待/失败并保留可恢复状态，不静默换目标或确认跳过。优先复用现有持久 operation；不再建立通用队列。新建嵌套库与正式 reconfiguration 共用归属合同。Unidentified 有独立有界阶段/cursor，不能恢复前置无界 UPDATE。

全新 kind 的默认 Library/root 在一个 IMMEDIATE 事务提交；任一步失败全部回滚。已有配置不补旧默认根，不在 open 时猜测修复历史数据。

**验收：** 旧父 scope 在迁移后发布嵌套路径必须被拒；100/150 后目标冻结及源冻结、写入落在 cursor 前、locale/removal/另一个根操作、每批重启/重试均不漏迁。新建嵌套库的 Unidentified 归属正确、可恢复。seed root INSERT 故障后无半组新事实，移除故障重试正常；正常重开保留用户根集合、顺序、default 与 revision。

原 F02 101 文件 NFO 丢失、F03 冻结目标暂停、F09 正常重开均有当前 GREEN；保留其回归。补至少301文件、同 Media 多目录/season、共享 work/season/episode NFO、locale/provenance/export digest 与逐批重启守恒，不能只靠3个根测试宣布全部验收完成。

## T4：marks 严格字段与实际存储范围（R03 / P1）

**范围：** GET/POST marks 及实际 Web 调用者。

**设计：** 共用严格白名单和目标解析，按 i32 存储范围 checked conversion；越界 400，不饱和、不截断。拒未知/旧字段、错误类型、非法负数、单独 episode 与重复非法查询参数。保留 whole、整季、S00E00。同步删除当前 Web 无用途的 device_id，或明确当前合同用途，不能只严格化后端而断开前端。

**验收：** 原 `4294967295` 反例、i32::MAX+1/u32::MAX/u32::MAX+1、camelCase 键、GET 旧键、坏类型均400；原始 assertion/favorite/恢复包和其他分集零写。合法三层标记/撤销保持当前时钟与归约语义。

## T5：测试 HTTP server 真正回收并有界退出（R07 / P2）

**范围：** FakeServer、MockQbit/MockTransmission owner、生命周期测试。

**设计：** handler state 与 server owner 分离；完成线程必须 reap/join，全部回收后统一传播 panic，不能 retain 丢 handle或第一个错误就提前退出。shutdown 主动关闭活跃 socket，设整个请求/退出期限，单次 read/write timeout 仅作补充。

**验收：** 子进程 watchdog 覆盖 handler 最后 owner、已完成 handler panic 后第二连接、多个 panic、慢速 header/半请求、64MiB peer 不读；退出不依赖 peer 主动关闭。macOS 原行为是 join 环被系统拒绝并隐藏 panic，不能错误宣称已实测本机永久死锁。

## T6：路径观察、NFO、rename 完整出锁与有限调度（R08 / P2）

**范围：** SQL normalizer、scan checkpoint/source 查询、library_owning_key、NFO 调用者、housekeeping/organize/startup、Probe 与 root driver。

**设计：** 锁外一次准备 owned canonical key、source/presence 与真正固定的根快照，携原路径/配置版本；锁内只查询/写入既有 key 与 CAS。SQL scalar 不做磁盘 IO。begin/complete/finish 不隐含 stat/read_dir/STRM/NFO/删除 staging；执行进入现有预算池，失败留恢复记录。每次恢复有限窗口，批间释放 guard 并让出调度；create/update/insert-root 不同步耗尽全队列。

**验收：** 门控实际 realpath/NFO/rename 每个磁盘阶段，同时 Store 查询/心跳限时完成；检查每路径不重复 realpath。root/rename 大量 pending 时可读/可停机/可续跑。同 Media 10k 文件/多用户单测归约、事务 p95、最大 Store 持锁时间、WAL 与 busy 错误。没有锁环证据时按阻塞/饥饿报告。

## T7：当前合同测试与最终测量门（R09 / P2）

1. 迁移 Subscribe 共享 fixture：创建当前 Filter 后用 filter_id，或删除旧 inline filter 使用默认；不恢复后端兼容。排序 fixture 的物理文件名用独立 UUID，Media 名称仍为 A/a，保留排序断言。共享图片失败修生产逻辑。
2. 拆分超800行的 nfo_scoped_inputs/metadata_adoption 测试模块；检查触及函数的120行上限。将实际 HTTP adapter 拒旧信封、裸 payload、缺 data 与当前 consumer data 回执验证保存为持久回归。
3. 重跑每个受影响 crate 和完整 workspace，再跑 Web test/typecheck/build。当前 workspace/store 红、Web/Jobs 绿的事实不能被历史05a7363全绿替代。竞争写者行为 RED（A8）、NFO export 区分性失败 RED（A12）仍是验收缺口，不据此直接判生产行为坏。
4. genre 仍需固定 ACL/locale/跨库去重/稳定排序/精确 total，按5k/20k/50k/100k测 EQP/VM/p50/p95/锁时长，再决定有序成员索引或读模型。keyset不能免掉精确count；不改成近似 total。person/search 暂无新增测量依据要求 FTS。
5. cache NORMAL、mmap、cache_size、temp_store、WITHOUT ROWID 仅按单变量 A/B 决定，记录耐久/内存/WAL取舍。Jobs 显式 FULL 与 OFF→policy→FULL 回归已完成，不再作为待办；独立 media cache 连接另行评估。
6. 完整停启/重启/清 cache、真实浏览器/Jellyfin设备、NAS及100k Media/1M文件/1M Playback在可用隔离环境执行；未测写 Not run，不操作真实用户实例。

完成后更新本页只保留剩余项，历史 finding/独有测量保留固定 SHA 审计；生产 DDL 是唯一结构来源，不复制第二份 SQL 或恢复已删除旧步骤文档。
