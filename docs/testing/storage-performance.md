# 存储与媒体性能测试

本手册用于下一轮存储、查询、播放及片头片尾优化的可重复验证。当前设计见 [存储性能设计](../design/storage-performance.md) 与 [片头片尾优化](../design/marker-optimization.md)；延迟规则见 [性能闸门](performance-slo.md)，实际媒体与页面步骤见 [E2E 手册](e2e-runbook.md)。

## 验证层次与顺序

1. 在公开行为入口补失败回归，再实现修复。常规单元/集成测试使用 fake 和本地 fixture，不访问真实 Metadata source 或 Downloader。
2. 跑受影响 crate、协议链和 Web 逻辑检查，保持相同事实、权限、返回字段和删除语义。
3. 运行确定性的查询/写入工作量门，再测独立实例的 idle、busy、cold。HTTP 正确但业务集合错误不能通过。
4. 验证空库到扫描、Transfer、Scrape、播放、重启、清缓存和删除的完整流程，以及真实 Web/VidHub。
5. 完成组合容量与多批内存观察；片头片尾另行做准确性、生命周期墙钟和真实章节图验证。
6. 稳定代码树按仓库规则运行最终全套。报告明确实际覆盖、失败、忽略、未执行与证据不足，不把局部通过推广到全部门。

同一轮固定产品、工具、配置、fixture 和输入摘要；变化后另立结果。报告可记录构建身份用于复现，本文不绑定某个历史提交。仅整理文档/历史不需要重新构建产品或重跑完整媒体验收。

## 资源与隔离

临时数据放 `mktemp -d /tmp/crawler-perf-XXXXXX`，使用独立五库、媒体副本、端口、测试用户及 PID 记录。复制一致的停服数据库或使用明确备份方法，包含 WAL/sidecar；每轮结束核对原数据库、STRM/NFO/图片/字幕和 pan-302 文件清单。凭据私下保留，敏感 URL 和认证信息不进日志或 Git。具体恢复步骤见 E2E 手册。

普通构建遵守仓库包装器规则；正式性能测量用 `scripts/with-build-lock.py --exclusive` 独占两槽，构建 `-j8`、测试 `--test-threads=8`，`TMPDIR=/tmp`。不要与另一 session 的构建或测量混跑。保持远程 probe 并发 1、冷却 500ms、Library reader 容量 8。

准备前检查磁盘空间，估算 DB、索引、WAL、备份与 release 产物峰值。大型 fixture 分批准备；空间不足记 Blocked/Not run，不删除真实媒体或其他 session 的缓存。只停止自有进程，交付报告后删除本轮数据副本。

## 确定性工作量门

延迟容易受系统调度、文件缓存、TLS 和远端影响，先验证结果与工作量：

| 场景 | 输入与操作 | 必须观察 |
|---|---|---|
| 无关数据增长 | 固定目标 Library、User、筛选和一页输出，无关数据由 10k 增至 1M | 可索引热路径 `FullscanStep=0`，`VmStep` 不超过基线两倍；结果集合、顺序和页长不变 |
| 深游标 | 首、中、深页，NULL/tie、Unicode、ASC/DESC、已播与未播排序 | 无重复/缺项，末页游标为 null；同类页工作量不随深度线性增加 |
| 批发布 | 每批最多 100 个文件，同 Media 多文件、跨多个目录/Season | 提交后即读事实、摘要、计数和用户视图一致；失败回滚和重启不留半批；不按文件重复归约整个 Media |
| 缓存独立 | 100k 次缓存更新，再清缓存并重开 | Library items、文件/覆盖、用户收藏/进度与日志等业务事实保持；缓存 miss 不变成业务缺失 |
| 写准入 | writer 等待期间只读/维护/Library 状态改变 | 取得实际 writer 后复查准入；拒绝请求零业务写入；读不获取 writer |
| 读池和快照 | 并发长读、跨库请求、异常/取消/超时 | 有界准入，租约及时归还，异常事务清理；同阶段结果一致，不声称跨库原子快照 |
| 删除与迟到发布 | Scan/probe/Transfer/Scrape 运行时删库或最后引用 | 先隐藏并关闭发布门、立即取消，再删事实和孤儿 Media 及其收藏/进度；不等待慢 worker 排空，迟到发布必须拒绝 |
| 投影完整性 | 正反向缺行/多行、超长 TV、多版本、跨库同 Media、多 User | 检出并修复，游标持续推进，达到终态后独立 oracle 一致，失败与重启可恢复 |

精确 facets、任意筛选交集、协议精确 total 和深 offset 的固有遍历成本单独报告；不能把简单索引页的边界套成所有查询 O(1)。记录 `VmStep`、`FullscanStep`、`Sort`、SQL 返回/解码行、字节和公开结果。`EXPLAIN QUERY PLAN` 可辅助定位，但不能用 SQL 文本相同代替行为和实际工作量。

