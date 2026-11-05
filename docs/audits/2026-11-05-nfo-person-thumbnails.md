# NFO 演员头像地址

## 问题与修复

TMDB 的 `profile_path` 通常只有 `/person.jpg`。此前直接 Scrape 和已采纳 Metadata 导出两条路径都可能把该值写入 NFO 的演员 `<thumb>`；外部扫描器无法据此确定图片服务器。

两条 TMDB 边界现在共用地址解析：`/person.jpg` 写为 `https://image.tmdb.org/t/p/w185/person.jpg`；已有 `/t/p/w500/person.jpg` 保留尺寸并补齐 TMDB 域名。完整外部 URL 和缺失头像保持原值。通用 NFO 读写不做供应商推断，本地 `actors/local.jpg` 仍可原样往返。

NFO 保存演员资料与头像地址，不嵌入图片，也不写入本实例代理地址。SQLite 中的演员资料、应用图片代理及磁盘缓存是独立的数据路径；本次没有修改图片请求、代理配置或缓存策略，也未重写现有 NFO。

## 客户端显示范围

当前 Jellyfin DTO 提供 `CommunityRating`、`People` 和 `ProviderIds`，没有输出 `CriticRating` 或 Rotten Tomatoes 专用评分。因此不能把 VidHub 显示的烂番茄评分归为本服务提供；其客户端外部来源或缓存来源仍未证实。演员头像可来自本服务返回的资料，NFO 中的地址本身不能证明客户端实际采用了哪个请求路径。

## 验证

先在公开 Metadata normalization 和实际 refresh/export 接口补回归，观察到原始相对地址导致失败，再实现修复。另有带尺寸路径的失败回归，以及通用 NFO 本地路径往返覆盖。测试使用 fake catalog，不访问 TMDB。

- RED：normalization 两项失败；直接 Scrape、已采纳 Metadata 导出及带尺寸头像分别失败。
- GREEN：`media` 全套 90 项，API Jellyfin 109 项、NFO export 8 项，Library NFO 读写 7 项，均无失败。
- `media` / `api` / `library` release all-target Clippy 和 workspace fmt check 通过。
- 独立限定范围代码复审无待修复发现；源码与上述验证时的 SHA-256 一致。

此修复未重新运行整个 workspace、实际客户端播放、容量或内存测试；这些沿用各自版本的既有证据，不将其改记为本次头像验证。
