# Media / Library 存储重设计：单表浏览与独立响应缓存

状态：**首版设计，供用户审阅，未实施**。本设计补充并取代旧缓存计划中“保留浏览投影失效链”的决定。允许舍弃全部旧数据库数据，但不允许以“重建数据库”为理由删减现有业务功能。本文的新表和新路径都是提案，不代表当前代码已经如此运行。

## 1. 决策摘要

1. `cache.db` 只缓存外部响应，不是本地 Media 元数据的事实来源；清缓存不改变 Library 列表、身份、观看状态和已采用的图片地址。
2. 将 Media 身份/采用的元数据、Library 配置/根、文件台账、单元覆盖、浏览摘要及Playback状态/会话/历史放在 **同一个 library.db**。Media 可以无本地文件，文件台账只是它的使用者。
3. 文件行显式保存 `library_id/root_id`。常规浏览单表读 `library_items`，详情单表读文件台账后按主键批量补充少量冷数据。禁止请求时全库路径归属判断、文件 stat、NFO 解析和外部网络访问。
4. 浏览摘要仍是冗余读模型，但按受影响的 `(library_id, media_id)` 在写事实的**同一事务**内更新。不再维护另一份完整文件投影，不再依赖跨库 journal、周期整库重建或请求触发重建。
5. 删除旧浏览专用 `_read_revisions`、`_browse_changes`、`browse_change_cursors`、`browse_generations`、`browse_media`、`browse_files` 及消费者/构建服务。不能把这些机制改名搬到另一张通用队列表。
6. **大表尽量不关联**；允许确有必要的索引半连接/反连接、分页后主键批量水合、单个 Media 范围内的事务聚合。禁用“大表先全量取出，在 Rust 里做 JOIN/过滤/排序”这种伪优化。
7. 无旧数据导入、兼容视图、双读双写、旧版本回退。一次停服重建后所有调用方直接使用新模型；实际媒体和 sidecar 文件保留。

## 2. 当前证据与问题定位

| 当前代码 | 已观察到的行为 | 本设计对应变化 |
|---|---|---|
| `crates/store/src/schema/tables.rs` | Media/Library 配置在 app.db，ledger/file_meta 在 library.db，Playback 单元在 subscribe.db | 同一业务原子更新所需记录合到 library.db；凭证仍隔离 |
| `crates/api/src/http/library/selection/page.rs` | 高级过滤/观看排序读取全库轻候选再在内存筛选排序 | 查询端完整 SQL 过滤/排序/分页，禁止筛后补页 |
| `crates/api/src/browse_projection/builder.rs` | 合并台账、Media、TMDB 缓存，观察文件与 sidecar，再发布整库投影 | 文件观察在扫描/探测/Scrape 写路径；元数据成为本地采用记录 |
| `crates/store/src/browse_reads.rs`、builder 的 collect_owned | 部分 Library 查询仍以路径推断归属，默认 Library 有补集扫描 | 显式归属列与以 library_id 开头的索引 |
| `crates/store/src/schema/browse_changes.rs` | 每个源最多1024条逐行通知+1条 browse_all，不是无限日志 | 新结构不需要这份浏览通知；不能据此声称当前日志无限增长 |
| `crates/store/src/browse_projection.rs` | 发布校验对 app/library/catalog 的数据库级修订比较 | 同库事务读取/更新局部事实，不再用全库修订判断整库构建 |

结论不是“已有 JOIN 证明慢”：当前还存在全量内存筛选、JSON 解码、文件 IO、全库派生计算和跨库一致性成本。真实收益须由新旧查询工作量及端到端测量证明。

## 3. 数据库文件与生命周期

| 文件 | 新职责 | 正常运行时能否随意清空 |
|---|---|---|
| `app.db` | User/凭证、Site、Downloader、Filter、Naming template、实例设置；其他控制面设置 | 否 |
| `library.db` | Media身份与已采用元数据、Library/根/权限配置、文件与单元覆盖、Collection、浏览摘要、Playback状态/会话/历史/指标，以及文件相关持久操作与探测状态 | 否 |
| `subscribe.db` | Subscribe策略/覆盖/搜索轮次事实；不在本次顺便重设计其政策模型 | 否 |
| `jobs.db` | Job 定义、执行、重试事实 | 否 |
| `cache.db` | 六来源外部响应缓存及缓存自身索引；无浏览修订/事件表 | 是（按停用连接或受控清理流程，不直接 unlink 活跃 WAL 文件） |

