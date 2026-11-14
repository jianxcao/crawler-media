# 内部 Media id，外部目录 id 作为别名

**Media** 记录使用系统分配的 UUID。TMDB、Douban、TVDB、Bangumi、AniList 的 id 是外部别名，不是主键；Subscribe 与 Library 文件引用内部 MediaId，NFO 可输出外部别名，HTTP 响应缓存不要求以内部分配的 id 为键。权威身份在 `library.db.media`，别名在 `media_aliases`，键为 `(source, kind, external_id)`，同来源可以有多个别名、至多一个 refresh target（`crates/store/src/schema/v1/library.sql`）。`domain::Media` 的可空 `*_id` 字段是单值读取投影，完整身份关系以别名表为准。

系统支持五种 **Metadata source**，包括没有 TMDB id 的作品。把 TMDB id 当作主键会禁止这类 Subscribe。以 `(source, id)` 作为主键则会让路径与 subscribe 行在识别选择了不同来源时漂移。内部 id 让引用保持稳定；确认多个别名属于同一作品后，可通过显式 Media 合并统一身份。

**备选方案**：仅用 TMDB 主键；复合 `(source, id)`。

## 身份变更边界

身份编辑保留多别名；显式文件改挂保留原始 Playback 事实，并按新目标重新校验来源绑定。Media 合并使用持久计划、冻结屏障与分阶段恢复，拒绝别名和人工锁冲突。跨库 Subscribe 引用通过持久意图恢复，不宣称跨库原子提交。数据归属及删除边界见 [存储设计](../design/storage-performance.md) 和 [ADR-0012](0012-library-removal.md)。
