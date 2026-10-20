# SQLite 是系统记录，按生命周期拆分

> 本文保留原拆库决策及后续历史记录；第“四个文件”至2026-10-12后记中的文件、表名与迁移行为均是当时方案/实现，不是当前操作指南。当前布局与尚未完成的生产切换见文末2026-10-19后记及[数据库文档](../database/README.md)。

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

此节记录当时拆库方向；这里列出的旧 schema 文件与 `ledger.root_id` 提案不再是当前 DDL。现行结构见 [Store 布局入口](../../crates/store/src/schema/layout.rs) 与 [数据库字典](../database/README.md)；创建新表不等于完成所有运行时调用方切换。

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

## 后记：新布局已建立，干净切换已收口（2026-10-20）

本节替代上面的历史文件归属。`feat/storage-redesign` 的生产 DDL 位于
[`crates/store/src/schema/v1/`](../../crates/store/src/schema/v1/)，目标合同见
[存储重设计](../superpowers/specs/2026-10-18-library-storage-redesign.md)。

| 文件 | 当前布局职责 | 可清空？ |
|---|---|---|
| `app.db` | User／token、Site、Downloader、Filter、Naming template、实例设置、搜索快照 | 否 |
| `library.db` | Media 身份与采用元数据、Library／根、文件与覆盖、浏览摘要与倒排、Collection、Playback、Probe 与 Scan 持久工作 | 否 |
| `subscribe.db` | Subscribe 策略、逐槽事实、待下载队列 | 否 |
| `jobs.db` | Job 定义、执行／重试及保留游标 | 否（终态历史按保护规则清理） |
| `cache.db` | 六来源外部响应缓存（每来源一表） | 是（受控清理，不 unlink 活跃 WAL 文件） |

Media 与 Playback 已迁入 `library.db`；旧浏览投影表/重建链已移除；`catalog.db`/`fanart.db` 由按来源分表的 `cache.db` 取代。Store 布局入口校验 `application_id` 与 `user_version`，没有旧数据库导入器或兼容视图；跨文件仍没有外键或原子事务。

2026-10-20 的 R1–R8 修复（`2f59103`…`6d35343`）关闭了此前的阻塞项：

- 电影旧 whole `(0,0)` 的复制、回读、断言归约与兼容测试已删除；movie/video 一律 `(-1,-1)`，TV S00E00 仍是真实分集。
- Transfer 收尾直接使用完整 `FileCoverage`，多集覆盖不再被单集形状截断；pending 重试复用已有身份。
- 生产心跳/会话改用会话绑定的物理源；自动续播优先仍可用的原源，换源返回 `source_changed` 且起点归零。
- 文件增删在同一事务重建受影响用户摘要；观看分面、详情续页、`content_rating` 已贯通。
- Jellyfin 物理多版本折叠为同一逻辑条目；genre/studio/search/person 过滤读取采用元数据，NFO 仅作无采用行时的兜底。
- 采用元数据按固定来源优先级合并；无效/未知 NFO 保留最后有效输入。
- 多覆盖文件与 `Unidentified` 不再阻断 Library 删除；新增嵌套根会在同一事务重归属文件并重建两侧摘要。

本机稳定树 `cargo test --workspace` 与前端 `typecheck` 已通过，记录见[证据报告](../benchmarks/2026-10-18-library-storage.md)第 1.2 节。**仍未完成**：真实 NAS 冷热缓存、并发扫描+心跳的 p50/p95/锁等待、100k Media / 1M 文件规模基准，以及真实 Jellyfin 客户端与浏览器播放验收。这些不因单元/集成测试通过而自动满足。

当前数据库字段、键与索引以[数据库文档](../database/README.md)为准。后续性能工作按[切换修复计划](../superpowers/plans/2026-10-19-storage-cutover-remediation.md)的 O1–O3 执行，不恢复双读双写或旧模型兼容层。
