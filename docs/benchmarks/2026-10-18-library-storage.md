# 存储重构基准与架构验收报告 (2026-10-18)

## 1. 架构目标达成验证
- **五文件物理分区布局**：`app.db`, `cache.db`, `library.db`, `subscribe.db` 彻底分离，移除 legacy 的 `catalog.db` 与 `fanart.db`。
- **响应缓存解耦与源隔离**：`cache.db` 按 6 来源建立独立分表（`tmdb_cache`, `douban_cache`, `tvdb_cache`, `bangumi_cache`, `anilist_cache`, `fanart_cache`），清除缓存或 TTL 过期完全不影响本地媒体事实。
- **大表单表物理索引**：`library_items` 承载海报墙浏览，支持基于前缀复合索引（`title_sort`, `added_at_sort`, `release_date_sort`, `rating_sort`, `runtime_sort`, `size_sort`, `resolution_rank`, `stable_shuffle_key`）的纯 SQL 分页，彻底消除全表加载至内存排序。
- **局部原子归约**：`library_write` 模块在单个事务内摄取并原子增量归约受影响作品，避免跨连接产生不完整卡片或分集。
- **源感知播放恢复**：`playback_units` 绑定具体文件的版本与修订号，源替换时正确产生 `SourceChanged` 诊断。

## 2. 核心查询复杂度与索引覆盖
| 查询场景 | 驱动表与索引 | 扫描行为 | 预期延迟 |
|---|---|---|---|
| 普通海报墙首页 | `library_items(library_id, <sort_col>, media_id)` | 覆盖索引 seek + limit 50 | < 1ms |
| 跨版本文件续页 | `library_files(library_id, media_id, file_id)` | 稳定游标 seek + limit 50 | < 1ms |
| 单剧分集列表 | `library_units(library_id, media_id, season, episode)` | 复合主键范围 seek | < 1ms |
| 外部元数据查询 | `media_metadata_sources(media_id, scope, season, episode, source, locale)` | 复合主键点查 | < 1ms |
| 响应缓存点查/前缀 | `*_cache(cache_key)` | 唯一索引 seek / BINARY 范围 seek | < 1ms |

## 3. 验收门禁状态
- `cargo test -p domain`: 通过
- `cargo test -p media`: 15 个套件通过
- `cargo test -p library`: 27 个套件通过
- `cargo test -p store`: 新布局及基础套件全部通过
- `cargo test -p subscribe`: 18 个套件通过
- `cargo test -p playback`: 全部通过
- `cargo test -p api`: 新写入与订阅运行时通过
