# SQLite 是系统记录，按生命周期拆分

> 固定审核基线及后续修复状态见[2026-11-02 存储审核](../audits/2026-11-02-storage-redesign-review.md)，运行验收见[最终验收清单](../superpowers/plans/2026-11-02-storage-redesign-final-acceptance.md)。历史缺陷与测量只描述其记录版本。

状态：接受；五文件边界保留。未合并的开发中间布局已收敛为唯一生产定义，五文件当前身份版本均为 1；这是当前结构标识，不是旧库迁移支持。字段/索引见[数据库字典](../database/README.md)。每库单 writer 与统一只读池已落地（见「读写分离」一节）。

## 决策

使用行存 SQLite 持久化 User、Site、Downloader、Filter、Subscribe、Media、Library、Playback、Job 与实例配置；视频、字幕、NFO、图片和 Indexer YAML 留在磁盘，行保存路径及身份。cache 是外部响应的可重建副本，不是 Media/Library 或已采用元数据的第二事实来源。

| 文件 | 现行职责 | 可清空 |
|---|---|---|
| `app.db` | User/token/凭证、Site、Downloader、Filter、通用设置、搜索快照、实例锁 | 否 |
| `library.db` | Media/别名/采用元数据、Library/根、文件/覆盖/浏览摘要、Collection、Playback、Probe/Scan、Transfer/Watch 与 Scrape 配置、持久文件和库操作 | 否 |
| `subscribe.db` | Subscribe 策略、逐槽事实、待下载记录 | 否 |
| `jobs.db` | Job 定义/调度配置、执行/重试、终态历史及保留游标 | 否；仅按规则清理终态历史 |
| `cache.db` | 六个 Metadata source 的原始 HTTP 响应，按来源分表 | 是；连接内受控清理或停写关连接后重建 |

默认目录为 `CRAWLER_MEDIA_DATA`；Store 管理 app/library/subscribe/cache 四文件，Jobs Queue 管理 jobs。`open_at` 只允许 app 指定其他路径。跨文件没有 FK 或原子提交；User 删除、Media 合并、文件跨分区引用等必须通过各自持久操作恢复，不以多次提交冒充一个事务。

## 同库边界与代价

Media 身份、采用元数据、Library/根、文件事实、覆盖、卡片、倒排与 Playback 放在同一 `library.db`，使事实和受影响摘要可以同事务更新。Media 可以没有文件；最后一个 Library 文件删除后，若没有 Collection 或 Subscribe 保留该 Media，则清理孤儿 Media 及其 favorites、观看状态和进度。仍被其他文件或上述业务引用保留的 Media 按 [ADR-0012](0012-library-removal.md) 处理。

文件显式持有 `library_id/root_id/path_key`，归属在写入时确定并重验当前最深根，普通读消费登记事实。原始 Playback 单元与物理源绑定恢复包区分；文件版本 CAS 阻止旧工作发布。冷轨道/章节不复制到完整浏览投影；卡片、协议 genre/person 成员、静态分类边际计数与稀疏用户摘要是必要冗余，写路径负责同事务差量维护，Integrity 操作负责从原始事实修复。

每库只有一个 writer 连接及其独立 Mutex，SQL 分批 COMMIT 不自动释放该锁；读请求通过统一只读池借出连接，同阶段每库复用一个 lease 和快照，不取得 writer 锁。磁盘/网络观察须锁外进行，根迁移、删除与修复需要持久游标、公平调度和有界窗口。静态分类计数、协议实体与用户摘要通过投影维护，列表使用 keyset 分页；过滤组合的实际成本仍须以规模测试和性能闸门验证。历史测量只代表对应版本，不能据此推断当前行为或性能。

## 初始化与连接

生产 DDL 只有 [Store v1](../../crates/store/src/schema/v1/)、[ResponseCache](../../crates/media/src/response_cache/mod.rs) 和 [Jobs 唯一 DDL](../../crates/jobs/src/schema.sql)；本文不复制第二套 SQL 或模板字段列表。

Store 对空库原子创建当前结构和身份头，当前布局可重开，旧/未知/未来布局拒绝；不导入旧数据、不建兼容视图、不双读双写。Store 核对 application_id/user_version，并从唯一生产 DDL 生成表、索引和视图定义清单，对照 sqlite_schema（包括表内约束），拒绝缺失、定义不同或额外对象；诊断/故障注入 trigger 不属于该结构清单。Jobs 同样从唯一 DDL 校验对象定义和单例。新默认 Library/root 已同事务初始化，正常重开不会复活用户移除的默认根。

连接显式 WAL、5000ms busy_timeout、foreign_keys=ON。app/library/subscribe 与 Jobs Queue 显式 synchronous=FULL；Store 的可重建 cache 使用 NORMAL。当前连接策略还设置 cache_size=-16384、temp_store=MEMORY、journal_size_limit=67108864、mmap_size=268435456；空库在 WAL/建表前设置 auto_vacuum=INCREMENTAL。实例 app 心跳 busy_timeout 为 10000ms。独立 media ResponseCache 仅显式设置 WAL/timeout/FK，策略统一尚需测量。页缓存和 mmap 是每连接预算，temp_store 与 journal_size_limit 不提供全进程内存/WAL硬上限；不把构建默认当耐久性合同。

Jobs crate 拥有唯一布局入口，真正空文件在一个 IMMEDIATE 事务内创建全部对象、单例和身份头；独立 Queue 与 API 共用。已有非空文件只读预检，旧/未知/未来或损坏当前布局拒绝且不修补；保留事实队列及备份提示。没有 Store→Jobs 生产依赖。

