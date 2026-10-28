# 存储复审修复与验证记录

> 本页保留 `7e3bd653` 上轮修复范围和当时门禁。后续现场反馈及隔离复审确认共享锁、Probe 重建和另一条协议观看筛选仍有问题；当前结论与未实施方案见[共享锁复审](2026-10-28-storage-lock-review.md)。下文“无已确认阻塞项/可进入合并”只指当轮已发现范围，不作为本版最终合并结论。

日期：2026-10-28。分支 `feat/storage-redesign`，main 基线 `756444f7`；修复前审查固定于 `7030c1dd`。用户后续授权从 review 转为实施修复。本轮确认的 H1–H6 及复查发现的删除/重命名/拆出、执行边界问题均已修复。最终源码的 crate 独立门禁和 workspace 全量已通过，未留已确认的合并阻塞项。

## 已实现与定向证据

- H1：指纹模型及全部成员在同一 IMMEDIATE 事务校验 Media 冻结/退休、归属、源版本、发布/有效根；拒绝时旧模型完整保留。迟到 pipeline 不重定向到合并后的 Media。公开恢复/零写与正常发布回归通过。
- H3：Series 断言由多条件 OR 连接改为 whole/season/episode 三次精确 seek。5000 episodes 指定 scalar 的 SQLite VM 从约 4 亿降至约 45 万；100/1000/5000 为约 9220/90220/450222，完整 leased 协议 Limit=1 约 9659/90659/450657。统计前后及用户隔离/false/同时间优先级已覆盖。数字是固定夹具工作量，不是设备延迟承诺。
- H4：重复文件 GET 复用完整 coverage 捕获，批读 Media，按索引构组并每文件观察一次。实际 Router 100→10000 files SQL 为固定4语句，约4468→430000 VM；慢观察不持 Store。DELETE 依照文件/单元事实核对冗余与版本，先持久意图再清理。
- H5：旧 Filter block kind 转换已删，旧 POST/PATCH 整份400零写；新 HDR/title_match 正常往返。旧根合并直接写路由删除，Web 统一走当前 Library operation。
- H6：每文件共享 coverage，避免每个 unit 复制完整 pack；NextUp/Resume 使用索引化用户单元事实。5000单元指定完整请求分配量分别由约326/372MB降为108/146MB；多版本、ACL、播放状态与排序回归通过。这是累计分配量，不是 RSS。
- Subscribe wanted：待下载 Release 范围/覆盖预先索引，消除按每个响应 episode 重复解析全部 Torrent。指定100→1000规模累计分配约2.73→25.3MB；固定1000响应、5000 pending约91.9MB，覆盖 pack/range/先匹配语义。
- H2：同步 Store 阶段在 worker 捕获 owned 事实，文件/网络/密码散列在锁外执行；真实 worker 退出前保持预算。新增 Router 锁竞争/慢IO/饱和/等锁后门禁等回归。复查扩展覆盖 Library/Collection、配置、Site/Downloader、User、Subscribe 与手动操作等真实调用链。

## 删除、重命名与唯一结构

- 删除入队前锁外记录原生文件身份。待删路径冻结发布、换源、改名；清理重验 source/version 与持久 path_key/deletion_id，根别名退休也不丢失意图。同一路径 drainer 串行，迟到完成不能清掉后续新意图。
- 重命名附属文件入队失败返回错误并保留 references_applied 阶段，重试幂等；同步 rename 时钟统一秒，避免失败退避被误设到远未来。
- Transfer 两个提交入口以原始 fingerprint 或 recorder 本轮发布证据固定删除 pin，在锁外观察后同 guard 重验版本、Media/Library/root 归属与覆盖；四种换源/改挂交错先复现误删再通过。质量否决的新目标沿 recorder 的实际源证据 CAS 绑定后清理，未从当前路径临时猜测身份。封面发现也已移出 Store。
- extras 拆出事务拒绝 Pending Transfer 及仍持有 rename 意图的文件；全批 park/清理/摘要变更共同回滚。公开 Store 的正常、过期、故障和所有权五项回归通过。
- fingerprint 的 full-window/adaptive 整条协程由已有有界 worker 驱动；chapter shutdown 的恢复 SQL 移出执行器。两个 current-thread 锁竞争反例由约515/507ms转为通过，同时校验真实发布/恢复事实及额度持有。
- 五文件只有一份当前 DDL；library/subscribe 开发中的版本编号已收敛为当前标识1，未保留其历史结构。没有升级、导入、双读写或旧字段别名。
- Store 按唯一生产 DDL 核对表、索引、视图及表内约束；显式额外对象拒绝。诊断/故障注入 trigger 不属于此结构清单。
- 删除历史 cache 切换脚本、错误启动提示、subscribe历史 fixture 和部署步骤。当前启动只初始化空文件或重开相同定义。
- 实测 DDL：app9表48字段2索引；library53表596字段83索引；subscribe4表47字段3索引；cache6表30字段12索引；jobs4表29字段9索引。总计76表750字段，Store命名索引100，library FK组37。

写入成本门槛保持原值：待删保护的精确查询合并进既有根归属 SQL，消除每文件额外语句；`perf_baseline` 七项门禁通过，三个手动规模报告仍按原规则忽略。

## 最终门禁

最终源码已冻结；下表按实际命令退出和输出汇总计数（包含测试启动的子进程验证）。编译被其他切片 WIP 阻断的运行不记为行为 red/green。

| 命令 | 当前结果 |
|---|---|
| `cargo test -p store --no-fail-fast` | 退出0；776 passed / 0 failed / 11 ignored |
| `cargo test -p api --no-fail-fast` | 退出0；1897 passed / 0 failed / 9 ignored |
| `cargo test -p media-server` | 退出0；40 passed / 0 failed |
| `cargo test --workspace --no-fail-fast` | 退出0；3314 passed / 0 failed / 24 ignored |
| `pnpm test`（web） | 退出0；851 passed / 0 failed |
| `pnpm typecheck`、`pnpm build`（web） | 退出0；构建保留既有 bundle 大小提示 |
| `cargo check -p api` | 退出0；Store原有3条非功能性编译警告仍在 |
| 修改范围 `rustfmt --check`、`git diff --check` | 退出0；变更 Rust 文件≤800行、函数/测试≤120行 |
| 变更 Markdown 相对链接 | 151个文件链接，0断链 |

完整门禁日志保存在 `/Users/jianxiong.cao/Library/Caches/crawler-media-review-logs/`：`api-final-confirm.log`、`store-final-confirm.log`、`media-server-final.log`、`workspace-final.log`、`web-final-{test,typecheck,build}.log`；API/Store/workspace另保存命令退出码JSON。普通API编译3条Store警告为无公开项的glob re-export及两个仅测试使用的helper，不属于本轮运行失败。

第一轮完整 API 的图片冻结错误码回归已恢复既有 `library.deleting`；两条依赖旧文件/锁字符串的审计分别更新为精确只读入口白名单和公开 Router 并发行为回归。完整重跑后上述问题已通过。独立线程3秒watchdog确保锁回归不会令测试永久挂起。

独立复核覆盖共享 coverage、scope/ACL、唯一 schema、refresh/person/title-preview、删除 generation/别名和失败恢复、Transfer 删除pin；发现的真实缺口均先复现再修复。workspace 最终结果退出0；在下述未运行边界内，本轮修复可进入合并。

本轮未操作真实实例、部署或合并；NAS、真实 Jellyfin 客户端、100k Media/1M 文件完整设备容量未运行。保留这些边界，不将线性工作量测试当作所有接口/规模的耗时保证。