“旧数据可丢”只适用于这次未上线切换，**不是以后可以删除 library/subscribe/jobs 的产品语义**。Media 从 app.db 移入 library.db 不意味着依附于文件：删除 Library 必须保留 Media 和观看状态。

同库FK用于明确的从属关系；禁止library/file删除通过FK级联删除Media或User观看事实。跨文件ID无外键，不宣称有跨文件原子事务。User删除、Subscribe创建/调度仍需可恢复的业务操作，不能误把整个实例当作一个事务；Playback进度/会话/历史则放同库并用同一事务提交。

## 4. 核心表与键（目标模型）

以下列出业务字段和关键约束；复用现有操作/探测/播放字段时不得因清单省略而删除功能。实现前需生成完整新 DDL/字典及逐调用方迁移矩阵，不以本文为已经完成的 schema。

### 4.1 Media 与元数据

| 表 / 主键 | 主要字段 | 规则 |
|---|---|---|
| `media` / `media_id TEXT` | kind、title、original_title、year、identity_state、aliases_display_json、metadata_revision | 内部 UUID，kind=movie/tv/video；无文件的 Subscribe 也可先创建 Media；不以路径或 TMDB ID 当身份 |
| `media_aliases` / `(source,kind,external_id)` | media_id | 同来源同 kind 外部别名唯一归属；相同数字 movie/TV 不冲突；反向索引(media_id,source)；不同别名指向竞争身份时报冲突，不静默合并 |
| `media_metadata_sources` / `(media_id,source,locale)` | normalized_json、present_fields_json、fetched_at、source_revision | 被本实例采用过的来源快照，不等于原 HTTP 响应缓存；只在明确识别/导入/刷新动作更新，不因 cache TTL/清理丢失 |
| `media_metadata` / `(media_id,locale)` | title/year、overview、release_date、rating、runtime_minutes、language、genres/countries JSON、poster_url/backdrop_url、cast/details JSON、field_origins_json、revision | 来源政策合成后的本地元数据；长简介/演职员等冷字段不复制进每个文件或卡片 |
| `media_metadata_overrides` / `(media_id,locale)` | 显式字段值/清空标志、revision | 全局人工字段锁；来源刷新不覆盖 |

TV 的季/分集详情单独保存 `media_seasons(media_id,season,locale)` 与 `media_episodes(media_id,season,episode,locale)`，含采用的名称、简介、播出日期、封面/剧照URL及来源版本；它们不是新的Media身份，也不代表已拥有该集。按Media/季定向分页，不能把整部TV详情复制到每个文件。Library详情将本Library的NFO覆盖与本地采用元数据按已知主键合成，允许单Media范围冷读，不访问响应缓存。

来源优先级固定、可解释：**显式人工覆盖 > 对应 Library 的有效 NFO 字段 > 按配置固定顺序的 Metadata source 字段 > 文件名/已有基础值**。初始来源顺序 TMDB、Douban、TVDB、Bangumi、AniList；fanart 只提供图片候选，不生成 Media 身份。某来源失败不清空已有值；候选字段缺失、显式空值与用户清空必须可区分。语言严格按请求/实例策略选择，不能用“缓存中最新任意语言”替换。

NFO 属于具体文件环境，不全局覆盖同 Media 的其他 Library：`library_metadata_inputs` 主键 `(library_id,media_id,input_key)`，保存选中 NFO 的标准化字段、版本和解析状态；`input_key` 为该根内规范化路径/来源标识，含 root_id。只有可发布的台账/sidecar 可作为候选。首选文件的排序采用现有消费者规则并固定 tie-break=file_id，不随 SQL 返回顺序变化。同库多份 NFO 不一致时保留候选、记录冲突与采用来源，而非“最后扫描者胜”。

展示 locale 的实际决策保存在 Library 配置中；一 Library 默认只有一套卡片语言，不做所有语言×所有文件的复制。单条详情按 locale 读取本地元数据，缺失可以降到实例默认；改变 Library 显示语言是显式可恢复重算操作，完成前不混用两种语言卡片。

### 4.2 Library、归属和文件台账

