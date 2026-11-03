# 当前数据库设计与数据字典

> 历史锁复审与测量只说明对应版本；当前连接与读写入口以本文和源码为准。

结构核对：2026-11-03，当前分支的唯一生产 DDL。各文件只声明当前定义，初始化空库并重开相同定义；没有历史布局、迁移阶梯或兼容分支。

## 权威来源

- Store打开/预检：[open.rs](../../crates/store/src/open.rs)、[layout.rs](../../crates/store/src/schema/layout.rs)；生产DDL：[app.sql](../../crates/store/src/schema/v1/app.sql)、[library.sql](../../crates/store/src/schema/v1/library.sql)、[subscribe.sql](../../crates/store/src/schema/v1/subscribe.sql)。
- cache唯一DDL及校验：[media::response_cache](../../crates/media/src/response_cache/mod.rs)，Store直接复用。
- Jobs 唯一 DDL：[schema.sql](../../crates/jobs/src/schema.sql)，布局校验/原子初始化：[schema.rs](../../crates/jobs/src/schema.rs)。
- 规范：[存储设计](../superpowers/specs/2026-10-18-library-storage-redesign.md)、开发合同（`git show 5e44f101:docs/superpowers/plans/2026-10-18-library-storage-contracts.md`）。字典描述源码结构，不替代行为验收。

## 五文件生命周期

| 文件 | application_id / user_version | 表数 | 字段数 | 职责 | 可按缓存清空 |
|---|---|---:|---:|---|---|
| [app.db](app.md) | 1129139248 / 1 | 9 | 48 | User/凭证、Site、Downloader、Filter、设置、搜索快照、实例锁 | 否 |
| [library.db](library.md) | 1129139249 / 2 | 66 | 661 | Media/采用元数据、Library/根、文件/覆盖/摘要、Collection、Playback、Transfer/Scrape 配置、持久工作 | 否 |
| [subscribe.db](subscribe.md) | 1129139250 / 1 | 9 | 67 | Subscribe策略、覆盖/搜索事实、在途记录 | 否 |
| [cache.db](cache.md) | 1129139251 / 1 | 6 | 30 | 六来源原始响应 | 是，受控清理 |
| [jobs.db](jobs.md) | 1129139252 / 1（Jobs自有预检） | 6 | 39 | Job定义/执行/重试、更新计数、保留游标 | 否 |

合计96张业务表、845字段，含Library的FTS5虚拟表，不计4张影子表、隐藏列及 sqlite_sequence。Store四库134个命名索引、49组FK、137个CHECK、0触发器；Jobs另11个命名索引、0 FK、3个CHECK、6触发器。SQLite自动索引不计入命名索引；跨文件没有FK或原子提交。

`Store::open_at`只允许app另选路径；Jobs由Queue打开。启动只检查和初始化当前定义；无自动清库或数据导入。连接显式WAL、5000ms busy_timeout、foreign_keys=ON；建库路径在切 WAL **之前**设 `auto_vacuum=INCREMENTAL`（文件头属性，只能在建表前改；WAL 库或事务内设置是静默 no-op）；Store各库writer与独立Jobs Queue writer走同一套连接策略：`cache_size=-16384`（每连接16MiB）、`temp_store=MEMORY`、`journal_size_limit=67108864`、`mmap_size=268435456`（每连接地址空间上限，不是常驻内存），打开时`PRAGMA optimize=0x10002`、关闭时`PRAGMA optimize`；`synchronous`按库区分——cache.db用NORMAL（可重建，容忍掉电丢最近提交），app/library/subscribe与Jobs Queue保持FULL（权威库绝不牺牲已提交事务）；实例租约心跳复用唯一app writer及其FULL/5000ms策略；各库读池均以只读方式打开，使用相同的cache/mmap/temp_store且`query_only=ON`。Jobs已有OFF→实际policy→FULL的连接级回归。独立media ResponseCache连接未显式设置synchronous，保留其构建默认。Store 用唯一生产 DDL 生成表、索引、视图的定义清单，比较 sqlite_schema 中的定义（包括表内约束），并拒绝额外对象；运行时诊断/故障注入触发器不纳入这份结构清单。Jobs 由其唯一 DDL 验证对象定义及单例。

## library.db事实与摘要

```mermaid
flowchart TD
    L[libraries] --> R[library_roots]
    R --> F[library_files]
    M[media] --> F
    M --> MD[来源快照与采用元数据]
    F --> D[file_details 冷详情]
    F --> FU[library_file_units 覆盖]
    FU --> U[library_units 去重摘要]
    F --> I[library_items 卡片]
    MD --> I
    I --> T[library_item_terms]
    I --> PT[library_protocol_terms / library_protocol_people]
    I --> QC[library_query_counts]
    L --> QCFG[library_query_config]
    P[playback_units 用户事实] --> UI[user_library_items 稀疏摘要]
    U --> UI
```

箭头表示业务依赖，不全是FK。一个物理文件一行；file_units支持一文件多集/同集多版本；units按逻辑单元去重；items按实际Library/Media归属保存。whole=(-1,-1)，TV S00E00合法，未知覆盖为空。Media/原始Playback事实使用独立逻辑身份；删除 Library 先隐藏、立即取消该库所有任务，再删除事实。Media 最后一个文件删除时，若无 Collection/Subscribe 等保留引用，则清理孤儿 Media、favorites 与播放进度（ADR-0012）；保留引用的 Media 继续存在。

