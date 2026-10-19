# SQLite 是系统记录，按生命周期拆分

行存 SQLite。字节（视频、NFO、图、Indexer YAML、Chromium）留在磁盘。行中的路径指向这些字节。**Users**、**Sites**、**Filters**、**Subscribes**、**Media**、**Library** ledger、**Naming templates**、**Metadata source** 负载不存在 ad-hoc 文件的第二事实来源。

**历史背景（拆库决策时，不是当前实现）：** 当时 `data/crawler-media.db` 持有 users、media（仅标题 + 别名）、sites、filters、subscribes、facts、ledger、tokens、playback，以及一个 key/value `settings` 袋。TMDB 响应放在 `data/tmdb/` 下，作为无 TTL 的文件。**Transfer** 用 `Media.title` 命名目录并保留 Downloader 文件名。Library 根是 `data/library`，不是一行记录。这就是为什么 live 运行产生 `The Long Watch - S01E01/<release-name>.mkv`，清缓存是 `rm -rf tmdb/` 而不是 SQL。

## 四个文件

跨文件外键不存在。共享 id 是 UUID 文本；由应用强制。拆分依据是*什么可以删*，而不是一个概念一个文件。

| 文件 | 可清空？ | 保存内容 |
|---|---|---|
| `app.db` | 否 | **Users**、tokens、**Sites**、**Downloaders**、**Filters**、实例设置、**Naming templates**、**Library** 根、**Media** 身份 |
| `catalog.db` | 是 | **Metadata source** HTTP 响应体（先 TMDB；Douban/TVDB/Bangumi/AniList 同一张表） |
| `library.db` | 否（这*就是*馆藏索引） | ledger、scrape 侧车索引 |
| `subscribe.db` | 否 | **Subscribes**、逐槽 facts、**Playback** 进度 |

`Media` 留在 `app.db`，因为 **Subscribe** 可以在拥有任何文件之前存在。`library.db` 中的 ledger 指向 `media_id`；它不拥有作品。

`CRAWLER_MEDIA_DATA` 仍为数据目录。默认路径：`{data}/app.db`、`{data}/catalog.db`、`{data}/library.db`、`{data}/subscribe.db`。一次性打开旧 `crawler-media.db` 会把行复制进四个文件，旧文件保留原位直到操作者删除。

## 留在磁盘上的内容

- 视频 / 字幕 / 音频文件在 **Library** 根下。当前 `ledger.path` 保存实际文件路径（通常为绝对路径），不能假定是相对 `root_id`；现行归属按路径最长唯一根解析，`root_id` 尚未成为持久归属事实。
- 这些文件旁的 NFO 与图（路径也在 `scrape_sidecars`）。
- Indexer overlay YAML（`{data}/indexers/`）。选择器是代码形态的配置，不是凭证。**Site** cookie/key/RSS 留在 `app.db`（ADR-0003）。
- 外部 Browser 的CDP端点配置；当前不下载／启动受管Chromium（[ADR-0007](<0007-cdp-browser-opt-in.md>)）。

不要把 mkv blob 存进 SQLite。`catalog.db` 填充后，不要再维护一份平行的 TMDB JSON 树。

## Naming template

两行，不是 Jinja，不是按 **Subscribe**：

```
naming_templates (kind TEXT PRIMARY KEY, pattern TEXT NOT NULL)
-- kind: movie | tv
```

`kind` 是 **Media** 类型。Pattern 是对应 **Library** 根下的相对路径，含 `{placeholders}`。未知占位符在保存时报配置错误。空值会去掉最小的连续分隔符，因此 `{title} ({year})` 在缺少年份时变成 `The Long Watch`。

v1 封闭占位符集合：

| Token | 来源 |
|---|---|
| `{title}` | **Media**.title |
| `{year}` | **Media**.year |
| `{season}` | 零填充季（`01`） |
| `{episode}` | 零填充集（`01`） |
| `{season_episode}` | `S01E01`（`episode_to` ≠ `episode` 时为范围 `S01E01-E04`） |
| `{part}` | **Release** 的 part（如有） |
| `{resolution}` `{source}` `{codec}` `{hdr}` | 先 probe，**Release** 兜底 |
| `{ext}` | 源文件扩展名（含点） |

默认值（MoviePilot 风格，无 Jinja、无中文集后缀）：

```
movie: {title} ({year})/{title} ({year}){part} - {resolution}{ext}
tv:    {title} ({year})/Season {season}/{title} - {season_episode}{part}{ext}
```

**Transfer** 渲染该路径，然后写 `ledger.path`。Downloader 文件名不保留。

## Catalog 缓存

```
catalog_cache (
  source TEXT NOT NULL,          -- tmdb | douban | tvdb | bangumi | anilist
  cache_key TEXT NOT NULL,       -- 请求路径含 query
  body TEXT NOT NULL,            -- 原始 JSON
  fetched_at INTEGER NOT NULL,   -- unix 秒
  expires_at INTEGER,            -- null = 不过期
  PRIMARY KEY (source, cache_key)
)
```

TMDB 客户端读写这张表，替代 `{data}/tmdb/` 文件。默认 TTL 7 天；miss 或过期命中网络并替换该行。Stale-if-error：网络失败但行存在时，即使过了 `expires_at` 也返回该行。解析出的 **Media** 不是缓存；payload 才是。识别仍会 upsert `app.db` **Media**（内部 id、别名、标题、年份）。

## 原始 Schema 增量提案（历史，不是现行 DDL）

此节记录当时拆库方向；实际表／迁移以 [Store schema](<../../crates/store/src/schema.rs>) 与 [tables](<../../crates/store/src/schema/tables.rs>) 为准。尤其 `ledger.root_id` 提案未等于完成backfill，不能据此直接跨文件JOIN或删除归属判断。