- `libraries(library_id PK, kind, name, lifecycle_state, config_revision, ...现有显示/访问/扫描/探测设置)`。
- `library_roots(root_id PK, library_id FK, path_key UNIQUE, display_path, enabled, ...)`。路径使用目录组件边界和平台既有大小写/规范化规则，不能简单小写所有路径。跨 Library 重叠根仍按最长唯一根处理，歧义拒绝，嵌套独立 Library 不互删。
- `library_files(file_id PK, library_id, root_id, media_id, path_key UNIQUE, display_path, kind, presence_state, publication_state, file_revision, size_bytes, mtime_ns, created_ns, source_digest, source_path, transfer_mode, ...质量/置信度/Transfer字段)`。
- 时间、路径、归属、基本质量、duration、has_poster/backdrop/still、选定本地 artwork 路径等高频标量直接在文件行；音轨/字幕/章节/指纹大体积数据放独立冷表 `file_details(file_id PK, source_revision, video/audio/subtitle/chapters JSON, ...)`。不把巨型 JSON 塞进热行。
- `library_file_units` 主键 `(file_id,season,episode)`，另存 library_id/media_id 和必要的可播放/版本标量。保留同单元多文件、多版本；同文件覆盖多集通过多行表示，文件大小仅计算一次。
- `library_units` 主键 `(library_id,media_id,season,episode)`：每个去重单元的文件计数、可用版本数、首选 file_id 和质量摘要。movie/video 用规范 whole `(-1,-1)`；TV 的 S00/E00 是合法分集，不能兼作 whole。
- 没有可靠分集编号的文件仍可登记/展示，不凭空生成覆盖单元，不据此宣称 Subscribe 已完成。`unidentified` 按 library/root/path 定向保存，保持人工认领流程。

文件归属列需用同库复合约束保证root属于声明的Library，kind与Media/Library一致；不是只存三个互相不一致的ID。presence使用present/missing/unknown，unknown（权限/IO错误/根不可达）不当missing，也不触发prune。STRM的source版本包含内容摘要，不能只比size/mtime；发布pending文件不出现在可播放列表。统计可区分missing和unknown，保留已有错误提示。

归属在写入时确定并持久化，不在每次列表读取时解析全库路径。根编辑、增加更深层根、跨根移动用显式 operation 对受影响范围重归属；不能留下读路径默默回退默认 Library 的规则。Transfer 的默认 Library 是**目标选择规则**，不是任意未归属文件的读时兜底。

### 4.3 单表浏览摘要

`library_items` 主键 **`(library_id,media_id)`**，每个实际有台账文件的 Media 每 Library 一行：

- 冗余身份展示：kind、title、title_sort、year、identity_state、常用外部 ID、metadata_revision。
- 冗余过滤排序：release_date_sort、rating_sort、runtime_sort、language、added_at_sort、size_bytes、resolution_rank、hdr、scraped、missing、stable_shuffle_key。
- 汇总：file_count、available_file_count、distinct_episode_count、distinct_season_count、缺失文件计数；计数列保持不变量并可核对。
- 卡片图片：preferred_file_id、poster_ref/backdrop_ref、has_still；引用可为已知本地文件路由或已采用远端 URL，不存图片字节。
- 版本：item_revision，随该行真实语义变化更新，不存在数据库级浏览版本计数。

不保存完整文件列表、全季分集数组、演职员或原始响应 body。`scraped` 保留既有筛选语义（可用元数据已采用），另用明确字段记录“已写 NFO/图片”，避免把缓存命中当作实际 Scrape 完成。

当前归约口径明确保留：`missing = missing_file_count > 0`；size仅累计确认是普通文件的每个物理文件一次，全部未知时NULL；added_at取各文件created优先/mtime兜底后的最早值，无值时NULL；图片/NFO首选按 `(season或0, episode或0, path)` 最大者，file_id只作最终稳定tie-break。浏览选图与播放按质量选版本是不同规则。删除最后文件删除卡片/覆盖，但保留Media和观看事实；HTTP不回退stat。

多值字段不靠 `LIKE '%...%'` 或热页 `json_each` 全库扫描：`library_item_terms(library_id,term_kind,term_value,media_id)` 为去重倒排表，主键同列顺序，反向索引 `(library_id,media_id,term_kind,term_value)`；genres/countries/resolutions/collection 等通过此表做必要的索引 EXISTS。一个类别内部 OR，不同类别 AND，跟现有筛选合同一致。

