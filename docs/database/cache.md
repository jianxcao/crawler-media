# cache.db：按来源分表的原始响应缓存

[返回数据库总览](README.md)

当前源码基线：`feat/storage-redesign@a3dab1b`。本页依据两条实际初始化路径，不把它们误写成完全相同的 DDL：

- Store：[v1/cache.sql](../../crates/store/src/schema/v1/cache.sql)，由 [schema/layout.rs](../../crates/store/src/schema/layout.rs) 初始化/校验文件身份。
- media：[response_cache/mod.rs](../../crates/media/src/response_cache/mod.rs) 的 `CACHE_SCHEMA_SQL`、`open` 和 `initialize`。同目录另有 [schema.sql](../../crates/media/src/response_cache/schema.sql)，但当前 `initialize` 执行的是 Rust 常量，不能仅据该 SQL 文件声称 media 已统一索引。

两者都定义 **6 张业务表、30 个字段**，文件身份为 `application_id = 1129139251`、`user_version = 1`。Store DDL 定义 **12 个命名索引**；media 独立 DDL 定义 **6 个命名索引**。本页是源码字典，不是本次真实 Store/media 初始化的运行提取结果，也不证明全应用缓存接线已经完成切换。`sqlite_sequence` 是 AUTOINCREMENT 产生的 SQLite 内部表，不计入业务表数。

缓存保存 Metadata source / fanart.tv 的原始响应，不保存规范 Media 身份、Library 文件或 Playback 事实。过期和重抓可以按缓存策略进行，但不能由此推导可清空 [app.db](app.md)、[library.db](library.md) 或 [subscribe.db](subscribe.md)。没有旧布局的兼容表、迁移别名或镜像合同。

## 来源与表目录

来源在表名中表达，**没有 `source` 列**。映射由 [CacheSource](../../crates/media/src/response_cache/source.rs) 和 [Store catalog.rs](../../crates/store/src/catalog.rs) 定义。

