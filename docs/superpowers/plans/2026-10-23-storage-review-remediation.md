# 存储重构剩余修复技术方案

状态：2026-10-24，基于 `feat/storage-redesign@cf7fb70` 的**只读复审方案，本轮未实施**。比较本地 `main@86122d7`，远端 `origin/main@14fdbb6`；基线区别、发现及证据见[当前复审](../../audits/2026-10-24-storage-rereview.md)。本文件是唯一当前执行入口；旧全文可用 `git show cf7fb70:docs/superpowers/plans/2026-10-23-storage-review-remediation.md` 读取。

保留五文件、显式归属、事实/摘要同事务、冷详情与持久意图。不新增旧库导入、兼容视图、双读双写、旧字段别名或接口转发 shim。静态图片 public，Jellyfin 按协议鉴权及输入输出。以下是后续方案，不是本轮写代码或操作实例的授权。

## 优先顺序

| 顺序 | 任务 | 发现 | 完成标准 |
|---|---|---|---|
| 1 | Jobs 原子初始化与单一布局入口 | S01 / P1 | 新库阶段故障可重试；旧/未来库拒绝且不写 |
| 2 | 根迁移持续驱动、生命周期与有限调度 | S02 / P1 | 解冻后自动收敛；暂停根不阻挡其他工作 |
| 3 | 删除旧目录聚合写接口，同批迁移 Web | S03 / P1 | 根只走 operation；实例设置只写目标字段 |
| 4 | 自有 Playback 强制签发绑定 | S04 / P1 | 缺/坏 session 不写事实；新 Web/Jellyfin 通过 |
| 5 | 删除 Downloader JSON 回退及旧分支 | S05 / P2 | 仅旧 key 不得选出 Downloader |
| 6 | 删除链使用显式归属并逐批放锁 | S06 / P2 | 无关库坏路径不影响目标；并行读取有期限 |
| 7 | Unidentified 全链消费登记身份与有界 seek | S07、S10 / P2 | 按库返回准确；候选工作量有界 |
| 8 | 协议索引按 dirty card 批末更新 | S08 / P2 | 同一卡片不逐文件重建 terms，最终语义等价 |
| 9 | 分类查询、count/facets 与连接策略测量 | S09 / P2 | 当前版本 5k–100k 曲线、锁时间和语义门齐全 |
| 10 | 故障区分性、现场和完整规模验收 | 独立验收门 | 自动化与现场结果分别记录 |

步骤 1–7 先处理正确性、无旧版兼容和阻塞边界，8–9 再优化吞吐。独立切片可并行；公开 DTO/DDL 的生产者、消费者和测试必须同批迁移，不能交付兼容中间态。

## 1. Jobs 统一初始化（S01）

让 Jobs crate 拥有布局标识、唯一 DDL、必需对象和只读校验。API 不再预写空身份头、再另开连接建表。Jobs 不依赖 api/store；API 备份协调器组合 Store 与 Jobs 的公开检查，Store 只拥有自身四库，不新增 Store→Jobs 反向布局依赖或复制对象清单。

- 已存在非空文件先只读检查，旧/未知/未来布局拒绝，不先改 header/journal/索引；当前布局重开只校验。
- 空布局在一个 IMMEDIATE 事务内创建表、索引、触发器、单例及身份头；错误整体回滚，重试等价于首次创建。
- 删除旧 `jobs_live_def` 清理及重开时“缺对象就 CREATE”的升级路径；不为半初始化坏头增加兼容认领。
- 公开行为先 RED：DDL/索引/触发器/身份头各阶段故障后重开，独立 Queue 与 API 交叉重开，旧/未来库零写拒绝，当前 Job 定义/在途事实保留。

## 2. 根迁移 driver 与锁边界（S02）

复用 `pending_root_reassignments` 和现有 Library operation/lease。当前 pending 只有 kind/root/cursor；需补稳定操作身份、配置版本、阶段/错误与公平调度状态，或纳入已有 Library operation 的持久子阶段。不加通用队列，不能只把无限 drain 搬进线程。

