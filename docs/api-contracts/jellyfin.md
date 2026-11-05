# Jellyfin 客户端协议与重构回归

本服务通过根路径与 `/emby` 提供 ADR-0005 定义的播放协议子集。自有 `/api/v1` 是独立表面；两者的认证规则不能相互替代。本文记录客户端依赖、自动化回归与真实设备验收的对应关系。VidHub无上报的触发点已定位为单项详情缺少MediaSources；`73da134e`生产修复已完成四源实播、双端续播、重启和成员隔离验证。随后复审另发现的事件来源绑定缺陷已修复，定向和最终稳定树全套通过；491真实来源完整页面和最终性能门尚未闭合。以下历史记录保留各自版本和当时结论，不将旧轮失败追认为通过。

## 验证完整请求链

回归必须从公开 Router 发出请求，使用响应中实际返回的 token、用户、条目、来源和播放会话继续下一步。只断言 HTTP 200 无法发现客户端所需字段被删，也无法证明播放事件已持久化。

| 环节 | 必须验证的可观察行为 | 当前回归位置 |
|---|---|---|
| 发现与登录 | 根路径和 `/emby` 的实例身份一致；密码登录签发可用且不同于密码的 AccessToken；错误凭据拒绝 | `jellyfin/library_routes.rs`、`jellyfin/authentication_routes.rs` |
| 协议认证 | MediaBrowser / Emby Authorization、X-Emby-Authorization、两种 token header 与支持的播放 URL 凭据生效；无效凭据及跨用户访问拒绝 | `jellyfin/authentication_routes.rs`、`jellyfin/visibility.rs`、`playback_read_regression/stream_credentials.rs` |
| 用户与 Library | 登录用户、资料、Views 的身份和访问范围一致；成员不可通过隐藏 Library 的直接条目或视频地址绕过限制 | `jellyfin/protocol.rs`、`jellyfin/visibility.rs` |
| 列表与详情 | Movie / Series / Episode 的身份、类型、父子关系、数量及元数据一致；搜索和分类命中可以打开同一详情；分页不丢失或重复条目 | `jellyfin/library_routes.rs`、`jellyfin/catalog/`、`jellyfin/metadata_parity.rs`、`jellyfin/filters.rs` |
| 播放信息与取流 | 可播放详情返回MediaSources，稳定fixture下与PlaybackInfo一致；消费返回的源/SID，本地Range字节正确，STRM保留直放语义 | `jellyfin/playback.rs`、`jellyfin_stream_binding.rs` |
| 播放事件 | Playing / Progress / Stopped 使用签发的绑定和明确的版本化来源；旧源、跨单元、隐藏来源或与SID冲突的事件不写入；缺失或 null PositionTicks 不伪造位置；进度与日志持久化，停止后会话关闭，另一用户不受影响 | `jellyfin/playback.rs`、`jellyfin/playback_contract.rs`、`jellyfin/event_sources.rs`、`jellyfin/playback_null_position.rs`、`playback_event_identity.rs` |
| 收藏与已播放 | 设置和撤销的响应、详情重入、过滤查询及持久事实一致；覆盖 VidHub 实际使用的 POST Delete、跨用户与隐藏条目的拒绝 | `jellyfin/user_marks_routes.rs`、`jellyfin/visibility.rs`、`jellyfin/filters.rs` |
| 图片 | 自有静态 poster / fanart / still / chapter / Library cover 保持匿名可读；验证 Content-Type 和实际字节；Jellyfin 图片按其独立规则测试 | `jellyfin/protocol.rs`、`executor_library_admin/images.rs` |
| 重构后的错误诊断 | 两个服务入口都能区分请求到达与响应头完成；拒绝和未知路由也可观察，日志不含敏感 query、认证头或正文 | `jellyfin/request_observation.rs` |

表中未写完整路径的文件均位于 `crates/api/tests/`。这些测试使用本地 fixture 和注入的 fake，不访问真实 TMDB、Downloader 或 Chromium。新增解析/序列化逻辑还应有聚焦的 crate 单元测试，但不能用私有 helper 的断言代替上述公开行为。

## 本轮已确认的回归

