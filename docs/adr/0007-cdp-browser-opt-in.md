# Browser 走 CDP，Chromium 可选开启，不进默认镜像

JS 渲染的搜索在 CDP 支撑的 **Browser** 抓取后（`render: true`）使用同一套 Indexer 选择器。登录与 **Check-in** 是该会话上的第一方插件（站点允许时也可走 HTTP）。默认 Docker 镜像不打包 Chromium。开启 **Browser** 会把受管的 headless Chromium 下载进数据卷，或由 **Site** 提供 `cdp_url` 指向已在运行的 Chrome（有头模式，用于 Cloudflare / 验证码）。这不是 Windows RDP。

默认打包 Chromium 会让每个只需要 HTTP 的 NAS 部署膨胀。禁止 CDP 会丢掉 Cloudflare 时代的 tracker。独立的 RDP 桌面在 Docker 中不可测试，予以否决。

**备选方案**：镜像始终带 Chromium；仅 CDP 的远程 browserless；Windows RDP 控制桌面浏览器。