| 来源标签 | 表 | 字段数 | 内容 |
|---|---|---:|---|
| `tmdb` | [tmdb_cache](#tmdb_cache) | 5 | TMDB 搜索、详情、季/集、图片等响应。 |
| `douban` | [douban_cache](#douban_cache) | 5 | Douban 搜索、目录、详情等响应。 |
| `tvdb` | [tvdb_cache](#tvdb_cache) | 5 | TVDB 响应。 |
| `bangumi` | [bangumi_cache](#bangumi_cache) | 5 | Bangumi 响应。 |
| `anilist` | [anilist_cache](#anilist_cache) | 5 | AniList 响应。 |
| `fanart` | [fanart_cache](#fanart_cache) | 5 | fanart.tv 图片元数据响应，不是图片二进制。 |

## 六表共用字段字典

以下字典完整适用于上列每一张表；所有列均无 DEFAULT 声明。NOT NULL 指显式声明，`entry_id INTEGER PRIMARY KEY` 另有 SQLite rowid 主键语义。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `entry_id` | `INTEGER` | 1 | 否 | 无 | 表内 AUTOINCREMENT 主键；不是跨来源全局 ID，也不是 Media ID。 |
| `cache_key` | `TEXT` | — | 是 | 无 | 表内唯一请求键；包含来源客户端决定的路径/参数/语言等身份信息。 |
| `body` | `TEXT` | — | 是 | 无 | 原始响应文本，通常 JSON，亦可为来源的 HTML；DDL 没有 JSON CHECK。 |
| `fetched_at` | `INTEGER` | — | 是 | 无 | 响应获取时间，Unix 秒；用于最近列表与保留清理排序。 |
| `expires_at` | `INTEGER` | — | 否 | 无 | 显式过期时间，Unix 秒；NULL 表示没有显式到期。 |

**每表键与约束**：

- 主键 `(entry_id)`，DDL 为 `INTEGER PRIMARY KEY AUTOINCREMENT`。
- UNIQUE `(cache_key)`，只在该来源表内唯一。Store 显式写 `COLLATE BINARY`；media 常量未写 COLLATE，使用 SQLite 默认 BINARY 文本排序。键按大小写敏感的完整文本区分，不执行 URL 规范化或语言归并。
- 没有额外 CHECK、外键或触发器；不存在 `expires_at > fetched_at`、合法 JSON 或时间非负约束。
- `put` 使用 `ON CONFLICT(cache_key) DO UPDATE` 更新 body/时间，不替换既有行的 `entry_id`；删除再插入则是新的表内身份。

## tmdb_cache

字段和键：[六表共用字段字典](#六表共用字段字典)。请求键由 [client.rs](../../crates/media/src/client.rs) 构造，例如搜索/详情路径，可附语言和其他查询参数；不能把所有 `/movie/{id}` 前缀当同一个完整键。

| 初始化路径 | 命名索引 | 列顺序 |
|---|---|---|
| Store | `tmdb_cache_recent_idx` | `(fetched_at DESC, entry_id ASC)` |
| Store | `tmdb_cache_retention_idx` | `(fetched_at ASC, entry_id ASC)` |
| media | `idx_tmdb_cache_fetched` | `(fetched_at DESC, entry_id ASC)` |

## douban_cache

字段和键：[六表共用字段字典](#六表共用字段字典)。[douban.rs](../../crates/media/src/douban.rs) 使用带参数的请求路径或详情路径作为键；响应未必是 JSON。

| 初始化路径 | 命名索引 | 列顺序 |
|---|---|---|
| Store | `douban_cache_recent_idx` | `(fetched_at DESC, entry_id ASC)` |
| Store | `douban_cache_retention_idx` | `(fetched_at ASC, entry_id ASC)` |
| media | `idx_douban_cache_fetched` | `(fetched_at DESC, entry_id ASC)` |

## tvdb_cache

字段和键：[六表共用字段字典](#六表共用字段字典)。[tvdb.rs](../../crates/media/src/tvdb.rs) 的搜索键包含编码查询与类型，如 `/search?query=…&type=…`。

| 初始化路径 | 命名索引 | 列顺序 |
|---|---|---|
| Store | `tvdb_cache_recent_idx` | `(fetched_at DESC, entry_id ASC)` |
| Store | `tvdb_cache_retention_idx` | `(fetched_at ASC, entry_id ASC)` |
| media | `idx_tvdb_cache_fetched` | `(fetched_at DESC, entry_id ASC)` |

## bangumi_cache

字段和键：[六表共用字段字典](#六表共用字段字典)。[bangumi.rs](../../crates/media/src/bangumi.rs) 的搜索键包含编码查询、type 与 responseGroup，例如 `/search/subject/…?type=…&responseGroup=small`。

| 初始化路径 | 命名索引 | 列顺序 |
|---|---|---|
| Store | `bangumi_cache_recent_idx` | `(fetched_at DESC, entry_id ASC)` |
| Store | `bangumi_cache_retention_idx` | `(fetched_at ASC, entry_id ASC)` |
| media | `idx_bangumi_cache_fetched` | `(fetched_at DESC, entry_id ASC)` |

## anilist_cache

字段和键：[六表共用字段字典](#六表共用字段字典)。[anilist.rs](../../crates/media/src/anilist.rs) 的搜索键包含编码查询与 format，例如 `/search?query=…&format=…`。

| 初始化路径 | 命名索引 | 列顺序 |
|---|---|---|
| Store | `anilist_cache_recent_idx` | `(fetched_at DESC, entry_id ASC)` |
| Store | `anilist_cache_retention_idx` | `(fetched_at ASC, entry_id ASC)` |
| media | `idx_anilist_cache_fetched` | `(fetched_at DESC, entry_id ASC)` |

## fanart_cache

字段和键：[六表共用字段字典](#六表共用字段字典)。[fanart.rs](../../crates/media/src/fanart.rs) 构造 `fanart/{media_type}/{id}/{prefix}`：TV 使用 TVDB id，movie 使用 TMDB id；`prefix` 是 API key 的 SHA-256 十六进制前 16 位，不是原始 API key。响应中的图片 URL 不等于图片文件本身。

| 初始化路径 | 命名索引 | 列顺序 |
|---|---|---|
| Store | `fanart_cache_recent_idx` | `(fetched_at DESC, entry_id ASC)` |
| Store | `fanart_cache_retention_idx` | `(fetched_at ASC, entry_id ASC)` |
| media | `idx_fanart_cache_fetched` | `(fetched_at DESC, entry_id ASC)` |

## 初始化路径差异

Store 对未初始化文件执行 `v1/cache.sql` 并写入身份；已存在且身份匹配的文件只校验 `application_id` / `user_version`，不逐项修补表或索引。media `initialize` 执行带 `IF NOT EXISTS` 的 `CACHE_SCHEMA_SQL`，即使文件已有匹配身份也会按名字尝试创建其 `idx_*_cache_fetched` 索引。

因此两条路径虽然列和业务键形状一致，**索引命名与覆盖不一致**：Store 有最近倒序和清理正序两套索引，media 独立初始化没有 retention 索引。Store 创建后再由 media 打开，可能另增同形的最近索引；反过来，仅通过 Store 身份校验不能保证 media 创建的文件拥有 Store 的 retention 索引。不能将这些结果描述成一个已经收敛的物理 schema。索引表列的是各自 DDL 所声明的内容，不是同一文件必然同时存在的 18 个命名索引。

## TTL 与读取边界

行为来源：[cache.rs](../../crates/media/src/cache.rs)、[response_cache/queries.rs](../../crates/media/src/response_cache/queries.rs)。

- `CatalogCache::put` 默认 TTL 为 **7 天（604800 秒）**，写 `fetched_at = now`、`expires_at = now + TTL`；这是客户端策略，不是数据库 DEFAULT。Store 和底层 response cache 的 `put` 允许调用者直接提供时间及 NULL 到期值。
- `CatalogCache::get_fresh` 在 `expires_at <= now` 时视为未命中；NULL 不触发显式到期。`get_stale` 忽略过期时间。
- `get_or_fetch` 在成功解码后才替换持久响应；刷新失败可使用可解码的 stale 响应。缓存过期不等于立即删除磁盘行，也不等于任何 stale 内容都可接受。
- 底层 `response_cache::get`、`list`、`details_for_prefixes` 不过滤 TTL；不能把底层命中当成 fresh 命中。列表按 `fetched_at DESC, entry_id ASC` 排序。
- 同一表的 `entry_id` 消歧同时间响应；跨来源 `entry_id` 可能相同。需要全局游标时必须同时保留来源身份，不能只拿 `entry_id` 当全库键。