Collection 定义/成员随Media迁入library.db，绑定canonical media_id，不依赖旧的路径式media_item_id；API旧格式由边界解析为同一身份，不能产生两套持久键。公共系列collection可进入公共terms；用户自建collection必须存有owner_user_id的独立成员索引，并在查询关联中显式校验owner，**不得把私有成员混入公共terms或全局facets**。成员变化与必要倒排索引同事务更新，移除Library不删除用户成员关系；卡片消失后只是不再出现在该Library可见结果。

### 4.4 User Playback 单元与摘要

- 将 `playback_units` 移入 library.db，主键 `(user_id,media_id,season,episode)`，保持 position/played/favorite/track preference/play_count/时间语义。保留 Media 身份而不是 file_id/path 作为进度身份。
- `user_library_items` 主键 `(user_id,library_id,media_id)`，保存 seen/played/favorite/last_played_at 与继续观看所需当前单元摘要；只为有相关状态的用户/Media 创建，不预建全用户×全 Library×全 Media 笛卡尔积。
- 一行缺失的默认状态是未看/未收藏。**未看查询必须包含缺失行**；不能只查摘要存在的行，也不能分页后排除已看条目。
- 用户写 Playback 单元时，同时更新该 Media 在其现有 Library 中的摘要；文件单元增删时，也更新受影响用户的摘要。只按 media_id 索引读该 Media 的用户单元与覆盖，不扫描所有用户全部历史；用户多、长 TV 单元多时写事务成本如实测量。
- 区分 Library 浏览既有 whole/episode favorite 与 seen 规则、播放页分集完成规则。不能把“某集看过”改成“整部看完”。取消 movie 旧 `(0,0)` 兼容分支，全新数据一律写规范 whole；TV S00E00 仍保留。
- `playback_sessions`、`playback_session_ids`、`playback_logs`、`playback_metrics`及非认证设备状态随单元一起移入library.db，保留现有字段/索引/保留窗口。会话generation/EventSequence校验、进度更新、摘要更新与终态历史提交使用同一事务；终态按稳定session标识幂等，旧Progress/Stop不能影响新会话，重复Stop不能增加播放次数。设备token/认证凭证仍在app.db，不因日志搬库而改变授权协议。

## 5. 原子写入与并发：冗余字段如何不漂移

Store 提供按用例的事务入口，而不是允许任何模块任意写事实后“记得刷新摘要”。所有索引事实、覆盖、terms、卡片和用户摘要在同一 library.db `BEGIN IMMEDIATE` 事务内发布；冷 IO/网络/JSON 解析必须在事务外完成。

| 写入事件 | 同事务内必须完成 |
|---|---|
| 扫描/Transfer 发布一个文件 | 校验 Library 生命周期与 config_revision；写 file/units；合并对应 library_units；更新该 item、terms、受影响用户摘要；登记 probe 义务 |
| 文件质量/本地图片变化 | 校验 file_revision/source version；更新文件事实与受影响 item/单元；冷数据以同源版本写入 |
| 元数据采用/用户编辑 | 写候选/覆盖/已采用元数据；按 media_id 索引更新此 Media 的相关 items/terms；不同 Library 的 NFO 不串用 |
| Playback 上报/收藏 | 更新单元事实及受影响 user_library_items；不更新公共卡片、不触发全库工作 |
| 缺失标记/显式 prune | missing 只标记，不自动删数据；显式 prune 删文件依赖与覆盖、刷新对应摘要，不删 Media/观看/字节 |
| 重新识别/身份合并 | 先验别名冲突；同库重绑定文件、覆盖、摘要与观看/收藏/collection 引用，确定冲突政策；Subscribe 等跨库引用用持久 operation 完成，不留悬挂旧 ID |

索引：文件 `(library_id,media_id,file_id)`、`(media_id,library_id,file_id)`、`(root_id,path_key)`；单元按 `(library_id,media_id,season,episode,file_id)` 与 `(media_id,season,episode,library_id)`；用户单元反向 `(media_id,user_id,season,episode)`。质量/版本变化后首选文件重新选择时，SQL/Rust 可读取**该 Media 范围**，不是全 Library。

首版采用受影响 Media 范围内重算汇总，避免一开始写复杂的增量加减器；每个操作对文件/单元按索引访问，批量扫描以 **100 个文件**为一个提交窗口，逐个 Media 合并重复更新。对超长 TV、同 Media 很多版本、很多用户的成本单独测试，不宣称单次更新永远 O(1)。网络/大 blob 不进入这个事务。

