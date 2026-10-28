# Storage 复审修复任务对账

本页取代原逐步待实施清单。原计划与当时只读授权见 `e212a4a9`；用户随后明确授权实施所有问题，并要求未合并的重构只保留一份最终结构，不维护旧布局。

目标：关闭 [7030c1dd 基线审查](../../audits/2026-10-28-storage-head-review.md) H1–H6。五文件边界和同库权威事务保持；不新增旧输入、路由转发、迁移或双读写。当前结果与最终门禁以[修复记录](../../audits/2026-10-28-storage-remediation-verification.md)为准。

| 项目 | 实现 | 公开行为验证 |
|---|---|---|
| H1 模型发布 | IMMEDIATE 事务拒绝 merging/retired、失效归属/成员/源版本，API 丢弃迟到 pipeline | Store 失败恢复、更新失败零写、正常发布；API 计算后改挂 |
| H2 执行边界 | owned SQL 快照移出 Tokio，文件/网络观察离开 Store；写拿锁后重验门禁 | Router current-thread 锁竞争、慢 IO、预算饱和、等待后关闸、取消/真实 worker 占用 |
| H3 Series 查询 | whole/season/episode 三次精确主键 seek，保留 false/时间与坐标优先级 | 100/1000/5000 episodes，统计前后 scalar 和完整 leased Limit=1 |
| H4 重复文件 | 一次 SQL capture、Media 批读、每文件一次观察；DELETE 固定版本重验和持久删除 | hardlink/未知覆盖/不同 unit、慢观察离锁、真实 Router SQL 工作量 |
| H5 旧 Filter | 删除旧 block kind 到新类型的转换，只收当前 atom | POST/PATCH 旧 kind 400 零写；当前 HDR/title_match 往返 |
| H6 多单元候选 | 每文件一份 Arc<FileCoverage>；NextUp/Resume 索引用户单元事实 | 多版本/ACL/进度/排序，100/1000/5000 单元完整请求分配量 |

H2 调用链扩展到配置、Site/Downloader、User/密码、Subscribe、手动 Admit/Claim/Probe、Library 图像/章节与元数据刷新、预览和人物图像；每个确证遗漏均补对应 seam 回归，不以单纯 grep 无命中为安全证明。

补充正确性：删除按持久 path_key + deletion_id 闭环，锁外观察身份、冻结待删路径、重试核对 generation；改名 sidecar 入队失败保留 references_applied 意图。extras 拆出使用一事务校验/写入/删除，不静默接受半成功。手动远端提交保留可跟踪意图。

当前结构只来自生产 DDL；Store 对表/索引/视图完整定义预检，不维护历史版本。运行时诊断触发器不纳入结构清单。根合并前端改走现行 Library PATCH operation + poll，旧直接写路由删除。

最终验收已完成：store/api/media-server 独立测试及最后 workspace 全量均退出0；Web 851 tests、typecheck/build、变更 Rust 文件/函数硬限与格式、独立复审、文档同步均通过。workspace 汇总3314 passed / 0 failed / 24 ignored；完整证据与未测设备/容量边界见修复记录。按工程约束在当前分支提交；未执行部署、真实实例重建或合并。
