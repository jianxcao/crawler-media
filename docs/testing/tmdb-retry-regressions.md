# TMDB 请求重试与回归

处理短暂连接故障、正文断流和上游服务错误。重试策略与代理根因是不同问题；图片代理 504 仍须沿内部排队、上游和响应交付独立定位。

## 请求边界

- 一个逻辑 GET 最多尝试三次，IPv4 → IPv6 → IPv4 共用此上限；不再套用 `http_agent::call` 的额外双栈重试。
- 上游预算保持六秒，包含缓存刷新等待、上游名额等待、请求、完整正文及退避；同时遵守更早的调用方截止时间。TMDB 缓存入口和直接 HTTP 入口都建立预算，后台调用也有界。
- 连接失败、DNS 暂时失败、超时、连接重置、意外 EOF，以及 HTTP 500/502/503/504 可以重试。HTTP 429 支持 `Retry-After` 秒数和 HTTP-date；剩余预算不足时直接保留失败，不提前重发。
- 退避为 100/200ms 加 0–50ms 随机抖动；等待期间检查取消。HTTP 401/403/404、其他永久状态、重定向、证书错误、无效 UTF-8 和正文大小超限不重试。正文仍限制为 10MiB。
- HTTP 404 保持 typed `NotFound` 及原七天负缓存语义。HTTP 200 的 JSON/业务合同解析失败由缓存层处理，不因重试而接受错误正文。

## 调用方、共享刷新与缓存

`BlockingBudget::run` 和 `Reservation::run` 将调用方截止与取消传入同步 catalog 工作。调用方退出后不再开始下一次尝试；同步 ureq 无法立刻中断已运行的网络调用，当前尝试受剩余全局超时约束，真实 worker 结束前继续持有其执行名额。`run_to_completion` 的副作用等待语义保持原样。

调用方自己的 `Deadline`/`Cancelled` 与共享上游的 `OriginTimeout` 分开。前者不会取消独立等待者；等待者可在旧 leader 结束后使用自己的剩余预算重新选举。普通上游超时作为共享失败交付，避免等待者轮流重新访问故障上游。

成功正文先完成读取与合同解析，再更新缓存。普通源失败、上游等待超时或刷新准入耗尽可使用有效 stale 数据；调用方取消和截止直接结束。缓存发布失败保留原 backend 错误与旧数据，不能用 stale 成功掩盖。恢复后可正常刷新。

API key 与代理配置在请求开始读取，等待和网络期间不持有数据库连接或锁；日志不包含带 key 的请求 URL。继续使用已有八个 origin 名额和直接连接池，不增加远程 probe 并发。

## 回归位置

- `crates/api/src/tmdb_http/tests.rs`：三次上限、双栈、永久错误、完整正文、抖动、限流及总预算。
- `crates/api/src/tmdb_http/tests/cancellation.rs`：请求取消、退避取消及两条 blocking 执行入口。
- `crates/api/src/tmdb_http/tests/cache.rs`：实际 HTTP 预算耗尽后的共享刷新结果。
- `crates/api/src/tmdb_http/tests/wire.rs`：本机模拟 HTTP 的重定向、真实截断正文和正文超时。
- `crates/media/tests/catalog_request_budget.rs`、`catalog_cancelled_leader.rs`、`catalog_origin_budget.rs`：冷请求、共享等待者、上游准入、stale 回退和取消隔离。
- `crates/media/tests/catalog_cache_publication_error.rs`：发布失败不掩盖、旧值保留及恢复。

所有测试使用 fake HTTP/时钟、真实隔离 SQLite 或本机模拟 HTTP，不访问真实 TMDB。共享等待者测试以缓存 miss 信号和有限无结果窗口辅助观察，不能精确断言某一瞬间已进入内部 flight；对应缺陷均有实际失败测试记录。

## 验证方法

修改策略时先通过公开调用链复现失败，再实现：包含重试次数与总预算、双栈、取消和退避、缓存/刷新/origin 等待、leader 取消、共享上游超时、stale 回退与发布错误。stale fixture 必须先满足有效缓存合同；缺少必需字段的准备失败不能当作产品缺陷。

串行执行 `media` 全套及 `api` 的相关单元/集成测试，按仓库锁规则使用 `TMPDIR=/tmp`、`-j8`、`--test-threads=8`。实际来源诊断另外记录 DNS/连接/TLS/完整正文/缓存发布的可观察阶段、重试数与最终状态。curl 可达不证明应用传输链正常；本地 fake 回归也不代替真实页面图片、慢源和正式 perfgate。