旧 `/directory` 聚合/roots 写、组合 Naming 拆解、Downloader JSON 回退和自有无签发会话 Playback 已删除；前端与 CLI 同批迁移。Transfer/Watch 设置独立 revision CAS，根替换只走 Library operation，Claim/Unidentified 消费登记归属。Jellyfin 按协议建立有效绑定，静态图片保持 public。

## 读写分离（2026-11-02）

`Store` 共享 `Arc<Writers>`，app/subscribe/library/cache 每库只有一个 writer 连接和独立 Mutex；没有进程级 Store Mutex。读写连接各自拥有 SQLite 连接，普通读不获取 writer 锁。锁纪律要求文件与网络观察在写锁外执行，writer 闭包只做本次写入所需的 SQL（包括事务内约束及 CAS 校验）。Transfer/Watch 与原始 Scrape 配置以 library.db 为唯一权威，配置观察、根和文件快照在同一 Library 读快照中完成；发布事务在同一数据库比较配置版本和文件证据，避免跨库配置 fence。

- **请求读**：`store_read` / `store_read_handle` 在 blocking worker 上调用 `Store::with_read_snapshot`，声明本阶段需要的数据库。`ReaderPool` 容量为 app 4、subscribe 2、library 8、cache 2（`crates/store/src/open.rs`）；Library/Jellyfin 共用 library 池。每个连接严格只读且 `query_only`，在 deferred 事务中读取快照。
- **快照准入**：按 library → app → subscribe → cache 稳定顺序获取；同阶段每库复用一个 lease/快照。普通 `store_read` / `with_store_read` 使用 `STORE_READ_WAIT=5s`；`store_read_handle` 使用调用方预算，Library/Jellyfin 多阶段读取使用请求剩余的绝对 deadline（请求预算 30s）。独立数据库的快照不构成跨库原子快照。认证、ACL/config/lifecycle 与选中文件的发布校验仍需明确处理观察间变化。
- **写准入**：`store_write` 获取 `http_work` permit，并以 `STORE_WRITE_WAIT=1s` 设置写作用域的绝对 deadline。在实际目标库 writer 获取后复查实例可写性与维护 WriteGate；该预算由作用域内获取共享，闭包计算及前一次获取会消耗余量。未获得 writer 返回 503 + `Retry-After`；worker 超时可能已写入，不能当作未执行。
- **writer 顺序**：现有 rank 为 app → subscribe → library → cache → jobs。跨库提交分开完成，不承诺原子性。禁止在 writer 闭包内获取另一库 writer 是当前审核的修复要求；存量 Subscribe 保留引用 fence 与 release 检查的具体缺口见审计 B3/L3，不能据 rank 合法就认可嵌套。
- **Jobs**：`jobs::Queue` 拥有 jobs.db 的唯一 writer；API `jobs_readers` 另有容量 2 的严格只读 `ReaderPool`，通过 `jobs::QueueRead` 查询。它独立于 Store 四库读池；Jobs writer 无界等待问题见审计 L2。

请求快照按绝对 deadline 与取消标志执行，SQLite progress handler 检查执行预算。超时不提前归还 lease/permit，实际 worker 清理并退出后才释放；物理 IO 不承诺即时中止。关闭或确认文件身份/布局变化后拒绝新租用并排空，数据库替换须关闭全部连接。

普通读不安装 SQL trace；诊断观察复用真实 sqlite3_stmt 状态，按连接和请求归属记录。连接池解除 writer 互斥锁对读取的阻塞，单条 SQL 的工作量仍须用[性能 SLO](../testing/performance-slo.md)与规模测试检验；池容量、取消或锁边界不构成性能通过证据。

## 运维边界

cache 清理不修改已采用元数据、文件、观看或 Job；不能 unlink 活跃 SQLite/WAL/SHM。备份需停写或使用协调一致的快照，分别复制活跃文件不能证明跨库一致。

本次重构不兼容旧布局。实际切换另行核实实例路径、关闭所有写者并按明确授权重建；保留媒体、字幕、NFO、图片、Indexer YAML 和 Downloader 文件。启动不自动删库，`jobs.db` 不作缓存处理。[部署说明](../deploy.md)记录操作边界；本轮没有执行实例切换。

## 备选方案与历史

单文件备份更简单，但外部响应缓存的高频更新/可丢弃生命周期与凭证、文件事实不同；因此保留生命周期分区。原先按概念拆分 Media/Library/Playback 会增加跨文件一致性成本，这些现已合入 library。TMDB JSON 文件树没有统一 TTL/来源查询，不再作为平行缓存。

Naming template 保留封闭占位符和实例配置，不引入脚本运行时；Transfer 的文件命名与 Scrape 各自按当前配置合同执行，完整字段以生产配置为准。Job 是持久执行事实，不能因为旧拆库图没画它就视为可弃派生数据。

历史演进：初始四个 Store 文件为 app/catalog/library/subscribe，后来另有 jobs/fanart；本次改成上述五文件，移除旧导入、ledger/file_meta 与浏览 journal。旧“四个文件”、导入器操作段和重复 SQL 已从本页撤下，原始决策理由/布局全文可用 `git show 86122d7:docs/adr/0008-sqlite-system-of-record.md` 取回；截至本轮前的完整后记可用 `git show cf7fb70:docs/adr/0008-sqlite-system-of-record.md` 读取。历史测量与反例保留原报告及版本，不重写为当前结果。