- blocking 预算池锁外准备 owned canonical 身份与原配置快照，短事务校验并登记工作。
- 启动、新根提交、重配置/冻结解除、删除终态触发，并有低频兜底；每次最多一个 100 文件窗口，批间放 Store、yield、检查取消/停机。
- 暂停根保留 cursor，轮转其他 ready 根；源/目标删除或配置再变更时重验当前最深根与生命周期。完成前复核 cursor 前新写入。
- 收敛前返回可追踪 operation，不能根生效而文件永远停留旧库仍报告完整成功；保留旧 scope/文件版本屏障。
- Watch 的归属 realpath、图片 is_file、fs watcher 根 canonicalize 一并移锁外，提交重验原 scope。

验收：真实 ApiState 并发 locale/create，不手动 drain、不重启就自动收敛；队首 frozen 加独立 ready 根、多 pending、重复通知、停启/删除/配置变化。门控实际 canonicalize/NFO/rename 阶段，证明并行 Store 读、Tokio 心跳和退出有期限；记录最大持锁时间、事务 p95、busy/WAL。无锁环证据时报告阻塞/饥饿。

## 3. 配置入口与无兼容清理（S03、S05）

删除旧 `/directory` 聚合写、`/directory/roots` 写及 `apply_legacy_naming/set_legacy_naming`。先枚举 `directory.ts` 全部生产消费者；有产品用途的读面改为明确的新 DTO，不能保留旧形状别名。

- Transfer mode/Watch 使用只带目标字段的严格 DTO，app 单事务 CAS；明确 missing=不改、null=清空、值=替换。修复当前 `setWatchIntake(null)` 无法关闭的问题。
- 根只走 `/libraries/{id}` 持久 202/轮询；Web 消费 operation，不直调默认根 setter。
- Naming template 统一当前 Scrape 字段，删除旧组合字符串反拆；不混写 app 设置与 library 根。
- 验收修改 Transfer/Watch 不改变根、命名、Scrape；旧 GET 快照不能覆盖并发新根；停用 Watch 后运行配置确实为空；旧 body/route 拒绝且零写。根更新的归属/NFO/版本和重启守恒继续覆盖。

Downloader 删除 `settings['downloader']`、SavedDownloader/from_saved 及专属旧测试；保留明确支持的环境覆盖和当前 `downloaders` 行。只有旧 key 时报错，不导入成新行。覆盖有效/缺失默认行、环境覆盖和显式 Downloader。

同时清理当前 NOT NULL root/file ID 的旧 nullable 回退、browse_reads 未参与 SQL 的旧谓词准备和失效注释；不凭 `legacy/fallback` 字样机械删除 locale、元数据合并等仍合法的业务规则。

## 4. Playback 只提交签发绑定（S04）

自有 Web progress DTO 将 `play_session_id/event_sequence` 设必填；所有事件先通过当前 session API 固定 target/source。handler 严格字段校验，Store 统一验证 user/device/session/generation/target/source/config/ACL。

删除自有入口的无 ID 分类、临时签发/复用和 `stage_unbound_event`；共享 classify_legacy 中协议仍需的分类下沉至 Jellyfin adapter。Jellyfin 按协议建立绑定再走同一 Store 提交，保留合法协议行为与鉴权；不能让自有旧入口继续补当前版本。

把“无 session 应 200 并写事实”回归替换为拒绝且原始表零写；覆盖 r1 准备事件→r2 换源→迟到提交不得将旧 position 记到 r2，缺失/空/伪造 ID、乱序/重复、跨设备/目标、管理员终止及重复 Stop；新 Web/Jellyfin 完整链正常。源感知恢复、未知 duration 和逻辑已看时钟沿现行合同。

## 5. Library 删除按登记事实工作（S06）

禁止全实例 `list_file_coverage` 解码、对所有同 kind 文件 realpath、默认库补缺和 Media+坐标猜会话。先短事务 freeze，再按目标 library_id 索引和登记 file/root/path/version 分批捕获快照，持久游标推进清理；API 每批真正放锁、yield。

磁盘观察仅针对目标且锁外执行，提交重验身份/配置。session 按当前 NOT NULL file_id 有界关闭，删除空 ID 回退。保持末库、嵌套根、Subscribe 不可用、Media/User Playback/Collection 和磁盘字节保留合同。

验收空 A + 坏/慢 B 不影响 A；1M 无关文件不放大目标工作；目标变化拒绝、每批重开守恒；并行 Store 读/心跳限时。SQL 分批与 Mutex 分批必须分别证明。

