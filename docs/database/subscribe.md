# subscribe.db：Subscribe 策略、覆盖事实与在途记录

[返回数据库总览](README.md)

结构核对：2026-11-03 当前生产 DDL；文件身份来自 [schema/layout.rs](../../crates/store/src/schema/layout.rs)：`application_id = 1129139250`、`user_version = 1`。这是源码字典，不是用户实例 schema。

`subscribes.created_at` / `updated_at` 为 INTEGER Unix 毫秒，写入须显式提供非 NULL 时间戳。Store 对当前 DDL 的表/索引定义进行只读预检，不导入数据或改写已有不同定义。没有历史 schema fixture、迁移或字段兼容。

共 **9 张业务表、67 字段、7 个显式命名索引、3 外键、7 CHECK、0 触发器**。

## 表目录

- [subscribes](#subscribes) — 25 个字段
- [subscribe_facts](#subscribe_facts) — 5 个字段
- [subscribe_wanted](#subscribe_wanted) — 9 个字段
- [pending_downloads](#pending_downloads) — 13 个字段
- [subscribe_jobs_cleanup](#subscribe_jobs_cleanup)、[subscribe_reference_ops](#subscribe_reference_ops)、[subscribe_merge_fences](#subscribe_merge_fences)、[subscribe_merge_controls](#subscribe_merge_controls)、[subscribe_recovery_cursor](#subscribe_recovery_cursor) — 持久引用及恢复状态

业务创建先记录 generation/epoch，Library 保留引用后确认，再提交 Subscribe。删除与精确 generation 的释放和 Jobs 清理意图同事务记录；跨库恢复按有界窗口处理。关闭的 generation 不可重开。API 时间输出为经过范围检查的固定24字符 UTC 毫秒格式，坏持久值返回错误。

## subscribes

User 对一个 Media 的长期策略：覆盖范围、Fetch mode、Filter、Downloader、Library 与 Wash-cut 设置。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 是 | 无 | Subscribe 内部 ID。 |
| `user_id` | `TEXT` | — | 是 | 无 | 逻辑引用 `app.db.users.id`。 |
| `media_id` | `TEXT` | — | 是 | 无 | 逻辑引用 `library.db.media.media_id`，不是外部 Metadata source ID。 |
| `coverage_kind` | `TEXT` | — | 是 | 无 | 覆盖类别；业务使用 movie/tv。 |
| `season` | `INTEGER` | — | 否 | 无 | TV 目标季；电影覆盖不需要季号。 |
| `episode_from` | `INTEGER` | — | 否 | 无 | 覆盖起始集号。 |
| `episode_to` | `INTEGER` | — | 否 | 无 | 覆盖结束集号；NULL 的范围由覆盖策略解释。 |
| `fetch_mode` | `TEXT` | — | 是 | 无 | 获取 Torrent 的方式，业务使用 search/rss/both。 |
| `filter_id` | `TEXT` | — | 是 | 无 | 逻辑引用 `app.db.filters.id`。 |
| `wash_cut` | `INTEGER` | — | 是 | `0` | Wash-cut 开关。 |
| `wash_cut_filter_id` | `TEXT` | — | 否 | 无 | 独立 Wash-cut Filter，逻辑引用 `app.db.filters.id`。 |
| `full_season_pack` | `INTEGER` | — | 是 | `0` | 是否要求整季包。 |
| `downloader_id` | `TEXT` | — | 否 | 无 | 逻辑引用 `app.db.downloaders.id`；NULL 时由调用链选择默认项。 |
| `tracking_state` | `TEXT` | — | 是 | `'active'` | 跟随状态文本；DDL 不限制状态枚举。 |
| `follow_future` | `INTEGER` | — | 是 | `0` | 是否跟随未来尚未明确的覆盖范围。 |
| `search_interval_secs` | `INTEGER` | — | 是 | `1800` | 搜索间隔，秒；DDL 未限制正值。 |
| `keep_old_versions` | `INTEGER` | — | 是 | `0` | 是否保留被替换的文件版本。 |
| `created_at` | `INTEGER` | — | 是 | 无 | Unix 毫秒时间戳。 |
| `updated_at` | `INTEGER` | — | 是 | 无 | Unix 毫秒时间戳。 |
| `library_id` | `TEXT` | — | 否 | 无 | 目标 Library，逻辑引用 `library.db.libraries.library_id`。 |
| `jobs_sync_generation` | `TEXT` | — | 是 | 无 | 本次 Subscribe incarnation 的 generation。 |
| `lifecycle_epoch` | `INTEGER` | — | 是 | 无 | Subscribe 生命周期单调 epoch。 |
| `jobs_sync_revision` | `INTEGER` | — | 是 | `1` | Jobs 配置意图修订号。 |
| `jobs_sync_pending` | `INTEGER` | — | 是 | `0` | 是否有待投递的 Jobs 配置。 |
| `jobs_sync_create` | `INTEGER` | — | 是 | `1` | 同步是否需要创建定义。 |

**生产表定义**

```sql
CREATE TABLE subscribes(id TEXT PRIMARY KEY NOT NULL,user_id TEXT NOT NULL,media_id TEXT NOT NULL,coverage_kind TEXT NOT NULL,season INTEGER,episode_from INTEGER,episode_to INTEGER,fetch_mode TEXT NOT NULL,filter_id TEXT NOT NULL,wash_cut INTEGER NOT NULL DEFAULT 0,wash_cut_filter_id TEXT,full_season_pack INTEGER NOT NULL DEFAULT 0,downloader_id TEXT,tracking_state TEXT NOT NULL DEFAULT 'active',follow_future INTEGER NOT NULL DEFAULT 0,search_interval_secs INTEGER NOT NULL DEFAULT 1800,keep_old_versions INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,library_id TEXT,jobs_sync_generation TEXT NOT NULL,lifecycle_epoch INTEGER NOT NULL,jobs_sync_revision INTEGER NOT NULL DEFAULT 1 CHECK(jobs_sync_revision>0),jobs_sync_pending INTEGER NOT NULL DEFAULT 0 CHECK(jobs_sync_pending IN (0,1)),jobs_sync_create INTEGER NOT NULL DEFAULT 1 CHECK(jobs_sync_create IN (0,1)));
```

**显式命名索引**

```sql
CREATE INDEX subscribes_jobs_sync ON subscribes(id) WHERE jobs_sync_pending=1;
CREATE INDEX subscribes_media_idx ON subscribes(media_id);
CREATE INDEX subscribes_user_idx ON subscribes(user_id);
```

### 用户删除时的所有权转交合同
当一个用户被删除时（见 [app.db 用户生命周期](../database/app.md)）：
- 该用户创建的所有 `subscribes` 记录不会被物理级联删除（避免静默丢失已建立的长期下载策略）；
- 其所有权字段 `user_id` 在独立事务中安全转交给当前 `role=admin` 且 id 排序最小的管理员（`Store::admin_user_id`）；
- 当前用户删除流程未改写 `jobs.db` 的定义参数；定时任务按 Subscribe 身份重新读取现行所有权，不能据本段推断存在跨库原子更新；
- 当 Subscribe 移除了显式 `library_id` 绑定时，策略评估读取新 coverage/units，但已保存的非目标库事实不会跳到默认库。

## subscribe_facts

每个覆盖槽位已拥有文件的评分和路径事实，用于填空与 Wash-cut。它不是 Library 文件台账的替代物。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `subscribe_id` | `TEXT` | 1 | 是 | 无 | 外键引用本库 `subscribes.id`，随父行删除级联清理。 |
| `season` | `INTEGER` | 2 | 是 | `-1` | 覆盖槽季号；无季用 -1。 |
| `episode` | `INTEGER` | 3 | 是 | `-1` | 覆盖槽集号；无集用 -1。 |
| `score` | `INTEGER` | — | 是 | 无 | 已拥有文件的评分。 |
| `path` | `TEXT` | — | 否 | 无 | 已拥有文件路径；没有指向 Library 文件的外键。 |

**生产表定义**

```sql
CREATE TABLE subscribe_facts(subscribe_id TEXT NOT NULL REFERENCES subscribes(id) ON DELETE CASCADE,season INTEGER NOT NULL DEFAULT -1,episode INTEGER NOT NULL DEFAULT -1,score INTEGER NOT NULL,path TEXT,PRIMARY KEY(subscribe_id,season,episode));
```

**使用注意**：电影通常使用 `(-1,-1)`，TV 使用季/集。路径关联不能替代 Library 文件身份，也不能将一个槽位直接 JOIN 同一 Media 的所有文件版本后求和。

## subscribe_wanted

待满足槽位的搜索、抓取、Transfer 与拒绝履历。没有独立 `status` 列；是否满足或在途还需结合覆盖事实与 pending 记录判断。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `subscribe_id` | `TEXT` | 1 | 是 | 无 | 外键引用本库 `subscribes.id`，随父行删除级联清理。 |
| `season` | `INTEGER` | 2 | 是 | `-1` | 履历对应的季槽；无季用 -1。 |
| `episode` | `INTEGER` | 3 | 是 | `-1` | 履历对应的集槽；无集用 -1。 |
| `search_attempts` | `INTEGER` | — | 是 | `0` | 累计搜索次数。 |
| `last_search_at` | `INTEGER` | — | 否 | 无 | 最近搜索时间，Unix 秒。 |
| `grabbed_at` | `INTEGER` | — | 否 | 无 | 已抓取时间，Unix 秒。 |
| `imported_at` | `INTEGER` | — | 否 | 无 | Transfer 完成时间，Unix 秒。 |
| `grab_title` | `TEXT` | — | 否 | 无 | 已抓取 Torrent 的显示标题。 |
| `last_reject_reason` | `TEXT` | — | 否 | 无 | 最近候选拒绝说明。 |

**生产表定义**

```sql
CREATE TABLE subscribe_wanted(subscribe_id TEXT NOT NULL REFERENCES subscribes(id) ON DELETE CASCADE,season INTEGER NOT NULL DEFAULT -1,episode INTEGER NOT NULL DEFAULT -1,search_attempts INTEGER NOT NULL DEFAULT 0,last_search_at INTEGER,grabbed_at INTEGER,imported_at INTEGER,grab_title TEXT,last_reject_reason TEXT,PRIMARY KEY(subscribe_id,season,episode));
```

## pending_downloads

已向 Downloader 提交的 Torrent 及后续进度、延迟选集、probe 等待事实。以 Subscribe/enclosure 去重；不是通用 Job 实例表。进度与等待字段的行为见 [pending_progress.rs](../../crates/store/src/pending_progress.rs)。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `subscribe_id` | `TEXT` | 1 | 是 | 无 | 外键引用本库 `subscribes.id`，随父行删除级联清理。 |
| `enclosure` | `TEXT` | 2 | 是 | 无 | Torrent 下载入口，参与去重。 |
| `title` | `TEXT` | — | 是 | 无 | Torrent 标题。 |
| `score` | `INTEGER` | — | 是 | 无 | 提交时评分。 |
| `torrent_json` | `TEXT` | — | 是 | 无 | 已提交 Torrent 的序列化事实，可包含 Site、实际 Downloader、Release override 等；DDL 不验证 JSON。 |
| `submitted_at` | `INTEGER` | — | 是 | `0` | 提交时间，Unix 秒；0 表示没有有效提交时间记录。 |
| `state` | `TEXT` | — | 是 | `'active'` | 状态文本；业务使用 active、imported、stalled、rejected，DDL 无枚举 CHECK。 |
| `last_progress` | `REAL` | — | 否 | 无 | 最近记录的增长进度，调用者约定范围 0..=1；DDL 不限制数值。 |
| `last_progress_at` | `INTEGER` | — | 否 | 无 | 最近检测到进度增长的时间，Unix 秒；并非每次轮询都更新。 |
| `info_hash` | `TEXT` | — | 否 | 无 | Downloader 分配的 Torrent info hash；写入者转为小写。 |
| `selective_missing` | `TEXT` | — | 否 | 无 | 等待文件列表后保留的集槽，格式 `s:e,s:e`；NULL 表示无延迟选择。 |
| `probe_wait_attempts` | `INTEGER` | — | 否 | 无 | 无法 probe 源文件的等待轮数；首次记录由写入者从 0 增加。 |
| `probe_wait_since` | `INTEGER` | — | 否 | 无 | 首次记录 probe 等待的时间，Unix 秒。 |

**生产表定义**

```sql
CREATE TABLE pending_downloads(subscribe_id TEXT NOT NULL REFERENCES subscribes(id) ON DELETE CASCADE,enclosure TEXT NOT NULL,title TEXT NOT NULL,score INTEGER NOT NULL,torrent_json TEXT NOT NULL,submitted_at INTEGER NOT NULL DEFAULT 0,state TEXT NOT NULL DEFAULT 'active',last_progress REAL,last_progress_at INTEGER,info_hash TEXT,selective_missing TEXT,probe_wait_attempts INTEGER,probe_wait_since INTEGER,PRIMARY KEY(subscribe_id,enclosure));
```

**显式命名索引**

```sql
CREATE INDEX pending_downloads_sub_state_idx ON pending_downloads(subscribe_id,state);
```

**使用注意**：`active` 表示在途；`imported` 是 Transfer 后仍保留用于跟踪的记录，不代表仍待 Transfer；`stalled` 不再占用覆盖槽。进度没有增长时不会刷新 `last_progress_at`，读取停滞时间可回退到非零 `submitted_at`。这些行为不等于数据库触发器或 CHECK。

## subscribe_jobs_cleanup

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 是 | 无 | 记录身份或单例键。 |
| `generation` | `TEXT` | — | 是 | 无 | 本次生命周期的 generation。 |

**生产表定义**

```sql
CREATE TABLE subscribe_jobs_cleanup(id TEXT PRIMARY KEY NOT NULL,generation TEXT NOT NULL);
```

## subscribe_merge_controls

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `operation_id` | `TEXT` | 1 | 否 | 无 | 所属持久操作标识。 |
| `state` | `TEXT` | — | 是 | 无 | 持久操作或 fence 状态。 |

**生产表定义**

```sql
CREATE TABLE subscribe_merge_controls(operation_id TEXT PRIMARY KEY,state TEXT NOT NULL CHECK(state IN ('active','finished')));
```

## subscribe_merge_fences

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `media_id` | `TEXT` | 1 | 否 | 无 | Media 身份。 |
| `operation_id` | `TEXT` | — | 是 | 无 | 所属持久操作标识。 |

**生产表定义**

```sql
CREATE TABLE subscribe_merge_fences(media_id TEXT PRIMARY KEY,operation_id TEXT NOT NULL);
```

## subscribe_recovery_cursor

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `INTEGER` | 1 | 否 | 无 | 记录身份或单例键。 |
| `epoch` | `INTEGER` | — | 是 | 无 | 单调生命周期 epoch。 |

**生产表定义**

```sql
CREATE TABLE subscribe_recovery_cursor(id INTEGER PRIMARY KEY CHECK(id=1),epoch INTEGER NOT NULL);
```

## subscribe_reference_ops

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `epoch` | `INTEGER` | 1 | 否 | 无 | 单调生命周期 epoch。 |
| `subscribe_id` | `TEXT` | — | 是 | 无 | Subscribe 身份。 |
| `generation` | `TEXT` | — | 是 | 无 | 本次生命周期的 generation。 |
| `media_id` | `TEXT` | — | 是 | 无 | Media 身份。 |
| `release_mode` | `TEXT` | — | 是 | 无 | 释放引用或补偿模式。 |
| `compensation_json` | `TEXT` | — | 否 | 无 | 补偿所需的持久事实。 |
| `state` | `TEXT` | — | 是 | 无 | 持久操作或 fence 状态。 |

**生产表定义**

```sql
CREATE TABLE subscribe_reference_ops(epoch INTEGER PRIMARY KEY AUTOINCREMENT,subscribe_id TEXT NOT NULL,generation TEXT NOT NULL UNIQUE,media_id TEXT NOT NULL,release_mode TEXT NOT NULL CHECK(release_mode IN ('ordinary','compensate')),compensation_json TEXT,state TEXT NOT NULL CHECK(state IN ('creating','retained','committed','aborting','releasing','done')));
```

**显式命名索引**

```sql
CREATE UNIQUE INDEX subscribe_creating_id ON subscribe_reference_ops(subscribe_id) WHERE state IN ('creating','retained');
CREATE INDEX subscribe_reference_media_pending ON subscribe_reference_ops(media_id,epoch) WHERE state IN ('creating','retained');
CREATE INDEX subscribe_reference_pending ON subscribe_reference_ops(state,epoch);
```
