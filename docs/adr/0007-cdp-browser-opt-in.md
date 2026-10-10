# Browser 走外部 CDP，Chromium 不进默认镜像

JS 渲染的搜索在 CDP 支撑的 **Browser** 抓取后（`render: true`）使用同一套 Indexer 选择器。登录与 **Check-in** 是该会话上的第一方插件（站点允许时也可走 HTTP）。默认 Docker 镜像不打包 Chromium。这不是 Windows RDP。

## 当前能力边界（2026-10-10，I05/I06）

当前生产能力明确为 **external-CDP-only**。仓库没有可靠的受管 Chromium 下载/启动实现；原 `enable_in` 创建的空目录及 HEADLESS 文件不能证明存在 Chromium。`BrowserConfig::enable_in` 与 `CRAWLER_MEDIA_BROWSER=1` 现在明确拒绝不支持的 managed 能力，不创建占位目录。旧占位目录无论是否存在，都不被报告为 Chromium。

Obscura 的下载/启动辅助器只证明可以尝试启动第三方 daemon，不能证明其提供兼容 CDP 或完整 JS 渲染能力。Browser 设置及开机不再自动调用它；用户须自行准备运行中的兼容外部服务。当前只支持 **HTTP(S) CDP discovery URL**（通过 `/json/new` 建立 page session），不支持直接 `ws/wss` 调试 WebSocket，也不保证 Cloudflare 质询处理能力。

## 渲染路由与配置

- 只对 Profile 的 `render: true` 请求使用 Browser；普通 HTTP 请求不自动切换。
- 路由优先级：显式 **Site** `cdp_url` → `obscura.enabled` 为真的 `obscura.url` → `cdp.sync.enabled` 为真的全局 `cdp.url`。显式 Site 端点在全局开关关闭时仍可使用；空/非法 Site 端点不能静默 fallback。
- 启动构造和每次真实 Indexer Fetcher 请求共用配置入口。API 以 provider closure 从 Store 读取实时设置，Indexer 不依赖 API/Store。保存后后续请求立即生效，重启选择保持一致。
- 启用的全局/Obscura 路由须有合法 HTTP(S) URL；无端点、原始 ws/wss、带凭据或 query 的 URL 在保存/启动时明确拒绝。启用但无法提供能力的 managed 组合返回配置错误。
- 设置写失败返回非 200 错误并尝试恢复先前配置，不吞掉持久化失败。保存只是配置成功，不进行联网连通性检查，也不启动真实进程。
- 设置 API 明确区分 `enabled`（配置意图）、`configured`（合法端点）、`running: null`（外部进程状态未探测）、`usable: false` / `status: unverified`（尚未建立可用性证据）。只有真实 render 请求中的 session 建立/导航/内容读取成功才构成该请求成功；失败向调用者传播并记录结构化日志。

## 验证边界

回归测试注入 PageSession/opener，注入的是 transport 而非路由：先选出和验证真实 endpoint，再传给 opener。公开 Indexer 搜索/API 搜索测试断言渲染 Torrent、实际传递 endpoint、动态保存与启动一致性、Site 优先级及错误。不下载/启动 Chromium/Obscura，不访问实网。

默认打包 Chromium 会让每个只需要 HTTP 的 NAS 部署膨胀。禁止 CDP 会丢掉需要人工 Browser 认证的 tracker。独立的 RDP 桌面在 Docker 中不可测试，予以否决。受管能力仍可在未来可靠实现生命周期和协议验证后另行扩展；本次不宣称已实现。

**备选方案**：镜像始终带 Chromium；仅 CDP 的远程 browserless；Windows RDP 控制桌面浏览器。