写事务统一IMMEDIATE；文件发布在同一library事务维护覆盖、卡片、terms与摘要。100文件批次按 Media 在同事务批末合并用户摘要、协议 genre/person 成员和边际计数；查询语言投影按单库权威语言维护。rename意图保存版本、源/目标磁盘证据与阶段，跨subscribe引用按持久阶段恢复。

嵌套根迁移使用≤100文件事务和持久游标；NFO按批迁移，源/目标冻结暂停且公平轮转，API driver 在解冻后继续并批间放锁。root_reassignment_state 保留可查询状态、稳定 operation_id、配置版本与错误。Unidentified 登记 path_key/library/root/revision，独立索引 seek 窗口≤100候选，游标前错归属来源会重置相应游标，正确新事实保持向前进度。删除先冻结目标库，indexed snapshot/cleanup 每窗口≤100文件，无关库路径不会触发磁盘观察。

## 运维与性能边界

本轮将指纹模型的 Media/成员校验和发布放入同一 IMMEDIATE 事务；Series 断言使用三个精确坐标 seek；重复文件复用单次 coverage 捕获，多单元候选共享每份 coverage。接口同步 SQL 在 blocking worker 捕获 owned 事实，文件/网络观察释放 Store 后执行。公开定向回归与最终门禁分别记录，不能由局部通过推断所有设备/规模已验收。

Store 共享 `Arc<Writers>`，四库各有唯一 writer；任何实际 writer 不允许嵌套，debug/release 均检查。`store_write` 共享整个作用域的1秒期限，实际 writer 获取后复查准入。Jobs reservation 是有界准入门，不占实际 writer。读池 app/subscribe/library/cache 容量4/2/8/2，读快照按 library → app → subscribe → cache 获取，同阶段每库复用 lease；Jobs 另有容量2的只读池。各库快照不是跨库原子快照；Subscribe 引用、Jobs 意图和 Media 合并通过持久阶段与 generation 恢复。

`sql_result_value_bytes` 是 SQLite 结果值逻辑字节：TEXT UTF-8、BLOB 实际字节，INTEGER/FLOAT 各8，NULL 为0；不是网络、SQLite 页、解码对象或 native heap。writer wait/hold 是准入和 mutex 持有墙钟时间；`covered_owned_write_tx_*` 仅覆盖 `write_tx` 自行拥有的事务，排除 raw rusqlite 事务、失败 BEGIN 和读事务。观测是同步线程局部作用域，不能推断全部事务耗时。

待删除路径持久保存规范键和意图 generation；入队前在锁外观察磁盘身份，入队后阻止对应路径被发布/换源/改名。后台按持久身份重验和重试，完成磁盘动作后再清除对应文件事实。重命名附属文件的删除入队失败会保留可恢复阶段。

- 当前无 ledger/file_meta/browse journal 持久表、旧布局导入器或兼容视图。旧 `/directory` 聚合/roots 写、旧 Naming 表和反拆、自有无签发 session Playback、Downloader settings JSON 回读均删除；Web/CLI 使用当前合同。
- kind 先收窄授权 Library，查询使用同库精确成员、边际计数和页内选源；QueryWorkStats 纳入 exact count/辅助 SQL。新模型的实测与未运行范围见2026-10-25 验收记录（`git show 5e44f101:docs/benchmarks/2026-10-25-large-hit-query-read-model.md`）；2026-10-24 历史测量（`git show 5e44f101:docs/benchmarks/2026-10-24-storage-query-rereview.md`）保留原版本。
- Probe/Claim/Watch/fs watcher/根配置的 filesystem 观察使用 owned 快照移出 Store；最终发布校验原归属、配置和源版本。原 NFO 失败退出已修；R6–R8 补齐 mutation 门禁、Playback 同事务身份屏障，以及有界 operation/补图执行，见复审与正式回归。未确认新的 SQLite 永久锁环，不能由已有 offload 推断全部安全。
- cache清空只允许连接内受控删除，或停写关闭全部连接后重建；备份不能直接复制活跃文件冒充跨文件一致快照。WAL/SHM不是额外业务分区。
- 本轮不访问真实实例，不部署、不重建数据库。媒体/字幕/NFO/图片/下载目录保留；NAS、真实Jellyfin客户端、100k Media/1M文件完整规模尚未在本轮运行。

## 结构复核方法

将 app/library/subscribe 的生产 SQL、media 中 `CACHE_SCHEMA_SQL` 原始字符串和 Jobs 唯一 SQL 分别执行到新建隔离库，再运行 `sqlite3 <隔离库路径> .schema`。表和字段按 `PRAGMA table_list` / `table_info` 核对（排除FTS shadow与sqlite内部表），命名索引/触发器来自 `sqlite_schema`，FK 按 `foreign_key_list` 的约束 id 去重，CHECK 按生产建表声明统计。这里的数量来自空库结构核对，不代表真实实例、负载或全部行为验收。
