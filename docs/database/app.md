# app.db：实例配置、身份与凭证

[返回数据库总览](README.md)

当前源码基线：`feat/storage-redesign@a3dab1b`。字段、类型、默认值、主键、唯一约束、CHECK 和命名索引逐项来自 [Store v1/app.sql](../../crates/store/src/schema/v1/app.sql)；文件身份来自 [schema/layout.rs](../../crates/store/src/schema/layout.rs)：`application_id = 1129139248`、`user_version = 1`。这是源码 DDL 字典，不是本次运行 Store 后提取的 schema，也不代表所有运行时调用者已经完成切换。

共 **10 张业务表、50 个字段、2 个显式命名索引**，没有声明外键或触发器。`app.db` 保存实例配置、User 身份与凭证；**不保存 Media、Library 或 Playback 事实**。`playback_device_credentials` 仅是设备凭证关联，不是播放会话、进度或历史；这些事实的存储边界见 [library.db](library.md)。不能将本库整体当缓存清空。

下表的 NOT NULL 指 DDL 是否显式声明；`INTEGER PRIMARY KEY` 自带 rowid 主键语义，即使未另写 NOT NULL。除逐表列出的约束外，不把业务枚举、JSON 形状或 0/1 约定误写为数据库 CHECK。自动主键/UNIQUE 索引不计入命名索引。

## 表目录

