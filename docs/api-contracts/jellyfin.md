# Jellyfin 客户端协议与重构回归

本服务通过根路径与 `/emby` 提供 [ADR-0005](../adr/0005-jellyfin-compatible-playback.md) 定义的播放协议子集。自有 `/api/v1` 是独立表面，认证规则不能相互替代。本文记录当前合同、容易在重构中遗漏的客户端依赖，以及自动化和真实客户端的验证方法；不代表任意构建已经通过验收。

## 完整请求链

回归必须从公开 Router 发出请求，消费响应中实际返回的 token、用户、条目、来源和播放会话。至少一条 Movie 链和一条 Episode 链覆盖两个协议前缀：

1. `Users/AuthenticateByName` 登录，取得 AccessToken 和 User。
2. 浏览 Views、Library 列表和单项详情，核对身份与访问范围。
3. 读取详情中的 `MediaSources`，请求 PlaybackInfo，取得实际来源和 PlaySessionId。
4. 使用返回地址取流；本地文件检查 Range 状态、Content-Range 与实际字节，STRM 检查合法直放地址。
5. 发送 Playing、Progress、Stopped，再通过列表、详情、resume 和持久记录回读结果。
6. 检查停止位置、日志终点和会话关闭；兄弟 Episode 与另一 User 的进度保持独立。

只断言 HTTP 200，或者由测试直接构造条目身份和播放事件，无法发现客户端因详情字段不全而根本不发送报告的问题。

| 环节 | 必须验证的可观察行为 | 回归位置 |
|---|---|---|
| 发现与登录 | 两个前缀的实例身份一致；登录签发可用且不同于密码的 AccessToken；错误凭据拒绝 | `jellyfin/library_routes.rs`、`jellyfin/authentication_routes.rs` |
| 协议认证 | MediaBrowser / Emby Authorization、X-Emby-Authorization、token header 与支持的播放 URL 凭据生效；无效凭据及跨用户访问拒绝 | `jellyfin/authentication_routes.rs`、`jellyfin/visibility.rs`、`playback_read_regression/stream_credentials.rs` |
| User 与 Library | 登录用户、资料、Views 的身份和权限一致；隐藏 Library 的直接条目、字幕和视频地址不能绕过授权 | `jellyfin/protocol.rs`、`jellyfin/visibility.rs` |
| 列表与详情 | Movie / Series / Episode 身份、类型、父子关系、数量和元数据一致；搜索、分类命中可打开同一详情；分页不丢失、不重复 | `jellyfin/library_routes.rs`、`jellyfin/catalog/`、`jellyfin/metadata_parity.rs`、`jellyfin/filters.rs` |
| 播放信息与取流 | 可播放详情有 MediaSources；稳定来源下与 PlaybackInfo 一致；消费源和 SID 后的实际字节正确；首选变化和缺 DeviceId 不破坏有效绑定 | `jellyfin/playback.rs`、`jellyfin/playback_contract.rs`、`jellyfin/event_sources/playback_info_race.rs`、`jellyfin_stream_binding.rs` |
| 字幕 | 消费详情与 PlaybackInfo 的版本化来源 URL；交付选定来源字节；拒绝用户、设备、单元、来源、会话及配置冲突 | `jellyfin/event_sources/subtitle_source.rs`、`jellyfin/event_sources/shared_episode_binding.rs` |
| 分类范围 | Genres/Studios 缺省 ParentId 时查可见范围，明确未知或隐藏 ParentId 时返回空页 | `jellyfin_coarse_routes.rs` |
| 播放事件 | 来源绑定、空位置、迟到事件、持久进度、日志、关闭会话和用户隔离一致 | `jellyfin/playback.rs`、`jellyfin/playback_contract.rs`、`jellyfin/event_sources.rs`、`jellyfin/playback_null_position.rs`、`playback_event_identity.rs` |
| 收藏与已播放 | 设置/撤销、详情重入、过滤查询与持久事实一致；覆盖 POST Delete、跨用户和隐藏条目拒绝 | `jellyfin/user_marks_routes.rs`、`jellyfin/visibility.rs`、`jellyfin/filters.rs` |
| 图片 | 自有静态 poster / fanart / still / chapter / Library cover 匿名可读，Content-Type 与实际字节正确；Jellyfin 图片按独立规则验证 | `jellyfin/protocol.rs`、`executor_library_admin/images.rs` |
| 请求观察 | 两个入口均区分请求到达和响应头完成；拒绝与未知路由可观察；日志不含敏感 query、认证头和正文 | `jellyfin/request_observation.rs` |

