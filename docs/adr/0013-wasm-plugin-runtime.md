# ADR-0013 WASM 组件插件运行时（扩展 ADR-0002）

- Status: Proposed
- Extends: ADR-0002（部分取代其中"第三方加载器以后再说"）
- Design: `docs/superpowers/specs/2026-10-15-plugin-system-design.md`

扩展并部分取代 ADR-0002 中"第三方加载器以后再说"一句

工作流 Hook 仍是唯一的扩展模型（ADR-0002 原则保留），但 Hook 升级为
带类型化载荷的三类：Filter（链式修改）、Gate（allow/deny/abstain）、Notify（异步）。
第三方插件以 WASM 组件（wasmtime + WIT，`crawler:plugin@1`）运行，默认无能力，
仅能调用 manifest 声明并经用户确认的宿主导入（HTTP allowlist、KV、只读查询、通知）。
插件失败（trap/超时/非法输出）一律视为 abstain，并触发熔断，不得使订阅流水线失败；
只有显式 deny 能阻断。宿主 API 覆盖下载器操作、站点凭据（敏感权限+管理员确认）、订阅写操作、通知渠道与插件 HTTP 端点；不提供 MoviePilot `get_module` 式任意方法劫持，改为类型化 Provider（indexer-provider / list-provider / notify-channel / discover-source）。
执行模型为同步 wasmtime + PluginHost 专用线程池，不得在 tokio worker 线程上同步调用插件；
CPU（epoch）/I/O/链路总预算分离。凭据只注入站点 base 主机的宿主指定槽位，响应脱敏。
固定 world，未授权导入链接拒绝桩并在安装时静态比对 imports；禁用 wasi:http/filesystem/sockets。
包签名校验随运行时（M1a）一起落地。v1 插件仅 Admin 管理。领域事件在事务提交后发出。第一方 Login/CheckIn 继续编译内置，但改为实现同一宿主侧
`IndexerProvider`/Hook 契约。声明式站点 YAML（indexer Profile）是首选站点扩展方式，
WASM 站点适配器仅用于 YAML 无法表达的站点。

理由：ADR-0002 担心过早冻结 ABI；现 Login/CheckIn 契约已稳定（hooks/src/plugins.rs），
且需求已扩展到第三方分发，必须有强隔离。WIT 版本化 + 双 MAJOR 适配层控制冻结风险。

备选：内嵌 Lua/JS（隔离靠裁剪，否决）；dlopen（否决）；外部 gRPC 进程（作为非沙箱
"外部服务插件"补充，M5，需管理员显式信任）。

后果：新增 plugin-api / plugin-host crate 与 wasmtime 依赖（编译体积/时间增加）；
Bus::emit 失败语义变更；v1 不支持 wasm 线程与 memory64。
r3 补充：同步调用约束以正向 thread-local 标记判定（不使用 Handle::try_current），release 构建同样检查；
PluginHost 按 provider/hook/task/notify 分池、有界队列满即 busy，禁止插件经宿主导入重入插件。
出站网络统一经 net-guard：解析即校验即连接（防 DNS rebinding）、拒绝私网/回环/链路本地/元数据
（含 IPv4-mapped/NAT64/6to4）、逐跳重定向校验；私网仅能由 Admin 在具体站点/下载器实例上授权；
插件提供的 .torrent URL 由宿主下载后以文件提交下载器，通知图片由宿主拉取（防二级 SSRF）。
现有 image_proxy 迁移到 net-guard。
