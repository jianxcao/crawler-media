# 数据与存储性能设计

本设计说明当前数据布局、查询成本和并发边界，供后续优化、代码审查和测试使用。字段和约束见[数据库字典](../database/README.md)，可重复的测量方法见[存储性能测试](../testing/storage-performance.md)，片头片尾与章节图像见[标记优化设计](marker-optimization.md)。本文不代表某次验收结果。

## 数据职责与生命周期

SQLite 是业务事实来源。数据按生命周期拆成五个文件，同一业务事务需要共同发布的 Media、文件、浏览摘要和 Playback 数据放在 `library.db`。

| 数据库 | 负责的事实 | 生命周期 |
|---|---|---|
| `app.db` | User、凭证、Site、Downloader、Filter、实例设置 | 控制面持久数据 |
| `library.db` | Media 身份和已采用元数据、Library/根、文件与覆盖、浏览摘要、Collection、Playback、文件操作与探测状态、Scrape/Transfer 配置和 Naming template | 媒体业务持久数据 |
| `subscribe.db` | Subscribe 策略、下载/搜索轮次、覆盖及跨库操作状态 | 订阅业务持久数据 |
| `jobs.db` | Job 定义、运行、重试、并发键和恢复状态 | 任务持久数据 |
| `cache.db` | Metadata source 的外部响应及其缓存索引 | 可通过受控维护清理的缓存 |

清理响应缓存不能改变本地 Media 身份、已采用元数据、图片地址、Library 列表、收藏或播放进度。活跃 SQLite/WAL 文件不能直接被 unlink。业务表、任务表和恢复记录不属于响应缓存。

每个数据库独立提交。跨库流程使用显式操作、引用代次、幂等步骤和版本复查，不承诺跨库原子性。布局校验严格拒绝不支持的数据库结构，不推测字段、不自动清库。

## 身份、文件与读模型

Media 使用内部身份；外部来源别名包含来源、Media kind 和外部 ID。TV 的季和集属于一个 Media 的逻辑单元，不另建作品身份。文件路径和 TMDB ID 都不是 Media 主键。

文件在写入时确定 `library_id`、`root_id` 和 `media_id`。根归属采用目录组件边界和最长唯一匹配，歧义拒绝。普通列表查询不重新判断全库路径归属，也不调用文件 stat、解析 NFO 或等待外部来源。

| 结构 | 用途与增长范围 |
|---|---|
| `library_files` | 每个物理文件的权威事实，保存归属、发布/存在状态、源版本及热标量 |
| `file_details` | 文件音轨、字幕、章节等冷详情，按文件点读 |
| `library_file_units` | 文件到逻辑单元的覆盖；支持一文件多集和一集多版本 |
| `library_units` | Library 内去重单元及首选文件摘要 |
| `library_items` | 每个实际有馆藏文件的 `(library_id, media_id)` 一张卡片 |
| `library_item_terms` | 类别、国家等多值筛选的倒排索引 |
| `playback_units` | 每用户、Media、逻辑单元的观看、收藏和恢复事实 |
| `user_library_items` | 有相关状态的用户在 Library 中的卡片摘要 |

一个 Media 有 12 集、每集两个版本时，可以有 24 个文件、24 条覆盖、12 个去重单元和一张卡片。文件覆盖多集时大小只按文件计一次；不同路径的 hardlink 仍是不同馆藏文件。没有可靠分集编号的文件可被登记，但不能据此推导已覆盖某集或 Subscribe 已完成。

热行不携带完整文件数组、整部 TV 的分集数组、演职员详情或上游原始响应。当前页的用户状态和冷字段按主键批量读取，避免逐卡片请求或解码整库 JSON。用户摘要只为实际状态创建，缺失行代表默认未看、未收藏；未看查询必须包括这些行。

浏览摘要、去重单元和倒排表增加写侧工作，以减少反复读取和归约。它们按受影响的 Media/Library 范围更新；不能用表数量少或没有 JOIN 推导性能良好。

## 元数据采用与 NFO

外部响应缓存和本地采用值分离。详情读取本地采用元数据，来源快照负责重新合成和追踪字段来源；刷新失败保留有效采用值。演职员等冷数据按 Media、实体坐标和 locale 存放。

