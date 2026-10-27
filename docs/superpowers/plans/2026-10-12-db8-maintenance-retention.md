# DB8 数据库维护与历史保留合同

**2026-10-20 文档整理，源码基线 `bd6c3c8`。** 主功能与后续控制修复已经存在；本页不再把 2026-10-13 的全部偏差描述为当前事实，也不宣布发布验收完成。历史发现见 [10-13 review](<../../audits/2026-10-13-database-api-review.md>)，未关闭门见 [follow-up](<2026-10-13-database-api-followup.md>)。数据库边界见 [ADR-0008](<../../adr/0008-sqlite-system-of-record.md>)，调度见 [ADR-0009](<../../adr/0009-job-queue.md>)。

## 默认定义与安全边界

策略只在 `job_defs.payload` 保存一份，enabled/schedule 沿用 Jobs；按 kind＋concurrency_key 幂等注册，重启不覆盖用户设置。调度只有 Interval，不是固定凌晨／cron。

| kind | 首次 enabled | interval 秒 | 默认 payload |
|---|---|---|---|
| database_maintenance | true | 21600 | `{version:1,vacuum_enabled:false}` |
| jobs_retention | true | 86400 | `{version:1,retention_days:90,dry_run:false}` |
| playback_history_retention | false | 86400 | `{version:1,retention_days:90,dry_run:false}` |
| data_retention | false | 86400 | `{version:1,dry_run:true,qoe_days:90,transient_cache_days:90,probe_days:90}` |

来源：`crates/api/src/maintenance/mod.rs`、`policy.rs`。首次默认开启任务可能立即入队，Jobs超90天历史可能在首次后台执行被清理；DELETE不可撤销，恢复依赖一致备份。本次整理未执行真实清理。

- 未知version／字段／损坏JSON拒绝；retention_days=1..36500，永久保留用禁用，不用0。维护interval=3600..31536000秒。
- cutoff固定本轮started_at（缺省now）减保留天数，checked算术；严格小于cutoff，边界保留。每批500、最多20批／2秒预算；行数／批边界预算不保证单SQL扫描和持锁硬上限。
- Jobs只删有finished_at的 succeeded/failed/cancelled，不删queued/running/job_defs。每个非NULL def_id保留最新终态并列组，已删def仍保护；NULL独立按龄。每批独立事务，失败整批回滚，跨批不全局回滚。
- Playback现在删 **library.db.playback_logs** 的超龄已结束行，候选按ended_at/started_at/id读取；不删进度、收藏、session、QoE或文件。来源：`crates/store/src/playback/retention.rs`。删除后历史页／统计窗口收缩，无全时期汇总归档。
- 分类retention仅QoE、已分类transient cache、受保护Probe终态历史；默认关闭＋dry-run。详情／未知cache stale-if-error不按TTL删，Probe每scope保留最新3及ties并保护sample引用，不删模型／样本。具体保留合同见 [整改总表](<2026-10-15-performance-resource-remediation.md>)。

## 当前控制、API与报告

- `maintenance/runner.rs::check_maintenance_stop` 在每批／每库前检查def.enabled、status/attempt与shutdown；读状态失败保守停止。当前已提交批不回滚；取消／关闭不等于进行中的SQL立即停止。
- 管理员PUT `/api/v1/jobs/{id}/maintenance-config` 使用 `expected_payload`，缺令牌400、冲突409。`jobs/src/store.rs::update_def_config_cas` 在同事务保存payload／可选周期、刷新未运行queued快照，running保留旧快照；省略周期返回实际保留值。
- **尚存控制缺口：token只覆盖payload，不覆盖schedule。** 两个只改周期的writer不能靠该token检测冲突。原T2要求全配置CAS仍需完成，不把“同事务”当并发合同全部成立。
- POST maintenance-preview经blocking budget执行一批只读窗口，不创建Job／改配置／写游标，不返回播放明细；Jobs与实跑使用同一含保护行keyset窗口。`truncated`、`protected_is_lower_bound`／`incomplete`是下界信号，不是全量精确计数。
- 报告存job.progress，含kind/version/cutoff/dry_run、计数、批数、预算、elapsed与每库outcome；scanned/examined是应用窗口计数，不是SQLite实际examined行。dry-run只检视一批，不重复累加同一批制造总数。
- 开发tick跳过维护，不取消后台维护子任务；手动触发与UI异步preview／报告的完整门继续见follow-up，不能凭历史修复提交猜测所有时序都通过。

## 连接、VACUUM与停止保证

当前allowlist为Store真实app_path以及data_dir下 **cache/library/subscribe/jobs.db**（`maintenance/database.rs::database_targets`），不再维护旧catalog/fanart文件。Queue生产构造固定jobs.db；不宣称支持任意自定义Queue路径。

独立READ_WRITE连接，不带CREATE、不重复migration/seed/WAL设置，busy_timeout250ms、请求FK ON；不删除／搬移DB/WAL/SHM，不自动TRUNCATE checkpoint。默认仅PRAGMA optimize，允许no-op；DELETE页可重用，文件不缩小不是删除无效。

VACUUM必须显式开、成功冷却≥604800秒、free页≥64MiB且≥20%、可用磁盘≥主体2倍＋64MiB。Library工作活跃跳过重型路径但可optimize；预检是快照，不构成扫描互斥。busy/locked为skip，IO/corrupt/full为fail，冷却读取失败保守skip。

**30秒interrupt是best-effort，不是硬停止。** SQLite backup-copy／阻塞FS阶段可能不响应；必须等真实执行器结束再join，不能以900秒Job timeout或丢JoinHandle宣称停止。当前执行器如实保留超限elapsed，重型发布方案仍需独立决策／实测。进程内shared mutex避免不同维护def重叠；各def保留独立key，不擅改key造成幂等seed重复。

## 历史证据与未关闭门

原实现：`c03f39c`策略、`cbc98bb`／`7ce1c60`事务批次、`dd5e009`执行器／注册、`48d6efa`API/UI、`b06eb9a` StateRunner验收、`d1c8205`保护前缀推进。后续T1–T9提交与剩余门保留在follow-up。

2026-10-13原review记录workspace **226套件／1666 passed／0 failed／4 ignored**、web typecheck exit0；历史Playback fixture 200001超龄行，run1=20批／10000删除／45ms，run2再10000。这些记录保持历史含义：不是真实background loop、不是维护并发HTTP p95，不证明全保护Jobs扫描有界或当前新模型已验收。

尚需：全配置CAS、真实job_loop与shutdown/drain、重型终止保证、DB8开关前后HTTP median/p95／锁等待／DB-WAL矩阵和设备门。旧稿完整细节可由 `git show bd6c3c8:docs/superpowers/plans/2026-10-12-db8-maintenance-retention.md` 追溯；不恢复旧库布局或兼容升级，也不据本页自动开启清理。
