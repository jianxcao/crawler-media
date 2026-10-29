# 搜索、订阅、转移与维护共用一个 Job 队列

实例能排队、运行、重试或检查的每个工作单元都是 **Job** 行。搜索、**Subscribe** 填充、**Transfer**、**Scrape**、**Check-in**、catalog 刷新和 Watch-directory 扫描都进同一张表。不存在带私有内存映射的第二个“scheduler 模块”。

MoviePilot 拆分：APScheduler 进程内任务（`subscribe_search`、`transfer`、`sitedata_refresh`、…）外加独立的 `JobManager` 管理单个媒体的 transfer 任务。重启会丢失运行中的 APScheduler 状态；transfer 任务又是另一个 dict。那是两套 UI 和两套重试故事。我们只有一个 NAS 进程和 SQLite——一个队列就够了。

**Hook** 保持步骤拦截器（ADR-0002）。**Job** 是工作单元；**Hook** 可以在其中的步骤上行使否决。登录/**Check-in** **Plugins** 由该类 **Job** *调用*，它们本身不是队列。

## 位置

Store 四文件之外的队列文件：`{data}/jobs.db`。它不是可随意清空的 cache：清空会丢用户修改的 def 策略／开关／周期、queued/running 工作、重试和历史。当前 Media/别名、Library 文件与 Playback 在 `library.db`，Metadata source HTTP 响应在 `cache.db`，Subscribe 在 `subscribe.db`，实例配置在 `app.db`（`crates/store/src/open.rs`、`schema/v1/*.sql`；[ADR-0008](<0008-sqlite-system-of-record.md>)）。终态历史可按明确的 latest/tie 保护规则分批删；不得据“不丢 Media 身份”推断整个 `jobs.db` 可丢。

两张核心表（下面是字段摘要，不是完整 DDL；另有 `retention_cursors`、`job_change_state` 与变更 triggers，见 `crates/jobs/src/store.rs`、`retention_cursor.rs`、`change_state.rs`）：

```
job_defs (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,            -- subscribe_search | subscribe_rss | transfer
                                 -- | scrape | check_in | catalog_refresh | watch_intake
  name TEXT NOT NULL,
  enabled INTEGER NOT NULL,
  schedule TEXT,                 -- null = 仅按需；现行仅 interval:<secs>，未实现 cron
  payload TEXT NOT NULL,         -- JSON: subscribe_id, site_id, root_id, …
  timeout_secs INTEGER,
  concurrency_key TEXT           -- 可选互斥，如 site:<id> 或 subscribe:<id>
)

jobs (
  id TEXT PRIMARY KEY,
  def_id TEXT,                   -- null 表示一次性（手动搜索）
  kind TEXT NOT NULL,
  payload TEXT NOT NULL,
  status TEXT NOT NULL,          -- queued | running | succeeded | failed | cancelled
  attempt INTEGER NOT NULL DEFAULT 0,
  run_after INTEGER NOT NULL,    -- unix 秒
  started_at INTEGER,
  finished_at INTEGER,
  error TEXT,
  progress TEXT,                 -- 简短的运维可见行，不是日志文件
  -- 现行分别对 queued 和 running 建部分UNIQUE(def_id)；各状态至多一条，
  -- 不是一个覆盖两状态的共同唯一约束。应用逻辑另控制普通排期的live child。
)
```

WAL。进程内通过Queue mutex串行访问该连接，**不是专用单写线程**。API进程就是worker；正式循环claim时持锁、StateRunner执行离锁、落结果短锁，无Redis或独立worker服务。

## 运行时

每秒一次循环：`SELECT … FROM jobs WHERE status='queued' AND run_after<=now ORDER BY run_after LIMIT N`。用 `UPDATE … SET status='running' WHERE status='queued'` 认领，崩溃不会双跑同一行。进程启动时，超过 `timeout_secs` 的 `running` 变为 `queued` 且 `attempt+1`。`attempt` 达到 5 后置为 `failed` 并停止。

带schedule的def在完成时spawn follow-up（run_after=完成时间＋interval）；缺live child时ensure_scheduled也可立即补首个子任务。这里只是Interval循环，不是已支持cron表达式。UI 列出 `job_defs`（能跑什么）与近期 `jobs`（跑了什么）。手动搜索 POST 一个 `def_id` 为 null 的一次性 `jobs` 行。

并发：每tick最多claim2条，当前tick内逐条执行；后台每秒派发blocking tick且不等待上一tick结束，因此**不等价于全实例最多2个运行任务**。`concurrency_key`阻止同key的running重叠；不同key可跨tick并行，若需全局并发上限须单独设计／验收。

## Kind → 现有模块

| kind | payload | 调用 |
|---|---|---|
| `subscribe_search` | `subscribe_id` | Indexer.search + Filter + subscribe::run |
| `subscribe_rss` | （无 / site 列表） | Indexer.rss，然后对每个匹配 **Subscribe** 跑同一 run |
| `transfer` | `downloader_id` 可选 | 轮询已完成文件、**Naming template**、ledger |
| `scrape` | runner 不使用 Job payload | `watch_scrape::run(&state)` |
| `check_in` | `site_id` | Check-in **Plugin** |
| `catalog_refresh` | `media_id` | TMDB（等）经 `cache.db` 的来源表缓存，更新 Media/采用元数据 |
| `watch_intake` | runner 不使用 Job payload | `watch_intake::run(&state)` |

**历史拆分动机**：早期 `subscribe::run` 一次性做 search-admit-add-poll-transfer；当时 live harness 出现 `ledger=0`。Job 边界的目标是 `subscribe_search` 加到 Downloader 后结束，`transfer` Job 稍后读取完成文件。此段保留原因，不应将早期轮询实现描述成今天所有路径的现状。

手动关键词搜索是没有 **Subscribe** 的 `subscribe_search` 一次性 Job（payload 有 `query` + `filter_id`）。同一 runner，同一 UI 日志行。

## 默认（启用）

- 每个 active **Subscribe** 一个 `subscribe_search`，间隔 30 分钟（search **Fetch mode** 或 both）。
- 实例级 `subscribe_rss`，间隔 10 分钟（rss 或 both）。
- 实例级 `transfer`，间隔 30 秒。
- 每个启用 **Site** 一个 `check_in`，间隔 12 小时，`concurrency_key=site:<id>`。
- 每个被 **Subscribe** 指向的 **Media** 一个 `catalog_refresh`，间隔 24 小时。

创建 **Subscribe** 会插入其 `job_defs` 行。删除它会取消 queued 子行并禁用该 def。

## 维护任务（2026-10-20 源码状态）

当前注册 `database_maintenance`、`jobs_retention`、`playback_history_retention`、`data_retention` 四类；默认只开启前两类，保留参数仅在 `job_defs.payload`（`crates/api/src/maintenance/mod.rs`）。schema version 不是配置并发 revision。2026-10-13 的 总复核（`git show 5e44f101:docs/audits/2026-10-13-database-api-review.md`） 和 DB8 合同（`git show 5e44f101:docs/superpowers/plans/2026-10-12-db8-maintenance-retention.md`） 保留为历史发现/要求，不代表所有发现今天仍存在或已重新验收。

当前 runner 在批/库边界检查禁用、取消、停机；配置 CAS 已同事务保存 payload/可选周期并刷新 queued payload，running 保持快照。令牌仍仅覆盖 payload，不能检测仅改周期冲突。Jobs 预览与实跑走含保护行的原始 keyset 窗口，预览经 blocking budget 执行；批间 2 秒预算不是 SQL 硬执行上限。开发 `/jobs/tick` 跳过维护并将已 claim 的维护无惩罚放回 queued，不再取消它们。依据 `maintenance/runner.rs`、`http/job_maintenance.rs`、`jobs/src/store.rs`、`job_loop.rs`；代码存在不等于本次执行过这些回归。

## 不做这些

- APScheduler / cron 守护进程 / 额外进程。
- 每文件 transfer 子任务（MoviePilot 的 `task` vs `job`）。一个 `transfer` Job 处理 Downloader 自上次运行以来完成的任何内容；ledger 行是每文件记录。
- 把 Indexer 或 Downloader *作为* Job kind 内嵌。它们保持模块。Job 只做编排。
- Job kind 的插件市场。只有第一方 kind，与 **Plugins** 相同（ADR-0002）。