同实体、同语言的字段采用遵循人工锁、本 Library 的外部 NFO 输入、固定来源优先级和基础值的顺序。work、season、episode 坐标分别处理，分集标题、简介和时长不能冒充作品字段。不同 Library 的 NFO 覆盖不互相污染；缺失、显式空和人工清空需要分别表示。

NFO 未声明语言时，在导入时绑定 Library 当时的显示语言并持久化。改变显示语言通过有状态的重算流程完成，不把既有 NFO 解释为新语言文本。

Scrape 导出记录包含实体、locale、文件摘要及字段来源。重扫相同的自产 NFO 只确认文件存在，不把它提升为人工覆盖；实际修改的字段才形成外部输入。无法读取、XML 无效和确认删除是不同结果：未知 IO 或无效输入不能清除最后有效值。文件替换与导出登记之间的崩溃依赖持久意图恢复。

NFO 中的演员头像保存可供外部工具使用的地址，不嵌入图片，也不写本实例的临时代理地址。TMDB 相对头像路径在来源边界转换为完整 URL，通用 NFO 读写保留其他合法外部 URL 或本地路径。应用的图片代理及磁盘缓存是独立交付路径。

## 查询成本与授权

所有授权、过滤、去重和排序必须在分页之前完成。精确数量与页在同库读快照中读取。不能先取一页，再在 Rust 中过滤不可见项或已看项。

| 查询 | 主要工作范围 | 需要单独测量的边界 |
|---|---|---|
| Library 普通卡片页 | 以 Library 和排序键定位的摘要页，页内批量水合 | 稀疏条件可能检查多于返回页的候选 |
| 作品/分集详情 | 指定 Media、季和分页单元 | 超长 TV、同单元多文件 |
| 选播放文件 | 当前逻辑单元的覆盖索引及候选文件 | 选定后仍复查权限、生命周期和版本 |
| 收藏、继续观看、NextUp | 用户摘要及定向单元事实 | 无状态默认值、跨库可见性、多用户隔离 |
| 名称搜索 | 本地搜索索引与包含匹配校验 | 短词、Unicode、前缀/包含和真实结果顺序 |
| facets、精确 count、Library 统计 | 相同权限/谓词内的窄摘要与索引聚合 | 可能随本库规模增长，不宣称恒定成本 |
| 跨库协议列表 | 授权范围内的实体、计数、去重和排序 | 深 offset、复杂排序和去重的实际工作量 |
| Jobs/Subscribe 页面 | 有界定义/运行事实及批量读取 | 终态历史增长、开放季数据和缺失关联 |

自有列表 cursor 绑定 Library、User、过滤及排序语义，包含稳定排序键和 ID。它避免可索引路径的深 offset 工作，但不保证多次请求之间冻结全局快照。Jellyfin `StartIndex` 是协议能力，仍需测量深页成本，不能用自有 cursor 的结果替代。

多值筛选通过索引关联完成，不在热请求中对大 JSON 做逐行搜索。全文搜索使用本地 FTS5 trigram 等适用索引，同时保持短词和 Unicode 行为。稀疏用户状态的关联必须同时保留“缺行即默认”的语义。

授权 Library 条件同时约束列表、搜索、count、facets、历史、统计和播放选源。播放历史与统计从可见性索引判断，不先构造所有可见 Media 再逐条遍历历史。私有 Collection 成员不能进入公共分面或公共倒排集合。

## 读写并发与短事务

`Store` 共享 `Arc<Writers>`，app、subscribe、library、cache 每库一个 writer 连接和独立互斥锁。`jobs::Queue` 独占 jobs writer。实际数据库 writer 不允许嵌套；同库重入、跨库嵌套在 debug 和 release 中都返回锁序错误。

API 读通过 `store_read` / `store_read_handle` 进入阻塞工作预算和只读池。容量固定为 app 4、subscribe 2、library 8、cache 2；Jobs 使用独立容量 2 的只读池。`Store::with_read_snapshot` 声明本阶段使用的库，按 library → app → subscribe → cache 准入，同阶段每库复用一个 lease/快照。不同数据库的快照不构成分布式原子快照。

API 写通过 `store_write`，`STORE_WRITE_WAIT` 为 1 秒。真正获取目标库 writer 后复查写准入；准入预算覆盖本次写作用域，不能在闭包内放网络、文件操作或大块序列化。Jobs reservation 是有界准入票据，不占实际 writer，也不能被当作已获得写连接。

