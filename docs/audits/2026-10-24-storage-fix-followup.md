# 存储修复再次复核与补修

**固定版本补修记录：cf7fb70。** 本页保留该轮具体反例及红绿结果；当前发现见[只读复审](2026-10-24-storage-rereview.md)。下文“仍需测量”不是当前完整缺陷清单，本页当轮的改代码授权不适用于之后的只读请求。

日期：2026-10-24。比较基线为 `main@86122d73a4a23439e1d83234590fdd98b388fc50`，本次起点为 `feat/storage-redesign@3ba25fb`；本文与补修代码同一提交。用户本轮已授权发现问题后直接修复。没有操作真实实例或部署。

## 判断

五文件、Media/Library/Playback 同库、显式文件归属、事实与摘要同事务的设计合理，无需增加兼容层。分支上已经修复的 marks 严格输入、SQL 纯路径归一化、旧 Subscribe fixture 和超大测试文件继续保留；本次主要补齐异步观察的身份屏障、根迁移、共享图片及测试退出。

下表针对[5bd696e 历史复核](2026-10-24-storage-fix-review.md)的具体反例对账，不代表所有性能和设备验收完成。剩余工作只维护在[当前方案](../superpowers/plans/2026-10-23-storage-review-remediation.md)。

## 逐项复核

| 项目 | 本次结果与代码 | 可重复行为证据 |
|---|---|---|
| R01：旧 Marker / 指纹缓存 | 固定入队 FileVersion 与全部比较成员，采集及比较前后在锁外校验磁盘源；Marker 事务验证包括未持写 lease 的比较成员。指纹 compare/upsert 同一 IMMEDIATE 事务；season digest 含 source version 和样本 hash。tracks 在写 NFO 前检查源；普通自动比较的Marker合并与章节缓存也在同一带源校验的事务提交，保留手动锁定范围。 | API `probe_source_races` 覆盖真实 ProbeManager comparison/capture、磁盘与 DB 换源、tracks 和 season digest；Store `fingerprint_cache_tests` 用两连接竞争，`fingerprint_source_safety` 覆盖部分刷新成员。 |
| R02：旧根 scope | 普通发布验证同 kind 的当前最深 enabled 根，包括 root-relative 路径。扫描在观察前固定 Library/root/config，提交事务重验；已有文件复核完整 file/Media/root/path/version。Claim 在同一 IMMEDIATE 事务验证原存在/不存在上下文并发布，另一连接不能改名后被回滚或删除后被复活。 | `root_write_ownership`、`root_reassign_batched`、`file_publication_context`、API `watch_ledger::tests`；后者覆盖新增嵌套根发生在观察与提交之间。 |
| R03：marks | 分支原修复已拒绝未知/旧字段、重复坐标及越界；本次明确合法范围为非负 i32，补齐原 u32::MAX 等区间反例和零写断言。Web 已移除无用途的 device_id。 | `storage_http_cutover` 通过真实 Router 验证 i32::MAX+1/u32::MAX/u32::MAX+1、camelCase、旧键、错误类型等均 400，原始 playback_units 零写；合法 whole/整季/S00E00 的既有回归保留。 |
| R04：共享图片 | 视频目录、sidecar 和显式引用均按登记路径身份比较；alias 根及跨目录显式引用不会误判成独占图片。 | `file_rename_shared_artwork` 与 `rename_phases_lock_free`；仍有使用者时保留源图片，目标取得副本。 |
| R05：Unidentified | create/insert-root 的迁移增加独立阶段，每事务最多移动 100 条；更新后的归属就是持久进度。源/目标冻结时暂停，不确认跳过。 | `root_unidentified` 的 251 行逐批重开及新增根入口；`root_reassign_batched` 的冻结/重启回归。候选选择的工作量边界见下节。 |
| R06：初始化原子性 | 每个缺失 kind 的默认 Library 与 root 在同一 IMMEDIATE 事务提交，失败整体回滚；正常重开不复活用户已移除的根。 | `seed_defaults_atomicity` 注入真实 Store::open 的 root INSERT 故障，验证回滚及解除故障后重试；原 seed 重开测试保留。 |
| R07：测试退出 | FakeServer handler state 与 owner 分离；回收所有 handle 后再报告 panic，主动 shutdown socket，增加整个请求 deadline，避免 unwind 时二次 panic。 | `fixture_lifecycle` 的最后 owner、完成后 panic、多 panic、慢 header、peer 不读等子进程 watchdog；`subscription_e2e` 和 downloader 套件。 |
| R08：锁边界 | Probe、Claim、扫描 NFO/artwork、rename intent/sidecars/staging/completion 的冷 IO 使用 owned plan 在 Store 锁外执行，最终 DB/CAS 短提交。固定 alias snapshot 不随 reload 改变。rename 启动恢复每批最多 100 个，由 blocking worker 执行并让出调度。 | `claim_io_isolation` 门控真实 FIFO NFO 并验证 Store 可读、换源/改配置拒绝；`rename_phases_lock_free`、`rename_startup`、已有 scan/Probe 并行读取回归。根配置/Watch 等剩余路径未宣称完全出锁。 |
| R09：合同与门禁 | 旧 Subscribe 测试字段、A/a 物理文件冲突及大文件拆分已在分支修复。本次补充公开行为回归并按职责拆分触及的超限函数。 | 下节记录本提交的最终门禁；历史失败不会覆盖新结果。 |