表中相对路径位于 `crates/api/tests/`。测试采用本地 fixture 和注入的 fake，不访问真实 TMDB、Downloader 或 Chromium。解析和序列化单测不能替代公开 HTTP 链。

## 客户端初始化与身份

- 可播放 Movie / Episode 的单项详情必须包含 `MediaSources`，与 PlaybackInfo 共用来源构造；Series 和普通列表不为此增加来源字段。VidHub 依赖详情先初始化来源，只在 PlaybackInfo 返回来源不足以保证它发送 Sessions 报告。
- Movie、Series、Episode 和 Season 的 UserData 都应有稳定 Key，以及与返回条目身份一致的 ItemId。不能只覆盖通用 DTO 而遗漏独立构造的季列表。
- 条目使用 typed ItemId，来源使用含文件 revision 的 PhysicalSourceId。不要从与官方服务的字面格式差异推断客户端故障，更不能为实验修改生产身份格式。
- Season 聚合请求解析到具体 Episode 后，字幕等派生 URL 必须绑定实际返回单元，不能沿用请求 SeasonId。
- STRM 内容读取在取得 owned 快照并释放数据库 lease 后进行，属于阻塞文件工作；不持有 writer 或 reader 进行文件/远程 IO。无效 STRM 的详情与 PlaybackInfo 应一致拒绝。
- 本服务不提供转码；不能通过虚报转码能力解决客户端初始化问题。

## 来源、会话与鉴权

PlaybackInfo 的来源与签发 SID 必须对应同一逻辑单元和版本。签发事务复查来源、覆盖、Library 生命周期、配置及当前授权。读取响应的不同阶段发生首选变化、文件替换或权限变化时，必须保持一致或拒绝，不能组合不同快照中的来源与 SID。多数据库观察不代表跨库原子事务。

事件的非空 MediaSourceId 只接受可见、覆盖目标且 revision 有效的 PhysicalSourceId。裸 file UUID、畸形或仅空白值、失效来源、其他单元/Media、隐藏 Library 来源及与 SID 冲突的来源应被忽略，不写进度或日志。合法的非首选来源必须保存到该来源。事件 body 中精确空字符串、null 和缺省表示未指定；播放 URL 中精确空参数也按未指定处理。不要把空白字符串与精确空值混为一谈。

已为来源 A 签发 SID 后，B 成为首选并不使 A 的有效会话失效。请求未指定来源且带 SID 时，按用户、设备和 SID 的绑定解析 A；A 失效则拒绝，不能回落到 B。明确指定 B 与 A 的 SID 冲突仍拒绝。无 SID 是独立的合法事件入口，但也须选择有效来源并经提交时的授权和生命周期复查。

签发、事件和取流采用相同的设备身份解析，包括缺省 DeviceId。测试同时覆盖 unknown/closed SID、错误设备/用户、跨 Media、同物理文件跨 Episode、隐藏来源，以及配置 revision 或生命周期改变。恢复 Library ready 后，失效 SID 仍不能复活；按当前授权使用严格来源 URL 或重新签发 SID 是不同正控。

字幕地址携带版本化 MediaSourceId，防止首选变化后读到另一文件的字幕。有 SID 时额外检查用户、设备、单元、来源、会话状态和 Library 配置；无 SID 时仍检查可见性、覆盖和来源版本。回归必须消费详情顶层与 MediaSources 内的实际 DeliveryUrl，并让 A/B 字幕字节不同。

Playing / Progress / Stopped 中缺失或 null PositionTicks 不代表零。活动/停止语义仍处理，已有位置保留；停止后会话关闭。毫秒与 ticks 的换算、时长、resume 和日志分别核对，不能把 204 的空操作当作成功持久化。

播放 URL 的显式凭据按限定范围优先于浏览器 Cookie；无效显式凭据不得回落到另一 User 的 Cookie，也不得扩大普通 API 的 query 鉴权范围。视频入口覆盖大小写及容器后缀，实际 Range 字节和撤销授权一并验证。

## 浏览、搜索与标记