元数据全局刷新按单Media事务发布；根编辑/显示语言等确实涉及整库的大操作必须有持久operation与生命周期屏障：标记reindexing、阻止旧scope工作发布，分批更新期间普通读返回明确operation状态而非混合新旧归属/语言，完成并校验后恢复ready。重启恢复该operation，不用通用浏览journal；不能长持一个覆盖百万文件的写事务。

同一路径被替换时增加 file_revision，probe/artwork worker 发布必须匹配 file_id、source version 和生命周期租约；旧 worker 不能覆盖新内容。不能用一个全局版本号代替这个检查。磁盘变更与 SQLite 无原子事务，Transfer/Scrape/删除仍需保留先登记意图、原子文件替换/确认、崩溃恢复和身份校验，不能删掉 pending_file_deletions、probe obligation 等必要持久工作。

损坏/缺失的摘要不能“成功返回更少条目”，也不能在 HTTP 内暗中全库重建：报明确存储错误，管理员可运行确定性校验/修复任务。修复从本地已采用元数据与文件事实重建，不访问外部缓存/网络，不 stat 全盘；外部文件变化由扫描另行摄取。

## 6. 查询、索引和 JOIN 例外

### 6.1 常规列表：单表 SQL

```sql
SELECT media_id, title, year, poster_ref, file_count
FROM library_items
WHERE library_id = :library_id
  AND (title_sort, media_id) > (:title_sort, :media_id)
ORDER BY title_sort ASC, media_id ASC
LIMIT :limit;
```

示例对应索引 `(library_id,title_sort,media_id)`，采用非NULL排序键进行复合seek。首屏省去cursor条件；降序排序为避免混合方向的OR条件退化，使用与排序方向一致的复合seek（例如DESC/DESC），cursor模式同值按ID同向排序；旧offset协议保留现有tie-break。标题/上映时间/评分/时长/大小/分辨率/固定随机键按真实查询建复合索引，NULL映射必须保留既有排序语义。标题排序须与现有大小写/Unicode规则一致，不用SQLite NOCASE假装解决全语言排序。

cursor 包含排序类型、方向、过滤指纹和末项排序键/ID，服务端绑定 Library/User；翻页中有数据变化允许下一页反映新状态，不承诺跨请求冻结快照。现有 offset/StartIndex API 保留可用并如实报告深页成本；前端可切 cursor，不能悄悄破坏 Jellyfin 或 CLI 协议。

### 6.2 必要关联：有边界、有索引、分页前完成

| 场景 | 允许形态 | 禁止替代 |
|---|---|---|
| 详情的冷数据/当前页观看状态 | 本页 ID 列表批量主键点查；一 Media 的文件按 library/media 索引查询并分页 | N+1 每卡片读取；为一页读全库文件 |
| 类型/国家/分辨率/collection 多值筛选 | 对倒排表的索引 EXISTS/IN 子查询，或选择性高的 term 索引驱动关联 | 在大 JSON 数组上 LIKE；先分页后过滤 |
| 未看/收藏/观看排序 | 与当前 User+Library 的稀疏摘要做索引 LEFT JOIN/EXISTS/NOT EXISTS，缺失为默认 | 复制所有公共卡片给每个用户；只查有状态行导致漏未看项 |
| 精确 facets/count | 相同权限和谓词下 SQL COUNT/GROUP；可扫描该 Library 的窄摘要/倒排行 | 认为单表精确 COUNT 也是恒定时间；把全部候选送到 Rust |
| 多 Library 首页/跨库去重 | 先确定可见 Library，在 SQL 内定义按 Media 去重与首选归属再分页；必要索引聚合/关联 | 分别每库取一页后合并，造成重复、漏项和错 count |
| Probe/任务派发/删除操作 | 有界工作窗口与索引关联 | 为追求零 JOIN 删除必要的工作项/状态表 |

初始热索引以现有排序键为准，不为所有任意过滤组合建笛卡尔数量索引。复杂多条件/观看排序可能仍需临时排序或扫描本库候选；必须在基准中暴露，不谎称“所有分页都 seek”。全量精确 facets 是按库规模增长的管理/交互成本，后续是否缓存需测量再决定。

名称搜索保留现有精确/前缀/包含及Unicode语义；精确和可索引前缀用本地列索引，任意包含搜索允许扫描本库窄摘要并报告成本，不用引入分词后悄悄改变匹配语义。page与精确count在同一读事务内查询，避免一个响应的页与数量来自不同提交；多语句事务保持短，不在其中执行文件IO或等待外部请求。混合条件参数全部绑定，排序/term类别来自固定枚举。

