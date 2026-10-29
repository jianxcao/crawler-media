# 文档入口

更新：2026-10-29，分支 `feat/storage-redesign`。旧计划、已被取代的审计/复审与原始测量产物已于 2026-10-29 删除（避免误导），需要时用 `git show 5e44f101:docs/<路径>` 取回。

## 当前先读

1. 术语：[CONTEXT](../CONTEXT.md)；工程约束与常量：[AGENTS](../AGENTS.md)。
2. **当前审查与修复状态：** [共享锁复审](audits/2026-10-28-storage-lock-review.md)（修复状态表）、[锁与工作量测量及复现源码](benchmarks/2026-10-28-storage-lock-work.md)。
3. **存储设计：** [存储设计 Spec](superpowers/specs/2026-10-18-library-storage-redesign.md)；[五文件数据库字典](database/README.md)。唯一可执行 DDL 在生产源码。
4. **结构/性能证据（被源码注释引用）：** [结构审计](audits/2026-10-26-library-schema-audit.md)、[性能基线](audits/2026-10-26-perf-baseline.md)、短复现 [endpoint](audits/2026-10-12-remediation-review-repro/endpoint-perf.md) / [Jobs](audits/2026-10-12-remediation-review-repro/jobs-query-plans.md) / [Media lookup](audits/2026-10-12-remediation-review-repro/media-lookup-plans.md)。
5. **运行/API：** [deploy](deploy.md)、[自有 API 约定](api-contracts/self.md)；完整路由以 [HTTP router](../crates/api/src/http/mod.rs)、[路由表](../crates/api/src/http/routes/mod.rs) 和 `web/lib/api/*` 为准。
6. **测试：** [e2e runbook](testing/e2e-runbook.md)、[system regression](testing/system-regression-cases.md)（场景清单，不是已运行证明）。
7. Agent 领域导航：[domain](agents/domain.md)。

## 规则

- 用户要求 / CONTEXT / 接受的 ADR 定义产品边界；源码与公开行为证明现状。文档与代码冲突时以代码为准并修正文档。
- 区分目标、已有代码、运行观察与历史记录；源码存在不等于行为通过。
- 完成的计划和被取代的审查直接删除，不在 docs 长期保留；历史从 Git 取回。

## ADR

| 决策 | 用途 |
|---|---|
| [0001 Media identity](adr/0001-media-identity.md) | 内部身份、kind隔离和多来源别名 |
| [0002 Hooks](adr/0002-hooks-not-plugin-runtime.md) | 编译期第一方扩展，不引入第二运行时 |
| [0003 Site credentials](adr/0003-plaintext-site-credentials.md) | 明文持久凭证风险，不等于可日志输出/进Git |
| [0005 Jellyfin](adr/0005-jellyfin-compatible-playback.md) | 协议表面与真实客户端验收边界 |
| [0006 Indexer](adr/0006-indexer-in-first-slice.md) | 架构取舍，不是外网验收结果 |
| [0007 Browser](adr/0007-cdp-browser-opt-in.md) | CDP开关/隔离/受管Obscura/回退与信任边界 |
| [0008 SQLite](adr/0008-sqlite-system-of-record.md) | 历史拆库与现行五文件，未完成生产切换 |
| [0009 Job](adr/0009-job-queue.md) | Interval/queued-running约束、状态事实与恢复 |
| [0010 Password](adr/0010-password-hashing.md) | Argon2id；旧明文fallback已移除，历史兼容描述不作当前合同 |
| [0011 Probe retry](adr/0011-probe-stage-retry.md) | 持久阶段/退避，不以NFO代替Probe成功 |
| [0012 Library removal](adr/0012-library-removal.md) | 删库即取消该库工作；清除无其他库引用的孤儿 Media；磁盘字节保留 |
