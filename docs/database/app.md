# app.db：实例配置、身份与凭证

[返回数据库总览](README.md)

字段、类型、默认值、主键、唯一约束、CHECK 和命名索引来自 [app.sql](../../crates/store/src/schema/v1/app.sql)；文件身份来自 [schema/layout.rs](../../crates/store/src/schema/layout.rs)：`application_id = 1129139248`、`user_version = 1`。这是生产 DDL 字段字典；结构复核方法见[数据库总览](README.md#结构复核方法)，运行时行为需要另行验证。

共 **28 张业务表、222 个字段、21 个显式命名索引**，13 组外键（`pragma_foreign_key_list` 共 14 行）、112 个 CHECK、0 触发器。P1a2/P1b2/P1c/P2/P5(M4.1)/M5.2 新增部分的计数来自唯一生产 DDL 的静态核对。`app.db` 保存实例配置、User 身份与凭证，以及 Plugin 元数据、信任密钥、UI 信任、权限、域内状态、Site 维护回执、持久意图与 `app.db` 归属事件 outbox（如 `site.checkin.after`）；**不保存 Media、Library 或 Playback 事实**。`playback_device_credentials` 仅是设备凭证关联，不是播放会话、进度或历史；这些事实的存储边界见 [library.db](library.md)。不能将本库整体当缓存清空。

下表的 NOT NULL 指 DDL 是否显式声明；`INTEGER PRIMARY KEY` 自带 rowid 主键语义，即使未另写 NOT NULL。除逐表列出的约束外，不把业务枚举、JSON 形状或 0/1 约定误写为数据库 CHECK。自动主键/UNIQUE 索引不计入命名索引。

## 表目录

- [instance_lock](#instance_lock) — 3 个字段
- [users](#users) — 5 个字段
- [sites](#sites) — 17 个字段
- [filters](#filters) — 4 个字段
- [user_tokens](#user_tokens) — 3 个字段
- [playback_device_credentials](#playback_device_credentials) — 3 个字段
- [settings](#settings) — 2 个字段
- [search_snapshots](#search_snapshots) — 5 个字段
- [downloaders](#downloaders) — 10 个字段
- [plugins](#plugins) — 9 个字段
- [plugin_versions](#plugin_versions) — 11 个字段
- [plugin_configs](#plugin_configs) — 4 个字段
- [plugin_grants](#plugin_grants) — 6 个字段
- [plugin_kv / plugin_blobs](#plugin_kv--plugin_blobs) — 各 5 个字段
- [plugin_quota](#plugin_quota) — 5 个字段
- [plugin_config_secrets](#plugin_config_secrets) — 4 个字段
- [plugin_operations](#plugin_operations) — 16 个字段
- [plugin_commands](#plugin_commands) — 14 个字段
- [plugin_command_payloads / plugin_operation_results](#plugin_command_payloads--plugin_operation_results) — 各 3 个字段
- [plugin_task_bindings](#plugin_task_bindings) — 11 个字段
- [plugin_trust_keys](#plugin_trust_keys) — 11 个字段
- [site_maintenance_receipts](#site_maintenance_receipts) — 17 个字段
- [plugin_event_entity_sequences](#plugin_event_entity_sequences) — 3 个字段
- [plugin_event_outbox](#plugin_event_outbox) — 18 个字段
- [plugin_event_deliveries](#plugin_event_deliveries) — 18 个字段
- [plugin_ui_trust](#plugin_ui_trust) — 7 个字段

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
   - 在单个 `app.db` `IMMEDIATE` 事务内将用户设为禁用（`enabled = 0`）、物理撤销 `user_tokens` 中该用户的全部会话 token（级联清除 `playback_device_credentials`），并在 `settings` 持久化操作状态：键为 `user_removal.<id>`，`version: 1`，包含 `operation_id`、`user_id`、`phase`（直接进入 `cleaning` → `finalizing`；`disabling` 仅保留作兼容恢复分支）、`cursor`、`error` 与 `updated_at`。在整个删除执行与故障恢复期间，被删除用户的 token 访问立即返回 401（Unauthorized）。
3. **资源所有权与清理边界**：
   - 保留现行产品行为：被删除用户的 Subscribe 所有权转移至系统管理员（`admin`），私有 Collection 亦转移至管理员，不静默删除用户创建的长期订阅与合集。
   - 在 `cleaning` 阶段，原始播放记录（`playback_units`、`playback_sessions`、`playback_session_ids`、`playback_logs`、`playback_metrics`）及稀疏用户摘要（`user_library_items` 及其级联 `user_library_continue_units`）被一并清理。
   - `cleaning` 完成后进入 `finalizing`：先在 `app.db` 删除 `users` 记录并释放写锁，再在独立 `library.db` 事务中从各 `libraries.member_ids_json` 移除该用户 ID（若前次中断已删 `users` 行仍幂等补做成员列表清理）；随后依次写入 `user_removal.result.<operation_id>`（`phase = "succeeded"`，保留 7 天后修剪）与 `user_removal.done.<user_id>`，最后清除 `user_removal.<id>` 在途项。重启可幂等重做 `finalizing`，终态回执继续可查。
4. **故障恢复**：进程启动与 `job_loop` 扫描 `user_removal.` 前缀的在途操作，直接驱动继续完成未决阶段。

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
| `browser_search` | `INTEGER` | — | 是 | `0` | CHECK `0/1`；是否允许该 Site 使用受控 Browser 搜索（`browser:search` opt-in）。 |
| `timezone` | `TEXT` | — | 是 | `'UTC'` | CHECK 非空；Site 维护窗口使用的本地时区标识。 |
| `revision` | `INTEGER` | — | 是 | 无 | 每次 Site 写入递增的正数，用于拒绝过期结果。 |
| `credential_epoch` | `TEXT` | — | 是 | 无 | 凭据域的不透明标识；显式替换凭据或修改 URL 时轮换。 |

**生产表定义**

```sql
CREATE TABLE sites(id TEXT PRIMARY KEY NOT NULL,name TEXT NOT NULL,url TEXT NOT NULL,profile_id TEXT NOT NULL,cookie TEXT,api_key TEXT,rss_url TEXT,proxy TEXT,rate_limit_per_minute INTEGER,cdp_url TEXT,downloader_id TEXT,enabled INTEGER NOT NULL DEFAULT 1,maintenance_via TEXT NOT NULL DEFAULT 'http' CHECK(maintenance_via IN ('http','browser')),browser_search INTEGER NOT NULL DEFAULT 0 CHECK(browser_search IN (0,1)),timezone TEXT NOT NULL DEFAULT 'UTC' CHECK(length(timezone)>0),revision INTEGER NOT NULL CHECK(revision > 0),credential_epoch TEXT NOT NULL);
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

认证 token 到 User 的映射及签发时间。普通登录/Jellyfin 会话 token 有效期为 30 天（`SESSION_TTL_SECS`）；当前管理员 CLI Bearer token（记录于 `settings['auth.cli_token.current']`）不受 30 天过期限制，替换时旧值立即移除。


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

将 User/设备与已签发 token 关联，用于设备凭证撤销。不是 Playback 事实表。一个设备可关联多个 token；DDL 不保证 token 在该表全局唯一。管理员 CLI token 与占位设备标识（空串、`jellyfin-client`、`unidentified:*`）不写入本表；设备撤销会删除关联 `user_tokens` 与本表行，并将 `{user_id}:{device_id}` 记录到 `settings['playback.revoked_devices']`。token 外键级联响应凭据撤销。


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

实例级 key/value 配置，也承载凭据扩展、用户偏好与维护游标命名空间（如 `auth.cli_token.current`、`downloader.<id>.{api_key,verified,last_error}`、`playback.revoked_devices`、`user.<user_id>.up_next.dismissed`、`user_removal.*`、`probe.settings`、`probe.remote_reads` 等）；不能把所有值当作可弃缓存。

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

Downloader 连接、启用开关、默认选择和路径映射配置。qBittorrent v5.2+ 的 API Key 与连接校验状态存放在同库 `settings` 表（`downloader.<id>.api_key`、`downloader.<id>.verified`、`downloader.<id>.last_error`）：`api_key` 在新建、原子修改（`Store::mutate_downloader`）与删除（`Store::delete_downloader`）的同一 `IMMEDIATE` 事务中同步维护或清理；`verified` 与 `last_error` 由连接校验（`Store::record_downloader_verification`）在 CAS `IMMEDIATE` 事务中写入，在 `Store::mutate_downloader` 修改连接/鉴权字段的同一 `IMMEDIATE` 事务中失效清除，并在 HTTP 删除入口中额外清理。


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

## plugins

Plugin 当前包、配置与执行权威。`revision`、`config_revision`、`generation` 分别计数，不能互换。元数据首次登记为 `installed-disabled`；登记不会读取包文件、校验签名、创建运行时或发布 enabled registry。

| 字段 | SQLite 类型 | 显式 NOT NULL | 键 / CHECK / 含义 |
|---|---|---|---|
| `plugin_id` | TEXT | 是 | 主键；manifest 稳定 ID。 |
| `active_package_hash` | TEXT | 是 | 当前包 digest；本片不提供任意切换 setter。 |
| `lifecycle` | TEXT | 是 | CHECK：installed-disabled/enabling/enabled/draining/disabled。 |
| `desired_enabled` | INTEGER | 是 | CHECK：0/1；持久启停意图。 |
| `revision` | INTEGER | 是 | CHECK >0；管理 CAS。 |
| `config_revision` | INTEGER | 是 | CHECK >=0；配置修订。 |
| `generation` | INTEGER | 是 | CHECK >=0；执行围栏。 |
| `active_operation_id` | TEXT | 否 | 当前 lifecycle intent 的 UUID；允许先记录意图，再由宿主组合 operation。 |
| `failure_reason` | TEXT | 否 | 宿主失败原因码，与 lifecycle 分开。 |

开始启停递增 revision/generation；完成须精确匹配 operation/revision/generation，并提供真实 worker 退出与效果对账的宿主证据。Store 自己的未决 command 仍阻止完成；管理 lifecycle operation 本身仅在 effect=none 时可作为当前控制操作排除。停用保留 confirmed 效果并置 blocked；unknown 效果或尚无效果观察的 running 置 unknown；其他安全的 none 工作取消。所有 claim 失效、清空 next_run_at，并产生禁用任务定义投影。没有外部 IO 位于事务内。

## plugin_versions

主键 `(plugin_id,package_hash)`；`UNIQUE(plugin_id,version)`，禁止同显示版本静默换 digest。`plugin_id` 外键引用 `plugins`。所有字段无 DEFAULT。显式部分索引 `plugin_versions_signing_key(signing_key_id,plugin_id) WHERE signing_key_id IS NOT NULL` 用于关联包数统计与密钥撤销排空查询。

| 字段 | SQLite 类型 | 显式 NOT NULL | 含义 |
|---|---|---|---|
| `plugin_id` | TEXT | 是 | 复合主键 1。 |
| `package_hash` | TEXT | 是 | 复合主键 2；不可变身份。 |
| `version` | TEXT | 是 | 发布版本，与执行 generation 无关。 |
| `manifest_json` | TEXT | 是 | 宿主包服务预先验证的 manifest 元数据。 |
| `config_schema_json` | TEXT | 是 | 配置 schema 元数据。 |
| `state_schema` | TEXT | 是 | 状态格式标识；跨版本保留/reset 政策由 P1c 实现。 |
| `artifact_path` | TEXT | 是 | 不可变包路径引用；Store 不读取文件。 |
| `signing_key_id` | TEXT | 否 | 签名 key 引用。 |
| `signature_status` | TEXT | 是 | 宿主验证结果元数据；不是 Store 自己进行的验证。 |
| `ui_metadata_ref` | TEXT | 否 | 已验证 UI 元数据引用。 |
| `credential_bound` | INTEGER | 是 | CHECK 0/1；1 时只能签发 Site/credential_epoch 域，禁止 global。 |

## plugin_configs

公开配置与 secret 分表。`plugin_id` 是主键，并外键引用 `plugins`。字段无 DEFAULT。

| 字段 | SQLite 类型 | 显式 NOT NULL | 含义 |
|---|---|---|---|
| `plugin_id` | TEXT | 是 | Plugin。 |
| `schema_revision` | TEXT | 是 | 宿主已验证的 schema 修订标识。 |
| `config_revision` | INTEGER | 是 | CHECK >0，与 plugins 配置计数同事务更新。 |
| `public_json` | TEXT | 是 | 经宿主 schema 验证的非 secret 值/槽位引用。 |

仅 disabled 且已 drain 时可 CAS 保存。普通 GET 只返回本表；带 secret 的保存与 keep/replace/clear 在同一个 app 事务完成。宿主必须先按 schema 将公开值与秘密槽位分开；Store 不解析包 schema 或替宿主猜测任意 JSON 字段是否敏感。

## plugin_grants

主键 `(plugin_id,package_hash,capability,object_scope)`；复合外键引用 `plugin_versions(plugin_id,package_hash)`。无 DEFAULT。替换仅在 disabled/drained 下接受，递增 revision/generation；旧行保留为 revoked，不能继续作为执行权限。

| 字段 | SQLite 类型 | 显式 NOT NULL | 含义 |
|---|---|---|---|
| `plugin_id` | TEXT | 是 | 复合主键 1。 |
| `package_hash` | TEXT | 是 | 复合主键 2；grant 精确绑定包。 |
| `capability` | TEXT | 是 | 复合主键 3；能力名，不接受模糊 `*`。 |
| `object_scope` | TEXT | 是 | 复合主键 4；typed Global/Site/exact Object 的规范 JSON。 |
| `status` | TEXT | 是 | CHECK granted/revoked。 |
| `sensitive` | INTEGER | 是 | CHECK 0/1；敏感授权标记。 |

新增 Site/对象不扩张既有 scope。KV/blob 的 grant scope 来自 Global/Site 凭据域；runtime command 的目标授权独立于该数据域。宿主从 typed handle 解析完整目标集合并检查当前 ACL，再以 `authorize_plugin_command` 签发绑定 command kind 的不透明权限：Global、Site 或精确 Library/Downloader/Subscribe Object，不从 Object 回退 Global。签发、接受/重放、claim、payload/outcome/result 访问均在实际数据库快照/事务内复查当前包、generation、凭据域和每个目标 grant。

## plugin_kv / plugin_blobs

两表结构相同，主键均为 `(plugin_id,security_scope,key)`；逻辑归属 `plugins.plugin_id`（未设 SQLite 外键，以便卸载时 `remove_private_data = false` 可跨重装保留私有状态），无 DEFAULT。

| 字段 | SQLite 类型 | 显式 NOT NULL | 含义 |
|---|---|---|---|
| `plugin_id` | TEXT | 是 | 复合主键 1。 |
| `security_scope` | TEXT | 是 | 复合主键 2；宿主签发的不透明 Global 或 Site/credential_epoch 域。 |
| `key` | TEXT | 是 | 复合主键 3；API 限 1..256 UTF-8 字节，DDL CHECK <=256 字节。 |
| `revision` | INTEGER | 是 | CHECK >0；取自插件级单调 state clock，删除后重建不会复用旧 etag。 |
| `value` | BLOB | 是 | CHECK KV <=65536 字节，blob <=1048576 字节。 |

blob 字节直接在 app DB，不写文件。API 不接受任意 scope 字符串；返回的 opaque bytes 携带原 scope。手动替换 Site 凭据或 URL、删除 Site 或删除再建同 ID 都产生新域，并在同事务清理已退役站点域下的 KV/blob/secret/command payload/result 并重算 `plugin_quota`；精确观察值上的 verified refresh 保留域。状态 reset/switch/export 仅由带显式围栏的宿主 authority 执行。

## plugin_quota

主键 `(plugin_id,security_scope)`，逻辑归属 `plugins.plugin_id`（未设 SQLite 外键，以便卸载保留私有数据时保留既有配额统计）。无 DEFAULT。一个内部 `aggregate` 行记插件合计与单调 clock，其他行按宿主域计额；内部 discriminator 不由 guest 构造。

| 字段 | SQLite 类型 | 显式 NOT NULL | 含义 |
|---|---|---|---|
| `plugin_id` | TEXT | 是 | 复合主键 1。 |
| `security_scope` | TEXT | 是 | 复合主键 2。 |
| `kv_bytes` | INTEGER | 是 | CHECK 0..5242880。 |
| `blob_bytes` | INTEGER | 是 | CHECK 0..52428800。 |
| `revision` | INTEGER | 是 | CHECK >=0；aggregate 行每次 state 修改递增。 |

成本为 **UTF-8 key 字节 + payload 字节**；单对象 payload 上限单独检查。空 value 仍计 key，因此不能无限创建零成本条目。每次替换/删除计算准确 delta；域计数、插件合计和字节变更位于同一个 App writer 事务，跨域并发不能超配额。

## plugin_config_secrets

主键 `(plugin_id,security_scope,key)`，`plugin_id` 外键引用 `plugins`；字段无 DEFAULT。存储保护遵守本地数据库的威胁边界，不承诺已有加密密钥管理。

| 字段 | SQLite 类型 | 显式 NOT NULL | 含义 |
|---|---|---|---|
| `plugin_id` | TEXT | 是 | 复合主键 1。 |
| `security_scope` | TEXT | 是 | 复合主键 2；与 KV/blob 使用同一凭据域。 |
| `key` | TEXT | 是 | 复合主键 3；API 1..256 UTF-8 字节，DDL CHECK <=256。 |
| `value` | BLOB | 是 | CHECK <=65536 字节。 |

写入只接收 disabled/drained 的宿主 ConfigAuthority，复查 digest/generation/resource revision/Site epoch。管理普通投影仅 configured，导出为单独 host-only typed 方法，禁止映射成 WIT/config.get。启用后的特定网络槽位消费留给 P1b/P2 的窄能力服务；未新增 `config:secret` grant 或 enabled 通用明文 getter。

## plugin_operations

`operation_id` 主键；`plugin_id` 可空并外键引用 `plugins`。`UNIQUE(subject,plugin_scope,idempotency_key)` 使用非空 `plugin_scope` discriminator，因此 NULL-plugin 管理操作也能原子去重。无 DEFAULT。

| 字段 | SQLite 类型 | 显式 NOT NULL | 含义 |
|---|---|---|---|
| `operation_id` | TEXT | 是 | UUID。 |
| `plugin_id` | TEXT | 否 | Plugin 管理/业务归属。 |
| `plugin_scope` | TEXT | 是 | `Option<plugin_id>` 的规范 discriminator。 |
| `subject` | TEXT | 是 | 规范 User / EndpointKey(key_id+issuer_user_id) / Plugin Task 主体。 |
| `idempotency_key` | TEXT | 是 | 同主体/Plugin 的幂等键。 |
| `kind` | TEXT | 是 | 宿主 command/管理操作种类。 |
| `canonical_method` | TEXT | 是 | 规范 HTTP 方法。 |
| `canonical_path` | TEXT | 是 | 规范路径。 |
| `body_digest` | BLOB | 是 | 32 字节请求摘要，不存请求明文。 |
| `revision` | INTEGER | 是 | CHECK >0；operation CAS。 |
| `generation` | INTEGER | 否 | runtime 必有执行代；management 可空且通过独立 typed 接受路径。 |
| `state` | TEXT | 是 | CHECK 精确十态，见下文。 |
| `phase` | TEXT | 是 | 宿主阶段码，不能装任意 guest 输出。 |
| `next_run_at` | INTEGER | 否 | Unix 秒；CHECK deferred 必须非空。 |
| `created_at` | INTEGER | 是 | Unix 秒。 |
| `updated_at` | INTEGER | 是 | Unix 秒。 |

十态为 queued/running/waiting/blocked/succeeded/rejected/deferred/failed/cancelled/unknown；仅 succeeded/rejected/failed/cancelled 终态。unknown 不可 resume/直接 cancel，须独立 fenced 宿主对账。一般 operation 投影从对应 command 读取 effect 分类，不另存一份事实；没有 payload/result 字节。confirmed 非终态不可 cancel/resume/claim；confirmed+deferred 规范为 blocked、phase=effect-disposition-required 且清空 next_run_at，避免自动 due 执行，仍在 unresolved 恢复扫描中可见。主体匹配只允许查看原主体，EndpointKey 轮换不继承。

显式索引：`plugin_operations_pending(COALESCE(next_run_at,0),operation_id) WHERE state IN ('queued','deferred')` 用于有界 due keyset；`plugin_operations_subject(subject,operation_id)`；`plugin_operations_plugin(plugin_id,state,operation_id)`；`plugin_operations_unresolved(operation_id) WHERE state NOT IN ('succeeded','rejected','failed','cancelled')` 用于包括 running/unknown 的启动恢复 keyset。发现工作不使用 offset。

## plugin_commands

`operation_id` 主键并外键引用 `plugin_operations`；无 DEFAULT。

| 字段 | SQLite 类型 | 显式 NOT NULL | 含义 |
|---|---|---|---|
| `operation_id` | TEXT | 是 | 稳定 command/operation 绑定。 |
| `command_class` | TEXT | 是 | CHECK management/runtime；runtime 接受不能传 None 绕过 authority。 |
| `package_hash` | TEXT | 否 | scoped command 捕获的 digest。 |
| `expected_revision` | INTEGER | 否 | 接受时资源 revision；ConfigAuthority 使用时继续复查。 |
| `security_scope` | TEXT | 否 | captured opaque 域；runtime 必有。 |
| `authority_generation` | INTEGER | 否 | captured authority generation；runtime 必有。 |
| `target_scopes` | TEXT | 是 | 宿主签发的精确目标集合规范 JSON；management 为 []，runtime 非空。与 security_scope 数据域分开。 |
| `cause_id` | TEXT | 是 | 因果链追踪标识（1..128 个可见 ASCII 字符）。 |
| `cause_depth` | INTEGER | 是 | CHECK 0..4；因果链跳数，runtime 每次派生 +1，超过 4 拒绝。 |
| `cause_visited` | TEXT | 是 | 已访问元组 `(plugin_id, kind, target_key)` 规范 JSON 数组，用于拒绝环路。 |
| `revision` | INTEGER | 是 | CHECK >0；command CAS。 |
| `attempt` | INTEGER | 是 | CHECK >=0；每次合法 claim 递增。 |
| `claim_nonce` | TEXT | 否 | 当前 claim 的不透明 UUID。 |
| `effect` | TEXT | 是 | CHECK none/confirmed/unknown；宿主效果观察。 |

DDL 另 CHECK runtime 的 package_hash/security_scope/authority_generation 都非空且 target_scopes 非 []。API 要求 1..200 个不重复 typed targets，规范排序；重排可重放，改变目标集合与原请求冲突。claim 返回不可反序列化的 owned handle；旧 revision/attempt/nonce/generation 不能发布。worker 丢失先持久化 unknown，不能覆盖 confirmed。宿主 reconciliation 以 operation revision/generation/attempt 验证并记录 reconciled 阶段和效果分类：unknown 可明确结算成功/失败或证实无效果；confirmed 的 waiting/blocked/deferred 仅可结算 succeeded/failed 并保留 confirmed，不能以 ConfirmedNoEffect 清除。业务效果的具体 receipt 仍由对应业务库/P1b 服务拥有。

## plugin_command_payloads / plugin_operation_results

两表隔离敏感原始字节。第一表 `operation_id` 主键并外键引用 `plugin_commands`；第二表引用 `plugin_operations`。无 DEFAULT。

| 字段 | SQLite 类型 | 显式 NOT NULL | 含义 |
|---|---|---|---|
| `operation_id` | TEXT | 是 | 主键。 |
| `security_scope` | TEXT | 是 | captured 域；从 claim/authority 继承，不能改成当前新域。 |
| `payload` / `result` | BLOB | 是 | 各自 CHECK <=1048576 字节。 |

payload 仅在当前 fenced claim 内读；result 额外匹配原 subject、当前有效 authority、plugin 与原域。无域管理操作不能写 opaque result；有秘密的管理操作走单独 ConfigAuthority 路径，generation=None 不解除域隔离。普通 operation GET 为纯读。

## plugin_task_bindings

`binding_id` 主键；`UNIQUE(plugin_id,task_id)`；`plugin_id` 外键引用 `plugins`。无 DEFAULT。

| 字段 | SQLite 类型 | 显式 NOT NULL | 含义 |
|---|---|---|---|
| `binding_id` | TEXT | 是 | 稳定 UUID，定义更新不改变身份。 |
| `plugin_id` | TEXT | 是 | Plugin。 |
| `task_id` | TEXT | 是 | 包内宿主验证的 task ID。 |
| `definition_revision` | INTEGER | 是 | CHECK >0；desired 定义 CAS。 |
| `generation` | INTEGER | 是 | CHECK >=0；desired 执行代。 |
| `enabled` | INTEGER | 是 | CHECK 0/1。 |
| `interval_secs` | INTEGER | 否 | CHECK >0；无定时定义可空。 |
| `public_config` | TEXT | 是 | 验证后的非 secret 配置/槽位引用 JSON。 |
| `projection_revision` | INTEGER | 是 | CHECK >=0；Jobs 已确认的定义修订。 |
| `projection_generation` | INTEGER | 否 | Jobs 已确认的 generation。 |
| `job_def_id` | TEXT | 否 | Jobs 定义引用，Store 不创建或操作 Queue。 |

`plugin_task_bindings_pending(binding_id)` 为 partial index，条件 `definition_revision!=projection_revision OR generation IS NOT projection_generation`。有界 keyset 发现待投影意图；ack 只接受同 binding/generation/definition_revision。停用生成 disabled desired 投影，旧 ack 不会抹掉它。真实 Queue 投影、执行/调度事实和跨库恢复由 P1b 实现。

## plugin_trust_keys

Admin 钉扎的离线 minisign/Ed25519 公钥及其吊销状态，用于 `.cmplugin` 签名验证与启用策略检查。

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `key_id` | `TEXT` | 1 | 是 | 无 | 签名密钥标识，CHECK 非空。 |
| `label` | `TEXT` | — | 是 | 无 | Admin 展示标签，CHECK 非空。 |
| `public_key` | `TEXT` | — | 是 | 无 | 规范公钥文本，CHECK 非空。 |
| `fingerprint` | `TEXT` | — | 是 | 无 | 公钥摘要指纹，`UNIQUE`，CHECK 非空。 |
| `status` | `TEXT` | — | 是 | 无 | CHECK：`trusted` / `revoked`。 |
| `revision` | `INTEGER` | — | 是 | 无 | CHECK >0；吊销 CAS 修订号。 |
| `created_at` | `INTEGER` | — | 是 | 无 | 钉扎时间，Unix 秒。 |
| `created_by` | `TEXT` | — | 是 | 无 | 操作主体标识，CHECK 非空。 |
| `revoked_at` | `INTEGER` | — | 否 | 无 | 吊销时间，Unix 秒；CHECK 与 `status='revoked'` 同步非空。 |
| `revoked_by` | `TEXT` | — | 否 | 无 | 吊销操作主体；CHECK 与 `status='revoked'` 同步非空。 |
| `revoked_reason` | `TEXT` | — | 否 | 无 | 吊销原因说明。 |

**生产表定义**

```sql
CREATE TABLE plugin_trust_keys(key_id TEXT PRIMARY KEY NOT NULL CHECK(length(key_id)>0),label TEXT NOT NULL CHECK(length(label)>0),public_key TEXT NOT NULL CHECK(length(public_key)>0),fingerprint TEXT NOT NULL UNIQUE CHECK(length(fingerprint)>0),status TEXT NOT NULL CHECK(status IN ('trusted','revoked')),revision INTEGER NOT NULL CHECK(revision>0),created_at INTEGER NOT NULL,created_by TEXT NOT NULL CHECK(length(created_by)>0),revoked_at INTEGER,revoked_by TEXT,revoked_reason TEXT,CHECK((status='revoked')=(revoked_at IS NOT NULL)),CHECK((status='revoked')=(revoked_by IS NOT NULL)));
```

**显式命名索引**

```sql
CREATE INDEX plugin_trust_keys_status ON plugin_trust_keys(status,key_id);
```

## site_maintenance_receipts

Site 维度维护动作（`verify-session` / `check-in`）的权威幂等与溯源回执。`UNIQUE(site_id, action, window_key)` 绑定到 Site 业务动作与窗口而非 `plugin_id`；`site_id` 外键引用 `sites(id) ON DELETE CASCADE`，对 `plugins` 不设外键，因此插件卸载、升级或凭据轮换不会清除既有 `completed` / `unknown` 拦截。

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `receipt_id` | `TEXT` | 1 | 是 | 无 | 回执 UUID，CHECK 非空。 |
| `site_id` | `TEXT` | — | 是 | 无 | 外键引用 `sites(id) ON DELETE CASCADE`。 |
| `action` | `TEXT` | — | 是 | 无 | CHECK：`verify-session` / `check-in`。 |
| `window_key` | `TEXT` | — | 是 | 无 | 窗口标识（如 Site 本地日期 `YYYY-MM-DD`），CHECK 非空。 |
| `timezone` | `TEXT` | — | 是 | 无 | 领取窗口时生效的 Site 时区，CHECK 非空。 |
| `window_start_utc` | `INTEGER` | — | 是 | 无 | 窗口起始 Unix 秒。 |
| `window_end_utc` | `INTEGER` | — | 是 | 无 | 窗口结束 Unix 秒，CHECK `window_end_utc > window_start_utc`。 |
| `plugin_id` | `TEXT` | — | 是 | 无 | 执行插件标识（仅溯源，不参与唯一键），CHECK 非空。 |
| `provider_id` | `TEXT` | — | 是 | 无 | 维护提供者标识，CHECK 非空。 |
| `generation` | `INTEGER` | — | 是 | 无 | 执行代际，CHECK `>= 0`。 |
| `credential_epoch` | `TEXT` | — | 是 | 无 | 领取时的 Site `credential_epoch`，CHECK 非空。 |
| `state` | `TEXT` | — | 是 | 无 | CHECK：`pending` / `completed` / `unknown` / `failed`。 |
| `outcome` | `TEXT` | — | 否 | 无 | 结构化或摘要结果文本。 |
| `evidence` | `TEXT` | — | 否 | 无 | 宿主确认证据或失败原因摘要。 |
| `revision` | `INTEGER` | — | 是 | 无 | CAS 修订号，CHECK `> 0`。 |
| `created_at` | `INTEGER` | — | 是 | 无 | 创建时间，Unix 秒。 |
| `updated_at` | `INTEGER` | — | 是 | 无 | 最近更新时间，Unix 秒。 |

**生产表定义**

```sql
CREATE TABLE site_maintenance_receipts(receipt_id TEXT PRIMARY KEY NOT NULL CHECK(length(receipt_id)>0),site_id TEXT NOT NULL REFERENCES sites(id) ON DELETE CASCADE,action TEXT NOT NULL CHECK(action IN ('verify-session','check-in')),window_key TEXT NOT NULL CHECK(length(window_key)>0),timezone TEXT NOT NULL CHECK(length(timezone)>0),window_start_utc INTEGER NOT NULL,window_end_utc INTEGER NOT NULL CHECK(window_end_utc>window_start_utc),plugin_id TEXT NOT NULL CHECK(length(plugin_id)>0),provider_id TEXT NOT NULL CHECK(length(provider_id)>0),generation INTEGER NOT NULL CHECK(generation>=0),credential_epoch TEXT NOT NULL CHECK(length(credential_epoch)>0),state TEXT NOT NULL CHECK(state IN ('pending','completed','unknown','failed')),outcome TEXT,evidence TEXT,revision INTEGER NOT NULL CHECK(revision>0),created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL,UNIQUE(site_id,action,window_key));
```

**显式命名索引**

```sql
CREATE INDEX site_maintenance_receipts_site_window ON site_maintenance_receipts(site_id,action,window_end_utc DESC,receipt_id);
CREATE INDEX site_maintenance_receipts_unresolved ON site_maintenance_receipts(updated_at,receipt_id) WHERE state IN ('pending','unknown');
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

`app.db` 事务性插件事件 outbox；与所属域事实在同一写事务内写入，所有时间字段统一为 Unix **毫秒**（`>= 0`）。

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `event_id` | `TEXT` | 1 | 是 | 无 | 稳定事件 ID（用于插件幂等去重），CHECK 非空。 |
| `schema_version` | `INTEGER` | — | 是 | `1` | 事件结构版本号，CHECK `> 0`。 |
| `event_domain` | `TEXT` | — | 是 | 无 | 事件域（如 `site`），CHECK 非空。 |
| `event_kind` | `TEXT` | — | 是 | 无 | 事件类型（如 `site.checkin.after`），CHECK 非空。 |
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

每插件投递状态、租约、重试与 dead-letter 回执表。主键 `(event_id, plugin_id)` 兼作外键 `event_id -> plugin_event_outbox(event_id) ON DELETE CASCADE` 的前缀索引；对 `plugins` 不设跨业务约束外键。时间字段统一为 Unix **毫秒**（`>= 0`）。

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

## plugin_ui_trust

按 `(plugin_id, package_sha256)` 持久化联邦 React UI 包的信任授权、撤销与 CAS 修订号，升级或 dev-reload 更换包哈希后需重新授权。

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `plugin_id` | `TEXT` | 1 | 是 | 无 | 插件 ID，CHECK 非空。 |
| `package_sha256` | `TEXT` | 2 | 是 | 无 | 对应 `.cmplugin` 包 SHA-256，CHECK 非空。 |
| `bundle_sha256` | `TEXT` | — | 是 | 无 | `ui/asset-manifest.json` 摘要 SHA-256，CHECK 非空。 |
| `granted_by_user_id` | `TEXT` | — | 是 | 无 | 最近授予信任的管理员用户 ID，CHECK 非空。 |
| `granted_at` | `TEXT` | — | 是 | 无 | 最近授予信任时间（RFC 3339），CHECK 非空。 |
| `revoked_at` | `TEXT` | — | 否 | 无 | 撤销信任时间（RFC 3339），NULL 表示当前生效，CHECK 非空。 |
| `revision` | `INTEGER` | — | 是 | `1` | CAS 修订号，CHECK `> 0`。 |

**生产表定义**

```sql
CREATE TABLE plugin_ui_trust(
  plugin_id TEXT NOT NULL CHECK(length(plugin_id)>0),
  package_sha256 TEXT NOT NULL CHECK(length(package_sha256)>0),
  bundle_sha256 TEXT NOT NULL CHECK(length(bundle_sha256)>0),
  granted_by_user_id TEXT NOT NULL CHECK(length(granted_by_user_id)>0),
  granted_at TEXT NOT NULL CHECK(length(granted_at)>0),
  revoked_at TEXT CHECK(revoked_at IS NULL OR length(revoked_at)>0),
  revision INTEGER NOT NULL DEFAULT 1 CHECK(revision>0),
  PRIMARY KEY(plugin_id,package_sha256)
);
```
