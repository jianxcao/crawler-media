# Browser 走 CDP：单一开关、仅用于登录与签到、受管 Obscura 兜底、导航前失败回退 HTTP

**Browser**（CDP 会话）只用于 Site 的登录与 **Check-in**，二者是该会话上的第一方插件。**搜索与 RSS 永远走 HTTP，不使用 CDP**。默认 Docker 镜像不打包 Chromium 或 Obscura；受管 Obscura 默认使用预置归档或带部署者清单的二进制，Admin 显式允许后才下载。

## 决策

部署与运维见 [部署说明](../deploy.md)。

### 单一开关

实例级只有一个 Browser/CDP 开关 `cdp.enabled`。

- **关闭**：完全不使用 CDP。登录与 **Check-in** 走 HTTP 插件。不探测端点、不下载、不启动任何浏览器进程。Site 上的 `cdp_url` 在开关关闭时**不生效**；`/settings/browser` 返回被忽略的数量 `ignored_site_endpoints`，UI 告警。
- **开启**：`maintenance_via = browser` 的 Site 用浏览器登录与签到，端点按下一节的顺序解析；`maintenance_via = http`（默认）的 Site 仍走 HTTP。
- **搜索不受开关影响**：始终 HTTP。Profile 的 `render` 字段不控制搜索；加载时被忽略并 `warn!`（`crates/indexer/src/profile.rs`）。
- 开关读取失败（非法值）报错，**不得**静默当作关闭。

### 端点解析（开关开启时）

1. **Site 显式 `cdp_url`** —— 最具体的用户配置，优先；空或非法即报错，不静默改走下一级。
2. **实例级 `cdp.url`**。
3. **受管 Obscura** —— 仅当本进程持有存活且身份已确认的受管 daemon 时才作为端点；端口被外部进程占用时不视为受管端点，按「无可用端点」回退 HTTP。

配置了连接的端点失败时**不**回退到 Obscura，直接按下一节处理。

### 失败回退 HTTP（分阶段）

- **登录 / Check-in**：只有**导航之前**的失败（端点不可用〔CDP 关闭或无可用端点〕、排队超时 `BrowserBusy`、target / WebSocket 建立、cookie 注入失败——服务端尚未收到请求）才回退 HTTP，并 `warn!`（Site、route、原因）。**端点配置非法**（设置读取失败、URL 不合法等）不回退，直接报错，以便尽早暴露配置问题。**导航已发出之后**的失败不回退，直接报错，避免重复签到或重复登录请求。
- 回退不等于成功；HTTP 遇到质询照常报错，不重试浏览器路径。
- 单个浏览器会话有总时限（默认 30s，可配置，含排队），各步骤共用剩余时间；排队另有独立上限（默认 15s），拿到名额后剩余时间不足 10s 即在导航前返回 busy（可回退 HTTP）。清理时 `Target.closeTarget` 与 `Target.disposeBrowserContext` 当前各自获得独立默认 2s 上限，可能累计两个清理窗口，超时只 `warn!`；不能把它写成整个清理必定只额外占 2s。cookie 不发删除命令。依据 `crates/indexer/src/cdp_page.rs` 的 Drop 实现。

### 并发与 cookie 隔离