## 本次补充发现

- 扫描 checkpoint/source 查询原来对现存文件重新 realpath：登记的根 alias 消失后会漏读断点。现在使用登记身份；`scan_seen` 主键改为 `(operation_id,path_key)`，同一物理身份不同显示拼写更新同一行。
- checkpoint 查询只读取本批路径，按最多 500 个键分片。公开查询在 5,000 条无关历史路径下，20,000 SQLite VM steps 预算内通过；没有把整次 operation 的历史逐批重新解码。
- 根目录登记时尚不存在，原实现未解析其已存在父目录的软链，后续 canonical 文件会被误判为库外。现在登记时解析已存在的父目录并固定身份；显式 alias/missing-root Store 回归和真实 Claim Router 图片/NFO 回归均覆盖此场景。
- 普通自动比较原来独立写 Marker 和章节。注入章节写入失败可留下半份 Marker，版本预检查与写入也有跨连接空隙；现改为单次事务验证全部比较源并合并 Marker/章节。目标坐标沿用比较时的首单元规则，whole 与其他坐标共存时也不会漏写。API 原路径失败回滚、Store 第二连接换源拒绝及首单元章节发布回归均经历 RED→GREEN。
- NFO 复核原来只比较 Library/source/config，另一连接改名而版本不变时仍能发布旧 locator 的观察。现在同时核验 file/Media/root/path/revision/source 和当前最深根；别名与 canonical 拼写的重扫继续有效。
- 已有文件的磁盘源在 NFO 观察时改变，旧观察会暂缓，扫描继续按既有失效/重处理合同处理；不会把替换源的 NFO 写到旧 FileVersion。新的源观察保留可用的文件创建时间。

## 验证

针对性反例先观察 RED，再验证修复后的 GREEN；测试使用临时数据库、假网络与注入 probe，不要求真实 Downloader、TMDB 或 Chromium。以下为本次实际命令的汇总，passed/ignored 按各测试进程的最终统计相加；ignored 不计入通过数。

| 命令 | 通过 | 失败 | 忽略 |
|---|---:|---:|---:|
| `cargo test --offline -p store` | 506 | 0 | 2 |
| `cargo test --offline -p api` | 1588 | 0 | 6 |
| `cargo test --offline -p library` | 114 | 0 | 0 |
| `cargo test --offline -p downloader` | 66 | 0 | 1 |
| `cargo test --offline --workspace` | 2714 | 0 | 12 |
| `cd web && pnpm test` | 842 | 0 | 0 |

以上命令均以 exit 0 完成；workspace 包含最后的首单元坐标补修。Web `pnpm typecheck`、`pnpm build` 均通过；build 仍有既有的大于 500 kB bundle 提示。98 个本次触及 Rust 文件的格式、800 行文件上限与 120 行函数上限检查通过；相关文档相对链接及 `git diff --check` 通过。

上述单 crate 的忽略项为人工 release/规模测量、真实 Obscura、真实实例副本和 qBittorrent 场景。原始命令输出与 RED/GREEN 日志位于 `/tmp/storage-final-fix-review/`；这不是长期归档路径，本文保存最终结果及可重跑的测试入口。

## 仍需测量或优化

没有确认新的生产锁环；R07 是测试 owner/join 生命周期缺陷。磁盘慢、长时间持有 Store、同步耗尽队列属于阻塞/调度风险，不能泛称数据库死锁。

- root create/update/insert 仍可同步 drain 全部队列；启动 root 恢复虽逐批释放 Store，仍同步循环。根配置 canonicalize 和部分 Watch 归属检查等尚有锁内路径 IO。不能把 R08 写成所有调用路径均已闭环。
- Unidentified 每事务移动行数有界，但选择 SQL 对 path 做归一化并求最深根，没有独立 path_key 索引，可能重复扫描历史；没有证明 VM steps/解码候选工作量严格有界。下一步方案是持久路径身份、索引及 keyset/phase，而非扩大事务。
- 大命中 genre 的精确 total、深 offset 仍可能线性增长。cache NORMAL、mmap、cache_size、temp_store、WITHOUT ROWID 尚无本次单变量 A/B 依据，不更改连接策略。
- NAS、真实浏览器/Jellyfin 设备、100k Media / 1M 文件 / 1M Playback 的完整组合验收：**Not run**。历史 50k 和无关库 1M 数字只对原版本/场景负责。