ACL 先从配置得出授权 Library 集，再将授权条件带入列表/count/facets/搜索/选文件查询，拒绝参数中的越权 library_id；数据层无权访问的文件不参与派生选择。最终播放文件打开前再校验当前权限/生命周期。静态图片仍公开，Jellyfin 视频按协议鉴权，不能把两者混淆。

## 7. 全链路保留合同

| 链路 | 新读写路径及不可退化点 |
|---|---|
| 空库启动/首次配置 | 新 schema 原子初始化、五文件布局；可创建默认 Library/根、User、Site/Downloader，不能要求旧数据存在 |
| 搜索/发现/创建 Subscribe | 外部候选可仅活于 cache；只有识别/选择/Subscribe/扫描采用时建本地 Media；无需先有文件；Search/RSS 并发/Filter 政策不变 |
| 扫描/Watch directory/NFO 重导入 | IO→识别/版本验证→局部事务发布；full/incremental/movie/tv/video、断点、根不可用保护、人工认领均保留；Video 不强制 Metadata source |
| Transfer/Wash-cut | 默认目标/显式绑定、hardlink/copy/move、质量优先、pending 发布和崩溃恢复不变；旧文件在新文件可用且身份确认后删除，不能提前改卡片宣称已成功 |
| Scrape/刷新/本地图片 | 采用元数据与写 sidecar 分离；NFO/图片成功原子替换后摄取事实；失败不覆盖有效候选、不破坏用户锁；图片候选请求可直接走 cache |
| Browse/详情/gallery/facets/搜索/collection | 持久化摘要/单元/冷表，全部过滤和去重发生在分页前，保留现有排序/筛选/计数与图片剔除语义 |
| Playback/Jellyfin/收藏/继续观看 | 单元进度独立于文件版本/路径；同 Media 多库/多版本按权限和质量选文件；会话/历史/字幕/音轨/markers/章节/STRM不丢 |
| Probe/指纹/章节帧 | 阶段退避、partial、单文件版本 CAS、probe obligation/重启重试保留；不将 NFO 当探测成功 |
| Library 冻结/移除/根编辑/重导入 | 保留 ADR-0012 freeze→drain→scoped cleanup；按明确 ownership 清文件索引、不删字节/Media/观看；重建同别名 MediaId 可重连进度；嵌套库/相同Media的另一库不受损 |
| 维护/备份/清缓存 | 缓存清空只影响后续网络命中；Job定义/在途状态不能按缓存清理；备份按停写或受控一致性流程，不复制活跃DB文件冒充一致快照 |

## 8. 规模预算、增长与验收

**测量目标，不是本次已取得的性能结果。** 规模至少 Media=100k、文件=1M、Playback 单元=1M；覆盖一个超长 TV、同 Media 多版本/多库、多用户。普通请求 page_size=50，API 最大分页保持协议既有上限；所有解码/补充只限实际返回页或明确有界窗口。

1. 单条/普通第一页：EXPLAIN QUERY PLAN + VmStep/FullscanStep/Sort；固定目标库数据，在无关库文件从10k→1M时，热点 SQL FullscanStep=0，VmStep≤基线2倍，不把 B-tree深度增长误判为必须完全相等。
2. 相同目标库增大时分别测第一页、深 cursor、深 offset、稀疏多值、未看、last_played排序、跨库去重。只对可索引 seek 路径断言上述工作量；精确 facets、任意多条件与 offset 报告实际扫描/排序成本，不拿它们冒充恒定。
3. 记录 SQL次数、引擎步数/返回行数、Rust解码行数与字节、分配/峰值内存、锁等待/事务时长、WAL/DB/索引字节、写入吞吐和 p50/p95。禁止只量 SQL耗时或只看结果行数。
4. 100文件批量发布后摘要可立即读；一处更新仅触及对应 Media 的索引范围。进度心跳不改公共卡片/响应缓存；缓存 100k次更新/清空不写本地 items 或重算任务。
5. 对每一写入口断言文件事实、覆盖、terms、items、用户摘要的事务不变量；进程在提交前/后退出，重启不能出现事实成功但摘要永久旧。删除/根修改/重新识别与并发worker的发布竞态覆盖。
6. 保留/更新现有可见行为回归：筛选边界/排序NULL/同值tie、TV S00、whole与分集观看、无状态未看、同Media多Library、用户可见性、缺失prune、删除重导入、恶意/冲突NFO、坏响应/断网、STRM换URL、Probe旧版本晚到。
7. 空目录真实启动→配置→fake来源识别→扫描本地小视频/注入probe→浏览/图片→播放进度→收藏/Subscribe→Transfer/Scrape→重启→清cache→仍浏览/播放→移除并重导入。API/CLI/Jellyfin真实请求，web涉及页面需浏览器实际验证；不只跑Store测试。
8. 每个触及crate独立 `cargo test -p <crate>`，稳定树workspace一次；前端typecheck/test/build。现有无关失败报告实际证据；NAS设备/真实来源未运行必须写Not run。

