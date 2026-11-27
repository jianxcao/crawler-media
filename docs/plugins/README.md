# 插件系统方案

本目录描述插件系统的完整设计与分阶段实施合同。当前分支 `plugin_system_development` 已交付 Phase 1 插件平台、运行时、宿主业务接入、React Module Federation UI 与开发者 CLI；Phase 2（`media-provider`、`downloader-plugin`、`notification-plugin`）继续保持关闭（路由返回 404、manifest 拒绝）。实际能力、边界和验证范围见[实施状态](implementation-status.md)。

基线：`feat/storage-redesign`，`52522549fbefece002d0354cb4434686939b4f4c`（2026-11-03）。
旧方案：`docs/plugin-design`，`6836a4eb7f9ecd9aa50cc7c9a3449e6817082647`。

阅读顺序：

1. [系统设计](../superpowers/specs/2026-11-03-plugin-system-design.md)：架构、存储、执行、业务接入与安全边界。
2. [插件编写与接口](authoring.md)：包结构、协议、SDK、插件示例、开发与发布。
3. [界面与管理 API](ui-and-api.md)：页面、用户流程、请求响应、权限与异常状态。
4. [React 插件 UI 与宿主 SDK](frontend.md)：独立前端编译、模块联邦、四类页面、静态文件鉴权、信任与升级。
5. [实施计划](../superpowers/plans/2026-11-03-plugin-system-plan.md)：依赖顺序、文件落点与验收；P6.1–P6.6专门实现React插件UI。
6. [审查记录](../audits/2026-11-03-plugin-design-review.md)：旧方案适用性及本轮独立 review。
7. [ADR-0013](../adr/0013-wasm-plugin-runtime.md)：架构决策摘要（Phase 1 已落地范围见[实施状态](implementation-status.md)）。

这组文档共同构成新方案。历史 r6 以 Git 提交保留，不再作为实施入口；不把历史样例编译结果视为当前验证结果。
