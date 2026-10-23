# subscribe.db：Subscribe 策略、覆盖事实与在途记录

[返回数据库总览](README.md)

结构核对基线：`feat/storage-redesign@8fbf81b`。字段、类型、默认值、主键和命名索引逐项来自 [Store v1/subscribe.sql](../../crates/store/src/schema/v1/subscribe.sql)；文件身份来自 [schema/layout.rs](../../crates/store/src/schema/layout.rs)：`application_id = 1129139250`、`user_version = 1`。这是生产DDL字段字典，并经隔离内存SQLite核对，不是用户实例schema，也不证明运行时调用者已完成切换。

共 **4 张业务表、47 个字段、3 个显式命名索引**。本库只保存 Subscribe 策略、覆盖/搜索事实和 pending 下载记录；不保存 Playback 会话、进度、历史或遥测。Playback 的存储边界见 [library.db](library.md)。Subscribe 是长期策略，不是 Job 执行队列；通用执行记录见 [jobs.db](jobs.md)。本库不可随意清空。

全部主键字段都显式 NOT NULL；没有额外 UNIQUE、CHECK、外键或触发器。下文枚举与 JSON 形状是写入/读取约定，不是 DDL 强制约束；`_id` 不自动获得引用完整性。主键自动索引不重复计入命名索引。

## 表目录

