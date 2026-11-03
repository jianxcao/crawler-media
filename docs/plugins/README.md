# 插件系统方案

本目录描述待实现的插件系统。当前代码仍执行 ADR-0002；本轮仅更新设计，不引入运行时。

基线：`feat/storage-redesign`，`52522549fbefece002d0354cb4434686939b4f4c`（2026-11-03）。
旧方案：`docs/plugin-design`，`6836a4eb7f9ecd9aa50cc7c9a3449e6817082647`。

阅读顺序：

1. [系统设计](../superpowers/specs/2026-11-03-plugin-system-design.md)：架构、存储、执行、业务接入与安全边界。
2. [插件编写与接口](authoring.md)：包结构、协议、SDK、插件示例、开发与发布。
3. [界面与管理 API](ui-and-api.md)：页面、用户流程、请求响应、权限与异常状态。
4. [实施计划](../superpowers/plans/2026-11-03-plugin-system-plan.md)：依赖顺序、文件落点与验收。
5. [审查记录](../audits/2026-11-03-plugin-design-review.md)：旧方案适用性及本轮独立 review。
6. [ADR-0013 提议](../adr/0013-wasm-plugin-runtime.md)：决策摘要；不表示功能已落地。

这组文档共同构成新方案。历史 r6 以 Git 提交保留，不再作为实施入口；不把历史样例编译结果视为当前验证结果。
