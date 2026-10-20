# 内部 Media id，外部目录 id 作为别名

**Media** 记录使用系统分配的 UUID。TMDB、Douban、TVDB、Bangumi、AniList 的 id 是外部别名，不是主键；Subscribe 与 Library 文件引用内部 MediaId，NFO 可输出外部别名，HTTP 响应缓存不要求以内部分配的 id 为键。当前权威身份在 `library.db.media`，别名在 `media_aliases`，键为 `(source, kind, external_id)`，同来源可以有多个别名、至多一个 refresh target（`crates/store/src/schema/v1/library.sql`）。原先五个可空 `*_id` 字段只是现有 `domain::Media` 的单值投影，不是新表的完整身份模型。

v1 需要五种 **Metadata source**，包括没有 TMDB id 的作品。把 TMDB id 当作主键会禁止这类 Subscribe。以 `(source, id)` 作为主键则会让路径与 subscribe 行在识别选择了不同来源时漂移。内部 id 只多一张表，外加两个别名后来被证明是同一作品时的合并路径。

**备选方案**：仅用 TMDB 主键；复合 `(source, id)`。

## 当前落地边界（2026-10-20）

此 ADR 保留身份决策，不证明合并/采用元数据/所有消费者切换已经验收。Store 有持久合并阶段代码（`crates/store/src/playback/merge.rs`），但管理 Media merge 路由在上一轮隔离复核仍为 404；`media.rs` 的单值写入路径还会删除重写别名，不能保证保留已合并的多别名。当前修复与验收按 [存储执行计划](<../superpowers/plans/2026-10-20-storage-clean-cutover.md>)，不恢复旧模型兼容层。
