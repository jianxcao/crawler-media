# cf7fb70 查询与写入工作量复测

日期：2026-10-24；被测 `feat/storage-redesign@cf7fb70`，审查基线本地 main=86122d7 / origin/main=14fdbb6。发现与后续方案见[复审](../audits/2026-10-24-storage-rereview.md)、[唯一方案](../superpowers/plans/2026-10-23-storage-review-remediation.md)。本轮没有修改源码、DDL 或仓库测试。

## 方法与限制

Rust 探针直接链接当前 target/debug/deps 的 **debug 库**，公开 Store 查询使用 bundled **SQLite 3.50.2**。公开查询合成库由 Store::open 建立、Python 填事实；Python 独立建库的 EQP/VM 探针使用 **SQLite 3.54.0**。耗时查询每个形状同进程执行 7 次，报告中位数/最大值；7 次最大值不能当可靠 p95。主任务同时运行 workspace，CPU/缓存有竞争；这些数据不能和历史 release 结果作绝对值前后对比，也不是 HTTP 延迟或 NAS SLA。

单库数据形状为 5k/20k/50k/100k Movie Media，每个一个 published/present 文件、whole coverage、一个 zh-CN work NFO 输入。每项含六种轮转 genre 之一和共同的 All，3 位演员按 2,000 人轮转，标题/简介带稳定编号。按生产 DDL 批量直接填合成事实，读取调用生产 Store 方法；建库不计时。没有真实磁盘探测、HTTP 水合或用户实例。

最后的 kind 实验将除前 1,000 个 Movie 外的记录移入独立 TV Library，结束时 FK 检查通过；它改变了前面三份数据库形状，所以原始查询结果必须引用实验前的日志。

## Media kind：固定电影，增加无关 TV

调用 `Store::protocol_item_page`，IncludeItemTypes=Movie、limit=100、精确 total=1000。Movie 数据固定，只增加另一个 TV Library。

| 无关 TV Media | 指定 Movie Library，中位/最大 ms | Root 范围，中位/最大 ms |
|---:|---:|---:|
| 4,000 | 5.385 / 6.402 | 8.320 / 9.135 |
| 19,000 | 5.676 / 6.155 | 27.087 / 27.688 |
| 49,000 | 5.717 / 6.679 | 72.928 / 74.544 |