- **多会话并发**：实例全局并发上限默认 3（1–8 可配）；同一可注册域串行。每个会话使用独立 BrowserContext；外部/第三方 Browser 的默认上下文不视为应用私有数据。
- **会话隔离**：不调用 `Network.deleteCookies`、`Network.clearBrowserCookies` 或 `Storage.clearCookies`；每个会话在独立 BrowserContext 中运行，结束即 dispose，第三方 Browser 默认上下文的 cookie 不受影响，会话 cookie 不跨会话残留。
- **host-only 注入与定向回读**：Site 已配置的 cookie 只经 `Network.setCookies` 使用当前 Site URL 做 host-only 注入；不得使用 `Network.setExtraHTTPHeaders`（会发给第三方主机）。读取 cookie 只发 `Network.getCookies{urls:[当前 Site URL]}`，结果按 name 去重后合并回该 Site 的 cookie 字段；不读取或回写无关域的 cookie。
- **host 规范化**：Site URL host 用于 cookie host-only 注入与并发分组时统一小写、去尾点、IDNA 转 ASCII、IPv6 去方括号并规范化；不遍历或归类 cookie domain。
- **公共后缀 cookie 前提**：使用真实端点前应实测是否拒收公共后缀级 cookie（如 `Domain=.co.uk`、`.github.io`）。若该 endpoint 接受此类 cookie，视为 cookie 隔离不兼容，不用它执行 Browser maintenance；不以删除全局 cookie 补救。
- IP 地址规范化为标准写法（如 `2001:db8::1`），按整个 IP 分组。
- **跳转 host**：不扫描、不回写、不删除跳转到的 SSO/质询 host cookie；`cookie_header` 只读取当前 Site URL 可见的 cookie，因此跨 host SSO 的认证状态不保证合并回 Site。
- **有界 FIFO 排队**：拿不到全局名额或分组时按票号先来先服务排队，超过排队上限或剩余时间不足即返回 busy（导航前失败，可回退 HTTP）。
- 运行前提包括 `Target.createBrowserContext`、上下文内 target 创建/attach、`Network.setCookies` 和 `Network.getCookies{urls}`。不要求 `Network.getAllCookies` / `Storage.getCookies` / cookie 删除方法；结束时 dispose 本会话上下文，而非清空共享 jar。实现见 `crates/indexer/src/cdp_page.rs`。

### 受管 Obscura 生命周期

**启动条件**（全部满足）：开关开启、未配置 `cdp.url`、至少一个启用中、`maintenance_via = browser` 且没有自己 Site `cdp_url` 的 Site（`managed_browser_sites > 0`；设置页另显示含自带端点的 `browser_sites`）、二进制已安装（或安装策略允许下载）。默认所有 Site 为 `http`，因此只开开关不会启动浏览器进程。不满足时 `/settings/browser` 返回未启动原因（`switch_off` / `instance_endpoint_configured` / `no_managed_browser_sites` / `not_installed` / `start_failed`）。Site 的 `maintenance_via`、`cdp_url`、启用状态变化后重新评估。

- **浏览器会话不使用 Site 代理**（浏览器自己出网）。不变式：`maintenance_via = browser` 的 Site 不能有 `proxy`——无论先设哪一项，写入后违反即 400，以免直连暴露真实出口或因 IP 不一致失败。
- **安装策略** `obscura.install`：默认 `preinstalled`（只用预置二进制，不联网）；Admin 显式改为 `download` 才会从发布地址下载。安装策略通过持久设置项保存，代码不读取同名安装策略环境变量。
- **端口与绑定**：每次启动选随机空闲端口，并以 `--host 127.0.0.1` 绑定回环；无法保证回环绑定则拒绝启用受管模式。就绪与身份以 `/json/version` 加本进程子进程存活判定。端口竞争导致启动失败时换端口重试至多 2 次。
- **进程身份**：pidfile `<data>/run/obscura.pid`（pid、端口、nonce）。启动时只回收 exe 指向 `<data>/bin/obscura` 的旧进程；其它进程只删 pidfile，不杀。应用正常退出时停止受管 daemon。
- 安装与启动在 Job（`BrowserReconcile`）中执行，保存设置不等待下载。下载或启动失败记 `error!` 并写入状态 `last_error`；开关保持开启，请求按上节回退。
- 版本核对来自 `obscura --version`；`/json/version` 用于 CDP 就绪发现，其 Browser 字段不是 Obscura 版本。版本不一致记 `warn!` / `last_error`（`crates/api/src/obscura_manager.rs`）。

### 信任模型（受管二进制）

- 发布归档的信任根是**代码中写死的各平台资产 SHA-256 与大小**，随 `OBSCURA_VERSION`（当前 `v0.2.3`）一起更新；不是无条件信任 `h4ckf0r0day/obscura` 仓库。`preinstalled` 另接受部署者提供 `obscura.sha256sums` 的二进制目录，该方式只校验与清单一致，不具有固定归档哈希同等的来源保证（`crates/api/src/obscura_install.rs`）。
- 下载先写临时文件，边写边计算哈希；大小上限 200 MiB，`Content-Length` 必须与表一致；不符即删除并报错。归档只接受 `obscura`、`obscura-worker` 两个普通文件，校验通过后原子 rename 到 `<data>/bin/`。
- 预置二进制同样必须通过哈希校验（见 deploy 文档的预置规则）。
- 更换受管二进制时，人工下载、核对哈希、更新资产表并重新执行端点兼容性检查。