等待 writer 失败与已经启动的 blocking worker 超过响应截止是两种结果。后者可能仍完成提交，客户端必须将其视为结果未知，并通过操作身份和状态读取确认，不能因为 HTTP 返回忙就假定写入一定没有发生。

SQL 和必要的事务约束留在 writer 内；源观察、NFO 解析、网络请求、文件操作、哈希与大 JSON 编解码在锁外准备。读快照复制原始数据后应尽快释放 lease。锁和 lease 不跨 `.await`、网络或文件 IO。

SQLite WAL 允许只读连接和 writer 并行，但同库仍只有一个写事务。扫描、Transfer、删除、探测发布、收藏和播放上报会竞争 Library writer。有限批量是成本边界，不是事务时长保证；COMMIT、WAL 同步、设备等待和调度仍可能产生尾延迟。

## 发布、恢复与删除

扫描、Transfer 和探测发布共同遵守“锁外准备、事务内复查并局部发布”：

1. 读取并固定 Library/root 配置、文件版本、覆盖和操作身份。
2. 在锁外观察源、解析或执行 IO，准备不可变结果。
3. 进入短 Library 事务，复查生命周期、文件 revision/source version、claim 和所需引用。
4. 同事务更新文件事实、覆盖、相关摘要、必要用户摘要及持久工作义务。
5. 提交后继续有界工作；输入失效则放弃过期结果并按业务状态重排。

相同源版本的扫描确认避免执行语义不变的 UPDATE，减少索引维护和 WAL 写放大。真实源变化仍参与版本屏障；不能通过跳过检查省工作。STRM 内容摘要属于源身份，不能只比较文件大小和修改时间。

探测派发、删除、校验和保留清理使用有界窗口及持久游标，避免一次装入整库。已发布与 pending 文件分开；卡片和播放不能提前承诺尚未完成的文件。presence 的 present、missing、unknown 分开，根不可达、权限失败不能当确认缺失。

磁盘和 SQLite 不具备共同原子提交。Transfer、Scrape、重命名、删除及章节图像使用意图、不可变产物身份、源校验和恢复步骤，在重启后继续或拒绝过期工作。

删除 Library 严格遵循 [ADR-0012](../adr/0012-library-removal.md)：先隐藏并关闭发布门，立即取消该库工作，再删除索引与清理孤儿 Media；不等待慢 worker 完成后才开始删除。晚到结果不能复活该库。磁盘媒体、STRM、字幕、NFO 和图片保持原有删除操作合同。

Media 没有文件且无 Collection/Subscribe 等保留引用时，清理其收藏、观看和进度事实；其他 Library 仍有文件或存在保留引用的 Media 不受误删。跨库 Subscribe 绑定清理有独立提交和恢复步骤，未完成时公开状态不能提前报告全部清理完成。具体历史日志和保留引用规则以 ADR、数据库字典及实际事务入口为准。

## Playback 与客户端一致性

逻辑 ItemId、物理 MediaSource、播放会话和文件时间线分别表达不同身份。一个多集文件不能让两集共享同一个逻辑播放目标；不具备单元到物理时间映射时不能猜测跳转位置。

恢复包同时绑定位置、时长、file ID/revision、source version、timeline 和轨道版本，不能拼接不同文件来源的字段。普通播放上报校验已签发的用户、设备、session、generation、目标单元、源文件及当前权限；迟到的停止/心跳、已替换文件和其他分集事件不能写错进度。

Web 与 Jellyfin 客户端使用同一持久观看事实。收藏、已播放断言、播放活动时间分别记录，收藏操作不伪造观看活动。用户写入只归约当前用户和受影响 Media/单元：episode 定向，season 按该季实际覆盖，whole 按作品范围；不扫描其他用户或重建公共卡片。

文件改挂与 canonical Media 合并是不同业务。前者只改指定文件归属，不移动另一作品的全局用户事实；后者通过显式冲突决议、冻结和可恢复操作合并身份、元数据及跨库引用。不能用任意外部别名或通用批量 UPDATE 替代合并合同。