| 触发条件 | 修复与必须保留的反例 |
|---|---|
| VidHub 以 POST `/Users/{userId}/{FavoriteItems\|PlayedItems}/{itemId}/Delete` 撤销标记 | `798606f3`：路由复用原授权及持久化；同时覆盖根路径和 `/emby`、另一用户与隐藏条目拒绝 |
| Web 从电影详情进入播放页，没有 Episode 单元段 | `eb87a4dc`：电影与单集都匹配当前播放入口，不能只测带单元参数的路由 |
| VidHub 请求小写 `/videos/{id}/stream.mp4` | `49cce686`：大小写及容器后缀入口复用绑定取流；检验真实 Range 字节、鉴权及撤销 |
| 浏览器的旧 Cookie 与签发播放 URL 的有效凭据同时出现 | `c690956e`：显式凭据按既定窄范围优先；错误显式凭据不得回退另一用户 Cookie，普通 API 的 query 鉴权范围不扩大 |
| VidHub 发送精确空参数 `MediaSourceId=` | `047e7d1e`：按未指定处理；非空错误值、空白、旧 revision 及错误会话继续拒绝 |
| Playing / Stopped 中 PositionTicks 缺失或为 null | `4ae4a85e`：活动与停止语义正确，已有位置保留；不能把缺失位置解释为 0 |
| 季列表独立构造 UserData，缺少身份字段 | `32591af5`：返回稳定的 Key 和与季 Id 一致的 ItemId；公开回归覆盖根路径、`/emby` 及重复读取 |
| 可播放单项详情缺少MediaSources，客户端没有初始化播放报告 | `73da134e`：详情与PlaybackInfo共用来源构造；Movie/Episode及Season字幕身份有公开失败回归，真实四源和双端矩阵另行验证 |
| 事件带有明确MediaSourceId，服务端却按当前首选来源保存进度 | 本次修复严格选择可见、覆盖目标且版本一致的来源，检查SID与来源一致；旧源、非首选、隐藏来源、跨Media及SID冲突有公开失败回归 |
| 已签发来源A的会话后，来源B成为首选；事件或取流只带SID，未指定MediaSourceId | 未指定来源时按该用户、设备及SID的有效绑定选择A；A失效时拒绝，不回落B；明确指定B与SID冲突仍拒绝 |

各修复都保留先失败、后通过的公开回归；提交后的真实客户端结果另外记录。完整测试不能仅使用手工固定的管理员 token 或自己构造的 ItemId：至少一条连续链必须消费真实登录与 PlaybackInfo 响应，并回读列表、详情和用户隔离状态。

`61ee9207`新增Movie与Episode两条连续HTTP回归，均分别执行根路径和`/emby`：使用AuthenticateByName签发的token浏览Views、列表及详情，消费PlaybackInfo返回的source/session，校验Range状态、Content-Range和实际字节，发送Playing/Progress/Stopped后回读列表和详情。Episode链另验证第二个Episode和另一用户仍为零进度。两条新链首次即通过，属于已有行为覆盖，不称为新产品缺陷的RED/GREEN。无效认证与错误会话绑定的负例继续由authentication_routes及jellyfin_stream_binding覆盖。

该次完整套件曾75/76失败：两个日志观察测试并行时可复现空捕获，单独运行通过。将两段原断言保留为一个测试内顺序执行，避免两个测试subscriber生命周期重叠，未加入跨await锁；最终75/75、相关release Clippy和fmt通过，独立审查无阻断项。原失败保留，不推断tracing内部的具体原因，也不将测试捕获问题解释为Native无上报原因。

## 当前进度问题的证据与待验证差异

晚间新候选`61ee92078a4b`的独立491来源轮已确认health和全部probe成功。Web实播《四喜猫》S01E01，seek、暂停并正常退出后，服务端单元位置和日志终点均为200531ms，Web签发SID已关闭。Native透明对照和单Episode身份诊断尚未取得新的播放/回写结论，不与下述bd3轮的233489ms停止态混用，也不把协议自动化通过当成实际客户端通过。

2026-11-04，候选 `bd3c16f9d3e6` 的独立491来源轮已完成全部实际 probe。Web 播放《四喜猫》S01E01、seek 并退出后保存233489ms，VidHub同用户显示03:53并实际续播；VidHub退出后服务端仍为233489ms，刷新Web仍03:53。覆盖直接后端与诊断转发入口的日志没有收到对应的Sessions报告，签发的Native SID仍pending。该结果将调查重点移到报告发出之前，不能把它说成已经发现某条报告被数据库拒绝。

已经直接核对官方 v12.4 源码：

