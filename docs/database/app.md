# app.db：实例配置、身份与凭证

[返回数据库总览](README.md)

字段、类型、默认值、主键、唯一约束、CHECK 和命名索引来自 [app.sql](../../crates/store/src/schema/v1/app.sql)；文件身份来自 [schema/layout.rs](../../crates/store/src/schema/layout.rs)：`application_id = 1129139248`、`user_version = 1`。这是生产 DDL 字段字典；结构复核方法见[数据库总览](README.md#结构复核方法)，运行时行为需要另行验证。

共 **9 张业务表、48 个字段、3 个显式命名索引**，2 组外键、2 个 CHECK、0 触发器。`app.db` 保存实例配置、User 身份与凭证；**不保存 Media、Library 或 Playback 事实**。`playback_device_credentials` 仅是设备凭证关联，不是播放会话、进度或历史；这些事实的存储边界见 [library.db](library.md)。不能将本库整体当缓存清空。

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

## instance_lock

数据目录的单实例租约；持有者刷新心跳，其他实例可按 TTL 判断租约是否过期。行为实现：[instance_lock.rs](../../crates/store/src/instance_lock.rs)。`AppHeartbeat` 共享 `Arc<Writers>` 的唯一 app.db writer 刷新心跳与租约，不再另外打开写连接；Library writer 不阻塞 app writer。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `INTEGER` | 1 | 否 | 无 | 固定为 1 的单例键。 |
| `owner` | `TEXT` | — | 是 | 无 | 当前租约持有者标识。 |
| `heartbeat_at` | `INTEGER` | — | 是 | 无 | 最近心跳时间，Unix **毫秒**；按秒传参的 `try_acquire_instance_lock` / `heartbeat_instance_lock` 会换算成毫秒写入。 |

**生产表定义**

```sql
CREATE TABLE instance_lock(id INTEGER PRIMARY KEY CHECK(id=1),owner TEXT NOT NULL,heartbeat_at INTEGER NOT NULL);
```

## users

User 登录身份、启用状态、角色和密码资料，不保存观看事实。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 是 | 无 | User 内部 ID。 |
| `login` | `TEXT` | — | 是 | 无 | 唯一登录名。 |
| `enabled` | `INTEGER` | — | 是 | `1` | 启用标记，业务使用 0/1。 |
| `role` | `TEXT` | — | 是 | `'member'` | 角色文本，业务解释 admin/member；DDL 未限制枚举。 |
| `password` | `TEXT` | — | 是 | `''` | User 密码资料，正常密码写入路径保存散列；不是 Downloader 连接密码。 |

**生产表定义**

```sql
CREATE TABLE users(id TEXT PRIMARY KEY NOT NULL,login TEXT NOT NULL UNIQUE,enabled INTEGER NOT NULL DEFAULT 1,role TEXT NOT NULL DEFAULT 'member',password TEXT NOT NULL DEFAULT '');
```

### 用户生命周期与跨库删除合同
用户删除通过 `DELETE /api/v1/users/{id}` 发起，采用分阶段幂等与故障可续模型：
1. **202 异步合同**：删除请求立即返回 `202 Accepted` 与 `operation_id`，不使用 200 掩盖尚未完成的跨库清理。
2. **凭据立即失效与阶段持久化**：
   - 首先在 `settings` 持久化操作状态：键为 `user_removal.<id>`，版本为 `version: 1`，包含 `operation_id`、`phase`（`disabling` → `cleaning` → `finalizing`）、`cursor`、`error` 与 `updated_at`。
   - 立即将用户设置为禁用（`enabled = 0`）并物理撤销 `user_tokens` 中该用户的全部会话 token。在整个删除执行与可能的故障恢复期间，被删除用户的 token 访问必须保证立即 401（Unauthorized）。
3. **资源所有权与清理边界**：
   - 保留现行产品行为：被删除用户的 Subscribe 所有权转移至系统管理员（`admin`），私有 Collection 亦转移至管理员，不静默删除用户创建的长期订阅与合集。
   - 原始播放记录（`playback_units`、`playback_sessions`、`playback_session_ids`、`playback_logs`、`playback_metrics`）及稀疏用户摘要（`user_library_items`）被一并清理，不产生孤儿稀疏摘要。
   - 跨库清理完成后，删除 `users` 记录；先在 `user_removal.result.<operation_id>` 保存终态结果、在 `user_removal.done.<user_id>` 保存操作 id，再清除 `user_removal.<id>` 在途项。重启可幂等重做 `finalizing`，终态回执继续可查。
4. **故障恢复**：开机启动时扫描 `user_removal.` 前缀的在途操作，沿用 lease 模式继续完成未决阶段。

## sites

Site 实例配置、认证资料、Indexer profile 与网络/Downloader 选择。本文只列字段，不读取或展示真实凭证。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
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

**生产表定义**

```sql
CREATE TABLE sites(id TEXT PRIMARY KEY NOT NULL,name TEXT NOT NULL,url TEXT NOT NULL,profile_id TEXT NOT NULL,cookie TEXT,api_key TEXT,rss_url TEXT,proxy TEXT,rate_limit_per_minute INTEGER,cdp_url TEXT,downloader_id TEXT,enabled INTEGER NOT NULL DEFAULT 1,maintenance_via TEXT NOT NULL DEFAULT 'http' CHECK(maintenance_via IN ('http','browser')));
```

**显式命名索引**

```sql
CREATE INDEX sites_enabled_idx ON sites(enabled);
```

## filters

命名 Filter 的原子条件和保留旧版本策略。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 是 | 无 | Filter 内部 ID。 |
| `name` | `TEXT` | — | 是 | 无 | Filter 名称。 |
| `atoms_json` | `TEXT` | — | 是 | 无 | 原子条件的 JSON 文本；DDL 没有 `json_valid` CHECK。 |
| `keep_old_versions` | `INTEGER` | — | 是 | `0` | 保留旧文件版本的策略标记。 |

**生产表定义**

```sql
CREATE TABLE filters(id TEXT PRIMARY KEY NOT NULL,name TEXT NOT NULL,atoms_json TEXT NOT NULL,keep_old_versions INTEGER NOT NULL DEFAULT 0);
```

## user_tokens

认证 token 到 User 的映射及签发时间。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `token` | `TEXT` | 1 | 是 | 无 | 认证 token；敏感字段。 |
| `user_id` | `TEXT` | — | 是 | 无 | 外键引用本库 `users.id`，删除 User 时级联清除。 |
| `created_at` | `INTEGER` | — | 是 | `0` | 签发时间，Unix 秒；默认 0 本身不证明 token 有效。 |

**生产表定义**

```sql
CREATE TABLE user_tokens(token TEXT PRIMARY KEY NOT NULL,user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,created_at INTEGER NOT NULL DEFAULT 0);
```

**显式命名索引**

```sql
CREATE INDEX user_tokens_user_idx ON user_tokens(user_id);
```

## playback_device_credentials

将 User/设备与已签发 token 关联，用于设备凭证撤销。不是 Playback 事实表。一个设备可关联多个 token；DDL 不保证 token 在该表全局唯一。token 外键级联响应凭据撤销。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `user_id` | `TEXT` | 1 | 是 | 无 | 逻辑引用本库 `users.id`。 |
| `device_id` | `TEXT` | 2 | 是 | 无 | 客户端设备标识。 |
| `token` | `TEXT` | 3 | 是 | 无 | 该设备关联的认证 token；外键引用 `user_tokens.token`，敏感字段。 |

**生产表定义**

```sql
CREATE TABLE playback_device_credentials(user_id TEXT NOT NULL,device_id TEXT NOT NULL,token TEXT NOT NULL REFERENCES user_tokens(token) ON DELETE CASCADE,PRIMARY KEY(user_id,device_id,token));
```

**显式命名索引**

```sql
CREATE INDEX playback_credentials_token ON playback_device_credentials(token);
```

## settings

实例级 key/value 配置，也承载特定维护游标命名空间；不能把所有值当作可弃缓存。

Transfer/Watch 与原始 Scrape 配置以 Library 的 `transfer_settings` / `scrape_settings` 单例为唯一权威；app settings 不接受 `transfer.settings`、`metadata.scrape` 或 `metadata.language`，默认语言亦由 Library 查询配置管理。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `key` | `TEXT` | 1 | 是 | 无 | 配置键，由写入模块定义命名空间。 |
| `value` | `TEXT` | — | 是 | 无 | 配置值文本；可能是标量、JSON 或凭证，按具体键解释。 |

**生产表定义**

```sql
CREATE TABLE settings(key TEXT PRIMARY KEY NOT NULL,value TEXT NOT NULL);
```

## search_snapshots

一次搜索的查询和结果快照，不是 Metadata source 原始响应缓存，也不是 Media/Library 权威事实。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 是 | 无 | 快照 ID。 |
| `vertical` | `TEXT` | — | 是 | 无 | 搜索业务分类。 |
| `query` | `TEXT` | — | 是 | 无 | 查询文本。 |
| `payload` | `TEXT` | — | 是 | 无 | 序列化搜索结果。 |
| `created_at` | `INTEGER` | — | 是 | 无 | 创建时间，由调用者传入；常规路径使用 Unix 秒。 |

**生产表定义**

```sql
CREATE TABLE search_snapshots(id TEXT PRIMARY KEY NOT NULL,vertical TEXT NOT NULL,query TEXT NOT NULL,payload TEXT NOT NULL,created_at INTEGER NOT NULL);
```

## downloaders

Downloader 连接、启用开关、默认选择和路径映射配置。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
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

**生产表定义**

```sql
CREATE TABLE downloaders(id TEXT PRIMARY KEY NOT NULL,name TEXT NOT NULL,kind TEXT NOT NULL,url TEXT NOT NULL,username TEXT,password TEXT,category TEXT,is_default INTEGER NOT NULL DEFAULT 0,path_maps TEXT,enabled INTEGER NOT NULL DEFAULT 1);
```
