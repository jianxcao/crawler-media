# 内部 Media id，外部目录 id 作为别名

**Media** 记录使用系统分配的 UUID。TMDB、Douban、TVDB、Bangumi、AniList 的 id 是外部别名，不是主键；Subscribe 与 Library 文件引用内部 MediaId，NFO 可输出外部别名，HTTP 响应缓存不要求以内部分配的 id 为键。当前权威身份在 `library.db.media`，别名在 `media_aliases`，键为 `(source, kind, external_id)`，同来源可以有多个别名、至多一个 refresh target（`crates/store/src/schema/v1/library.sql`）。原先五个可空 `*_id` 字段只是现有 `domain::Media` 的单值投影，不是新表的完整身份模型。

v1 需要五种 **Metadata source**，包括没有 TMDB id 的作品。把 TMDB id 当作主键会禁止这类 Subscribe。以 `(source, id)` 作为主键则会让路径与 subscribe 行在识别选择了不同来源时漂移。内部 id 只多一张表，外加两个别名后来被证明是同一作品时的合并路径。

**备选方案**：仅用 TMDB 主键；复合 `(source, id)`。

## 当前落地边界（2026-10-20）

此ADR保留身份决策；身份编辑、显式文件改挂、持久merge及管理路由均已有实现。2026-10-22复审不再报告“编辑删除别名”，但确认非刷新目标别名扫描失败、同来源新快照被丢及merge失败恢复缺口，见[复审R21/R22](../audits/2026-10-22-storage-repair-review.md)和[当前技术方案Task10](../superpowers/plans/2026-10-22-storage-repair-remediation.md#task-10-合并快照决议冲突与恢复别名识别r21r22)。既有测试不足以关闭来源冲突矩阵，不恢复旧模型兼容层。
