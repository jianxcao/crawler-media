# 文档入口

本目录说明当前实现、设计约束和可重复执行的验证方法。存储与数据访问、片头片尾识别、章节图像属于同一组性能优化；它们共享数据库、任务调度、远程读取与播放展示的约束。

## 设计与运行

| 文档 | 内容 |
|---|---|
| [术语](../CONTEXT.md)、[工程约束](../AGENTS.md) | 产品边界、代码导航、资源与协作规则 |
| [存储与数据性能设计](design/storage-performance.md) | 五库职责、读写隔离、查询投影、事务与内存边界 |
| [片头片尾与章节图像优化](design/marker-optimization.md) | 音频指纹、模型与缓存、远程读取预算、章节图像任务 |
| [数据库字典](database/README.md) | 表、字段、索引和数据关系；可执行 DDL 以源码为准 |
| [部署](deploy.md) | 运行环境、数据目录与服务启动 |
| [自有 API](api-contracts/self.md)、[Jellyfin API](api-contracts/jellyfin.md) | 鉴权、用户数据、播放与客户端协议 |
| [领域导航](agents/domain.md) | 各领域实现与测试入口 |

完整路由见 [HTTP router](../crates/api/src/http/mod.rs)、[路由表](../crates/api/src/http/routes/mod.rs) 和 `web/lib/api/*`。

## 测试与后续优化

| 文档 | 内容 |
|---|---|
| [E2E 操作手册](testing/e2e-runbook.md) | 独立数据库与媒体副本、启动、页面测试、恢复和清理 |
| [存储性能验证](testing/storage-performance.md) | 单元/集成门、查询工作量、容量、并发、多批内存与诊断方法 |
| [性能 SLO](testing/performance-slo.md) | 延迟门槛、可比输入、idle/busy/cold、结果判定 |
| [系统回归用例](testing/system-regression-cases.md) | 数据一致性、媒体库权限、任务生命周期、Web 与 VidHub |
| [Marker 验证](testing/marker-accuracy-runbook.md) | 真实媒体、盲验、准确性与资源预算、章节图像验收 |
| [TMDB 重试回归](testing/tmdb-retry-regressions.md) | 有界重试、取消、超时与错误传播 |

## 规则

- 用户要求 / CONTEXT / 接受的 ADR 定义产品边界；源码与公开行为证明现状。文档与代码冲突时以代码为准并修正文档。
- 设计与用例不代表已运行通过。每次验证应记录实际输入、测量结果及 Pass / Fail / Unverified / Not run，保留失败与未验证边界。
- 长期文档维护设计、合同和方法；具体测试日志与报告独立保存，凭据及 `docs/test-reports/`、`.e2e/` 不纳入提交。

## ADR

| 决策 | 用途 |
|---|---|
| [0001 Media identity](adr/0001-media-identity.md) | 内部身份、kind隔离和多来源别名 |
| [0002 Hooks](adr/0002-hooks-not-plugin-runtime.md) | 编译期第一方扩展，不引入第二运行时 |
| [0003 Site credentials](adr/0003-plaintext-site-credentials.md) | 明文持久凭证风险，不等于可日志输出/进Git |
| [0005 Jellyfin](adr/0005-jellyfin-compatible-playback.md) | 协议表面与真实客户端验收边界 |
| [0006 Indexer](adr/0006-indexer-in-first-slice.md) | 架构取舍，不是外网验收结果 |
| [0007 Browser](adr/0007-cdp-browser-opt-in.md) | CDP开关/隔离/受管Obscura/回退与信任边界 |
| [0008 SQLite](adr/0008-sqlite-system-of-record.md) | 五库事实来源与持久化边界 |
| [0009 Job](adr/0009-job-queue.md) | Interval/queued-running约束、状态事实与恢复 |
| [0010 Password](adr/0010-password-hashing.md) | Argon2id 密码存储与登录验证 |
| [0011 Probe retry](adr/0011-probe-stage-retry.md) | 持久阶段/退避，不以NFO代替Probe成功 |
| [0012 Library removal](adr/0012-library-removal.md) | 删库即取消该库工作；清除无其他库引用的孤儿 Media；磁盘字节保留 |