[visible_libraries](../../crates/store/src/library_query/protocol/scope.rs#L102)先纳入全部授权 Library，[kind_clauses](../../crates/store/src/library_query/protocol/predicates.rs#L63)才在行级过滤。Root 查询的结果完全相同，成本却随无关 kind 增长。先按 IncludeItemTypes/粒度/IsFolder 缩小授权 Library roster，再测索引；保留混合 Movie/Series/Episode/Video 行为。

## facets：已选择条件仍重复评估

调用 `Store::library_facets(tier=all)`；Drama 约 1/6 命中，All 100% 命中。

| Media | 无筛选，中位/最大 ms | Drama，中位/最大 ms | All，中位/最大 ms |
|---:|---:|---:|---:|
| 5,000 | 34.494 / 111.017 | 138.934 / 139.533 | 145.161 / 154.129 |
| 20,000 | 200.567 / 509.453 | 662.906 / 687.781 | 671.517 / 674.809 |
| 50,000 | 567.692 / 1354.864 | 1759.303 / 1872.478 | 1845.583 / 1909.981 |
| 100,000 | 1217.737 / 2615.299 | 3798.064 / 3875.856 | 3947.796 / 4092.742 |

[conditional_counts](../../crates/store/src/library_query/facets.rs#L323)为每个 bucket 重复展开 where_sql，包括相关 genre EXISTS；其他维度还各自扫描。API 在共享 Store guard 内完成整轮 facets。历史无筛选 Q3b 优化不能关闭此门。

方案先测热组合，在同一读快照一次计算各维度命中，再按“排除当前维度”规则聚合。保留候选全集、count=0、条件 favorite 与精确 total；不要直接加入大量索引/长期缓存。验收增加 genre+country+watch 和并行 Store 读/心跳及最大锁时间。

## 协议 genre / person / search

调用 `Store::protocol_item_page`，以下为中位 ms。

| Media | Movie 首页 | Drama 约 1/6 | All 100% | person | search |
|---:|---:|---:|---:|---:|---:|
| 5,000 | 20.953 | 25.614 | 127.314 | 2.260 | 3.864 |
| 20,000 | 90.712 | 102.533 | 550.569 | 3.198 | 20.898 |
| 50,000 | 249.787 | 251.561 | 1459.878 | 5.514 | 67.662 |
| 100,000 | 531.257 | 537.671 | 3752.070 | 11.669 | 249.237 |

person 命中依次为 8/30/75/150，search 恒为 10。只使用纠正 person UUID 后完整重跑的 result-corrected.txt，首次 result.txt 无效。

当前已经有写入期 terms_value 和全 locale 安全预筛，不能再报历史“无索引全库 JSON”问题。[genre 解析](../../crates/store/src/library_query/protocol/facts.rs#L210)和最终精确语言谓词仍处理命中集，SQL 去重/排序/精确 total 在 LIMIT 前。先消除重复解析，再评估有效 locale 成员与有序索引；keyset 不消除精确 count，没有当前证据要求立刻引入 FTS。

## 文件批次重复重建协议索引

公开 `Store::publish_files`：已有 TV 卡片，采用元数据含 100 位演员，一批新增 100 文件。临时库 trigger 只记录行写入，未改生产 DDL，不用于测延迟。此写入实验执行一次；批前直接填入采用元数据，尚无 person_id 索引，首文件建立 100 行，后续 99 文件重复删建。若批前索引完整，预计 DELETE 也是 10000；本轮未另测该形状。

| 指标 | 结果 |
|---|---:|
| person_id INSERT | 10,000 |
| person_id DELETE | 9,900 |
| 最终 person_id 行 | 100 |
| user summary writes | 0 |

[逐文件 reduce](../../crates/store/src/library_write/reduce_incremental/mod.rs#L177)→[terms](../../crates/store/src/library_write/reduce_incremental/terms.rs#L110)→[sync_card](../../crates/store/src/protocol_index.rs#L31)反复删除/插入整卡协议索引并写 search_text。已有用户摘要批末去重不覆盖这里。建议同事务 dirty-card 去重一次，进一步按实际元数据/terms 变化差量同步。

## Unidentified 选择工作量

执行当前生产选择 SQL：目标根内 100 条错误归属路径，目标根外递增无关路径；norm 函数使用计数恒等函数，证明查询形状，**没有测生产归一化耗时**。每个规模执行一次工作量测量；VM 由进度回调估算，非精确 statement counter。

| 无关行 | 返回待迁移行 | norm 调用 | VM 约数 |
|---:|---:|---:|---:|
| 5,000 | 100 | 10,700 | 45,000 |
| 50,000 | 100 | 100,700 | 360,000 |
| 100,000 | 100 | 200,700 | 710,000 |

EQP 包含 SCAN u、root PK SEARCH、CORRELATED SCALAR SUBQUERY、SCAN r 与两处临时 ORDER BY B-tree。LIMIT 100 限制移动行，未限制读取/计算候选。登记 path_key、范围 seek、持久 phase/keyset 和生命周期重验仍为正确方向，不能只增加 batch 或改成单条大 UPDATE。

## 请求工作量观测缺口

100k Media、Drama 条件，公开 items 查询：不带 total 为 1.387ms，带 total 为 129.221ms；两者 QueryWorkStats 均为 VM=13723、FullscanStep=0、rows=100。

[page.rs](../../crates/store/src/library_query/items/page.rs#L275)统计 page statement，[count](../../crates/store/src/library_query/items/page.rs#L324)在统计之外。此计数不能证明整个端点工作量有界。验收分别记录 page/count/facets，再汇总请求 SQL/VM/锁时间，避免只优化或观察 LIMIT 后的小页。

## 复跑与证据定位

原始探针和日志在 `/tmp/storage-review-query-20261024/`：seed.py/query.rs/result-corrected.txt、browse.rs/browse-result.txt、kind.py/kind.rs/kind-result.txt、write_amp.py/write_amp.rs、unidentified.py、version.rs。临时文件不保证长期保留；本页固化数据形状、数字和限定条件，不将临时工具变成另一套生产实现。

Rust 采用 `rustc --edition=2024 <probe.rs> -L dependency=target/debug/deps --extern store=<当前libstore.rlib> --extern domain=<当前libdomain.rlib> -o <临时输出>`；publication 写入探针另链接 fixture 所需依赖。哈希随构建变化，不固定为仓库 API。Python 脚本 base 指向新的临时目录，按 seed→query/browse→kind 顺序，不能复用已被 kind 实验改变的数据冒充原形状。

仓库已有 release 入口：`PERF_SIZES=5000,20000,50000,100000 cargo test --release -p api --test jellyfin_page_perf -- --ignored --nocapture`。本轮未运行该 release 命令；它不包含 All genre、filtered facets、kind 隔离，后续须扩充场景。真实 NAS、HTTP 全链和 100k Media/1M 文件/1M Playback 组合验收均 Not run。
