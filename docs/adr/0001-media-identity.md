# 内部 Media id，外部目录 id 作为别名

**Media** 记录使用系统分配的 UUID。TMDB、Douban、TVDB、Bangumi、AniList 的 id 是外部别名，不是主键；Subscribe 与 Library 文件引用内部 MediaId，NFO 可输出外部别名，HTTP 响应缓存不要求以内部分配的 id 为键。当前权威身份在 `library.db.media`，别名在 `media_aliases`，键为 `(source, kind, external_id)`，同来源可以有多个别名、至多一个 refresh target（`crates/store/src/schema/v1/library.sql`）。原先五个可空 `*_id` 字段只是现有 `domain::Media` 的单值投影，不是新表的完整身份模型。

v1 需要五种 **Metadata source**，包括没有 TMDB id 的作品。把 TMDB id 当作主键会禁止这类 Subscribe。以 `(source, id)` 作为主键则会让路径与 subscribe 行在识别选择了不同来源时漂移。内部 id 只多一张表，外加两个别名后来被证明是同一作品时的合并路径。

**备选方案**：仅用 TMDB 主键；复合 `(source, id)`。

## 当前落地边界（2026-10-23，8fbf81b）

身份编辑保留多别名、显式文件改挂保留原始Playback事实、元数据写屏障、强类型merge冻结计划与管理/恢复入口均已落地。本轮不重复把旧R21/R22状态表作为当前缺陷；历史合并反例仍见[固定版本审查](../audits/2026-10-22-storage-repair-review.md)。当前未完项见[再次复审](../audits/2026-10-23-storage-followup-review.md)及[技术方案](../superpowers/plans/2026-10-23-storage-review-remediation.md)，不添加旧模型兼容层。
