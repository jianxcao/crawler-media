# jobs.db：通用 Job 调度、执行、重试与历史保留

[返回数据库总览](README.md)

结构核对：2026-11-03 当前生产 DDL；Jobs crate 的 [schema.sql](../../crates/jobs/src/schema.sql) 是唯一 DDL；[schema.rs](../../crates/jobs/src/schema.rs)拥有身份、只读预检、对象定义验证与原子初始化；独立 Queue 和 API 都使用 [Queue::open](../../crates/jobs/src/queue.rs)。Store 不再定义 Jobs 布局，也不依赖 Jobs 生产代码。

共 **6 张业务表、39 字段、11 个显式命名索引、6 触发器、3 个 CHECK、0 外键**。身份为 `application_id=1129139252 / user_version=1`。仅真正空的 0/0 文件在一个 IMMEDIATE 事务创建全部表、索引、触发器、初始计数单例与两个身份头；故障整体回滚。当前文件重开只校验，已填充的无头文件、旧/未知/未来布局或当前头却缺失/错误对象均拒绝且不修复。对象定义和非 sqlite 内部对象全集直接与唯一 DDL 的隔离内存执行结果比较，额外旧索引也拒绝；未维护第二份表/索引清单。

Queue 连接明确设置 WAL、busy_timeout、FK 和 synchronous=FULL，并与 Store 四个库共用同一套连接调优（`cache_size=-16384`、`temp_store=MEMORY`、`journal_size_limit=67108864`、`mmap_size=268435456`，打开时 `PRAGMA optimize=0x10002`、关闭时 `PRAGMA optimize`）；建库路径同样在切 WAL 之前设 `auto_vacuum=INCREMENTAL`，因此维护任务可以对该库跑 `PRAGMA incremental_vacuum`。Jobs 是持久执行事实，拒绝时提示先备份；启动不会删除队列。初始化各阶段故障、独立 Queue→ApiState 重开、损坏布局零写拒绝已有回归，详见执行记录（`git show 5e44f101:docs/superpowers/plans/2026-10-23-storage-review-remediation.md`）。

Queue 持有 jobs.db 唯一 writer，由独立 Queue 锁保护；API 查询通过 [jobs_readers.rs](../../crates/api/src/jobs_readers.rs) 的容量 2 严格只读 `store::ReaderPool` 和 `jobs::QueueRead` 执行，不获取 Queue writer 锁。

本库没有声明外键。下表 NOT NULL 指显式声明：`job_defs.id`、`jobs.id` 与 `retention_cursors.def_id` 的 `TEXT PRIMARY KEY` 没有另写 NOT NULL，不能误报成显式非空。主键自动索引不计入命名索引。Job 状态枚举、payload JSON、enabled 的 0/1 都是业务约定，不是 DDL CHECK。

## 表目录