`app.db`

- `media`：加 `year INTEGER`、`original_title TEXT`。
- `downloaders`：把 JSON `settings.downloader` 袋提升为表（`id`、`name`、`kind`、`url`、`username`、`password`、`category`、`is_default`）。
- `library_roots`（`id`、`kind` movie|tv、`path`、`is_default`）。
- `naming_templates` 如上。
- `settings` 保留给剩余标量（transfer_mode、scrape 开关、TMDB API key）。

`library.db`

- 历史提案为 `ledger.root_id TEXT`＋相对路径；当前有root_id列但未作为已填充归属来源，path按实际文件路径使用。质量列含义不变，归属／删除仍遵守 [ADR-0012](<0012-library-removal.md>)。
- `scrape_sidecars`（`ledger_id`、`kind` nfo|poster|fanart、`path`、PRIMARY KEY（`ledger_id`、`kind`））。

`subscribe.db` 沿用今天的 `subscribes`、`subscribe_facts`、`playback_progress`，含义不变。

## 备选方案

单个 SQLite 文件：备份更简单，但 catalog 变动与 ledger 增长与凭证共享 WAL。否决，因为操作者要求多文件且缓存必须可弃。

TMDB 缓存为文件：已实现；无查询、无 TTL、无来源列。否决。

Jinja 模板：MoviePilot 的能力。v1 否决——封闭占位符集合对两种默认布局足够，也让 `library` 免于脚本运行时（ADR-0002）。

按 **Subscribe** 命名：对一台 NAS 媒体库来说布局太多。按类型的实例全局。以后可以在 **Library** 根上挂 overlay，如果两块磁盘需要不同目录树。

## 后记：四个Store文件之外的队列与缓存（2026-10-12，10-13纠正职责）

本 ADR 的“四个文件”指 **Store 管理的生命周期分区**，其中catalog本身也是可重建cache，并非四者都是不可丢弃权威事实。实现还有两个文件；jobs保存执行／调度事实，fanart是缓存，不能统一叫“两个可弃派生库”：

| 文件 | 性质 | 可清空？ | 连接策略 |
|---|---|---|---|
| `jobs.db` | **Job** 定义与执行／重试记录（`job_defs`＋`jobs`），本身是运行状态事实 | 不可当缓存随意清空：会丢用户调度配置、在途工作、重试与历史 | WAL + 5000ms busy_timeout + `foreign_keys=ON`（`jobs::Queue::open`） |
| `fanart.db` | fanart.tv 图片元数据缓存，复用与 `catalog.db` **相同**的 `catalog_cache` 表结构 | **是**（纯缓存） | WAL + 5000ms busy_timeout + `foreign_keys=ON`（`media::CatalogCache::open_source`） |

两者不是 Media/Library/Subscribe 的第二份记录，但不能据此说删除 jobs.db “只丢历史”：还丢def中的用户维护参数／开关／周期和queued/running工作。fanart可重抓，jobs只允许按明确保护规则清理终态历史。连接策略由各 crate 内的 `apply_connection_policy` 显式设置，不依赖 bundled SQLite 的构建默认值；`journal_mode` 是文件头属性（`crates/*/tests/connection_policy.rs` 从外部连接断言），`busy_timeout`/`foreign_keys` 是连接级设置（各 crate 的单元测试在**自己打开的连接**上断言）。

跨文件仍然没有外键，也没有跨文件事务：这一点对这两个库同样成立。

## 后记：存储重设计已落地（2026-10-19，替换上面的四文件归属）

上面的四文件划分与 `ledger`/`file_meta`/浏览投影链已被
[存储重设计](../superpowers/specs/2026-10-18-library-storage-redesign.md)取代，实现见
`crates/store/src/schema/v1/`。当前事实：

| 文件 | 保存内容 | 可清空？ |
|---|---|---|
| `app.db` | User／token、Site、Downloader、Filter、Naming template、实例设置、搜索快照 | 否 |
| `library.db` | Media 身份与已采用元数据、Library／根、文件台账与覆盖、浏览摘要与倒排、Collection、Playback 单元／会话／历史／日志/指标、Probe 与 Scan 的持久工作 | 否（这就是馆藏索引） |
| `subscribe.db` | Subscribe 策略、逐槽事实、待下载队列 | 否 |
| `jobs.db` | Job 定义与执行／重试记录 | 否（按保护规则清理终态历史） |
| `cache.db` | 六来源外部响应缓存（每来源一张表） | 是 |

与本文原决策的三处实质差异：

1. **Media 与 Playback 从 app/subscribe 移入 library.db**：一个文件的发布要同时写文件台账、覆盖、浏览摘要与受影响用户摘要，这些必须在一个事务内完成，不能跨库。
2. **浏览投影链被删除**：`_read_revisions`、`_browse_changes`、`browse_change_cursors`、`browse_generations`、`browse_media`、`browse_files` 以及重建服务都不再存在。浏览读 `library_items`/`library_units`/`library_item_terms`，在写事实的同一事务内更新。
3. **`cache.db` 按来源分表**，`catalog.db`/`fanart.db` 不再单独存在；清缓存不影响任何本地事实。

仍然成立的约束：跨文件没有外键、没有跨文件事务；文件所属 root/Library/Media 在写入时确定并持久化，不在读路径按字符串或路径归属推断；`pending` 文件不是馆藏内容，只进管理／恢复视图。

没有旧数据导入器、兼容视图或双读双写。未知或较新的库布局拒绝打开（各文件 `application_id` + `user_version`），升级只做前向 schema 版本变更。