- [SessionManager 的登录构造](https://github.com/jellyfin/jellyfin/blob/v12.4/Emby.Server.Implementations/Session/SessionManager.cs#L1619) 返回 User、SessionInfo、AccessToken、ServerId；当前登录只返回 AccessToken 与 User.Id/Name。
- [DtoService 的条目构造](https://github.com/jellyfin/jellyfin/blob/v12.4/Emby.Server.Implementations/Dto/DtoService.cs#L328) 为 BaseItemDto 设置 ServerId；当前 Movie / Series / Episode DTO 缺少此字段。
- [PlaystateController](https://github.com/jellyfin/jellyfin/blob/v12.4/Jellyfin.Api/Controllers/PlaystateController.cs#L187) 提供三个播放报告接口；本服务也已经实现这三个入口。
- 官方服务端的 ItemId 使用 Guid，本服务使用版本化 typed ItemId。官方当前 Swift 生成模型接受字符串和可选登录字段，不能由服务端类型差异直接推断 VidHub 必然解码失败。

以上差异尚未被证明是当前故障根因。诊断按单变量执行：A为原始透明响应；B只补登录根对象的ServerId；C恢复同一停止态数据库，只补媒体条目的ServerId，Auth保持透明。B实际登录的JSON差异仅为根ServerId；C实际登录字节一致，11个条目响应的77个目标条目仅增加ServerId。两轮都有真实帧、seek和正常退出，但均未收到Sessions请求，Native SID仍pending、持久值仍233489ms，Web刷新仍03:53。因此单独补这两处字段均不足以恢复上报；不能由阴性结果推断字段组合或其他身份差异无关。C首帧HUD为03:54，末HUD不可读，未推算精确停止位置。

用户另指定本地`/Users/jianxcao/work/lab/movieclaw`作为实现参考。其`src/movieclaw_jellyfin/ids.py`用32位hex表示协议实体；`routes/users.py`返回完整User、SessionInfo、AccessToken和ServerId；`tests/jellyfin/test_http_flow.py`从登录串起目录、取流与播放事件，断言字段和持久结果。当前服务的UserData.Key/ItemId已存在；此处的差异是身份格式，不能误写为缺字段。本服务PlaySessionId是36字符带连字符UUID，诊断收据中的16字符值是脱敏哈希，不是实际SID。参考实现和官方源码的差异只作为待验证假设，不代替本产品真实客户端的因果验证。

上述已有Key/ItemId指Movie、Series和Episode。季列表手工生成UserData时缺少这两个字段，已由`32591af5`独立修复。公开回归先因ItemId为null失败，修复后Jellyfin集成74项、media-server全套40项及相关Clippy/fmt通过，独立审查无阻断项。该合同修复尚未证明能恢复Native报告，实际播放仍须单独验收。

每项诊断都必须记录实际首帧、seek、退出、入口请求、响应状态、SID与持久值及对侧刷新；无实际帧或缺少可读时间时，明确记录限制。不得叠加字段后宣布找到根因。

### 官方 Docker 与完整报文对照（2026-11-04 晚间）

官方 `jellyfin/jellyfin:12.4` 使用全新的配置、数据库和成员账户。本地视频与 HTTP STRM 指向同一段 600 秒 H.264/AAC 视频；产品候选 `61ee92078a4b` 使用独立数据库、账户及两个互不合并进度的条目。两边的 VidHub 均选择 Jellyfin 服务器类型，经本机透明桥和 Reqable MCP 采集。未开启系统代理、断点、重写或脚本；完整请求头、正文及响应只保留在权限受限的临时目录，本文不包含凭据与原始标识。

| 实际操作 | Reqable 与持久结果 | 结论 |
|---|---|---|
| 官方本地 File：出画、seek、暂停、正常关闭播放器 | Playing / Progress / Stopped 均 204；停止报告与回读位置同为 1431165307 ticks，Resume 包含该条目 | 本轮官方 File 上报和回写通过 |
| 官方 HTTP STRM：出画、seek、暂停、正常关闭播放器 | Playing / Progress / Stopped 均 204；停止报告与回读位置同为 3535199618 ticks，Resume 包含该条目；File 位置未变 | 本轮官方 STRM 上报和回写通过 |
| 产品本地 File：出画、seek、暂停、正常关闭播放器 | 登录与 PlaybackInfo 为 200、取流为 206；未观察到 Sessions 报告，播放单元与日志仍为空，签发 SID 为 pending | 缺少 Native 上报的问题已在同片源对照中复现 |
| 产品 HTTP STRM | 详情已打开，点击播放时桌面工具返回 Mac locked；没有新的 PlaybackInfo | 播放未确认，不能作为阴性结果 |

这些操作的实际观察窗口不相等，GUI 工具切换造成了额外播放时间；不能据此比较时延或吞吐。尚未完成本轮官方重新进入后的实际续播及产品双向进度验收。

实际完整 JSON 对照还确认：官方登录含完整 User、SessionInfo 和 ServerId；产品登录只有 AccessToken 与 User.Id/Name。官方条目详情含 MediaSources，产品详情没有；因此官方 VidHub 的 PlaybackInfo 请求带 MediaSourceId，产品请求未带。官方条目 ID 为 32 位 hex，产品为 typed ID；官方 source 的 Id 与 PlaySessionId 均为 32 位 hex，产品分别是版本化来源和带连字符 UUID。产品 PlaybackInfo source 还缺 Type 等默认字段，且正确声明不支持转码。这些均为可核对的差异，尚未证明哪项影响 VidHub 上报；不能通过虚报转码支持来消除差异。

### 单条目 GUID 诊断（2026-11-05）

在同一600秒File控制片上，仅将选中Movie的`Id`及`UserData.ItemId`替换为32位hex，并在隔离桥回译该条目的已批准请求引用。真实详情、PlaybackInfo与取流请求均使用新ID，私有字节审计确认Auth/PlaybackInfo原样，opaque Key、source/session/token及其他字段未变；不是只看响应字段就假定客户端已消费。VidHub首帧烧录00:00.167，五次快进后01:00.500，暂停时01:10.333，随后Command-W正常退出。服务端没有Sessions入站，新增SID仍pending，播放单元与日志为空，详情无续播。

此轮没有映射拒绝；图片与视频各一次BrokenPipe发生在响应头之后，是取消传输，单独保留。结论仅为该条目的双字段GUID变化不足以恢复上报，不支持据此改生产ID，也不排除其他身份或初始化条件。

完整报文进一步显示：官方详情含MediaSources，随后PlaybackInfo请求已携带MediaSourceId，取流也携带非空源ID；产品详情没有MediaSources，PlaybackInfo请求未带源ID，虽然返回了版本化source Id，取流参数仍为精确空值。条目GUID轮仍出现同一空值。尚需区分详情缺少源初始化与source ID格式这两个竞争解释；不能把空值直接归因为某种格式，也不能将此前允许空参数的取流修复当成上报恢复证据。

### 详情媒体源单变量正向结果（2026-11-05）

完整恢复独立cold S0的五个数据库及sidecar后，仅在选中File的详情根对象追加实际PlaybackInfo的`MediaSources`数组。原条目typed ID、版本化`f1_` source ID、opaque Key、登录和PlaybackInfo的原始字节均保持不变；未改Type、ServerId或会话格式，没有合成播放事件。

VidHub实际首帧00:00.250，五次快进后01:02.500，暂停01:15.833，Command-W正常退出后显示“继续播放01:16”。Reqable MCP确认真实PlaybackInfo请求开始携带原`f1_`源ID，三次取流也使用该源；Playing一次、Progress四次、Stopped一次全部204。Stopped的758080044ticks与服务端75808ms位置、日志终点一致，真实SID关闭，无活动会话。保护字段和私有字节审计通过，没有映射拒绝或传输错误，三次客户端取消单独保留。

该对照将此File场景的触发点定位为详情缺少媒体源初始化：仅返回PlaybackInfo中的源，不足以使VidHub建立报告所需状态。补齐详情字段即可让现有typed条目ID及版本化源ID完成真实上报，当前证据不支持修改这些身份格式。生产修复、公开HTTP失败回归、STRM/Episode及Web双向验证继续执行；此结果来自临时隔离桥，不能提前标记生产验收通过。

后续重构必须同时保留详情初始化和实际事件闭环：只测试PlaybackInfo返回源、或由测试直接构造Playing/Progress/Stopped并不能发现客户端根本不发报告的缺陷。公开HTTP回归从实际登录取得token，同时读取详情与PI、断言MediaSources一致，再使用PI返回的源/SID取流、报告和回读；Movie、Episode和本地STRM分别覆盖两个详情入口及两协议前缀。该自动化检查字段和服务端合同，真实VidHub与Web仍需独立复验，不能据此证明第三方客户端一定启动上报。

### 生产修复与自动化回归

生产详情与PlaybackInfo共用私有source构造，可播放Movie/Episode详情补齐MediaSources，Series与列表不新增此字段。STRM在取得owned快照后通过阻塞任务读取本地文件，不访问远端、不持有Store读租约或writer；源revision、typed条目ID、会话、认证及不支持转码的声明保持原语义。无效STRM详情与PI一致返回404。

公开HTTP回归先有两条Movie/Episode链因详情字段缺失而失败，再通过。独立审查另发现Season请求解析成Episode后，新增源字幕URL使用请求SeasonId，与返回Episode不符；新增真实侧车字幕GET回归先失败，再改为绑定返回条目的身份。最终Jellyfin76项、绑定5项、media-server去重40项及相关release Clippy/fmt通过；media-server首轮有两项回环监听EPERM，授权仅重跑对应target后通过，保留该失败。`73da134e`稳定代码的最终release workspace含doc-tests退出0：596个结果组汇总3911 passed、0 failed、25 ignored（含子进程汇总、非去重数），不将忽略项计作通过；正式候选真实双端矩阵继续执行。

`73da134e`正式候选新建四个独立播放单元与两个成员，使用未注入字段的生产详情和透明转发。本地Movie、HTTP STRM Movie、本地E01和HTTP STRM E02均实际出画、seek、暂停并正常关闭，真实Sessions均204、SID关闭，停止ticks转换成毫秒后与unit和log一致；首次位置分别为75723、75262、85895和82395ms。本地Movie重入再停为75068ms。Movie连续链为Native75068→Web144457→Native169317→Web183088ms；E02为Native82395→Web114586→Native160854→Web177745ms，每次新日志起点等于前次停止，兄弟单元不变。

同一数据库正常停服重启，四张Playback表逐行一致，无快照恢复；重启后Web从183088继续至201197ms，Native再从201197开始并保存194895ms。成员B真实播放从0保存82792ms，成员A四个单元不变，切回A可见其独立进度。终态5个用户单元、14条日志、14个closed SID、0活动会话；正常停止、原app/sidecar与STRM/pan清单一致，无强杀。Native首帧可能早于请求位置，E02画面烧录166.667秒与报告160.854秒不同，不宣称帧精确恢复；实际Native直取HTTP origin的Range未捕获，不能用setup的206代替。该轮通过范围是四源控制片及上述连续操作，不能代替491真实来源完整页面。

本轮首次实际登录401另有明确输入原因：Reqable比对成功setup请求与失败请求，密码完全一致而用户名不同，GUI工具漏入用户名开头两个中文字符；完整输入后实际用户名和密码均匹配、登录200。保留该失败，不归因为密码校验或API授权回归。

### 播放事件明确来源的复审修复

73da后的独立复审发现既有来源绑定缺陷：事件JSON的MediaSourceId未被读取，provider按ItemId选当前首选文件。无SID的旧revision事件因此可能对新来源签发绑定并写进度；合法的非首选来源也被绑定到首选来源；有SID时明确的另一来源没有参与冲突检查。此缺陷与此前VidHub不发Sessions是两条独立因果链。

公开Router回归先复现旧revision、非首选来源、SID来源冲突，以及同Media隐藏Library来源和另一Media有效来源的错误写入，再实现严格的版本化来源选择与绑定核验。非空值只接受PhysicalSourceId，裸file UUID、畸形、空白、失效revision、其他单元、不可见来源和SID冲突返回204并忽略；合法非首选来源须保存到该来源。精确空字符串、null和缺省按未指定处理，与既有取流空值合同一致；事件body的空值是设计约定，不冒称实际Native抓包。无SID仍是合法入口，来源/coverage/配置/lifecycle及最终提交授权复查继续保留。来源定向8项GREEN，独立审查未发现新增可操作缺陷；本次稳定工作树最终release workspace含doc-tests退出0：596个结果组汇总3919 passed、0 failed、25 ignored（含子进程汇总、非去重数）。畸形来源的补项还直接验证pending绑定未变及下一次正常签发generation只增1；其他拒绝来源测试观测session/unit/log，零签发另由provider的提前返回保证，不把这些状态断言夸成完整数据库逐行对账。

新增两前缀下隐藏条目的两种详情、PlaybackInfo及两类POST Delete的跨用户/隐藏条目负例，验证拒绝且持久标记不变。其首次执行通过，属于授权合同补覆盖，不声称此前存在已复现的绕过漏洞。

### 首选来源变化后的会话绑定

后续公开Router回归又复现一条独立缺陷：PlaybackInfo已为来源A签发SID，A仍可见且版本未变；同一Media的另一文件B提升revision成为首选后，省略MediaSourceId的事件仍先解析B。绑定分类因文件不一致而忽略合法Playing、Progress和Stopped，取流也返回404。它不要求A被替换或撤销，不能将拒绝解释为正常的旧来源保护。

两条失败回归分别观察到合法会话未激活，以及预期206的Range请求返回404。修复仅在未指定来源且提供SID时，从Library reader读取该用户和设备的绑定，按绑定的文件及revision解析当前可见来源；不持有writer锁，不回落当前首选。事件仍经事务内来源、授权与代际复查，取流仍检查单元、来源版本和会话状态。明确来源与SID冲突及无SID路径保留原规则。

公开回归覆盖根路径与`/emby`、pending和active SID、事件中省略/null/精确空来源、取流省略/精确空来源，断言来源A的实际Range字节、进度、日志与会话关闭；同SID明确指定B必须拒绝。另保留A真实替换后的反例：旧SID事件204且不改变事实，旧SID取流404，不得用新revision复活旧绑定。本节的修复验证结果在审核与最终验收清单中按提交分别记录，不用自动化替代真实客户端验收。

补充的公开负例验证未知SID、错误设备、另一用户、跨Media、closed SID和签发后Library隐藏均不写入、不返回媒体字节。同物理文件覆盖E01/E02的回归还验证：来源首选变化后E01仍按原绑定播放和停止，E02不能借用E01的SID更新进度或取流。稳定定向矩阵通过89项Jellyfin、6项取流绑定、14项事件身份和12项播放读取；上述授权补项首次执行通过，属于覆盖补强。最终稳定工作树release workspace含doc-tests退出0：596个结果组汇总3925 passed、0 failed、25 ignored（含子进程汇总、非去重数）；代码文件哈希与验证收据一致。Season/Work聚合入口的同类来源切换未直接实测，不将单集测试推广为所有聚合入口已验证。

## 每次重构的完成条件

1. 为实际缺陷补公开行为的失败回归，再实现修复；保留字段、状态、字节和持久化断言。
2. 运行改动 crate 的单元/集成测试与相关协议矩阵；最终稳定代码树运行一次 workspace 全套。遵守 AGENTS 中的 Cargo 锁、并发和测试线程要求。
3. 对照本文件与 [系统回归](../testing/system-regression-cases.md) 的 VH / PLAY 用例，在隔离实例实测 Web 和 VidHub。两端都消费同一逻辑单元的进度；本地视频与 STRM 分别验收。
4. 每轮恢复原数据库与 STRM/pan 文件，删除本轮私有凭据、数据库副本、临时连接与自有进程；凭据及完整敏感URL不进入日志或Git。
5. 在 [审核记录](../audits/2026-11-02-storage-redesign-review.md) 与 [最终验收清单](../superpowers/plans/2026-11-02-storage-redesign-final-acceptance.md) 区分实际通过、失败和未执行。自动化通过不能代替真实客户端进度同步通过。

协议路由、认证、DTO、来源构造或Store provider改动时，必须同时运行`media-server`测试和`api`中的公开Router合同；只运行`media-server`不会执行本轮捕获详情缺字段的失败回归。取得AGENTS约定的Cargo锁后，顺序执行以下定向门，不能并行启动Cargo：

```sh
TMPDIR=/tmp cargo test --release -j8 -p media-server -- --test-threads=8
TMPDIR=/tmp cargo test --release -j8 -p api \
  --test jellyfin --test jellyfin_stream_binding \
  --test playback_event_identity \
  --test playback_read_regression -- --test-threads=8
```

分页、搜索、ACL或图片行为变更还须执行表中对应的分页/图片/可见性目标，不能把这组播放定向门称为整个Jellyfin表面的完整覆盖。最终稳定树仍按仓库要求执行一次workspace全套，真实设备矩阵分别覆盖本地与STRM、Movie与Episode、同单元跨端续播、重启和两个User。