已存在的回归入口包括：

- Store：`storage_work_bounds`、`library_read_pool`、`reader_lease_scope`、`read_writer_independence`、`writer_rank_errors`、`writer_diagnostics`、`scan_operations`、`library_integrity_facts`、`library_integrity_media_pages`、`integrity_guard_work`。
- API：`reads_during_scan`、`writer_isolation`、`library_reads_off_writer`、`probe_backlog_capacity`、`storage_query_scale`、`library_statistics_scale`、`storage_integrity_runtime`、`playback_writer_latency`。
- 协议：按 [Jellyfin 合同](../api-contracts/jellyfin.md) 同时跑 media-server 和 API Router，不能只测 DTO 或手工播放事件。

先检查目标源码对应的具体场景，再选择命令；目标名称存在不证明覆盖上述全部矩阵。

基础测量入口也应保留：`store --test perf_baseline` 默认执行 `small_scale_gate`，以缩小 fixture 检查相同 SQL statement/fullscan 门槛；`--release --ignored` 执行约 20k Media / 60k 文件的公开 Store 播种与操作矩阵，墙钟单列而不作为 CI 硬断言。`api --test endpoint_perf` 是无网络、无 Chromium/ffmpeg 的公开 Router 延迟矩阵，可用 `PERF_SIZES`、`PERF_MODES`、`PERF_SAMPLES` 限定输入，必须在报告中写明。`jobs --test query_plans --ignored` 使用 200k 合成任务、ANALYZE、EQP 和中位数耗时比较候选索引；这些合成查询证据需与生产行为回归一起解释。

```sh
python3 scripts/with-build-lock.py --exclusive cargo test -j8 -p store \
  --test perf_baseline small_scale_gate -- --test-threads=8
python3 scripts/with-build-lock.py --exclusive cargo test --release -j8 -p store \
  --test perf_baseline -- --ignored --nocapture --test-threads=8
PERF_SIZES=100,500,2000,10000 PERF_MODES=movie,tv PERF_SAMPLES=5 \
  python3 scripts/with-build-lock.py --exclusive cargo test --release -j8 -p api \
  --test endpoint_perf -- --ignored --nocapture --test-threads=8
python3 scripts/with-build-lock.py --exclusive cargo test --release -j8 -p jobs \
  --test query_plans -- --ignored --nocapture --test-threads=8
python3 scripts/with-build-lock.py --exclusive cargo test -j8 -p store \
  --test media_lookup_indexes --test library_schema_audit --test library_query_plans \
  -- --test-threads=8
python3 scripts/with-build-lock.py --exclusive cargo test --release -j8 -p store \
  --test storage_work_bounds -- --test-threads=8
CRAWLER_MEDIA_SEEK_GROWTH_ITEMS=1000000 python3 scripts/with-build-lock.py --exclusive \
  cargo test --release -j8 -p store --test storage_work_bounds \
  seek_work_stays_flat_while_an_unrelated_library_grows -- --nocapture --test-threads=8
INTEGRITY_SCALE_N=100000 python3 scripts/with-build-lock.py --exclusive \
  cargo test --release -j8 -p store --test library_integrity_scale \
  -- --ignored --nocapture --test-threads=8
```

这些环境变量是现有测试支持的参数。`library_integrity_scale` 每 Media 一文件一单元，测公开推进和线性输入工作；它单独通过不等于百万文件/百万 Playback 的组合容量通过。`storage_query_scale` 的 HTTP fixture 也不是完整容量生成器。

`media_lookup_indexes` 保留来源别名顺序、MediaKind 隔离和未知年份的公开行为；`library_schema_audit` / `library_query_plans` 检查索引、键顺序、查询工作量和正式表结构。索引检查通过不能替代结果正确性，也不能用增加索引后的单次快查询忽略写入、空间与清理成本。

## 组合容量门

至少准备 100k Media、1M 文件、1M Playback 单元，同时包含超长 TV、同 Media 多文件版本、多 Library 引用、多 User、收藏、已播和历史。明确统计口径：Media、物理文件、覆盖单元、播放单元和日志是不同数量。24k STRM 压力实例不能替代此门。

仓库没有一个开箱即用命令自动完成这个组合 fixture。准备脚本必须经过核对，使用正式 DDL 和明确的发布流程；直接播种 SQL 适合隔离查询测量，但不能据此宣称生产写路径已验证。保留 generator、输入摘要、主键集合、外键检查和业务 oracle。