### 端点与协议约束

- 只接受 **`http` CDP discovery URL**，通过 `/json/version` 发现 Browser WebSocket，再用 Target 命令创建上下文和页面；不依赖 Obscura 不支持的 `/json/new`、`/json/close`。不接受 `https`，也不接受直接 `ws`/`wss` 调试 WebSocket。
- discovery 返回的 `webSocketDebuggerUrl` 的 host:port 必须与端点一致。
- 端点不接受内嵌凭据与 query string；空/非法端点写入时 400 拒绝，不静默 fallback。
- 启动构造与每次真实请求共用同一配置入口：API 以 provider closure 从 Store 读取实时设置，Indexer 不依赖 API/Store。保存后立即生效。
- 设置写失败返回非 200 并恢复先前配置。保存只表示配置成功，不做联网连通性检查。
- 浏览器只能由 Admin 在 Site 上选择 `maintenance_via = browser` 触发；Profile（含插件提供的 YAML）无法触发浏览器。浏览器网络栈不经过 net-guard，若插件将来能创建 Site，插件方案须禁止其设置 `maintenance_via`/`cdp_url`。

### 数据与配置边界

- Store 初始化空文件或重开匹配生产定义的文件；启动先只读预检文件身份和结构，不自动重置数据目录。依据 `crates/store/src/open.rs`、`schema/layout.rs`。
- `sites.maintenance_via` 默认 `http`；实例设置为 `cdp.enabled`、`cdp.url` 与 `obscura.install`。只有明确启用开关并选择 Browser 的 Site 才触发 CDP。
- 实例/Site 端点只接受 `http`；内嵌凭据、query、`https`、`ws`/`wss` 在写入时返回 400。

### 凭据

- 浏览器会话读回的 cookie 是真实站点凭证：只持久化 CDP `Network.getCookies{urls:[当前页]}` 实际返回的值，按 name 去重，绝不合成占位会话值。
- 读不到 cookie 时保留原有 cookie，不写入空值或占位值。
- cookie 明文不得进日志与前端「系统日志」。

### 部署

- 默认镜像仍不带 Chromium，也不带 Obscura。离线或默认（`preinstalled`）部署须按 deploy 文档预置二进制，或配置外部 `cdp.url`。
- `CRAWLER_MEDIA_BROWSER` 只设置开关的启动默认值（仅在 `cdp.enabled` 未设置时写入），不再导致启动失败；它**不**决定是否下载，也不保证启动浏览器（见上方启动条件）。

## 验证边界

回归测试注入 PageSession/opener 与受管生命周期 seam（installer/launcher），断言路由选择、回退确实发生、以及回退时请求确实走了 HTTP。不下载或启动真实 Obscura，不启动 Chromium，不访问实网（localhost 假 CDP 服务是允许的，仓库已有先例）。

真实端点检查使用 `crates/indexer/tests/obscura_t0.rs`：确认 discovery、独立 BrowserContext、flatten attach、cookie 注入/回读、导航、受管启动与停止。Obscura cookie 回读只取 path=`/`；默认禁止访问私网，需要时由部署者显式启用 `CRAWLER_MEDIA_OBSCURA_ALLOW_PRIVATE_NETWORK=1`。公共后缀 cookie 拒收缺少实测证据，不能宣称已验证；端点或平台变化后须重新检查。

## 理由与备选方案

默认打包 Chromium 会让只需要 HTTP 的 NAS 部署膨胀；完全不支持 CDP 会让需要浏览器才能通过的登录/签到无法自动化。在已预置（或允许下载）二进制、且确有 Site 选择浏览器登录/签到时，受管 Obscura 提供默认端点；导航前失败回退 HTTP 保留 HTTP 尝试机会。搜索保持 HTTP，控制资源成本与降级复杂度。单一开关使启用状态明确。

**备选方案**：默认自动下载（被否决：开关一开就执行第三方程序，供应链风险由用户被动承担）；镜像始终带 Chromium；保留两个独立开关；失败即报错而不回退；端点失败后改用 Obscura 重试（被否决：会让故障表现为不可预期的第二跳延迟，且与「用户配置的连接优先」相矛盾）。