设计阶段已做独立SQL烟测：Python SQLite 3.51.0内存库100k卡片，复合title cursor返回第50001起精确50条，EXPLAIN显示覆盖索引复合SEARCH；未看NOT EXISTS用User/Library/Media主键SEARCH，缺失用户状态行正确入选、过滤在LIMIT前。仅证明该SQL形态可执行及这组语义，不是Rust bundled SQLite、真实应用或NAS性能验收，后者均未运行。

增长说明：`library_items` 随实际 Library×Media归属增长，不随访问次数增长；terms随这些记录的标签/collection成员增长；用户摘要随有状态用户及相关归属增长，不预铺满；file/unit表随实际文件与覆盖增长。操作/会话历史有既有保留策略，持久队列成功ACK后可裁剪；响应缓存仍按独立维护政策清理。没有通用浏览事件日志不等于所有业务表可以无保留策略地积累。

## 9. 切换、实施边界与交付门槛

- 这是需要更新 ADR-0008 的新决策：同一事务需求优先于原先把 Media/Library配置/Playback单元分散在不同文件；不修改已接受的 Media 内部身份和文件删除安全合同。
- 用户批准设计后再编写新的完整实施计划，不执行旧缓存计划。先定新DDL和领域/Store事务接口，再改写链路、全部调用方和测试；不能只改表后让旧服务勉强启动。
- 实施分为可验收批次：①新schema/事务基础；②来源缓存与元数据摄取；③扫描/Transfer/Scrape/探测写路径；④Library浏览/详情/统计；⑤Playback/Subscribe/collection/删除与根变更；⑥空库全流程、性能与旧路径清理。**这些是依赖分解，不是允许交付半套功能。** 全部通过后才切换用户实例。
- 删除旧投影/通知表与代码之前，枚举所有修订计数/全量fallback/投影依赖者，迁完播放页、首页、CLI、Jellyfin与测试；不能仅删cache触发器。若审计发现独立用途，迁成该用途的明确局部版本/状态，不保留误导兼容别名。
- 所有公共API/DTO默认保留；有意变化（locale采用规则、消除任意语言最新缓存、显式归属）在计划中列出消费者映射并更新文档/测试，不悄悄省略原筛选能力。
- 真正切换先停所有写进程，列出实例真实路径与SQLite/WAL/SHM后按授权删除旧DB；只删明确SQLite文件，不删视频、字幕、NFO、图片、Indexer YAML、下载目录、编译缓存。启动自动建新库，不在每次启动清库。
- 新结构使用自身schema版本和固定布局标识；未知/较新布局拒绝打开，不尝试把旧表就地猜成新表。未来变更正常做前向schema版本升级，本次不写旧数据迁移器。
- 本次文档交付不改现行 `CONTEXT.md`/ADR为已生效状态；批准并实现时同步术语/数据库字典/部署说明/变更记录。现有缓存旧实施计划标为已取代，避免执行者继续安装journal。

## 10. 尚需实测而非臆断的取舍

- 同库保证局部原子性，代价是Playback心跳、扫描、元数据写共享SQLite写锁；必须测高并发心跳+扫描。先短事务/窄热行/合批，不先引入分布式消息。
- 冗余items/terms/用户摘要降低读成本，增加受影响Media写成本；很多文件/用户的单Media更新是重点，不声称“无JOIN且零写放大”。
- 必要的多值/用户状态索引关联比冗余整个用户卡片集更易维护；接受此明确例外，依据计划/实测优化，而不是按JOIN关键字数量判性能。
- 基准若不达标，调整索引、摘要范围或按用例拆查询；不得通过取消精确计数、去掉过滤/权限、删掉Playback功能来达标。任何进一步产品语义变化需要另行批准。
