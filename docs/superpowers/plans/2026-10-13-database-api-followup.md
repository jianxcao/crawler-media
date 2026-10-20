# 数据库与接口收敛：历史实施映射与剩余门

**2026-10-20 整理；不是重跑旧T1–T9的执行计划。** 最初基线 `723394e`，历史发现 [F01–F12](<../../audits/2026-10-13-database-api-review.md>)、[DB8合同](<2026-10-12-db8-maintenance-retention.md>)仍保留。旧四Store＋独立catalog/fanart、ledger／projection等实现指令已被当前布局替代；存储正确性按 [当前clean-cutover计划](<2026-10-20-storage-clean-cutover.md>)执行，不把历史性能／设备门无条件塞进存储切换。

## 1. 已有实现提交，不等于全部验收

| 原任务 | 提交 | 原合同／仍需核验 |
|---|---|---|
| T1 | `6f34c7a` | 禁用／取消／shutdown在批／库边界停止，真实blocking工作drain；当前runner已有检查，不再写全部缺失 |
| T2 | `9c4a1f1` | payload＋周期＋queued快照同事务；当前token仍只有payload，**schedule-only冲突仍未被完整CAS覆盖** |
| T3 | `d996788` | 保护计数下界／截断不误报全量，不跨批重复累加诊断；当前窗口语义见DB8 |
| T4 | `af4dbbc` | keyset／预算／preview隔离；单SQL与VACUUM真实停止／发布决策不能以批数代替 |
| T5 | `57a4837` | draft变更清preview、晚响应丢弃、last_report、queued立即执行／already-running、DevHttp不破坏维护子任务 |
| T6 | `f51b3f6` | Jobs每def最多live＋last的bounded top-1，坏旧payload不污染当前child，真实最新坏数据仍报错 |
| T7 | `59b9c98` | legacy封面不可变CAS；冻结／用户替换／触发器拒绝不改当前bytes，失败只清未引用新版本 |
| T8 | `ce40b56` | Playback／Artwork owned离锁、输入400／SQL故障5xx，不能故障伪装200空页 |
| T9 | `7aa1c7e` | release／真实loop／协议验收；提交存在不等于下述矩阵已完整取得 |

10-15状态记录明确T4/T5/T8未新增原计划要求的RED目标；不得补写未发生的RED。历史checkbox与“本次未实施”段落已移除，原文可从 `git show bd6c3c8:docs/superpowers/plans/2026-10-13-database-api-followup.md` 追溯。

## 2. 保留的验收门（原T9）

- 真实background job_loop临时库，不用StateRunner直调冒充：首次默认清理警告、播放默认关闭0删、queued策略刷新、disable批边界、shutdown停止claim并等待真实workers、busy／恢复。gate可控，无真实外网。
- 原release矩阵：固定seed≥5轮，100/500/2000/10000 Media、200001历史，movie/tv/mixed及NFO/图片对称、多Library；相同DTO对比median/p95、response bytes、CPU/RSS、锁等待、维护耗时、DB/WAL。
- 原gallery2000行 **延迟下降≥30%** 目标尚无完整达到证据；不能把1.3× speedup等同30%下降，也不能删除目标假装通过。新模型下需重新选择可比基线／明确批准替代目标，而非照搬旧查询。
- DB8开／关并发读矩阵、VACUUM停止合同、真实shutdown/drain仍须实证。缺失记录 `docs/benchmarks/2026-10-13-optimization-acceptance.md` 不是应先创建空白报告的理由；有真实运行再记录。
- VidHub／Infuse版本、脱敏method/path/response、Range206/416、断开取消与图片展示；无设备写Not run。fake/router成功不表示完整协议通过。
- 所有最终checks由后续实际实现任务统一执行并记录exit；本次文档整理未运行测试、构建或生产操作。后续 [10-15整改总表](<2026-10-15-performance-resource-remediation.md>)不自动覆盖这些缺口。

## 3. 独立未关闭要求（原T10）

| 原项 | 保留要求／边界 |
|---|---|
| env_override UI／空DATA | UI只读提示与后端409一致；空DATA明确拒绝或受控提示；startup/restart/_FILE隔离进程测试，不以文档猜测当前状态 |
| DB11 typed settings／overlay | 逐域key/类型/默认/version/损坏策略及单一来源；不自动增加热加载或兼容旧数据库 |
| R5外部同路径替换 | generation／来源身份＋publication gate，列出全部file/ledger发布者；旧文件名概念须映射现行file/source，不复制过期授权 |
| 封面孤儿版本GC | 只删内部版本目录、无当前引用且无inflight保留，竞态／重启验收；不删用户任意路径 |
| DB12冗余索引 | 逐条当前查询EQP、体积、读写成本后单独批准；旧索引不存在不构成DROP授权 |
| pool/root_id/持久read model | 旧模型设计门已被存储cutover改变；不恢复旧root权威／projection，先测当前瓶颈再单独设计 |
| 删除离锁／heartbeat strict | 全部发布者、per-item排他／generation、same-path重用、失败重试；不能把只读快照机械用于副作用，当前缺陷见新存储计划 |
| 默认分页／operation handle | 所有FE/CLI traversal调用方与当前契约完整迁移，不静默截断、不加旧行为shim |
| 全Jellyfin／streambuffer | 真实客户端调用；吞吐／Range／CPU／RSS／取消数据，原≥10%真实收益门仍未因合成测试自动通过 |
| 09-29／10-05／10-10历史审计 | 每ID=已修（代码＋回归）／仍坏（公开复现）／设计变化／证据不足；不能全关或全开 |
| adaptive／rename／removal／probe | 按各自现行合同核验取消恢复、整季STRM、UI／设备；不重执行旧unchecked实现步骤 |
| 文件／日志维护 | 触及时按责任遵守大小上限；损坏payload日志重复、冷却读取失败保守性独立核验，不替代核心缺陷 |

## 4. 执行约束

使用CONTEXT词汇；Store/Queue不嵌套、Registry→Store锁序；owned blocking工作持permit到真结束，任何timeout不等于取消；静态图public，流／字幕鉴权即时撤销。发布／删除须重验当前scope/source/freeze，不自动清理stale详情缓存、不默认VACUUM/TRUNCATE、不擅自DROP索引。不访问live DB或自动删除旧目录，不把备份后的生产切换当本页授权。后续实施只补当前可复现缺陷与命名验收门，不照抄旧接口或预计测试结果。
