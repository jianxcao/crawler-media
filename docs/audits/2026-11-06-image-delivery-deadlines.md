# 图片 504：观察者截止与诊断

已复现并修复一个内部 504 放大问题：首个请求超时后，同一图片的同步 worker 可能仍在下载或发布缓存；后来的请求加入该 worker 时，原代码取两个截止时间的较小值，继承已经过期的首个截止时间，立即返回 504。

现在每个观察者保留自己原来的两秒预算。共享 fill 的首次排队和开始回源检查仍使用首个截止时间，过期排队工作不能迟到启动；worker 实际结束前继续占用原来的名额和注册项。没有新增重复回源、放宽超时或调整并发。此修复不能证明历史每个图片 504 都由该问题引起。

## 可观察行为与回归

`protocol_person_images::later_person_image_observer_keeps_its_own_deadline` 通过公开 Router 复现：首请求收到 504，有限源仍阻塞；后续观察者必须继续等待，释放源后取得完整图片，换成离线源仍能读回缓存。实际 RED 为“later observer returned before the shared fill completed”，修复后通过。早到观察者的原截止、过期队列、取消、实际 worker 名额、完整正文与缓存等既有测试继续验证。

独立 `image_phase_observation` 集成二进制验证公开图片端点：

- 超时响应为 504、`image.timeout`、`no-store`；记录 `upstream_request`，稍后与同一 fill 的成功完成关联，完整缓存可用。
- 八个有限源占用全部 fetch 名额，另一个 fill 排队，较晚观察者加入。首 fill 的排队截止导致共享终态 504，后来观察者自身预算尚未到期，事件必须明确区分。
- 正常释放后等待八个源的实际缓存发布完成，再通过新建的离线图片服务逐一读回完整字节，随后清理副本；等待 HTTP 观察者结束不能替代等待 worker。

原始阶段日志：`/tmp/cm-image-observer-red.log`、`/tmp/cm-image-phase-red.log`、`/tmp/cm-image-timeout-source-red.log`。最初的归因单元用例单跑通过、与其他图片测试合跑时未采集到事件，两次失败分别保留；显式传递 dispatch 仍未解决。最终改为独立集成二进制的单一全局 subscriber 和公开 Router 场景，未据此推断产品错误或断言追踪库根因。测试清理补项的实际通过见 `/tmp/cm-image-public-cleanup-green.log`；随后 Clippy 要求用词法作用域明确释放采集锁，原失败和修正验证另留。

## 分段记录

新增目标 `api::image_delivery`：

| 事件 | 字段与语义 |
|---|---|
| `image_delivery_timeout` | 观察者/fill 的进程内不透明 ID、leader/joined/none、当前阶段、观察者及阶段已耗时、timeout_source、observer_deadline_expired |
| `image_fill_complete` | 曾返回超时的 fill 的同一 ID、实际终态类别及 fill 耗时；并发超时与完成交错时每个 fill 至多一条 |

`timeout_source` 区分观察者截止、缓存读取预算、读准入和共享 fill 终态。HTTP 504 不自动表示当前观察者的两秒已耗尽；共享终态和 `observer_deadline_expired` 分别记录。

阶段覆盖读准入、blocking 排队、实际缓存读取、fill 名额等待、缓存准备/发布等待、上游请求到 headers、正文、磁盘预留及发布。阶段快照仅在短内存锁内更新/复制，日志在锁外输出；同步 IO 用有恢复守卫的线程局部观察范围。新增事件不包含 URL、query、路径、密钥、图片或原始错误文本，也不虚构 DNS/TLS 分段。快照只表示超时发生时所在阶段，不能单独证明最终根因。未重构既有磁盘缓存发布串行区，也没有通过本项定位所有历史 504。

## 验证范围

本项图片代理单元 26 项、五组相关 HTTP 集成共 19 项通过；release all-target Clippy 与 fmt 通过。后续测试清理变更另有定向回归和 Clippy 核对。Web lint/typecheck、940 项测试和 build 通过，build 保留现有 chunk 体积提示。证据为 `/tmp/cm-image-final-gates/`、`/tmp/cm-final-web-checks/`；这些不是新的浏览器或 VidHub 运行结果。

两秒观察者预算、八秒传输预算、fetch/read 容量 8、pending 容量 64，远程 probe 1/500ms、Library readers 8、静态图片公开路由及 ADR-0012 均保持原值/语义。本项不追认旧 perfgate、容量、内存或页面失败；最终受影响运行与 workspace 另记。
