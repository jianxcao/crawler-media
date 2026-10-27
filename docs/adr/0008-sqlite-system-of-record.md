# SQLite 是系统记录，按生命周期拆分

状态：接受；五文件边界保留，`library.db` 当前布局为 version 6。原问题版本 `cf7fb70`，实施与验证见[执行记录](../superpowers/plans/2026-10-23-storage-review-remediation.md)，字段/索引见[数据库字典](../database/README.md)。

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

文件显式持有 `library_id/root_id/path_key`，归属在写入时确定并重验当前最深根，普通读消费登记事实。原始 Playback 单元与物理源绑定恢复包区分；文件版本 CAS 阻止旧工作发布。冷轨道/章节不复制到完整浏览投影；卡片、协议 genre/person 成员、静态分类边际计数与稀疏用户摘要是必要冗余，写路径负责同事务差量维护，Integrity 操作负责从原始事实修复。

同库提高原子性，但单写者和外层 Store Mutex 仍会争用；SQL 分批 COMMIT 不自动释放 Mutex。磁盘/网络观察须锁外进行，根迁移、删除与修复需要持久游标、公平调度和有界窗口。精确 count、已筛选 facets、genre 大命中和深 offset 不承诺常量成本；当前验收记录见[10-25 报告](../benchmarks/2026-10-25-large-hit-query-read-model.md)，旧实现数据保留于[10-24 报告](../benchmarks/2026-10-24-storage-query-rereview.md)。

## 初始化与连接

生产 DDL 只有 [Store v1](../../crates/store/src/schema/v1/)、[ResponseCache](../../crates/media/src/response_cache/mod.rs) 和 [Jobs 唯一 DDL](../../crates/jobs/src/schema.sql)；本文不复制第二套 SQL 或模板字段列表。

Store 对空库原子创建当前结构和身份头，当前布局可重开，旧/未知/未来布局拒绝；不导入旧数据、不建兼容视图、不双读双写。Store 的 application_id/user_version 和必需表/索引名校验不是完整逐列约束验证；Jobs 从唯一 DDL 校验对象定义和单例。新默认 Library/root 已同事务初始化，正常重开不会复活用户移除的默认根。

连接显式 WAL、5000ms busy_timeout、foreign_keys=ON。Store 四连接及 Jobs Queue 显式 synchronous=FULL；独立 media ResponseCache 未显式指定 synchronous。NORMAL/mmap/cache_size/temp_store/WITHOUT ROWID 需测量后决策，不把构建默认当耐久性合同。

Jobs crate 拥有唯一布局入口，真正空文件在一个 IMMEDIATE 事务内创建全部对象、单例和身份头；独立 Queue 与 API 共用。已有非空文件只读预检，旧/未知/未来或损坏当前布局拒绝且不修补；保留事实队列及备份提示。没有 Store→Jobs 生产依赖。

旧 `/directory` 聚合/roots 写、组合 Naming 拆解、Downloader JSON 回退和自有无签发会话 Playback 已删除；前端与 CLI 同批迁移。Transfer/Watch 设置独立 revision CAS，根替换只走 Library operation，Claim/Unidentified 消费登记归属。Jellyfin 按协议建立有效绑定，静态图片保持 public。

## 有界 Library 读连接

实例为覆盖的 Library/Jellyfin 查询持有最多两个独占只读 lease，连接只打开已验证的当前 `library.db`，不初始化布局、不获取 writer 租约、不 ATTACH app.db。短 Store 阶段持原请求 token 形成 owned 上下文；单库快照重验 Library roster 后执行；快照关闭并归池后，短 Store 阶段重验原凭据、角色、ACL/config/lifecycle、当前 roster 与选中文件引用。Latest 的多个片段统一发布，最多重试一次；跨文件权限最终重验是授权线性化点。

Store Mutex 和 reader lease 不同时持有；writer 不等待读池。认证前的 30 秒预算贯穿所有阶段，busy 不超过剩余预算与 5000ms 较小值，progress 每 1000 VM 检查取消/截止。超时不提前归还 lease/permit，实际 worker 清理后才释放；物理 IO 不承诺即时中止。关闭或确认身份/布局变化后拒绝新租用并排空，保留只读实例现有读写合同。

普通读不安装 SQL trace；诊断观察复用真实 sqlite3_stmt 状态，并按连接和请求归属，覆盖初始布局、BEGIN、roster、选页/水合、ROLLBACK、末次布局。连接池改善锁边界，单条 SQL 继续受原[查询预算](../superpowers/specs/2026-10-25-large-hit-query-read-model.md#91-设计目标不是已测结果)约束。实测、WAL/取消边界及未跑现场项见[隔离记录](../benchmarks/2026-10-25-library-read-isolation.md)。

## 运维边界

cache 清理不修改已采用元数据、文件、观看或 Job；不能 unlink 活跃 SQLite/WAL/SHM。备份需停写或使用协调一致的快照，分别复制活跃文件不能证明跨库一致。

本次重构不兼容旧布局。实际切换另行核实实例路径、关闭所有写者并按明确授权重建；保留媒体、字幕、NFO、图片、Indexer YAML 和 Downloader 文件。启动不自动删库，`jobs.db` 不作缓存处理。[部署说明](../deploy.md)记录操作边界；本轮没有执行实例切换。

## 备选方案与历史

单文件备份更简单，但外部响应缓存的高频更新/可丢弃生命周期与凭证、文件事实不同；因此保留生命周期分区。原先按概念拆分 Media/Library/Playback 会增加跨文件一致性成本，这些现已合入 library。TMDB JSON 文件树没有统一 TTL/来源查询，不再作为平行缓存。

Naming template 保留封闭占位符和实例配置，不引入脚本运行时；Transfer 的文件命名与 Scrape 各自按当前配置合同执行，完整字段以生产配置为准。Job 是持久执行事实，不能因为旧拆库图没画它就视为可弃派生数据。

历史演进：初始四个 Store 文件为 app/catalog/library/subscribe，后来另有 jobs/fanart；本次改成上述五文件，移除旧导入、ledger/file_meta 与浏览 journal。旧“四个文件”、导入器操作段和重复 SQL 已从本页撤下，原始决策理由/布局全文可用 `git show 86122d7:docs/adr/0008-sqlite-system-of-record.md` 取回；截至本轮前的完整后记可用 `git show cf7fb70:docs/adr/0008-sqlite-system-of-record.md` 读取。历史测量与反例保留原报告及版本，不重写为当前结果。