- [instance_lock](#instance_lock) — 3 个字段
- [users](#users) — 5 个字段
- [sites](#sites) — 13 个字段
- [filters](#filters) — 4 个字段
- [user_tokens](#user_tokens) — 3 个字段
- [playback_device_credentials](#playback_device_credentials) — 3 个字段
- [settings](#settings) — 2 个字段
- [search_snapshots](#search_snapshots) — 5 个字段
- [downloaders](#downloaders) — 10 个字段
- [naming_templates](#naming_templates) — 2 个字段

## instance_lock

数据目录的单实例租约；持有者刷新心跳，其他实例可按 TTL 判断租约是否过期。行为实现：[instance_lock.rs](../../crates/store/src/instance_lock.rs)。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `INTEGER` | 1 | 否 | 无 | 固定为 1 的单例键。 |
| `owner` | `TEXT` | — | 是 | 无 | 当前租约持有者标识。 |
| `heartbeat_at` | `INTEGER` | — | 是 | 无 | 最近心跳时间，调用者以 Unix 秒传入。 |

**键与约束**：主键 `(id)`；`CHECK(id=1)`。没有额外 UNIQUE 或外键。

**索引**：无额外命名索引。

## users

User 登录身份、启用状态、角色和密码资料，不保存观看事实。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 是 | 无 | User 内部 ID。 |
| `login` | `TEXT` | — | 是 | 无 | 唯一登录名。 |
| `enabled` | `INTEGER` | — | 是 | `1` | 启用标记，业务使用 0/1。 |
| `role` | `TEXT` | — | 是 | `'member'` | 角色文本，业务解释 admin/member；DDL 未限制枚举。 |
| `password` | `TEXT` | — | 是 | `''` | User 密码资料，正常密码写入路径保存散列；不是 Downloader 连接密码。 |

**键与约束**：主键 `(id)`；UNIQUE `(login)`；没有 CHECK 或外键。

**索引**：无额外命名索引；登录唯一性由 UNIQUE 自动索引提供。

### 用户生命周期与跨库删除合同
用户删除通过 `DELETE /api/v1/users/{id}` 发起，采用分阶段幂等与故障可续模型：
1. **202 异步合同**：删除请求立即返回 `202 Accepted` 与 `operation_id`，不使用 200 掩盖尚未完成的跨库清理。
2. **凭据立即失效与阶段持久化**：
   - 首先在 `settings` 持久化操作状态：键为 `user_removal.<id>`，版本为 `version: 1`，包含 `operation_id`、`phase`（如 `disabling` -> `cleaning`）、`cursor`、`error` 与 `updated_at`。
   - 立即将用户设置为禁用（`enabled = 0`）并物理撤销 `user_tokens` 中该用户的全部会话 token。在整个删除执行与可能的故障恢复期间，被删除用户的 token 访问必须保证立即 401（Unauthorized）。
3. **资源所有权与清理边界**：
   - 保留现行产品行为：被删除用户的 Subscribe 所有权转移至系统管理员（`admin`），私有 Collection 亦转移至管理员，不静默删除用户创建的长期订阅与合集。
   - 原始播放记录（`playback_units`、`playback_sessions`、`playback_session_ids`、`playback_logs`、`playback_metrics`）及稀疏用户摘要（`user_library_items`）被一并清理，不产生孤儿稀疏摘要。
   - 跨库清理完成后，删除 `users` 记录并清除 `settings` 中的 `user_removal.<id>` 操作项。
4. **故障恢复**：开机启动时扫描 `user_removal.` 前缀的在途操作，沿用 lease 模式继续完成未决阶段。


## sites

Site 实例配置、认证资料、Indexer profile 与网络/Downloader 选择。本文只列字段，不读取或展示真实凭证。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 是 | 无 | Site 内部 ID。 |
| `name` | `TEXT` | — | 是 | 无 | Site 显示名称。 |
| `url` | `TEXT` | — | 是 | 无 | Site 端点 URL。 |
| `profile_id` | `TEXT` | — | 是 | 无 | 对应 Indexer profile ID。 |
| `cookie` | `TEXT` | — | 否 | 无 | Site cookie；敏感认证资料。 |
| `api_key` | `TEXT` | — | 否 | 无 | Site API 凭证；敏感字段。 |
| `rss_url` | `TEXT` | — | 否 | 无 | RSS 入口，可能含认证 token。 |
| `proxy` | `TEXT` | — | 否 | 无 | 代理配置，可能含认证信息。 |
| `rate_limit_per_minute` | `INTEGER` | — | 否 | 无 | 每分钟请求限额。 |
| `cdp_url` | `TEXT` | — | 否 | 无 | Site 专属 CDP discovery 端点；仅用于适用的登录/Check-in。 |
| `downloader_id` | `TEXT` | — | 否 | 无 | 逻辑引用本库 `downloaders.id`。 |
| `enabled` | `INTEGER` | — | 是 | `1` | 启用标记，业务使用 0/1。 |
| `maintenance_via` | `TEXT` | — | 是 | `'http'` | 登录/Check-in 的维护方式；不改变搜索/RSS 始终使用 HTTP 的边界。 |

**键与约束**：主键 `(id)`；`CHECK(maintenance_via IN ('http','browser'))`；没有额外 UNIQUE 或外键。

**索引**：

```sql
CREATE INDEX sites_enabled_idx ON sites(enabled);
```

## filters

命名 Filter 的原子条件和保留旧版本策略。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 是 | 无 | Filter 内部 ID。 |
| `name` | `TEXT` | — | 是 | 无 | Filter 名称。 |
| `atoms_json` | `TEXT` | — | 是 | 无 | 原子条件的 JSON 文本；DDL 没有 `json_valid` CHECK。 |
| `keep_old_versions` | `INTEGER` | — | 是 | `0` | 保留旧文件版本的策略标记。 |

**键与约束**：主键 `(id)`；没有额外 UNIQUE、CHECK 或外键。

**索引**：无额外命名索引。

## user_tokens

认证 token 到 User 的映射及签发时间。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `token` | `TEXT` | 1 | 是 | 无 | 认证 token；敏感字段。 |
| `user_id` | `TEXT` | — | 是 | 无 | 逻辑引用本库 `users.id`。 |
| `created_at` | `INTEGER` | — | 是 | `0` | 签发时间，Unix 秒；默认 0 本身不证明 token 有效。 |

**键与约束**：主键 `(token)`；没有额外 UNIQUE、CHECK 或外键。

**索引**：

```sql
CREATE INDEX user_tokens_user_idx ON user_tokens(user_id);
```

## playback_device_credentials

将 User/设备与已签发 token 关联，用于设备凭证撤销。不是 Playback 事实表。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `user_id` | `TEXT` | 1 | 是 | 无 | 逻辑引用本库 `users.id`。 |
| `device_id` | `TEXT` | 2 | 是 | 无 | 客户端设备标识。 |
| `token` | `TEXT` | 3 | 是 | 无 | 该设备关联的认证 token；敏感字段。 |

**键与约束**：主键 `(user_id, device_id, token)`；没有额外 UNIQUE、CHECK 或外键。一个设备可关联多个 token；DDL 不保证 token 在该表全局唯一。

**索引**：无额外命名索引。

## settings

实例级 key/value 配置，也承载特定维护游标命名空间；不能把所有值当作可弃缓存。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `key` | `TEXT` | 1 | 是 | 无 | 配置键，由写入模块定义命名空间。 |
| `value` | `TEXT` | — | 是 | 无 | 配置值文本；可能是标量、JSON 或凭证，按具体键解释。 |

**键与约束**：主键 `(key)`；没有额外 UNIQUE、CHECK 或外键。

**索引**：无额外命名索引。

## search_snapshots

一次搜索的查询和结果快照，不是 Metadata source 原始响应缓存，也不是 Media/Library 权威事实。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 是 | 无 | 快照 ID。 |
| `vertical` | `TEXT` | — | 是 | 无 | 搜索业务分类。 |
| `query` | `TEXT` | — | 是 | 无 | 查询文本。 |
| `payload` | `TEXT` | — | 是 | 无 | 序列化搜索结果。 |
| `created_at` | `INTEGER` | — | 是 | 无 | 创建时间，由调用者传入；常规路径使用 Unix 秒。 |

**键与约束**：主键 `(id)`；没有额外 UNIQUE、CHECK 或外键。

**索引**：无额外命名索引。

## downloaders

Downloader 连接、启用开关、默认选择和路径映射配置。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 是 | 无 | Downloader 内部 ID。 |
| `name` | `TEXT` | — | 是 | 无 | 显示名称。 |
| `kind` | `TEXT` | — | 是 | 无 | Downloader 实现标签，由业务解码。 |
| `url` | `TEXT` | — | 是 | 无 | Downloader API 端点。 |
| `username` | `TEXT` | — | 否 | 无 | 认证用户名。 |
| `password` | `TEXT` | — | 否 | 无 | 外部连接密码；敏感资料，不是 User 密码散列。 |
| `category` | `TEXT` | — | 否 | 无 | 提交下载时使用的分类标签。 |
| `is_default` | `INTEGER` | — | 是 | `0` | 默认选择标记；DDL 未约束只能有一个默认项。 |
| `path_maps` | `TEXT` | — | 否 | 无 | 外部路径到本机路径的序列化映射。 |
| `enabled` | `INTEGER` | — | 是 | `1` | 启用标记，业务使用 0/1。 |

**键与约束**：主键 `(id)`；没有额外 UNIQUE、CHECK 或外键。

**索引**：无额外命名索引。

## naming_templates

实例级 Naming template，按 Media kind 保存 Transfer 相对路径模式；不是 Library 文件事实。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `kind` | `TEXT` | 1 | 是 | 无 | 模板适用类型，业务使用 movie/tv；DDL 无枚举 CHECK。 |
| `pattern` | `TEXT` | — | 是 | 无 | 带占位符的相对路径模式。 |

**键与约束**：主键 `(kind)`；没有额外 UNIQUE、CHECK 或外键。

**索引**：无额外命名索引。
