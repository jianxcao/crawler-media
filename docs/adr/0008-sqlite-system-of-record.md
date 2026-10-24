# SQLite 是系统记录，按生命周期拆分

状态：接受；现行五文件边界核对至 `cf7fb70`（2026-10-24）。本轮只整理文档，不改变 DDL 或实施切换。当前缺口见[复审](../audits/2026-10-24-storage-rereview.md)，字段/索引见[数据库字典](../database/README.md)。

## 决策

使用行存 SQLite 持久化 User、Site、Downloader、Filter、Subscribe、Media、Library、Playback、Job 与实例配置；视频、字幕、NFO、图片和 Indexer YAML 留在磁盘，行保存路径及身份。cache 是外部响应的可重建副本，不是 Media/Library 或已采用元数据的第二事实来源。

| 文件 | 现行职责 | 可清空 |
|---|---|---|
| `app.db` | User/token/凭证、Site、Downloader、Filter、Naming template、设置、搜索快照、实例锁 | 否 |
| `library.db` | Media/别名/采用元数据、Library/根、文件/覆盖/浏览摘要、Collection、Playback、Probe/Scan、持久文件和库操作 | 否 |
| `subscribe.db` | Subscribe 策略、逐槽事实、待下载记录 | 否 |
| `jobs.db` | Job 定义/调度配置、执行/重试、终态历史及保留游标 | 否；仅按规则清理终态历史 |
| `cache.db` | 六个 Metadata source 的原始 HTTP 响应，按来源分表 | 是；连接内受控清理或停写关连接后重建 |

默认目录为 `CRAWLER_MEDIA_DATA`；Store 管理 app/library/subscribe/cache 四文件，Jobs Queue 管理 jobs。`open_at` 只允许 app 指定其他路径。跨文件没有 FK 或原子提交；User 删除、Media 合并、文件跨分区引用等必须通过各自持久操作恢复，不以多次提交冒充一个事务。

## 同库边界与代价

Media 身份、采用元数据、Library/根、文件事实、覆盖、卡片、倒排与 Playback 放在同一 `library.db`，使事实和受影响摘要可以同事务更新。Media 可以没有任何文件；删除最后文件不删除身份、用户观看或 Collection 事实。

文件显式持有 `library_id/root_id/path_key`，归属在写入时确定并重验当前最深根，普通读消费登记事实。原始 Playback 单元与物理源绑定恢复包区分；文件版本 CAS 阻止旧工作发布。冷轨道/章节不复制到完整浏览投影；卡片与稀疏用户摘要是必要冗余，写路径负责原子维护，完整性操作负责发现/修复已登记不一致。

同库提高原子性，但单写者和外层 Store Mutex 仍会争用；SQL 分批 COMMIT 不自动释放 Mutex。磁盘/网络观察须锁外进行，根迁移、删除与修复需要持久游标、公平调度和有界窗口。精确 count、已筛选 facets、genre 大命中和深 offset 不承诺常量成本；当前测量见[10-24 报告](../benchmarks/2026-10-24-storage-query-rereview.md)。

## 初始化与连接

生产 DDL 只有 [Store v1](../../crates/store/src/schema/v1/)、[ResponseCache](../../crates/media/src/response_cache/mod.rs) 和 [Jobs](../../crates/jobs/src/store.rs)；本文不复制第二套 SQL 或模板字段列表。

Store 对空库原子创建当前结构和身份头，当前布局可重开，旧/未知/未来布局拒绝；不导入旧数据、不建兼容视图、不双读双写。application_id/user_version 和必需表/索引名校验不是完整逐列约束验证。新默认 Library/root 已同事务初始化，正常重开不会复活用户移除的默认根。

连接显式 WAL、5000ms busy_timeout、foreign_keys=ON。Store 四连接及 Jobs Queue 显式 synchronous=FULL；独立 media ResponseCache 未显式指定 synchronous。NORMAL/mmap/cache_size/temp_store/WITHOUT ROWID 需测量后决策，不把构建默认当耐久性合同。

**Jobs 当前有偏差：** API 预写身份头与 Queue DDL 分开提交，独立 Queue 又不设置该头；初始化中断可阻止下次启动。后续由 Jobs 统一原子建库/重开校验，删除旧升级分支。此缺口不授权自动认领或删除现有队列。

**调用方当前有偏差：** 旧 `/directory` 写、无签发 session 的自有 Playback、Downloader settings JSON 回退，以及删除/Unidentified 的路径归属猜测仍未清完。它们不属于本决策允许的兼容政策；修复顺序见[唯一方案](../superpowers/plans/2026-10-23-storage-review-remediation.md)。

## 运维边界

cache 清理不修改已采用元数据、文件、观看或 Job；不能 unlink 活跃 SQLite/WAL/SHM。备份需停写或使用协调一致的快照，分别复制活跃文件不能证明跨库一致。

本次重构不兼容旧布局。实际切换另行核实实例路径、关闭所有写者并按明确授权重建；保留媒体、字幕、NFO、图片、Indexer YAML 和 Downloader 文件。启动不自动删库，`jobs.db` 不作缓存处理。[部署说明](../deploy.md)记录操作边界；本轮没有执行实例切换。

## 备选方案与历史

单文件备份更简单，但外部响应缓存的高频更新/可丢弃生命周期与凭证、文件事实不同；因此保留生命周期分区。原先按概念拆分 Media/Library/Playback 会增加跨文件一致性成本，这些现已合入 library。TMDB JSON 文件树没有统一 TTL/来源查询，不再作为平行缓存。

Naming template 保留封闭占位符和实例配置，不引入脚本运行时；Transfer 的文件命名与 Scrape 各自按当前配置合同执行，完整字段以生产配置为准。Job 是持久执行事实，不能因为旧拆库图没画它就视为可弃派生数据。

历史演进：初始四个 Store 文件为 app/catalog/library/subscribe，后来另有 jobs/fanart；本次改成上述五文件，移除旧导入、ledger/file_meta 与浏览 journal。旧“四个文件”、导入器操作段和重复 SQL 已从本页撤下，原始决策理由/布局全文可用 `git show 86122d7:docs/adr/0008-sqlite-system-of-record.md` 取回；截至本轮前的完整后记可用 `git show cf7fb70:docs/adr/0008-sqlite-system-of-record.md` 读取。历史测量与反例保留原报告及版本，不重写为当前结果。