- [subscribes](#subscribes) — 20 个字段
- [subscribe_facts](#subscribe_facts) — 5 个字段
- [subscribe_wanted](#subscribe_wanted) — 9 个字段
- [pending_downloads](#pending_downloads) — 13 个字段

## subscribes

User 对一个 Media 的长期策略：覆盖范围、Fetch mode、Filter、Downloader、Library 与 Wash-cut 设置。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
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
| `created_at` | `TEXT` | — | 是 | `''` | 创建时间文本；业务使用 RFC3339 UTC，DDL 允许默认空串。 |
| `updated_at` | `TEXT` | — | 是 | `''` | 配置更新时间文本；业务使用 RFC3339 UTC。 |
| `library_id` | `TEXT` | — | 否 | 无 | 目标 Library，逻辑引用 `library.db.libraries.library_id`。 |

**键与约束**：主键 `(id)`；没有额外 UNIQUE、CHECK 或外键。

**索引**：

```sql
CREATE INDEX subscribes_user_idx ON subscribes(user_id);
CREATE INDEX subscribes_media_idx ON subscribes(media_id);
```

### 用户删除时的所有权转交合同
当一个用户被删除时（见 [app.db 用户生命周期](../database/app.md)）：
- 该用户创建的所有 `subscribes` 记录不会被物理级联删除（避免静默丢失已建立的长期下载策略）；
- 其所有权字段 `user_id` 在独立事务中安全转交给系统默认管理员（`00000000-0000-0000-0000-000000000001`）；
- 相关的定时任务定义（`jobs.db`）按 typed 引用与参数更新，不对 payload 进行全字符串替换；
- 当 Subscribe 移除了显式 `library_id` 绑定时，策略评估读取新 coverage/units，但已保存的非目标库事实不会跳到默认库。


## subscribe_facts

每个覆盖槽位已拥有文件的评分和路径事实，用于填空与 Wash-cut。它不是 Library 文件台账的替代物。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `subscribe_id` | `TEXT` | 1 | 是 | 无 | 逻辑引用本库 `subscribes.id`。 |
| `season` | `INTEGER` | 2 | 是 | `-1` | 覆盖槽季号；无季用 -1。 |
| `episode` | `INTEGER` | 3 | 是 | `-1` | 覆盖槽集号；无集用 -1。 |
| `score` | `INTEGER` | — | 是 | 无 | 已拥有文件的评分。 |
| `path` | `TEXT` | — | 否 | 无 | 已拥有文件路径；没有指向 Library 文件的外键。 |

**键与约束**：主键 `(subscribe_id, season, episode)`；没有额外 UNIQUE、CHECK 或外键。

**索引**：无额外命名索引；按 Subscribe 的前缀查询可使用复合主键索引，没有单独的 `subscribe_facts_sub_idx`。

**使用注意**：电影通常使用 `(-1,-1)`，TV 使用季/集。路径关联不能替代 Library 文件身份，也不能将一个槽位直接 JOIN 同一 Media 的所有文件版本后求和。

## subscribe_wanted

待满足槽位的搜索、抓取、Transfer 与拒绝履历。没有独立 `status` 列；是否满足或在途还需结合覆盖事实与 pending 记录判断。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `subscribe_id` | `TEXT` | 1 | 是 | 无 | 逻辑引用本库 `subscribes.id`。 |
| `season` | `INTEGER` | 2 | 是 | `-1` | 履历对应的季槽；无季用 -1。 |
| `episode` | `INTEGER` | 3 | 是 | `-1` | 履历对应的集槽；无集用 -1。 |
| `search_attempts` | `INTEGER` | — | 是 | `0` | 累计搜索次数。 |
| `last_search_at` | `INTEGER` | — | 否 | 无 | 最近搜索时间，Unix 秒。 |
| `grabbed_at` | `INTEGER` | — | 否 | 无 | 已抓取时间，Unix 秒。 |
| `imported_at` | `INTEGER` | — | 否 | 无 | Transfer 完成时间，Unix 秒。 |
| `grab_title` | `TEXT` | — | 否 | 无 | 已抓取 Torrent 的显示标题。 |
| `last_reject_reason` | `TEXT` | — | 否 | 无 | 最近候选拒绝说明。 |

**键与约束**：主键 `(subscribe_id, season, episode)`；没有额外 UNIQUE、CHECK 或外键。

**索引**：无额外命名索引；按 Subscribe 的前缀查询可使用复合主键索引，没有单独的 `subscribe_wanted_sub_idx`。

## pending_downloads

已向 Downloader 提交的 Torrent 及后续进度、延迟选集、probe 等待事实。以 Subscribe/enclosure 去重；不是通用 Job 实例表。进度与等待字段的行为见 [pending_progress.rs](../../crates/store/src/pending_progress.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `subscribe_id` | `TEXT` | 1 | 是 | 无 | 逻辑引用本库 `subscribes.id`。 |
| `enclosure` | `TEXT` | 2 | 是 | 无 | Torrent 下载入口，参与去重。 |
| `title` | `TEXT` | — | 是 | 无 | Torrent 标题。 |
| `score` | `INTEGER` | — | 是 | 无 | 提交时评分。 |
| `torrent_json` | `TEXT` | — | 是 | 无 | 已提交 Torrent 的序列化事实，可包含 Site、实际 Downloader、Release override 等；DDL 不验证 JSON。 |
| `submitted_at` | `INTEGER` | — | 是 | `0` | 提交时间，Unix 秒；0 表示没有有效提交时间记录。 |
| `state` | `TEXT` | — | 是 | `'active'` | 状态文本；业务使用 active、imported、stalled，DDL 无枚举 CHECK。 |
| `last_progress` | `REAL` | — | 否 | 无 | 最近记录的增长进度，调用者约定范围 0..=1；DDL 不限制数值。 |
| `last_progress_at` | `INTEGER` | — | 否 | 无 | 最近检测到进度增长的时间，Unix 秒；并非每次轮询都更新。 |
| `info_hash` | `TEXT` | — | 否 | 无 | Downloader 分配的 Torrent info hash；写入者转为小写。 |
| `selective_missing` | `TEXT` | — | 否 | 无 | 等待文件列表后保留的集槽，格式 `s:e,s:e`；NULL 表示无延迟选择。 |
| `probe_wait_attempts` | `INTEGER` | — | 否 | 无 | 无法 probe 源文件的等待轮数；首次记录由写入者从 0 增加。 |
| `probe_wait_since` | `INTEGER` | — | 否 | 无 | 首次记录 probe 等待的时间，Unix 秒。 |

**键与约束**：主键 `(subscribe_id, enclosure)`；没有额外 UNIQUE、CHECK 或外键。`info_hash` 没有唯一约束。

**索引**：

```sql
CREATE INDEX pending_downloads_sub_state_idx ON pending_downloads(subscribe_id,state);
```

**使用注意**：`active` 表示在途；`imported` 是 Transfer 后仍保留用于跟踪的记录，不代表仍待 Transfer；`stalled` 不再占用覆盖槽。进度没有增长时不会刷新 `last_progress_at`，读取停滞时间可回退到非零 `submitted_at`。这些行为不等于数据库触发器或 CHECK。
