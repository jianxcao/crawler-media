# 插件方案适用性与设计审查

日期：2026-11-03。范围仅文档；不宣称插件运行时、SDK、真实PT/CDP或性能已通过测试。

## 固定基线与结论

- 当前代码：`feat/storage-redesign` @ `52522549fbefece002d0354cb4434686939b4f4c`。
- 旧方案：`docs/plugin-design` @ `6836a4eb7f9ecd9aa50cc7c9a3449e6817082647`，共有六个独有文档提交。
- 新方案：[入口](../plugins/README.md)，工作分支 `codex/plugin-design-storage`。
- 旧方案不能直接执行。保留WASM/宿主能力/类型化Provider的方向，以现行存储、生命周期和用户补充需求重写，不按旧版结构补丁式续写。

原工作区已有 `docs/audits/2026-11-02-storage-redesign-review.md` 和 `docs/superpowers/plans/2026-11-02-storage-redesign-final-acceptance.md` 未提交修改。本工作在独立worktree进行，不包含这两项修改。

## 旧方案逐项处理

| 旧设计/假设 | 当前核对证据 | 处理 |
|---|---|---|
| plugin-store或独立DB待选 | ADR-0008、writers.rs、schema/v1 | 确定并入五文件；按配置/下载/文件/Job生命周期归属 |
| Store外围锁包业务、guest同步KV任意连接 | http/store_wait.rs、read_scope.rs | 统一池/单writer；snapshot→guest/IO→重验写；禁止嵌套writer |
| Subscribe insert后发created | subscribe_create.rs、subscribe_retention.rs、subscribe_job_sync.rs | 以持久creation阶段finalize为完成事实；跨库恢复不重复业务 |
| choose::is_complete为completed事件 | completion.rs::accept_completion | 仅当前事实/revision接受的active→completed转换发事件 |
| add.rs/submission/admit三处替换即统一 | delivery.rs、admit/delivery.rs、worker/finish_search.rs、management/subscribes/run/admission.rs | 枚举所有入口，保留RoutedDownloader、pending与精确归属 |
| add超时失败可删提交记录/稍后重试 | 外部Downloader不能参与SQLite事务 | Unknown持久、先对账；有副作用疑虑不释放配额 |
| 在旧Rename emit原位加可改路径Hook | subscribe/src/collect/transfer.rs先recorder.prepare | 所有路径建议在reserve前；之后沿原FileVersion发布 |
| fs_watcher即非订阅Transfer执行点 | fs_watcher调Job；watch_intake/claim独立执行 | 覆盖各实际入口与发布边界 |
| ADR-0012默认删文件进回收站 | ADR-0012保留媒体/sidecar字节 | 删除错误承诺；真实删文件走持久意图、身份/版本校验 |
| epoch是CPU预算，timeout后线程很快释放 | 同步IO不可被epoch中断 | epoch墙钟/fuel指令量；真实worker退出才释放资源 |
| 双MAJOR、旧Bus适配、旧add默认转调、KV migration | AGENTS零兼容规则 | 精确ABI/一次更新调用者；不兼容schema显式reset或拒绝 |
| 两个独立ArcSwap分别换profile/provider | 同一请求需要一致registry | 单一RegistrySnapshot，所有调用者固定观察代 |
| Admin确认代理主机即可防SSRF | 代理/Browser可能远程重新解析 | 明确真实连接/egress验证，不具能力拒绝第三方执行 |
| 明文site.get-credentials配合脱敏 | 变体编码无法通用证明去密 | 取消明文API；宿主槽位，包整体目的地限制，脱敏仅纵深保护 |
| cron已由Jobs支持 | jobs/src/kinds.rs只有Interval | 本版interval；签到窗口另以Site时区记录 |
| 刷流已具备实际执行能力 | http/sites/boost.rs、check_in.rs::boost_check | 重建为独立插件闭环；磁盘budget与uploaded_bytes分离 |
| Browser只用于维护 | 本次用户明确要求CDP搜索/签到 | ADR-0013提议扩展，三重opt-in与真实网络隔离前置验收 |
| 方案只需宿主概要 | 用户要求编写方案、全面API、界面 | 增加作者合同/示例、管理API/页面/异常状态、分阶段实施 |