1. **前置 oracle**：停服核对数量、外键、Library/User 授权、文件覆盖和独立预期；避免从被测计数/查询投影生成自己的答案。
2. **操作终态**：运行公开查询、写入、完整性与恢复工作，持续记录 operation ID、游标、阶段、总步数及错误。必须跑到成功终态，再做后置 oracle；中断/超时只能报告已观察区间。
3. **成本**：每阶段记录 SQL 工作量、解码/分配字节、吞吐、reader 等待、writer 等待和持有、事务/commit、WAL/DB/索引体积及磁盘剩余。
4. **重启与竞态**：在批边界、发布前后和恢复阶段重复，验证幂等与事实/投影守恒。完成状态与实际可读结果都要核对。
5. **报告边界**：固定目标的无关库增长、单 Media 高扇出、整库线性推进和组合规模分别给结论，不相互替代。

## 延迟定位

按 [perfgate](performance-slo.md) 保留全部路由、准备结果、有效业务基线、idle/busy/cold 和完整正文时钟。完整门的业务期望生成器需先适配当前生产布局并验证输入合同；缺少相符 oracle 时保持 Unverified。用户可见的 Similar ready、页面 API 集合、图片解码与实际播放各有自己的完成点。

慢写按同一请求/操作关联以下区间：准入等待 → 实际 writer 等待 → SQL/归约 → COMMIT → 响应正文完成。必要时观察 SQLite PROFILE/VFS 的 WAL `xWrite`/`xSync`，区分 SQL、提交同步和调度墙钟。慢 COMMIT 只证明相应 writer 被占用，不能自动归因为某条 SELECT、FTS 或 Transfer 发布；Integrity 推进与 Scan/Transfer 发布分别标记。

插桩的普通日志与结构化测量事件分开通道收集，先核实各自实际输出流，再检查记录数、请求 ID、扫描 running 窗口和最终水印。日志解析失败使关联证据无效，不能沿用尚未验证的归因。监控与采样有开销，必须有相同工作量的普通构建对照，正式门使用无额外插桩构建。

图片 504 需要拆开等待图片槽、共享 fill、上游请求/正文、缓存发布与每位观察者的交付截止。后加入者应有自己的预算，不能继承已过期等待者的截止；但实际 fill 的资源名额必须持有到 worker 结束。验证共享成功、共享失败、单个观察者超时和离线缓存恢复。TMDB 策略另见 [请求重试回归](tmdb-retry-regressions.md)。

## 多批 Scan / Transfer 内存

同一 backend 连续运行至少三批，记录启动空闲、每批开始/峰值/成功后持久空闲、下一批以及同源重扫后的数据。Scan 与 Transfer 分别执行；Transfer 覆盖不同 Media 和同 Media 多版本，记录真实文件发布、哈希、NFO、任务终态与重启读回。

同时采集 RSS、物理占用、heap 存活分配、队列/缓存大小、SQLite 连接/页面缓存、网络/TLS 和日志缓冲。macOS 可使用 `vmmap`、`heap`、`malloc_history`，先核对当前系统工具参数与进程权限。分配栈只能说明分配点；没有 allocation 身份和引用关系时不能宣称已证明完整 owner。

用已知分配/释放正控确认采集器有效。将 SQLite 缓存、连接池、TLS 缓冲和分配器保留分别解释；RSS 不立即下降不等于泄漏，单次下降也不证明无泄漏。不要把重启进程当成修复。虚拟映射大小、heap 与 RSS 不直接相加。报告有限时间/批数及尚未归属的部分，不推导长期无泄漏。

## 片头片尾与章节图

依据 [准确性与耗时手册](marker-accuracy-runbook.md)，固定完整来源 hash、音轨和时间映射，独立双人盲标，按 Media 划分 development/validation/holdout。人工标注缺失时准确性保持 insufficient，不从预测生成真值。

分别报告首次采集、有效音频复用刷新、强制重采、增量与重试。计时从 enqueue 前开始，到识别、发布、probe 释放和全部要求的 scene JPEG 成功解码终态；准备/哈希/编译在外。逐 kind 区分 matched、no_match、uncertain、error、保留结果与实际新发布。一个 kind 成功不能掩盖另一个未解决，任务 completed 也不等于识别准确。

真实页面从详情进入、生成、离开再重进、刷新、失败重试和正常服务重启，检查任务身份/generation/attempt、当前来源及每个 JPEG。缺失/损坏图不算 ready，旧源图不可复活，公开图片请求不依赖自定义 Authorization。晚片头有限窗口、采样预算、未知时长和网络故障分别记录，不扩大窗口后删除失败分母。

## 交付清单

每个门记录数据/工具/产品身份、命令、退出码、有效样本/尝试数、业务 oracle、实际结果与限制。原始 Fail 不被重跑覆盖，Blocked/Not run/Unverified 不计通过。标注为诊断的结果不替代正式验收。

交付范围包含代码审查、单元/集成、Web、真实 STRM/本地文件、VidHub、perfgate、容量、内存和 marker 中实际执行的项；若接受具体遗留，明确写接受范围而不改写测量结果。报告与私有产物不入 Git，收尾核对原数据清单、自有进程、监听、句柄和磁盘。