- [job_change_state](#job_change_state) — 3 个字段
- [job_defs](#job_defs) — 9 个字段
- [jobs](#jobs) — 14 个字段
- [retention_cursors](#retention_cursors) — 5 个字段
- [subscribe_config_receipts](#subscribe_config_receipts) — 3 个字段
- [subscribe_lifecycle_fences](#subscribe_lifecycle_fences) — 5 个字段

`(kind, scope_key)` 是结构化身份，payload 是参数。配置 receipt 与 Jobs 变更同事务提交；普通身份删除可删除 receipt，但 lifecycle fence 仍保留，阻止旧 incarnation 迟到重开。Subscribe 的投递与确认跨库进行，新 incarnation 投递前可短暂看到旧配置；runner 重读当前 Subscribe。已关闭 fence 和终态 epoch 可能累积，未承诺自动压缩或总量上限。

## job_change_state

Job 更新流用的单例修订/终态计数，避免每次订阅更新都全表聚合。

主要实现：[crates/jobs/src/change_state.rs](../../crates/jobs/src/change_state.rs)。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `INTEGER` | 1 | 否 | 无 | 单例主键，固定为 1（CHECK id = 1）。 |
| `revision` | `INTEGER` | — | 是 | `0` | 单例单调修订号；不是跨 DB 全局事务序号。 |
| `finished_count` | `INTEGER` | — | 是 | `0` | 满足 `finished_at IS NOT NULL AND finished_at > 0` 的 jobs 行数；触发器不按 status 判定，不是只增不减的累计完成量。 |

**生产表定义**

```sql
CREATE TABLE job_change_state (
    id INTEGER PRIMARY KEY CHECK(id = 1), revision INTEGER NOT NULL DEFAULT 0,
    finished_count INTEGER NOT NULL DEFAULT 0
);
```

**使用注意**：单例 id=1（CHECK）。新库事务写入 revision=0、finished_count=0；重开不重建计数。revision 同时由 jobs 和 job_defs 的 INSERT/UPDATE/DELETE 推进，即使 UPDATE 未改变业务值也会增加；finished_count 随删除回减。它不是按终态 status 聚合，也不等于历史累计完成次数。retention_cursors 变化不推进该修订号。

## job_defs

Job 定义：调度周期、启用状态、payload 与并发/超时策略；用户配置不可当缓存丢弃。

主要实现：[crates/jobs/src/store.rs](../../crates/jobs/src/store.rs)。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 否 | 无 | 该表记录主键；UUID 文本为主，具体类型见字段类型。 |
| `kind` | `TEXT` | — | 是 | 无 | 与 jobs.kind 同一 JobKind 标签体系。 |
| `scope_key` | `TEXT` | — | 是 | 无 | 结构化 Job 身份的作用域键。 |
| `name` | `TEXT` | — | 是 | 无 | 显示名称或内部对象名；依表上下文。 |
| `enabled` | `INTEGER` | — | 是 | 无 | 是否启用；SQLite 0/1。 |
| `schedule` | `TEXT` | — | 否 | 无 | Job 调度序列化文本；当前 Schedule::Interval 写成 interval:<secs>，NULL 表示无周期。 |
| `payload` | `TEXT` | — | 是 | 无 | 序列化业务负载/查询结果；字段名不保证所有表都使用相同 JSON 结构。 |
| `timeout_secs` | `INTEGER` | — | 否 | 无 | Job 超时秒数。 |
| `concurrency_key` | `TEXT` | — | 否 | 无 | Job 并发互斥键；相同键的运行实例需串行。 |

**生产表定义**

```sql
CREATE TABLE job_defs (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            scope_key TEXT NOT NULL,
            name TEXT NOT NULL,
            enabled INTEGER NOT NULL,
            schedule TEXT,
            payload TEXT NOT NULL,
            timeout_secs INTEGER,
            concurrency_key TEXT
        );
```

**显式命名索引**

```sql
CREATE UNIQUE INDEX job_defs_identity ON job_defs(kind, scope_key);
```

**触发器**

```sql
CREATE TRIGGER trg_job_defs_change_del AFTER DELETE ON job_defs
         BEGIN
             UPDATE job_change_state SET revision = revision + 1 WHERE id = 1;
         END;
CREATE TRIGGER trg_job_defs_change_ins AFTER INSERT ON job_defs
         BEGIN
             UPDATE job_change_state SET revision = revision + 1 WHERE id = 1;
         END;
CREATE TRIGGER trg_job_defs_change_upd AFTER UPDATE ON job_defs
         BEGIN
             UPDATE job_change_state SET revision = revision + 1 WHERE id = 1;
         END;
```

**使用注意**：schedule 当前为 interval:<secs> 或 NULL；payload 是按 JobKind 解码的 JSON 文本，维护策略/启用状态属于用户配置，不是可弃缓存。

## jobs

通用 Job 执行实例与重试状态；由 def_id 指向可选定义，也支持一次性 Job。

主要实现：[crates/jobs/src/store.rs](../../crates/jobs/src/store.rs)。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 否 | 无 | 该表记录主键；UUID 文本为主，具体类型见字段类型。 |
| `def_id` | `TEXT` | — | 否 | 无 | 可选逻辑引用 jobs.db.job_defs.id；一次性 Job 可以没有定义。 |
| `kind` | `TEXT` | — | 是 | 无 | JobKind 的序列化名称；包括 subscribe_search、subscribe_rss、transfer、scrape、check_in、catalog_refresh、watch_intake 及维护类。 |
| `scope_key` | `TEXT` | — | 是 | 无 | 结构化 Job 身份的作用域键。 |
| `payload` | `TEXT` | — | 是 | 无 | 序列化业务负载/查询结果；字段名不保证所有表都使用相同 JSON 结构。 |
| `status` | `TEXT` | — | 是 | 无 | 通用 Job 状态：queued/running/succeeded/failed/cancelled。 |
| `attempt` | `INTEGER` | — | 是 | `0` | 已记录的失败尝试次数（含失败/超时恢复路径）；领取和成功完成不递增，不是执行总次数。 |
| `run_after` | `INTEGER` | — | 是 | 无 | Job 最早可运行时刻，Unix 秒。 |
| `started_at` | `INTEGER` | — | 否 | 无 | 本次 Job 开始时间，Unix 秒。 |
| `finished_at` | `INTEGER` | — | 否 | 无 | 通用 Job 结束时间，Unix 秒。 |
| `error` | `TEXT` | — | 否 | 无 | 可选错误说明；不是错误状态的唯一判据。 |
| `progress` | `TEXT` | — | 否 | 无 | Job 进度/报告的序列化文本。 |
| `concurrency_key` | `TEXT` | — | 否 | 无 | Job 并发互斥键；相同键的运行实例需串行。 |
| `timeout_secs` | `INTEGER` | — | 否 | 无 | Job 超时秒数。 |

**生产表定义**

```sql
CREATE TABLE jobs (
            id TEXT PRIMARY KEY,
            def_id TEXT,
            kind TEXT NOT NULL,
            scope_key TEXT NOT NULL,
            payload TEXT NOT NULL,
            status TEXT NOT NULL,
            attempt INTEGER NOT NULL DEFAULT 0,
            run_after INTEGER NOT NULL,
            started_at INTEGER,
            finished_at INTEGER,
            error TEXT,
            progress TEXT,
            concurrency_key TEXT,
            timeout_secs INTEGER
        );
```

**显式命名索引**

```sql
CREATE INDEX jobs_finished_idx ON jobs(finished_at)
            WHERE finished_at IS NOT NULL;
CREATE INDEX jobs_identity_idx ON jobs(kind, scope_key, run_after DESC);
CREATE INDEX jobs_last_def_idx
            ON jobs(def_id, finished_at DESC, run_after DESC)
            WHERE status IN ('succeeded','failed','cancelled');
CREATE INDEX jobs_live_def_idx
            ON jobs(def_id, status, run_after) WHERE status IN ('queued','running');
CREATE INDEX jobs_live_identity_idx ON jobs(kind, scope_key) WHERE status IN ('queued','running');
CREATE UNIQUE INDEX jobs_queued_def
            ON jobs(def_id) WHERE def_id IS NOT NULL AND status = 'queued';
CREATE UNIQUE INDEX jobs_running_def
            ON jobs(def_id) WHERE def_id IS NOT NULL AND status = 'running';
CREATE INDEX jobs_running_key_idx
            ON jobs(concurrency_key) WHERE status = 'running';
CREATE INDEX jobs_running_status_idx
            ON jobs(status) WHERE status = 'running';
CREATE INDEX jobs_status_run_after_idx
            ON jobs(status, run_after) WHERE status = 'queued';
```

**触发器**

```sql
CREATE TRIGGER trg_jobs_change_del AFTER DELETE ON jobs
         BEGIN
             UPDATE job_change_state
             SET revision = revision + 1,
                 finished_count = finished_count - CASE WHEN OLD.finished_at IS NOT NULL AND OLD.finished_at > 0 THEN 1 ELSE 0 END
             WHERE id = 1;
         END;
CREATE TRIGGER trg_jobs_change_ins AFTER INSERT ON jobs
         BEGIN
             UPDATE job_change_state
             SET revision = revision + 1,
                 finished_count = finished_count + CASE WHEN NEW.finished_at IS NOT NULL AND NEW.finished_at > 0 THEN 1 ELSE 0 END
             WHERE id = 1;
         END;
CREATE TRIGGER trg_jobs_change_upd AFTER UPDATE ON jobs
         BEGIN
             UPDATE job_change_state
             SET revision = revision + 1,
                 finished_count = finished_count
                     - CASE WHEN OLD.finished_at IS NOT NULL AND OLD.finished_at > 0 THEN 1 ELSE 0 END
                     + CASE WHEN NEW.finished_at IS NOT NULL AND NEW.finished_at > 0 THEN 1 ELSE 0 END
             WHERE id = 1;
         END;
```

**使用注意**：queued/running/succeeded/failed/cancelled；def_id 可空。两个部分唯一索引分别约束每定义的 queued 和 running 实例，允许一个 running 加一个 queued；并非 def_id 在全表唯一。concurrency_key 的运行互斥另由领取逻辑检查。

## retention_cursors

按维护 Job 定义保存历史清理的 keyset 断点和周期截点。

主要实现：[crates/jobs/src/retention_cursor.rs](../../crates/jobs/src/retention_cursor.rs)。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `def_id` | `TEXT` | 1 | 否 | 无 | 历史保留任务的游标命名空间/定义 ID；通常按维护 def 隔离，不声明 FK。 |
| `cutoff` | `INTEGER` | — | 是 | 无 | 本轮历史清理固定的时间截点；沿用 Job 终态时间的 Unix 秒口径。 |
| `finished_at` | `INTEGER` | — | 是 | 无 | keyset 窗口最后处理的 Job finished_at，Unix 秒；与 last_rowid 一起消歧。 |
| `last_rowid` | `INTEGER` | — | 是 | 无 | 清理断点同时间戳下的 rowid 消歧游标。 |
| `cycle_started_at` | `INTEGER` | — | 是 | 无 | 最近一次非最终窗口保存游标的时间，Unix 秒；当前实现每窗口覆盖，并非稳定的周期起点。 |

**生产表定义**

```sql
CREATE TABLE retention_cursors (
    def_id TEXT PRIMARY KEY, cutoff INTEGER NOT NULL, finished_at INTEGER NOT NULL,
    last_rowid INTEGER NOT NULL, cycle_started_at INTEGER NOT NULL
);
```

**使用注意**：这里是 jobs.db 通用历史保留游标；data_retention 的分类清理断点还使用 app.db.settings 命名空间，不能假定所有清理游标都在本表。

## subscribe_config_receipts

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `subscribe_id` | `TEXT` | 1 | 是 | 无 | Subscribe 身份。 |
| `generation` | `TEXT` | 2 | 是 | 无 | 本次生命周期的 generation。 |
| `revision` | `INTEGER` | — | 是 | 无 | 已应用或待同步修订号。 |

**生产表定义**

```sql
CREATE TABLE subscribe_config_receipts (
 subscribe_id TEXT NOT NULL, generation TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>0),
 PRIMARY KEY(subscribe_id,generation)
);
```

## subscribe_lifecycle_fences

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `subscribe_id` | `TEXT` | 1 | 否 | 无 | Subscribe 身份。 |
| `epoch` | `INTEGER` | — | 是 | 无 | 单调生命周期 epoch。 |
| `generation` | `TEXT` | — | 是 | 无 | 本次生命周期的 generation。 |
| `revision` | `INTEGER` | — | 是 | 无 | 已应用或待同步修订号。 |
| `closed` | `INTEGER` | — | 是 | 无 | 是否关闭该生命周期。 |

**生产表定义**

```sql
CREATE TABLE subscribe_lifecycle_fences(subscribe_id TEXT PRIMARY KEY,epoch INTEGER NOT NULL,generation TEXT NOT NULL,revision INTEGER NOT NULL,closed INTEGER NOT NULL CHECK(closed IN (0,1)));
```
