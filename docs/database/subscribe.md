# subscribe.db：Subscribe 策略、覆盖事实与在途记录

[返回数据库总览](README.md)

文件身份来自 [schema/layout.rs](../../crates/store/src/schema/layout.rs)：`application_id = 1129139250`、`user_version = 2`。这是生产 DDL 字段字典；结构复核方法见[数据库总览](README.md#结构复核方法)。

`subscribes.created_at` / `updated_at` 为 INTEGER Unix 毫秒，写入须显式提供非 NULL 时间戳。Store 对生产 DDL 的表/索引定义进行只读预检，不导入数据或改写已有不同定义。

共 **18 张业务表、202 字段、31 个显式命名索引、8 外键（含 1 组复合外键，SQLite `pragma_foreign_key_list` 共 9 行）、104 CHECK、0 触发器**。

## 表目录

- [subscribes](#subscribes) — 26 个字段
- [subscribe_facts](#subscribe_facts) — 5 个字段
- [subscribe_wanted](#subscribe_wanted) — 9 个字段
- [pending_downloads](#pending_downloads) — 13 个字段
- [subscribe_library_fences](#subscribe_library_fences) — 已删除 Library 的永久 Subscribe 绑定关闭记录
- [download_operations](#download_operations) — 24 个字段
- [download_ownership](#download_ownership) — 16 个字段
- [boost_reservations](#boost_reservations) — 14 个字段
- [boost_task_facts](#boost_task_facts) — 23 个字段
- [subscribe_deferred_candidates](#subscribe_deferred_candidates) — 17 个字段
- [plugin_event_entity_sequences](#plugin_event_entity_sequences) — 3 个字段
- [plugin_event_outbox](#plugin_event_outbox) — 18 个字段
- [plugin_event_deliveries](#plugin_event_deliveries) — 18 个字段
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
| `policy_revision` | `INTEGER` | — | 是 | `1` | Subscribe 策略修订号；更新或解绑 Library 时单调递增并失效历史 defer 授权。 |

**生产表定义**

```sql
CREATE TABLE subscribes(id TEXT PRIMARY KEY NOT NULL,user_id TEXT NOT NULL,media_id TEXT NOT NULL,coverage_kind TEXT NOT NULL,season INTEGER,episode_from INTEGER,episode_to INTEGER,fetch_mode TEXT NOT NULL,filter_id TEXT NOT NULL,wash_cut INTEGER NOT NULL DEFAULT 0,wash_cut_filter_id TEXT,full_season_pack INTEGER NOT NULL DEFAULT 0,downloader_id TEXT,tracking_state TEXT NOT NULL DEFAULT 'active',follow_future INTEGER NOT NULL DEFAULT 0,search_interval_secs INTEGER NOT NULL DEFAULT 1800,keep_old_versions INTEGER NOT NULL DEFAULT 0,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,library_id TEXT,jobs_sync_generation TEXT NOT NULL,lifecycle_epoch INTEGER NOT NULL,jobs_sync_revision INTEGER NOT NULL DEFAULT 1 CHECK(jobs_sync_revision>0),jobs_sync_pending INTEGER NOT NULL DEFAULT 0 CHECK(jobs_sync_pending IN (0,1)),jobs_sync_create INTEGER NOT NULL DEFAULT 1 CHECK(jobs_sync_create IN (0,1)),policy_revision INTEGER NOT NULL DEFAULT 1 CHECK(policy_revision>0));
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
| `state` | `TEXT` | — | 是 | `'active'` | 状态文本；业务使用 `active`、`imported`、`stalled`、`rejected`、`orphan`，DDL 无枚举 CHECK。 |
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

**使用注意**：`active` 表示在途；`imported` 是 Transfer 后仍保留用于跟踪的记录，不代表仍待 Transfer；`stalled` 不再占用覆盖槽；`rejected` 表示种子源被确定性拒绝、Wash-cut 探测质量未达标或 `probe_wait` 超限，`load_rejected_enclosures` 同时加载 `rejected` 与 `stalled` 使自动轮次永久跳过该 enclosure（仅用户显式手动提交才会重新激活）；`orphan` 在目标 Downloader 被删除时由 `mark_pending_orphaned` 写入，停止原下载器的 Transfer 轮询，但允许自动轮次在其他可用下载器上重新选中并恢复为 `active`。进度没有增长时不会刷新 `last_progress_at`，读取停滞时间可回退到非零 `submitted_at`。这些行为不等于数据库触发器或 CHECK。

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

## subscribe_library_fences

Library 删除跨文件完成时，Subscribe 库先在同一事务写入关闭记录，再将既有 Subscribe 解绑。创建和更新的最终 Subscribe writer 检查此表，因此延迟或旧快照都不能恢复已删除 Library 的绑定。记录长期保留，Library ID 不可复用为 Subscribe 目标。

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `library_id` | `TEXT` | 1 | 是 | 无 | 被永久关闭的 Library 身份。 |

**生产表定义**

```sql
CREATE TABLE subscribe_library_fences(library_id TEXT PRIMARY KEY NOT NULL);
```

## download_operations

统一 `DownloadSubmitter` 持久化操作台账：幂等去重、冻结计划摘要、`unknown` 效果对账与操作追踪。`subscribe_id` 可空且无外键，删除 Subscribe 后仍保留以便异步对账与 Boost/手动记录溯源。

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `operation_id` | `TEXT` | 1 | 是 | 无 | 全局唯一下载操作 ID。 |
| `idempotency_key` | `TEXT` | — | 是 | 无 | 幂等键（与 `subject` 组合唯一）。 |
| `source_kind` | `TEXT` | — | 是 | 无 | 触发入口类别（subscribe/manual/admit/boost/plugin）。 |
| `subscribe_id` | `TEXT` | — | 否 | 无 | 逻辑关联 `subscribes.id`（无外键级联删除）。 |
| `plugin_id` | `TEXT` | — | 否 | 无 | 发起或关联的插件 ID。 |
| `subject` | `TEXT` | — | 是 | 无 | 幂等主体标识。 |
| `cause_id` | `TEXT` | — | 是 | 无 | 因果链根 ID。 |
| `site_id` | `TEXT` | — | 否 | 无 | 来源站点 ID。 |
| `downloader_id` | `TEXT` | — | 否 | 无 | 冻结目标下载器 ID；进入 `submitting`/`submitted`/`unknown` 时非空。 |
| `endpoint_fingerprint` | `TEXT` | — | 否 | 无 | 冻结提交端点指纹。 |
| `enclosure` | `TEXT` | — | 是 | 无 | Torrent 入口 URL 或 magnet。 |
| `ownership_tag` | `TEXT` | — | 是 | 无 | `crawler-media-owned-<sha256>` 所有权标签。 |
| `info_hash` | `TEXT` | — | 否 | 无 | 小写 hex info hash。 |
| `bytes_digest` | `TEXT` | — | 否 | 无 | 已验证 `.torrent` 全文 SHA-256。 |
| `verified_bytes` | `INTEGER` | — | 否 | 无 | 解析并校验后的 Torrent 总字节数（`>= 0`）。 |
| `request_digest` | `TEXT` | — | 是 | 无 | 规范化请求摘要，用于校验幂等重放一致性。 |
| `state` | `TEXT` | — | 是 | 无 | 状态：`intent`/`reserved`/`submitting`/`submitted`/`rejected`/`deferred`/`failed`/`unknown`。 |
| `outcome_code` | `TEXT` | — | 否 | 无 | 结构化结果码。 |
| `error_message` | `TEXT` | — | 否 | 无 | 失败或异常原因说明。 |
| `revision` | `INTEGER` | — | 是 | `1` | CAS 乐观锁版本号（`> 0`）。 |
| `created_at` | `INTEGER` | — | 是 | 无 | 创建时间（Unix 秒）。 |
| `updated_at` | `INTEGER` | — | 是 | 无 | 最近更新时间（Unix 秒）。 |
| `submitted_at` | `INTEGER` | — | 否 | 无 | 确认提交给下载器的时间（Unix 秒）。 |
| `reconciled_at` | `INTEGER` | — | 否 | 无 | 对账完成时间（Unix 秒）。 |

**生产表定义**

```sql
CREATE TABLE download_operations(
  operation_id TEXT PRIMARY KEY NOT NULL CHECK(length(operation_id)>0),
  idempotency_key TEXT NOT NULL CHECK(length(idempotency_key)>0),
  source_kind TEXT NOT NULL CHECK(length(source_kind)>0),
  subscribe_id TEXT,
  plugin_id TEXT,
  subject TEXT NOT NULL CHECK(length(subject)>0),
  cause_id TEXT NOT NULL CHECK(length(cause_id)>0),
  site_id TEXT,
  downloader_id TEXT,
  endpoint_fingerprint TEXT,
  enclosure TEXT NOT NULL CHECK(length(enclosure)>0),
  ownership_tag TEXT NOT NULL CHECK(length(ownership_tag)>0),
  info_hash TEXT,
  bytes_digest TEXT,
  verified_bytes INTEGER CHECK(verified_bytes IS NULL OR verified_bytes>=0),
  request_digest TEXT NOT NULL CHECK(length(request_digest)>0),
  state TEXT NOT NULL CHECK(state IN ('intent','reserved','submitting','submitted','rejected','deferred','failed','unknown')),
  outcome_code TEXT,
  error_message TEXT,
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  submitted_at INTEGER,
  reconciled_at INTEGER,
  UNIQUE(subject,idempotency_key),
  CHECK(state NOT IN ('submitting','submitted','unknown') OR downloader_id IS NOT NULL)
);
```

**显式命名索引**

```sql
CREATE INDEX download_operations_subscribe_idx ON download_operations(subscribe_id,updated_at DESC,operation_id) WHERE subscribe_id IS NOT NULL;
CREATE INDEX download_operations_unresolved_idx ON download_operations(updated_at,operation_id) WHERE state IN ('intent','reserved','submitting','unknown');
CREATE INDEX download_operations_scope_idx ON download_operations(plugin_id,site_id,downloader_id,state,updated_at DESC,operation_id);
CREATE INDEX download_operations_hash_idx ON download_operations(downloader_id,info_hash) WHERE info_hash IS NOT NULL;
```

## download_ownership

下载器任务所有权账本：以 `(downloader_id, ownership_tag)` 为主键，严格区分 `owned`/`foreign`/`unproven`/`broken`/`deleted`，防止同 hash 外部任务误晋升为自有任务，并在 Subscribe 删除后持久化留存。

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `downloader_id` | `TEXT` | 1 | 是 | 无 | 下载器 ID。 |
| `ownership_tag` | `TEXT` | 2 | 是 | 无 | 所有权标签。 |
| `info_hash` | `TEXT` | — | 否 | 无 | 关联的 Torrent info hash。 |
| `operation_id` | `TEXT` | — | 是 | 无 | 外键引用 `download_operations(operation_id)`（无级联）。 |
| `subscribe_id` | `TEXT` | — | 否 | 无 | 逻辑关联 `subscribes.id`（无外键级联删除）。 |
| `plugin_id` | `TEXT` | — | 否 | 无 | 插件 ID。 |
| `subject` | `TEXT` | — | 是 | 无 | 归属主体。 |
| `site_id` | `TEXT` | — | 否 | 无 | 来源站点 ID。 |
| `enclosure` | `TEXT` | — | 是 | 无 | Torrent 入口。 |
| `ownership_state` | `TEXT` | — | 是 | 无 | `owned`/`foreign`/`unproven`/`broken`/`deleted`。 |
| `provenance_kind` | `TEXT` | — | 是 | 无 | `created_fresh`/`reconciled_operation`/`existing_owned`/`existing_foreign`/`broken_existing`/`unproven`。 |
| `verified_bytes` | `INTEGER` | — | 否 | 无 | 已验证 Torrent 字节数（`>= 0`）。 |
| `bytes_digest` | `TEXT` | — | 否 | 无 | 已验证 `.torrent` SHA-256。 |
| `revision` | `INTEGER` | — | 是 | `1` | CAS 版本号（`> 0`）。 |
| `created_at` | `INTEGER` | — | 是 | 无 | 创建时间（Unix 秒）。 |
| `updated_at` | `INTEGER` | — | 是 | 无 | 更新时间（Unix 秒）。 |

**生产表定义**

```sql
CREATE TABLE download_ownership(
  downloader_id TEXT NOT NULL CHECK(length(downloader_id)>0),
  ownership_tag TEXT NOT NULL CHECK(length(ownership_tag)>0),
  info_hash TEXT,
  operation_id TEXT NOT NULL REFERENCES download_operations(operation_id),
  subscribe_id TEXT,
  plugin_id TEXT,
  subject TEXT NOT NULL CHECK(length(subject)>0),
  site_id TEXT,
  enclosure TEXT NOT NULL CHECK(length(enclosure)>0),
  ownership_state TEXT NOT NULL CHECK(ownership_state IN ('owned','foreign','unproven','broken','deleted')),
  provenance_kind TEXT NOT NULL CHECK(provenance_kind IN ('created_fresh','reconciled_operation','existing_owned','existing_foreign','broken_existing','unproven')),
  verified_bytes INTEGER CHECK(verified_bytes IS NULL OR verified_bytes>=0),
  bytes_digest TEXT,
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY(downloader_id,ownership_tag)
);
```

**显式命名索引**

```sql
CREATE INDEX download_ownership_hash_idx ON download_ownership(downloader_id,info_hash) WHERE info_hash IS NOT NULL;
CREATE INDEX download_ownership_plugin_idx ON download_ownership(plugin_id,downloader_id,ownership_state,updated_at DESC,ownership_tag);
CREATE INDEX download_ownership_subscribe_idx ON download_ownership(subscribe_id) WHERE subscribe_id IS NOT NULL;
```

## boost_reservations

Boost 配额原子预留表：采用两阶段预留（`provisional` -> `final` -> `committed`/`unknown_held`/`released`），保证 `final`/`unknown_held` 必须具备已校验的 `bytes_digest` 并参与 `used + reserved + retained` 原子 CAS。

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `reservation_id` | `TEXT` | 1 | 是 | 无 | 预留记录 ID。 |
| `operation_id` | `TEXT` | — | 是 | 无 | 唯一外键引用 `download_operations(operation_id)`。 |
| `quota_key` | `TEXT` | — | 是 | 无 | 配额池键。 |
| `plugin_id` | `TEXT` | — | 是 | 无 | 插件 ID。 |
| `site_id` | `TEXT` | — | 否 | 无 | 站点 ID。 |
| `downloader_id` | `TEXT` | — | 是 | 无 | 下载器 ID。 |
| `info_hash` | `TEXT` | — | 否 | 无 | Torrent info hash。 |
| `bytes_digest` | `TEXT` | — | 否 | 无 | `.torrent` SHA-256（`final`/`unknown_held` 非空）。 |
| `reserved_bytes` | `INTEGER` | — | 是 | 无 | 预留字节数（`>= 0`）。 |
| `stage` | `TEXT` | — | 是 | 无 | `provisional`/`final`/`unknown_held`/`committed`/`released`。 |
| `expires_at` | `INTEGER` | — | 否 | 无 | 预留过期时间（Unix 秒）。 |
| `revision` | `INTEGER` | — | 是 | `1` | CAS 版本号（`> 0`）。 |
| `created_at` | `INTEGER` | — | 是 | 无 | 创建时间（Unix 秒）。 |
| `updated_at` | `INTEGER` | — | 是 | 无 | 更新时间（Unix 秒）。 |

**生产表定义**

```sql
CREATE TABLE boost_reservations(
  reservation_id TEXT PRIMARY KEY NOT NULL CHECK(length(reservation_id)>0),
  operation_id TEXT NOT NULL UNIQUE REFERENCES download_operations(operation_id),
  quota_key TEXT NOT NULL CHECK(length(quota_key)>0),
  plugin_id TEXT NOT NULL CHECK(length(plugin_id)>0),
  site_id TEXT,
  downloader_id TEXT NOT NULL CHECK(length(downloader_id)>0),
  info_hash TEXT,
  bytes_digest TEXT,
  reserved_bytes INTEGER NOT NULL CHECK(reserved_bytes>=0),
  stage TEXT NOT NULL CHECK(stage IN ('provisional','final','unknown_held','committed','released')),
  expires_at INTEGER,
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  CHECK(stage NOT IN ('final','unknown_held') OR bytes_digest IS NOT NULL)
);
```

**显式命名索引**

```sql
CREATE INDEX boost_reservations_quota_active_idx ON boost_reservations(quota_key,stage,updated_at,reservation_id) WHERE stage IN ('provisional','final','unknown_held');
CREATE INDEX boost_reservations_plugin_hourly_idx ON boost_reservations(plugin_id,created_at) WHERE stage IN ('final','unknown_held','committed');
CREATE UNIQUE INDEX boost_reservations_active_hash_idx ON boost_reservations(quota_key,downloader_id,info_hash) WHERE info_hash IS NOT NULL AND stage IN ('provisional','final','unknown_held');
```

## boost_task_facts

Boost 任务占用与保种/清理事实表：精确追踪活动任务占用（`occupied_bytes`）、删任务后保留文件占用（`retained_bytes`）、H&R 与最小保种窗口保护以及 10 分钟 `prepare-cleanup` 计划；`occupied_bytes` 与 `retained_bytes` 互斥以防重复计数。

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `task_fact_id` | `TEXT` | 1 | 是 | 无 | 任务事实 ID。 |
| `plugin_id` | `TEXT` | — | 是 | 无 | 插件 ID。 |
| `quota_key` | `TEXT` | — | 是 | 无 | 配额池键。 |
| `downloader_id` | `TEXT` | — | 是 | 无 | 复合外键引用 `download_ownership(downloader_id,ownership_tag)`。 |
| `ownership_tag` | `TEXT` | — | 是 | 无 | 复合外键引用 `download_ownership(downloader_id,ownership_tag)`。 |
| `info_hash` | `TEXT` | — | 是 | 无 | Torrent info hash（与 `quota_key, downloader_id` 唯一）。 |
| `site_id` | `TEXT` | — | 否 | 无 | 站点 ID。 |
| `state` | `TEXT` | — | 是 | 无 | `downloading`/`seeding`/`paused`/`retained_only`/`cleanup_planned`/`cleaned`。 |
| `occupied_bytes` | `INTEGER` | — | 是 | 无 | 活动任务占用字节（`retained_only`/`cleanup_planned`/`cleaned` 时必为 0）。 |
| `retained_bytes` | `INTEGER` | — | 是 | `0` | 仅留文件占用字节（`downloading`/`seeding`/`paused`/`cleaned` 时必为 0）。 |
| `hr_active` | `INTEGER` | — | 否 | 无 | H&R 状态：NULL 表示未知（fail-closed 禁止清理）、0/1 表示明确状态。 |
| `hr_until` | `INTEGER` | — | 否 | 无 | H&R 截止时间（Unix 秒）。 |
| `hold_until` | `INTEGER` | — | 否 | 无 | 最小保种截止时间（Unix 秒）。 |
| `free_until` | `INTEGER` | — | 否 | 无 | 免费窗口截止时间（Unix 秒）。 |
| `protection_state` | `TEXT` | — | 是 | `'unknown'` | `unknown`/`protected`/`eligible`/`blocked`。 |
| `protection_reason` | `TEXT` | — | 否 | 无 | 保护或阻断说明。 |
| `cleanup_plan_handle` | `TEXT` | — | 否 | 无 | 清理计划句柄。 |
| `cleanup_plan_expires_at` | `INTEGER` | — | 否 | 无 | 清理计划过期时间（最长 600 秒）。 |
| `cleanup_plan_json` | `TEXT` | — | 否 | 无 | 清理计划 JSON（需满足 `json_valid`）。 |
| `observed_at` | `INTEGER` | — | 是 | 无 | 最近观测时间（Unix 秒）。 |
| `revision` | `INTEGER` | — | 是 | `1` | CAS 版本号（`> 0`）。 |
| `created_at` | `INTEGER` | — | 是 | 无 | 创建时间（Unix 秒）。 |
| `updated_at` | `INTEGER` | — | 是 | 无 | 更新时间（Unix 秒）。 |

**生产表定义**

```sql
CREATE TABLE boost_task_facts(
  task_fact_id TEXT PRIMARY KEY NOT NULL CHECK(length(task_fact_id)>0),
  plugin_id TEXT NOT NULL CHECK(length(plugin_id)>0),
  quota_key TEXT NOT NULL CHECK(length(quota_key)>0),
  downloader_id TEXT NOT NULL CHECK(length(downloader_id)>0),
  ownership_tag TEXT NOT NULL CHECK(length(ownership_tag)>0),
  info_hash TEXT NOT NULL CHECK(length(info_hash)>0),
  site_id TEXT,
  state TEXT NOT NULL CHECK(state IN ('downloading','seeding','paused','retained_only','cleanup_planned','cleaned')),
  occupied_bytes INTEGER NOT NULL CHECK(occupied_bytes>=0),
  retained_bytes INTEGER NOT NULL DEFAULT 0 CHECK(retained_bytes>=0),
  hr_active INTEGER CHECK(hr_active IS NULL OR hr_active IN (0,1)),
  hr_until INTEGER,
  hold_until INTEGER,
  free_until INTEGER,
  protection_state TEXT NOT NULL DEFAULT 'unknown' CHECK(protection_state IN ('unknown','protected','eligible','blocked')),
  protection_reason TEXT,
  cleanup_plan_handle TEXT,
  cleanup_plan_expires_at INTEGER,
  cleanup_plan_json TEXT CHECK(cleanup_plan_json IS NULL OR json_valid(cleanup_plan_json)),
  observed_at INTEGER NOT NULL,
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  FOREIGN KEY(downloader_id,ownership_tag) REFERENCES download_ownership(downloader_id,ownership_tag),
  UNIQUE(quota_key,downloader_id,info_hash),
  CHECK((state IN ('downloading','seeding','paused') AND retained_bytes=0) OR (state IN ('retained_only','cleanup_planned') AND occupied_bytes=0) OR (state='cleaned' AND occupied_bytes=0 AND retained_bytes=0))
);
```

**显式命名索引**

```sql
CREATE INDEX boost_task_facts_quota_state_idx ON boost_task_facts(quota_key,state,updated_at DESC,task_fact_id);
CREATE INDEX boost_task_facts_plan_idx ON boost_task_facts(cleanup_plan_handle) WHERE cleanup_plan_handle IS NOT NULL;
CREATE INDEX boost_task_facts_plugin_idx ON boost_task_facts(plugin_id,downloader_id,state,task_fact_id);
```

## subscribe_deferred_candidates

Subscribe `download.before` `defer` 候选持久队列：严格绑定 `(subscribe_generation, policy_revision)`，最多 10 次或 72 小时（259,200 秒），超限转为 `rejected`；Subscribe 删除级联清理，策略变更或 Library 解绑立即失效旧记录。

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `defer_id` | `TEXT` | 1 | 是 | 无 | 延迟记录 ID。 |
| `subscribe_id` | `TEXT` | — | 是 | 无 | 外键引用 `subscribes(id) ON DELETE CASCADE`。 |
| `subscribe_generation` | `TEXT` | — | 是 | 无 | 绑定时的 `subscribes.jobs_sync_generation`。 |
| `policy_revision` | `INTEGER` | — | 是 | 无 | 绑定时的 `subscribes.policy_revision`（`> 0`）。 |
| `enclosure` | `TEXT` | — | 是 | 无 | Torrent 入口。 |
| `info_hash` | `TEXT` | — | 否 | 无 | Torrent info hash。 |
| `plugin_id` | `TEXT` | — | 否 | 无 | 触发 defer 的插件 ID。 |
| `hook_id` | `TEXT` | — | 是 | 无 | 触发 defer 的 hook ID。 |
| `reason` | `TEXT` | — | 是 | 无 | defer 原因说明。 |
| `attempt_count` | `INTEGER` | — | 是 | 无 | 已延迟次数（`1..=10`）。 |
| `first_deferred_at` | `INTEGER` | — | 是 | 无 | 首次延迟时间（Unix 秒）。 |
| `next_retry_at` | `INTEGER` | — | 是 | 无 | 下次重试时间（Unix 秒）。 |
| `max_deadline_at` | `INTEGER` | — | 是 | 无 | 最大截止时间（`<= first_deferred_at + 259200`）。 |
| `state` | `TEXT` | — | 是 | 无 | `deferred`/`rejected`/`invalidated`/`resolved`。 |
| `revision` | `INTEGER` | — | 是 | `1` | CAS 版本号（`> 0`）。 |
| `created_at` | `INTEGER` | — | 是 | 无 | 创建时间（Unix 秒）。 |
| `updated_at` | `INTEGER` | — | 是 | 无 | 更新时间（Unix 秒）。 |

**生产表定义**

```sql
CREATE TABLE subscribe_deferred_candidates(
  defer_id TEXT PRIMARY KEY NOT NULL CHECK(length(defer_id)>0),
  subscribe_id TEXT NOT NULL REFERENCES subscribes(id) ON DELETE CASCADE,
  subscribe_generation TEXT NOT NULL CHECK(length(subscribe_generation)>0),
  policy_revision INTEGER NOT NULL CHECK(policy_revision>0),
  enclosure TEXT NOT NULL CHECK(length(enclosure)>0),
  info_hash TEXT,
  plugin_id TEXT,
  hook_id TEXT NOT NULL CHECK(length(hook_id)>0),
  reason TEXT NOT NULL CHECK(length(reason)>0),
  attempt_count INTEGER NOT NULL CHECK(attempt_count>=1 AND attempt_count<=10),
  first_deferred_at INTEGER NOT NULL,
  next_retry_at INTEGER NOT NULL,
  max_deadline_at INTEGER NOT NULL CHECK(max_deadline_at>=first_deferred_at AND max_deadline_at<=first_deferred_at+259200),
  state TEXT NOT NULL CHECK(state IN ('deferred','rejected','invalidated','resolved')),
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(subscribe_id,subscribe_generation,policy_revision,enclosure)
);
```

**显式命名索引**

```sql
CREATE INDEX subscribe_deferred_due_idx ON subscribe_deferred_candidates(next_retry_at,defer_id) WHERE state='deferred';
CREATE INDEX subscribe_deferred_sub_idx ON subscribe_deferred_candidates(subscribe_id,state,enclosure);
```

## plugin_event_entity_sequences

按 `(entity_type, entity_id)` 持久化单调递增序列号（`WITHOUT ROWID`），确保即使已投递的 `plugin_event_outbox` 记录被清理，后续事件序列号也不会回退。

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `entity_type` | `TEXT` | 1 | 是 | 无 | 实体类型，CHECK 非空。 |
| `entity_id` | `TEXT` | 2 | 是 | 无 | 实体标识，CHECK 非空。 |
| `last_sequence` | `INTEGER` | — | 是 | 无 | 最新已分配单调序列号，CHECK `> 0`。 |

**生产表定义**

```sql
CREATE TABLE plugin_event_entity_sequences(
  entity_type TEXT NOT NULL CHECK(length(entity_type)>0),
  entity_id TEXT NOT NULL CHECK(length(entity_id)>0),
  last_sequence INTEGER NOT NULL CHECK(last_sequence>0),
  PRIMARY KEY(entity_type,entity_id)
) WITHOUT ROWID;
```

## plugin_event_outbox

`subscribe.db` 事务性插件事件 outbox；与所属域事实（如 `download.after`、`subscribe.*`）在同一写事务内写入，所有时间字段统一为 Unix **毫秒**（`>= 0`）。

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `event_id` | `TEXT` | 1 | 是 | 无 | 稳定事件 ID（用于插件幂等去重），CHECK 非空。 |
| `schema_version` | `INTEGER` | — | 是 | `1` | 事件结构版本号，CHECK `> 0`。 |
| `event_domain` | `TEXT` | — | 是 | 无 | 事件域（如 `download`、`subscribe`），CHECK 非空。 |
| `event_kind` | `TEXT` | — | 是 | 无 | 事件类型（如 `download.after`），CHECK 非空。 |
| `entity_type` | `TEXT` | — | 是 | 无 | 顺序作用域实体类型，CHECK 非空。 |
| `entity_id` | `TEXT` | — | 是 | 无 | 顺序作用域实体 ID，CHECK 非空。 |
| `entity_sequence` | `INTEGER` | — | 是 | 无 | 实体内单调序列号，CHECK `> 0`，与 `(entity_type, entity_id)` 联合唯一。 |
| `entity_revision` | `INTEGER` | — | 是 | `1` | 实体修订号，CHECK `>= 0`。 |
| `cause_id` | `TEXT` | — | 是 | 无 | 因果链追踪 ID，CHECK 非空。 |
| `subject` | `TEXT` | — | 是 | `'system'` | 触发主体标识，CHECK 非空。 |
| `site_id` | `TEXT` | — | 否 | 无 | 关联 Site ID（逻辑引用），非空串 CHECK。 |
| `subscribe_id` | `TEXT` | — | 否 | 无 | 关联 Subscribe ID（逻辑引用），非空串 CHECK。 |
| `library_id` | `TEXT` | — | 否 | 无 | 关联 Library ID（逻辑引用），非空串 CHECK。 |
| `handles_json` | `TEXT` | — | 是 | `'[]'` | JSON 数组字符串，CHECK `json_valid` 且为 `'array'`。 |
| `payload_json` | `TEXT` | — | 是 | 无 | JSON 对象字符串，CHECK `json_valid` 且为 `'object'`。 |
| `occurred_at` | `INTEGER` | — | 是 | 无 | 事件发生时间，Unix 毫秒（`>= 0`）。 |
| `fanout_state` | `TEXT` | — | 是 | `'pending'` | 扇出状态，CHECK `pending` / `seeded`。 |
| `created_at` | `INTEGER` | — | 是 | 无 | 入队时间，Unix 毫秒（`>= 0`）。 |

**生产表定义**

```sql
CREATE TABLE plugin_event_outbox(
  event_id TEXT PRIMARY KEY NOT NULL CHECK(length(event_id)>0),
  schema_version INTEGER NOT NULL DEFAULT 1 CHECK(schema_version>0),
  event_domain TEXT NOT NULL CHECK(length(event_domain)>0),
  event_kind TEXT NOT NULL CHECK(length(event_kind)>0),
  entity_type TEXT NOT NULL CHECK(length(entity_type)>0),
  entity_id TEXT NOT NULL CHECK(length(entity_id)>0),
  entity_sequence INTEGER NOT NULL CHECK(entity_sequence>0),
  entity_revision INTEGER NOT NULL DEFAULT 1 CHECK(entity_revision>=0),
  cause_id TEXT NOT NULL CHECK(length(cause_id)>0),
  subject TEXT NOT NULL DEFAULT 'system' CHECK(length(subject)>0),
  site_id TEXT CHECK(site_id IS NULL OR length(site_id)>0),
  subscribe_id TEXT CHECK(subscribe_id IS NULL OR length(subscribe_id)>0),
  library_id TEXT CHECK(library_id IS NULL OR length(library_id)>0),
  handles_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(handles_json) AND json_type(handles_json)='array'),
  payload_json TEXT NOT NULL CHECK(json_valid(payload_json) AND json_type(payload_json)='object'),
  occurred_at INTEGER NOT NULL CHECK(occurred_at>=0),
  fanout_state TEXT NOT NULL DEFAULT 'pending' CHECK(fanout_state IN ('pending','seeded')),
  created_at INTEGER NOT NULL CHECK(created_at>=0),
  UNIQUE(entity_type,entity_id,entity_sequence)
);
```

**显式命名索引**

```sql
CREATE INDEX plugin_event_outbox_pending_fanout_idx ON plugin_event_outbox(created_at,event_id) WHERE fanout_state='pending';
CREATE INDEX plugin_event_outbox_created_idx ON plugin_event_outbox(created_at,event_id);
CREATE INDEX plugin_event_outbox_domain_created_idx ON plugin_event_outbox(event_domain,event_kind,created_at,event_id);
CREATE INDEX plugin_event_outbox_entity_created_idx ON plugin_event_outbox(entity_type,entity_id,created_at,event_id);
```

## plugin_event_deliveries

每插件投递状态、租约、重试与 dead-letter 回执表。主键 `(event_id, plugin_id)` 兼作外键 `event_id -> plugin_event_outbox(event_id) ON DELETE CASCADE` 的前缀索引；对 `app.db.plugins` 不设跨库外键。时间字段统一为 Unix **毫秒**（`>= 0`）。

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `event_id` | `TEXT` | 1 | 是 | 无 | 外键引用 `plugin_event_outbox(event_id) ON DELETE CASCADE`。 |
| `plugin_id` | `TEXT` | 2 | 是 | 无 | 目标订阅插件 ID，CHECK 非空。 |
| `entity_type` | `TEXT` | — | 是 | 无 | 冗余实体类型（用于实体内保序索引），CHECK 非空。 |
| `entity_id` | `TEXT` | — | 是 | 无 | 冗余实体 ID（用于实体内保序索引），CHECK 非空。 |
| `entity_sequence` | `INTEGER` | — | 是 | 无 | 冗余单调序列号，CHECK `> 0`。 |
| `status` | `TEXT` | — | 是 | `'pending'` | CHECK：`pending` / `delivering` / `delivered` / `dead_letter`。 |
| `attempt_count` | `INTEGER` | — | 是 | `0` | 已认领尝试次数（认领进入 `delivering` 时 `+1`），CHECK `>= 0`。 |
| `next_attempt_at` | `INTEGER` | — | 是 | 无 | 下次可投递时间，Unix 毫秒（`>= 0`）。 |
| `claimed_at` | `INTEGER` | — | 否 | 无 | 租约认领时间（Unix 毫秒），仅 `delivering` 非空。 |
| `lease_until` | `INTEGER` | — | 否 | 无 | 租约过期时间（Unix 毫秒，`>= claimed_at`），仅 `delivering` 非空。 |
| `claim_nonce` | `TEXT` | — | 否 | 无 | 本次认领 CAS nonce，仅 `delivering` 非空。 |
| `last_error` | `TEXT` | — | 否 | 无 | 脱敏失败摘要（`<= 256` 字符），`dead_letter` 必非空。 |
| `component_state_json` | `TEXT` | — | 是 | `'{}'` | 组件级投递检查点 JSON 对象，CHECK `json_valid` 且为 `'object'`。 |
| `delivered_at` | `INTEGER` | — | 否 | 无 | 投递完成时间（Unix 毫秒），仅 `delivered` 非空。 |
| `dead_lettered_at` | `INTEGER` | — | 否 | 无 | 进入死信时间（Unix 毫秒），仅 `dead_letter` 非空。 |
| `revision` | `INTEGER` | — | 是 | `1` | CAS 修订号，CHECK `> 0`。 |
| `created_at` | `INTEGER` | — | 是 | 无 | 播种创建时间，Unix 毫秒（`>= 0`）。 |
| `updated_at` | `INTEGER` | — | 是 | 无 | 最近状态更新时间，Unix 毫秒（`>= 0`）。 |

**生产表定义**

```sql
CREATE TABLE plugin_event_deliveries(
  event_id TEXT NOT NULL REFERENCES plugin_event_outbox(event_id) ON DELETE CASCADE,
  plugin_id TEXT NOT NULL CHECK(length(plugin_id)>0),
  entity_type TEXT NOT NULL CHECK(length(entity_type)>0),
  entity_id TEXT NOT NULL CHECK(length(entity_id)>0),
  entity_sequence INTEGER NOT NULL CHECK(entity_sequence>0),
  status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','delivering','delivered','dead_letter')),
  attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count>=0),
  next_attempt_at INTEGER NOT NULL CHECK(next_attempt_at>=0),
  claimed_at INTEGER CHECK(claimed_at IS NULL OR claimed_at>=0),
  lease_until INTEGER CHECK(lease_until IS NULL OR lease_until>=0),
  claim_nonce TEXT CHECK(claim_nonce IS NULL OR length(claim_nonce)>0),
  last_error TEXT,
  component_state_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(component_state_json) AND json_type(component_state_json)='object'),
  delivered_at INTEGER CHECK(delivered_at IS NULL OR delivered_at>=0),
  dead_lettered_at INTEGER CHECK(dead_lettered_at IS NULL OR dead_lettered_at>=0),
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0),
  created_at INTEGER NOT NULL CHECK(created_at>=0),
  updated_at INTEGER NOT NULL CHECK(updated_at>=0),
  PRIMARY KEY(event_id,plugin_id),
  CHECK((status='delivering')=(claimed_at IS NOT NULL)),
  CHECK((status='delivering')=(lease_until IS NOT NULL)),
  CHECK((status='delivering')=(claim_nonce IS NOT NULL)),
  CHECK(status!='delivering' OR (attempt_count>0 AND lease_until>=claimed_at)),
  CHECK((status='delivered')=(delivered_at IS NOT NULL)),
  CHECK((status='dead_letter')=(dead_lettered_at IS NOT NULL)),
  CHECK(status!='dead_letter' OR (last_error IS NOT NULL AND length(last_error)>0))
);
```

**显式命名索引**

```sql
CREATE INDEX plugin_event_deliveries_due_idx ON plugin_event_deliveries(next_attempt_at,event_id,plugin_id) WHERE status='pending';
CREATE INDEX plugin_event_deliveries_lease_idx ON plugin_event_deliveries(lease_until,event_id,plugin_id) WHERE status='delivering';
CREATE INDEX plugin_event_deliveries_entity_order_idx ON plugin_event_deliveries(plugin_id,entity_type,entity_id,entity_sequence) WHERE status IN ('pending','delivering');
CREATE INDEX plugin_event_deliveries_dead_letter_idx ON plugin_event_deliveries(dead_lettered_at DESC,event_id,plugin_id) WHERE status='dead_letter';
CREATE INDEX plugin_event_deliveries_plugin_status_idx ON plugin_event_deliveries(plugin_id,status,updated_at DESC,event_id);
```