## 自检已处理

1. 宿主/作者/界面共享Operation状态与终态定义；Unknown不自动重发，不直接标cancelled。
2. 幂等键统一为plugin+subject+key，command kind纳入请求hash；稳定cause防重入而不跳过安全Gate。
3. 配置更新仅停用并drain后PUT；enable才运行组件validate，不在同步HTTP请求执行guest。
4. Task更新用202 operation，app意图与Jobs投影分开；补齐blob和固定事件Notify发送接口。
5. 删除manifest拼接错误；内存/fuel/wall预算与示例字段一致；不把候选未知做种/HR当0或false。

## 独立审查

首轮独立审查只读核对新设计、作者/API、UI/API、计划和当前代码，发现5项Important、3项Minor、0项Critical。以下修正已写入方案及实施验收，并经第二位独立审查者读取修订稿复核关闭。

| 等级 | 问题 | 已写入的修正 |
|---|---|---|
| Important | 同包A Site认证响应经KV流到B Site调用 | security_scope贯穿持久状态、读取、operation与输出；标准候选释放边界明确，声明不能检测任意隐写，授权Provider包含认证内容信任 |
| Important | HTTP import触发网络Hook后又HTTP，可能递归或等待自身permit | transport子上下文深度1纯内联Store、借父执行permit、额外内存计额、禁网络/DB/KV/command，自动Browser子资源只走静态net-guard |
| Important | guest缺刷流权威容量查询和清理计划生成入口 | 增加boost.snapshot与downloader.prepare-cleanup，10分钟有效的持久版本计划，执行前再次检查 |
| Important | Check-in receipt含plugin_id，换插件可重复签到 | 唯一Site/action/window，plugin仅来源；切Provider/凭据/时区不清成功或Unknown |
| Important | 按不可信candidate_size预留后直接add | 宿主解析metainfo的verified_total_bytes，最终同事务预留后才add；unknown-size magnet不自动刷流 |
| Minor | 正常模式无法离线上传签名包，未接包外签名 | upload支持detached minisig，已信任签名包无需开发模式 |
| Minor | 至少一个WASM组件阻断YAML-only包 | components允许0，但必须有有效profile；空包拒绝 |
| Minor | UI示例ID不符合反向域名规则 | 使用org.example.checkin与org.example.brush-budget |

复核进一步补齐三个API闭环：独立trust-key导入/指纹确认/撤销；state导出/受鉴权短期下载/reset及升级target_config/state_policy；EndpointKey独立主体、capabilities/object_scope与签发者当前权限交集。相关实施切片增加对应失败用例。

最后校正升级JSON必填字段、endpoint声明capabilities、WIT错误枚举、CLI validate名称与先drain后切换顺序。审查者对修正后的实际文件逐项复读，最终结论：**设计静态复核通过，可按P0–P8进入实施；剩余Critical 0、Important 0、Minor 0。**

此结论不是运行时已实现/可发布证明；正式WIT/SDK、资源预算、Browser真实egress和崩溃恢复仍须按计划执行验收。两位审查者均只读，没有修改Git或运行build/test。

## 本轮验证边界

本轮只改Markdown，验证链接、结构、JSON/TOML样例语法、跨文档合同与代码落点，不运行Rust/前端全量测试。WIT/Rust片段是明确标注的未来示例；绑定生成、组件构建、真实Browser egress和业务行为测试是实施计划的验收条件，不借用历史r5/r6通过声明。

实际静态检查：8份Markdown、59处本地链接、7个JSON代码块均无错误；manifest经Python tomllib解析通过；Markdown围栏平衡且无行尾空白，`git diff --check`通过。

旧设计内容通过Git历史保留；新分支提交、推送和旧分支清理的实际结果以本次交付的Git核对为准。