- `NameStartsWith` 对最终显示名称执行大小写不敏感的字面前缀筛选。Series 使用 Media 标题；Movie/Video 使用显示标题；Episode 使用采纳的单集名称或 `SxxExx`。`%` 和 `_` 是普通字符。筛选先于精确计数、跨库去重与分页，授权不变。返回 Library 根 Folder 的快捷入口单独验证。
- 页内名称与选页条件须来自一致的 Library 快照；选页后改名不能使响应 Name 不再满足筛选。覆盖 NFO、采纳、override、locale、显式 Empty、跨库同 Media 和短查询/空结果。
- Episodes 省略 Limit 时返回剩余全部授权单集；仅 StartIndex 也不套用 Items 的默认页长。显式 Limit 保留请求值。用超过 200 集的 fixture 同时检查省略 Limit、只给 StartIndex、显式大页、Season 范围和隐藏/未知父项。
- Genres/Studios 未给 ParentId 与明确给错 ParentId 不等价，后者不能扩大到全局范围。
- VidHub 的 POST `/Users/{userId}/{FavoriteItems|PlayedItems}/{itemId}/Delete` 与对应设置入口共享授权和持久化语义。两前缀、整片与 Episode、另一 User 和隐藏条目的正反例都保留。

## 真实客户端对照与排障

真实验收在 VidHub 选择 **Jellyfin** 类型，连接与 Web 共用的隔离实例和 User。准备本地 Movie、HTTP STRM Movie、本地 Episode 与 HTTP STRM Episode，并增加多个 Media、Season、长集数和隐藏 Library，避免两个条目的冒烟被当作完整浏览验收。

每个源执行出画 → seek → 暂停 → 正常退出 → 重新进入，再执行 Web → VidHub → Web 的同单元连续续播。核对可读 HUD、实际请求、SID、持久位置和日志；正常重启同一数据库再读回；切换第二 User 检查隔离。测试不能通过恢复数据库掩盖新进度是否持久化。首次出画点可能与报告位置不同，未取得帧时间证据时不宣称帧精确续播。

若客户端不保存进度，先确定 Sessions 请求是否实际发出：

1. 将同一控制片分别加入隔离的本服务和官方 Jellyfin，记录实际服务/客户端构建与配置，两边都选 Jellyfin 连接类型。
2. 通过已授权的本机诊断入口或 Reqable 采集登录、详情、PlaybackInfo、取流和 Sessions 的完整链；原始敏感报文权限受限、不得入 Git。
3. 比较字段是否存在及客户端是否真正消费。单变量实验每轮恢复相同隔离起点；不叠加多项响应改写后宣称找到唯一根因。
4. 没有报告时调查初始化/客户端状态；有报告时沿授权、来源绑定、提交与回读定位。日志缺失、工具锁屏或没有实际帧属于证据不足。
5. 将确定的触发条件转成公开 Router 失败回归，再修复并重做真实客户端验证。人工模拟 Sessions 通过不证明 VidHub 会发送它们。

图片、人物头像和评分应同时查看服务端 DTO 与客户端显示。NFO 可写外部来源地址；协议展示的图片地址、服务端代理缓存与客户端自行补充的数据是不同来源，不能仅凭截图认定已经存入本服务。

## 重构完成条件

协议路由、认证、DTO、来源构造或 Store provider 改动必须同时验证 `media-server` 和 `api` 的公开 Router。按资源约束串行运行；下列命令独占构建锁：

```sh
python3 scripts/with-build-lock.py --exclusive cargo test --release -j8 -p media-server -- --test-threads=8
python3 scripts/with-build-lock.py --exclusive cargo test --release -j8 -p api \
  --test jellyfin --test jellyfin_stream_binding \
  --test playback_event_identity --test playback_read_regression -- --test-threads=8
```

分页、搜索、ACL 或图片变化还需运行表中相应目标。最终稳定代码按仓库规则运行一次 workspace 全套。记录实际命令、退出码、ignored 和失败，不以定向重跑覆盖原失败。

[系统回归](../testing/system-regression-cases.md) 的 VH / PLAY 用例及 [隔离 E2E 流程](../testing/e2e-runbook.md) 是独立的页面/设备门。每轮记录 Pass / Fail / Blocked / Not run，证据不充分记 Unverified。测试结束移除本轮 VidHub 连接、抓包和自有进程，核对原数据库及 STRM/pan 清单，按约定交付后清理私有临时数据。