协议鉴权、进度上报、恢复、收藏、已播放、隐藏库和详情字段必须通过公开 HTTP 与真实客户端共同验证。单独 Store 用例不能证明客户端发送了所需事件。详见 [Jellyfin API 合同](../api-contracts/jellyfin.md)和[系统回归](../testing/system-regression-cases.md)。

## 网络与图片的有界成本

Library 本地展示读取不以可选 catalog 回源为前提。确需外部数据的路径保留独立的网络、准入、截止和取消控制，不在数据库锁内等待。

TMDB 请求采用有限尝试和共享上游总预算，429 的退避服从截止；永久错误不无限重试。可用 stale、typed NotFound、共享请求失败与单个观察者取消分别处理。策略和可观察用例见 [TMDB 请求回归](../testing/tmdb-retry-regressions.md)。

图片读取复用本地缓存和共享 fill，保持 fetch/read 容量 8、pending 容量 64。每个观察者持有自己的两秒预算；加入正在执行的 fill 不继承首个观察者已经过期的截止。首次 fill 的排队/开始回源仍受原准入截止控制，真实 worker 结束前继续占用名额与注册项，不因 HTTP 超时而重复启动下载。

图片超时诊断区分读准入、blocking 排队、缓存读取、fill 名额、上游 headers/正文、磁盘预留和发布。504 本身不能证明网关、DNS、TLS 或上游故障。观察者超时和共享 fill 最终结果通过不透明 ID 关联，日志不输出凭据、URL query 或敏感路径。

`/posters/*`、`/fanart/*`、`/stills/*`、`/chapters/*` 和 Library cover 保持 public router，供不携带 Authorization 的图片请求使用。视频流按 Jellyfin 协议鉴权，两种权限合同不能互换。

## 优化证据与持续边界

后续优化同时证明结果正确、工作量下降和端到端体验，没有一种指标可替代其他两种：

- 查询使用 EQP、VmStep、FullscanStep、Sort、SQL 次数和解码量判断增长范围；固定目标数据，增加无关 Library、用户或历史，检查是否产生无关工作。
- 写入记录准入等待、writer 等待、SQL、COMMIT、WAL/文件同步和总耗时；有限输入批次不能推出最大持锁时间。
- 容量覆盖十万 Media、百万文件和百万 Playback 单元；大 Media、多版本、多用户和稀疏筛选单列。
- 内存按冷启动、重复 Scan/Transfer 批次、队列排空及静置阶段记录。RSS 变化需区分活跃对象、SQLite/TLS/分配器保留和 OS 缓存，有限观测不证明无限运行无泄漏。
- perfgate 区分 idle、busy、cold，计时包含完整正文与异步结果完成；实际首次外部工作、缓存命中、空结果缓存和 pending 不能混作同一基线。
- 真实 Library 使用隔离数据库和 STRM 副本，前后核对原始数据与 sidecar 清单。Web/VidHub 的播放、搜索、列表和详情证据独立记录。

精确统计、复杂跨库排序、超长作品归约、外部来源延迟和 SQLite 提交尾部仍有真实成本。任何优化都不能通过减少授权过滤、放松结果正确性、改变远程读取限制或删除业务功能通过门禁。测试方法和报告边界见[存储性能测试](../testing/storage-performance.md)与[性能 SLO](../testing/performance-slo.md)。

## 实现导航

| 职责 | 入口 |
|---|---|
| 布局与连接 | `crates/store/src/schema/v1/`、`crates/store/src/open.rs` |
| writer / reader / 快照 | `crates/store/src/writers.rs`、`writer_rank.rs`、`library_read/`、`read_scope.rs` |
| HTTP 存储准入、Jobs 读取 | `crates/api/src/http/store_wait.rs`、`crates/api/src/jobs_readers.rs` |
| 文件发布与局部归约 | `crates/store/src/library_write/`、`crates/store/src/scan_seen/` |
| 列表、详情、协议查询 | `crates/store/src/library_query/` |
| 元数据采用与 NFO | `crates/store/src/metadata/` |
| 播放事实与源校验 | `crates/store/src/playback/`、`crates/api/src/media_server_provider/` |
| 删除与跨库恢复 | `crates/api/src/library_lifecycle/`、`crates/store/src/library_deletion/`、`library_write/orphan_media.rs` |
| 图片交付与观测 | `crates/api/src/http/image_proxy/` |