## 6. Unidentified 登记身份与有界选择（S07、S10）

固定登记 path_key 并建真实范围索引，display path 仅呈现。列表、过滤、Claim、根迁移全链消费持久 library_id/root_id；删除 Movie→TV/default 的猜归属路径。新增字段按本次新布局，不加旧行补录兼容。

选择 SQL 使用 root 范围 seek 与持久 keyset/phase，重验最深根、两侧生命周期和配置；最终覆盖 cursor 前新增。除每事务移动≤100行，还需候选读取、VM steps 和持锁时间界限。

验收 TV/Movie/嵌套根列表和按库过滤正确；Claim 固定原 Unidentified 身份/登记归属作为待认领来源，合法 IdentifyAndTransfer 按明确动作/目标库生成新归属并原子移除原记录；5k/50k/100k 无关/已归属记录、冻结项下记录 EQP/VM/候选数/事务 p95。至少 301 文件、同 Media 多目录及 work/season/episode NFO，每批重开比较 locale/provenance/export digest 与归属守恒。

## 7. 写放大与分类查询（S08、S09）

协议 terms/search_text 同步从逐文件链移到同事务批末，按 `(library_id,media_id)` dirty 集合去重，或只在元数据/genre/NFO 实际变化时差量更新；不能移到事务外留下陈旧索引。保留已有用户摘要批末去重、全部 locale 安全预筛和精确谓词。

先验证公开 publish_files 的人物/分类/搜索/locale 等价，再约束索引写量随不同卡片或 term 变化增长；100 文件同 Media 不应重复插入 10000 个不变 person terms。覆盖首卡、多 Media、NFO、元数据刷新、改挂和删除。

读取分别记录 Media kind 与 genre；固定 ACL/locale/跨库去重/稳定排序/精确 total。5k/20k/50k/100k，大/小命中、首页/深 offset、count/page/facets 分测 EQP/VM/p50/p95/最大锁时间及数据形状。genre 已有 terms_value，先消除重复解析/查询，再评估同库有序成员索引或局部读模型；keyset 不消除精确 count，不准改近似 total。

kind 先在授权 Library roster 按 IncludeItemTypes/粒度/IsFolder 排除不可能命中的库，覆盖混合类型语义；固定 Movie、增加无关 TV 的曲线不能继续线性放大。QueryWorkStats 需纳入 count 等辅助 statement 并汇总请求，当前仅 page counter 不能当端点预算。

facets 必须测已选 genre/多维条件，不能用无筛选历史 Q3b 关闭。考虑同一快照一次计算各维度命中，再按“排除自身维度”聚合，保留零计数候选/精确 total；若缓存需绑定 revision/User/locale，先证明并发一致性。person/search 没有新证据要求直接引入 FTS。

cache NORMAL、mmap、cache_size、temp_store、WITHOUT ROWID 做单变量 A/B，记录耐久/RSS/磁盘/WAL/写吞吐；Jobs FULL 和连接级回归已完成。独立 media ResponseCache synchronous 未显式设置，另行测量，不把构建默认当决策依据。

## 8. 验收与文档门

- 原 A8 竞争写者行为 RED、A12 NFO export 区分性失败 RED 单独补齐；缺反例不能直接判生产行为错误。
- 已修 Marker/指纹源版本、marks 严格输入、alias 图片、seed 原子性、Claim/NFO 观察和测试退出反例继续保留，不重新列待办；失效“未实现/仍失败”按证据撤下。
- 真实浏览器/Jellyfin 设备、NAS、完整停启/清 cache 与 100k Media/1M 文件/1M Playback 组合目前 **Not run**。隔离实例记录版本、数据形状及原始结果，历史 50k 或无关库 1M 不能替代。
- 后续实施按公开行为 RED→GREEN，满足 AGENTS 文件/函数限制和结构化日志；受影响 crate 独立测试后 workspace，前端 test/typecheck/build。只因新失败/改动或未决风险扩测。
- 本轮仅 docs；未来实施按用户请求执行。生产 DDL 是唯一可执行结构来源；完成项从本页移除，证据留固定版本审计，不再生成互相覆盖的步骤文档。
