# 外部来源缓存拆表旧实施计划：已取代，不可执行

状态：**Superseded，未实施。** 用户进一步批准将本地Media元数据与Library读写模型一起重新设计，允许全部舍弃旧数据库数据，但要求实施后完整功能可运行。

当前依据：

- [Media / Library 存储重设计首版](../specs/2026-10-18-library-storage-redesign.md)
- [响应缓存修订设计](../specs/2026-10-18-source-separated-cache.md)

本文件原有逐任务实施步骤已撤销，避免执行者继续创建已废止的机制。历史内容保留在Git提交 `9b3aefd`，不需要复制成另一份历史文档。

## 已撤销的决定

- cache.db共享一套 `_read_revisions` / `_browse_changes`、浏览消费游标及失效触发器。
- 保留原先跨app/library/catalog的整库浏览投影重建与发布机制。
- 缓存重建时重置Library消费游标、标脏整库投影。
- 将外部响应缓存中的最新详情直接当作本地浏览元数据。
- 仅替换物理缓存表而不改Media、Library和Playback相关的读写调用链。

## 继续保留的约束

- cache.db按六来源拆表并共用实现；TTL、stale-if-error、语言/参数隔离与管理接口正常可用。
- 旧数据可以全部舍弃；不迁移、不兼容、不双写，不在每次正常启动清库。
- 只清理明确SQLite文件/WAL/SHM，不删除媒体、NFO、图片、下载目录或Indexer配置。
- 大表尽量单表查询，必要关联必须有索引/范围与实测证据，不用全量内存过滤代替JOIN。
- 全部现有扫描、Transfer、Scrape、Subscribe、浏览、Playback、Jellyfin、权限和删除安全行为必须迁完；新表存在或单项测试通过不代表可交付。

## 下一阶段入口

先审阅新的整体设计，再为该设计编写完整实施计划（新DDL、Store事务接口、调用方清单、行为测试、规模基准、真实空库全链路烟测）。当前只交付设计；没有执行代码修改、数据库删除或新schema发布。
