# 实例是 Jellyfin 兼容的播放服务器

**Library** 是已拥有文件的唯一事实来源。Infuse 等客户端通过 Jellyfin 兼容 HTTP API 连接，把这个进程当作 Jellyfin 服务器：浏览、直连播放、同步进度。我们只实现这些客户端实际调用的子集（认证、视图、条目、播放信息、Range 流、会话进度）。不克隆完整 Jellyfin（LiveTV、SyncPlay、DLNA、插件目录）。

这是从 MoviePilot（为外部服务器整理）向自己拥有媒体库并直接提供服务的产品分叉。v1 的这套 API 不做转码：MediaSources 只宣称直连播放。以后的 Web 播放器可以在不同路径上转码，而无需改变面向 Infuse 的响应。qBittorrent / Transmission 仍是下载器；播放永远不会拉取 Torrent。

**备选方案**：无播放，把媒体库作为 Emby/Jellyfin 的文件夹；仅海报墙 UI；实现 Emby 或 Plex。

## Jellyfin 协议认证约定

`/` 与 `/emby` 是 Jellyfin 客户端表面，认证必须兼容 Jellyfin 客户端协议；不能把自有 `/api/v1` 的 Bearer 认证规则当成唯一格式套到这里。`/api/v1` 的管理 API 仍使用自己的认证约定，两者不可混为一谈。

登录使用 `POST /Users/AuthenticateByName`，JSON body 包含 `Username` 和 `Pw`。成功响应中的 `AccessToken` 是服务端签发的会话凭证；客户端后续请求应携带该凭证，不能把用户密码当作 token。Jellyfin 的标准 Authorization 头使用 `MediaBrowser` scheme 和客户端/设备元数据，例如：

```http
Authorization: MediaBrowser Client="VidHub", Device="iPhone", DeviceId="device-id", Version="1.0", Token="<AccessToken>"
```

保持对已观察客户端的兼容：当前服务还接受 `X-Emby-Authorization` 中的 `MediaBrowser` / `Emby` token，以及 `X-Emby-Token` 和 `X-MediaBrowser-Token`。实现目前也接受 `Authorization: Bearer` 和 Jellyfin 播放 URL 中可能出现的 `api_key`、`apikey`、`token` 查询参数；这些兼容入口不能成为移除 `MediaBrowser` 认证的理由。查询参数可能进入代理访问日志，抓包、日志与错误报告必须遮盖 token 和完整敏感 URL。

认证只回答凭证对应哪个用户；媒体库可见性、用户范围和各路由的授权检查仍须执行。不能为了消除 401 而公开受保护的 Jellyfin 路由或跳过用户授权。只读静态图片的公开路由规则另见仓库 `AGENTS.md`。

`AuthenticateByName` 签发的会话 token 当前有效期为 30 天；配置的管理员 CLI token 属于另一种凭证，替换或重新登记时旧值会失效，不能把它当作 VidHub 的长期 Jellyfin 登录凭证。持久媒体客户端应通过 `AuthenticateByName` 获取自己的 `AccessToken`。任何更改 token 过期、撤销或清理逻辑的安全回归，都必须评估 VidHub 这类已登录客户端的续登行为，并补上协议层集成回归，不能只因格式不同就拒绝有效的 Jellyfin token。

鉴权回归至少覆盖：

1. `AuthenticateByName` 返回的 `AccessToken` 可访问受保护 Jellyfin 路由。
2. 标准 `Authorization: MediaBrowser ... Token=...` 以及上面列出的 VidHub 兼容头可通过鉴权。
3. 缺失、错误、过期或已撤销的 token 得到 `401`；有效 token 仍受用户范围和媒体库可见性约束。
4. 401 排查先核对脱敏后的请求认证 scheme、token 是否来自登录响应、会话年龄及是否被撤销；不得记录原始 token，也不得通过关闭鉴权来“修复”兼容性。

协议格式以 [Jellyfin TypeScript SDK 生成 Authorization 头的实现](https://github.com/jellyfin/jellyfin-sdk-typescript/blob/1ef06252cef5d1729646331e64059e230f11a08b/src/utils/authentication.ts) 为参考；URL 中携带凭证时的日志风险见 [Jellyfin 反向代理文档](https://jellyfin.org/docs/general/post-install/networking/reverse-proxy/#logging)。VidHub 实际需要的兼容形式以抓包证据和本仓库集成测试为准。

## 「最近入库」的播放状态口径

`Items/Latest` 的 `IsPlayed` 按 Jellyfin 的两条语义实现：**给了就严格筛**（`true` / `false` 各只要那一半），**没给**时按服务端的默认策略走。Jellyfin 的默认策略来自用户配置 `HidePlayedInLatest`（出厂 `true`）：它把 `isPlayed` 置为 `false`，全看过的库于是返回**空列表**——客户端首页上那个库连入口都没有。

我们的默认策略比它多走一步，口径是「未观看优先」：有没看过的就只回没看过的，一部没看过的都没有时才回全部（实现见 `crates/library/src/latest.rs` 的 `prefer_unwatched`）。同一个函数也被自有 API 复用：`GET /api/v1/libraries/{id}/items?w=unwatched&w_fallback=true` 是首页库行的取数，墙上用户手选的「未观看」不带 `w_fallback`，因此仍是严格筛。

理由：一个"全看完了"的媒体库在首页应该有内容（最新入库），而不是像什么都坏了那样整段消失；客户端只是**多**看到几条已经看过的（`UserData.Played` 为真，客户端自己会标记）。这条偏差是有意的，显式传 `IsPlayed` 的客户端不受影响。

还没有实现的部分：Jellyfin 客户端的用户设置开关（`UserConfiguration.HidePlayedInLatest` 与 `POST /Users/{id}/Configuration`）尚未落地，`/Users/Me` 也不返回 `Configuration` 块；客户端目前无法把默认策略改成"混排已看与未看"。要在客户端里真正关掉「隐藏已看」，需要补这套用户配置表面。
