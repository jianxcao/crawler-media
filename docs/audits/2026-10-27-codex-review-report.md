# Codex 原生 CLI 独立审查报告

- **基准**: `review-base` (`f2d4f033`) 到 `feat/storage-redesign` 提交线。
- **审查模型**: `gpt-5.6-luna` (通过本地 Codex CLI `/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex` 独立运行)。
- **审查重点**: 对照 `docs/superpowers/plans/2026-10-26-codex-review-brief.md`（ACL/所有权正确性、布局完整性、死锁与并发饥饿、热点查询表现、测试完整性与规模、文件与函数大小硬限制、可观测性规范、公共静态图片路由）。

---

## 审查结论汇总

对 600+ 个文件的完整 diff 进行深度符号追踪和 SQL 语义审查，未发现以下阻塞项：
- 无未经显式 `library_id` 的已发布文件路径推断回退；
- 无跨 tokio worker 的 parking_lot 锁阻塞或死锁隐患；
- 无游标分页漏查、重查或索引扫描降级为全表扫描现象；
- 无未授权的公网图片静态路由鉴权破坏；
- 无任何文件超过 800 行硬上限。

### 发现的唯一架构级缺陷及修复闭环：

- **[P1] 保留成功完成的媒体库删除根记录（Keep removal roots when pruning succeeded library removals）**
  - **位置**: `crates/store/src/operations_retention.rs`
  - **问题分析**:
    原 Task 13 计划将 `library_removals` 及其子表 `library_removal_roots` 列入了操作日志保留期清理列表（默认 30 天后删除）。
    然而，根据 `crates/store/src/library_deletion/mod.rs:5-7` 的核心契约，`library_removal_roots` 是防范已删媒体库目录被重新意外识别、扫描或自动绑定的**永久安全防线（Permanent boundary）**。
    一旦在 30 天后将其物理删除，若物理磁盘上该目录依然存在且发生变动，watcher 或 auto_resolve 会将已删目录重新识别并入库，导致已删除路径死灰复燃。
  - **修复措施**:
    已在 commit `f71bfe36` 完成修复：
    高频操作日志清理（`JOURNALS`）明确排除了 `library_removals`。已被成功删除的媒体库及其根路径作为不可破坏的永久安全墓碑保留，彻底封闭了删库 30 天后目录重新泄露进入库流程的安全漏洞。相关单元测试与 API 契约文档均已同步更新并通过。
