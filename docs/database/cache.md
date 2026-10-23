# cache.db：按来源分表的原始响应缓存

[返回数据库总览](README.md)

结构核对基线：`8fbf81b`（2026-10-23）。唯一建表定义为 [media::response_cache::CACHE_SCHEMA_SQL](../../crates/media/src/response_cache/mod.rs)；[Store layout](../../crates/store/src/schema/layout.rs) 直接复用该常量及身份/校验函数。此前两份 SQL 附本已删除，不存在 Store 与 media 各维护一套索引的现行合同。

文件身份：`application_id=1129139251`，`user_version=1`。共 **6 张业务表、30 个字段、12 个显式命名索引**；AUTOINCREMENT 的 `sqlite_sequence` 不计业务表。以下是源码 DDL 字典，不把历史初始化结果当成本轮应用验收。

## 职责与来源

只保存外部响应，不保存 Media 身份、Library 文件、已采用元数据、观看状态或图片字节。正常清缓存不能修改这些事实；当前协议分页读取持久采用元数据及输入，旧请求时NFO/catalog缺陷不能直接照搬为现状。清cache的完整现场验收与边界见[当前复审](../audits/2026-10-23-storage-followup-review.md)。

来源是固定 [CacheSource](../../crates/media/src/response_cache/source.rs) 白名单，不是用户提供的 SQL 表名；表内没有 `source` 列。

| 来源 | 表 | 最近列表索引 | 保留清理索引 |
|---|---|---|---|
| `tmdb` | `tmdb_cache` | `idx_tmdb_cache_fetched` | `idx_tmdb_cache_retention` |
| `douban` | `douban_cache` | `idx_douban_cache_fetched` | `idx_douban_cache_retention` |
| `tvdb` | `tvdb_cache` | `idx_tvdb_cache_fetched` | `idx_tvdb_cache_retention` |
| `bangumi` | `bangumi_cache` | `idx_bangumi_cache_fetched` | `idx_bangumi_cache_retention` |
| `anilist` | `anilist_cache` | `idx_anilist_cache_fetched` | `idx_anilist_cache_retention` |
| `fanart` | `fanart_cache` | `idx_fanart_cache_fetched` | `idx_fanart_cache_retention` |

六张表结构相同。最近索引列为 `(fetched_at DESC, entry_id ASC)`，保留索引列为 `(fetched_at ASC, entry_id ASC)`；是否存在两个方向索引的净收益待实测，不在文档中预先删除或宣称冗余。

## 共用字段字典

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `entry_id` | `INTEGER` | 1 | 否 | 无 | AUTOINCREMENT 表内身份，不是 Media ID；INTEGER PRIMARY KEY 自带 rowid 语义 |
| `cache_key` | `TEXT` | — | 是 | 无 | `COLLATE BINARY UNIQUE`，完整请求键，区分大小写 |
| `body` | `TEXT` | — | 是 | 无 | 原始 JSON/HTML 文本；无 JSON CHECK |
| `fetched_at` | `INTEGER` | — | 是 | 无 | Unix 秒 |
| `expires_at` | `INTEGER` | — | 否 | 无 | Unix 秒；NULL 表示无显式到期 |

没有外键、CHECK 或触发器。唯一性仅限单来源；不同来源可有相同 `cache_key/entry_id`。UPSERT 使用 `ON CONFLICT(cache_key) DO UPDATE`，保留既有 entry_id；删除后重新插入产生新身份。

## 初始化与校验

- Store 和 media 都只初始化空且未版本化的目标文件；DDL 和文件身份在事务内提交。
- media `open`、Store preflight 先只读检查已有文件。旧布局、未知版本、必需 cache 表/索引缺失时拒绝，不创建兼容表或悄悄修复索引。
- cache `validate_schema` 检查必需表/索引名称存在；不是逐列/索引定义的完整一致性审计，也不证明任意外部破坏可被识别。
- 连接显式WAL、5000ms busy timeout、foreign_keys=ON。Store的cache连接显式synchronous=FULL；独立media ResponseCache连接未显式设置synchronous，不能将同文件连接策略混写。同文件仍共享SQLite写锁，没有跨文件原子性。
- 受控清空走连接内删除；若要重建文件，先停写并关闭所有连接。不得 unlink 活跃 DB/WAL/SHM。空间复用不意味着文件立即缩小。

## 请求键、TTL 与读取边界

实现：[cache.rs](../../crates/media/src/cache.rs)、[queries.rs](../../crates/media/src/response_cache/queries.rs)、[Store catalog.rs](../../crates/store/src/catalog.rs)。

- 来源客户端负责保留完整路径、参数、语言及必要凭证摘要；不能把 `/movie/1` 与 `/movie/10` 或子资源视作同一详情。fanart 键为 `fanart/{media_type}/{id}/{prefix}`，prefix 是 API key 的 SHA-256 前16位十六进制摘要，不是原始密钥。
- `CatalogCache::put` 默认 TTL **604800 秒（7天）**；底层 put 可显式传 expires_at 或 NULL，DDL 不指定 TTL。
- `get_fresh` 在 `expires_at <= now` 时视为过期；`get_stale` 忽略期限。刷新成功解码后才替换持久响应；失败仅可使用可解码的 stale 值。
- 底层 `get/list/details_for_prefixes` 不过滤 TTL，命中不等于 fresh。列表按 `fetched_at DESC, entry_id ASC`。
- 来源聚合游标必须携带来源及表内 ID，不能只用 entry_id。清理预算/详情保护仍按[来源缓存合同](../superpowers/specs/2026-10-18-source-separated-cache.md)，不是到期自动删除。
