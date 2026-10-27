# 性能基线 + 确定性回归门禁（Task 17）

日期：2026-10-26。分支：`feat/storage-redesign`。任务：计划
[`2026-10-26-storage-redesign-review-fixes.md`](../superpowers/plans/2026-10-26-storage-redesign-review-fixes.md) 的 Task 17。

本分支**以性能为第一裁决标准**。在本文之前，“没有性能回退”只能靠直觉和零散的 `EXPLAIN QUERY PLAN` 断言；
本文把它变成一个可以在几秒到两分钟内跑完、并且会用**确定性计数**判红/判绿的门禁。

## 0. 一键门禁命令

```bash
# 完整门禁（真实规模：21k Media / 61k 文件）。约 2 分钟播种 + 测量，必须 --release。
CARGO_TARGET_DIR=target/perf cargo test -p store --release --test perf_baseline -- --ignored --nocapture
```

`CARGO_TARGET_DIR=target/perf` 是硬要求：本工作区同时有多个 agent 在编译，重活跑在默认 `target/` 会把
build 锁和磁盘 IO 全部抢走（实际发生过：`lease-concurrency` 等锁等了约 15 分钟）。

**耗时**（2026-10-26 实测）：空闲磁盘下首次播种 2.5 分钟、整轮约 4 分钟；5 个 agent 同时编译/跑测试时
整轮 953 秒（播种几乎全部是 fsync 等待）。因此夹具带**种子缓存**：键 = `FIXTURE_VERSION` + 规模 +
SQLite 版本 + `crates/store/src/schema/**/*.sql` 的哈希，命中就直接拷文件并重新打开，一轮只需十几秒。
缓存默认在 `target/perf-fixtures/`，用 `PERF_FIXTURE_CACHE=<dir>` 改位置，`PERF_FIXTURE_CACHE=off` 强制重新播种。
DDL 一改，键就变，绝不会量到旧布局；跑门禁时打印 `fixture: seed cache hit …` 或 `fixture: seeded, cached at …`。

**同一时刻只有一个播种者。** 多个 agent 同时跑门禁时，重复播种会互相抢 fsync，两边都跑不完（实测过一次：
两个进程同时播 REALISTIC，40 分钟没结束）。所以缓存用 `<key>.lock` 决定轮次：抢到 `create_new` 的进程播种，
播种完用一次原子 `rename` 发布并释放锁；其他进程**等待**（轮询直到缓存出现，然后拷这份），而不是再播一次；
锁文件超过 45 分钟（`PERF_FIXTURE_CACHE_STALE_SECS` 可调）视为崩溃残留并被抢占，所以等待不会无限期。
实测：并发两个进程时一个输出 `seeded, cached at …`，另一个输出 `another process is seeding …; waiting`
然后 `seed cache hit …`——只播种一次。

```bash
# 默认目标里的廉价版本（同一夹具形状，1/144 的行数，预算完全相同）。约 5 秒。
cargo test -p store --test perf_baseline
```

**每个 Task 收尾都要跑完整门禁**，并把表里的 `statements` / `full-scan steps` 与本文件第 3 节对比。
本文件第 3 节是**唯一**基线；`main` 不是基线。

## 1. 为什么断言确定性计数，而不是墙钟

墙钟阈值在慢 CI / 多 agent 并发的机器上一定会飘，松掉阈值等于把门禁废掉。所以：

- **会判红的**：SQL 语句数、全表扫描步数（`SQLITE_STMTSTATUS_FULLSCAN_STEP`）、以及 store 自带的
  三个诊断计数器（`root_ledger_scans`、`media_ledger_reads`、`role_lookups`）。这些数字与机器速度无关。
- **只记录、永不断言**：所有 `p50/p95/max` 毫秒数，以及除 §3.2 那十条之外的 `vm_steps`。
  `vm_steps` 的默认用途是同一提交内的 A/B 对比：`ANALYZE` / `PRAGMA optimize`（Task 12）会改变计划与步数。
  **唯一的例外**是十个 facet 维度的 `vm_steps` 上限（§3.2 / §4）：在固定夹具上它们逐位可复现，
  而"语句数没变、行访问翻倍"这类回归只有它能看见。

对应计划 Review Focus 第 9 条：“性能不回退：以确定性计数（SQL 语句数、全表扫描步数）作为门禁，而不是墙钟阈值。”

门禁还额外强制两件事：

1. **夹具内自证确定性**：每个操作跑 N 次，N 次测出的 `Work` 必须逐字段相等，否则直接 panic。
   一条会飘的指标不许进门禁。
2. **覆盖完整性**：每个被测操作都必须在预算表里有一行，预算表里的每一行都必须被测到。新增热点操作
   而不给它定预算，会立刻判红。

## 2. 夹具

`crates/store/tests/perf_baseline/fixture.rs`。所有 id 都是固定标签的 UUID v5，所有事实都是行号的纯函数，
因此**同一份 `Scale` 每次产出同一份 `library.db`**。数据全部经 store 公开 API 写入
（`insert_media`、`publish_files`、`set_metadata_overrides`、`mutate_playback`、`issue_play_session`）：
没有 sleep、没有网络、没有 Chromium、磁盘上没有数据库以外的文件。

| 规模 | Movies | TV works | 每部集数 | 第三个库 | 预留发布位 | media | files | episodes |
|---|---|---|---|---|---|---|---|---|
| `REALISTIC` | 14,000 (+1,000 双版本) | 5,000 | 9 | 1,000 | 1,000 | 21,000 | 61,000 | 45,000 |
| `SMALL` | 240 (+24) | 40 | 3 | 40 | 200 | 520 | 424 | 120 |
| `TINY` | 90 (+10) | 8 | 2 | 8 | 20 | 124 | 124 | 16 |
| `WRITE_PROBE` | 1,200 (+120) | 120 | 3 | 120 | 1,000 | 2,640 | 1,800 | 360 |

- 三个播种库（Movie / TV / Movie）+ `Store::open` 自带的两个空默认库（电影库、剧集库），共 5 个 Library。
- 十个 facet 维度全部有真实事实：`genres`、`countries`、`decade`（来自 `release_date`/`year`）、
  `watch`（用户播放行）、`ratings`、`runtimes`、`languages` 来自 NFO 采纳字段；`resolutions`、`hdr`
  来自已发布文件的 probe 事实；`stock=unscraped` 来自每 5 个 Media 中留空的 1 个。
- `TINY` 只给负向对照用；`WRITE_PROBE` 只给 Task 10 的写侧成本测量用（第 6 节）。

## 3. 基线（layout v4，2026-10-26）

`REALISTIC`：21,000 Media / 61,000 files / 45,000 episodes。SQLite 3.50.2，`library.db` 约 335 MB，
layout **v4**（`LIBRARY_USER_VERSION = 4`）。下表是 `cargo test -p store --release --test perf_baseline
-- --ignored --nocapture` 的原始输出（2026-10-26 23:53，机器上另一个 agent 同时跑着同一门禁；毫秒数
**仅供参考**，确定性计数才是门禁）。

| operation | iterations | p50 ms | p95 ms | max ms | statements | full-scan steps | vm steps | ledger scans | media ledger reads | role lookups |
|---|---|---|---|---|---|---|---|---|---|---|
| browse page 1 (offset 0) | 20 | 0.070 | 0.097 | 0.098 | 6 | 0 | 1938 | 0 | 0 | 0 |
| browse deep offset | 20 | 0.146 | 0.182 | 0.225 | 6 | 0 | 24338 | 0 | 0 | 0 |
| browse deep cursor | 20 | 0.068 | 0.103 | 0.105 | 5 | 0 | 2147 | 0 | 0 | 0 |
| browse via read lease (production path) | 10 | 0.488 | 0.607 | 0.607 | 17 | 4 | 6785 | 0 | 0 | 0 |
| item detail (movie) | 20 | 0.089 | 0.154 | 0.159 | 11 | 0 | 305 | 0 | 0 | 0 |
| item detail (tv work) | 20 | 0.110 | 0.192 | 0.193 | 11 | 0 | 681 | 0 | 0 | 0 |
| facets: genres | 10 | 15.533 | 17.756 | 17.756 | 6 | 33753 | 899556 | 0 | 0 | 0 |
| facets: countries | 10 | 19.473 | 20.910 | 20.910 | 6 | 40271 | 1130467 | 0 | 0 | 0 |
| facets: decades | 10 | 12.479 | 13.444 | 13.444 | 6 | 28136 | 660227 | 0 | 0 | 0 |
| facets: watch | 10 | 47.513 | 50.626 | 50.626 | 5 | 98157 | 3106802 | 0 | 0 | 0 |
| facets: ratings | 10 | 35.674 | 37.370 | 37.370 | 5 | 61147 | 2001168 | 0 | 0 | 0 |
| facets: runtimes | 10 | 22.833 | 26.098 | 26.098 | 6 | 40304 | 1140585 | 0 | 0 | 0 |
| facets: languages | 10 | 22.034 | 23.798 | 23.798 | 6 | 40271 | 1130469 | 0 | 0 | 0 |
| facets: resolutions | 10 | 24.249 | 27.638 | 27.638 | 6 | 49158 | 1367095 | 0 | 0 | 0 |
| facets: hdr | 10 | 6.035 | 6.325 | 6.325 | 6 | 0 | 240562 | 0 | 0 | 0 |
| facets: stock | 10 | 12.684 | 14.473 | 14.473 | 6 | 33654 | 653637 | 0 | 0 | 0 |
| playback authorization roster | 20 | 0.007 | 0.008 | 0.009 | 2 | 4 | 129 | 0 | 0 | 1 |
| playback source selection (no user scope) | 20 | 0.041 | 0.071 | 0.077 | 1 | 0 | 333 | 0 | 0 | 0 |
| playback source selection | 20 | 0.056 | 0.057 | 0.057 | 3 | 4 | 552 | 0 | 0 | 1 |
| integrity status lookup | 20 | 0.005 | 0.005 | 0.005 | 3 | 0 | 43 | 0 | 0 | 0 |
| scan publication (fresh insert) | 1 | 166.454 | 166.454 | 166.454 | 8,050 | 4,000 | 380,500 | 0 | 0 | 0 |
| scan publication (idempotent republish) | 3 | 68.067 | 237.678 | 237.678 | 8,050 | 4,000 | 380,500 | 0 | 0 | 0 |
| playback progress write | 50 | 17.465 | 170.464 | 230.908 | 23 | 4 | 984 | 0 | 0 | 1 |
| control: full ledger coverage scan | 1 | 352.256 | 352.256 | 352.256 | n/a | n/a | n/a | 61999 | 0 | 0 |

**计数列的来源（Task 21）**：`statements` / `full-scan steps` / `vm steps` 三列原先在这些行上是
`n/a`（store 不暴露计数器），现在由 `Store::with_stats` 围绕**生产入口**测量后补齐；确定性计数与规模
无关，所以两列数字在 424 / 61,000 files 两个尺度上逐字相同（`vm steps` 会随文件数变化，属正常）。
毫秒列仍是 2026-10-26 那次原始运行的输出，没有重跑覆盖。

`SMALL`（默认 `cargo test -p store --test perf_baseline` 里跑的那份，debug 构建，用于确认
语句数/扫描步数预算与规模无关；毫秒数是 debug + 高负载，仅供形状参考）：

| operation | iterations | p50 ms | p95 ms | max ms | statements | full-scan steps | vm steps | ledger scans | media ledger reads | role lookups |
|---|---|---|---|---|---|---|---|---|---|---|
| browse page 1 (offset 0) | 20 | 0.248 | 0.314 | 0.326 | 6 | 0 | 1938 | 0 | 0 | 0 |
| browse deep offset | 20 | 0.259 | 0.339 | 0.388 | 6 | 0 | 2322 | 0 | 0 | 0 |
| browse deep cursor | 20 | 0.249 | 0.299 | 0.371 | 5 | 0 | 2147 | 0 | 0 | 0 |
| browse via read lease (production path) | 10 | 1.029 | 1.323 | 1.323 | 17 | 4 | 7070 | 0 | 0 | 0 |
| item detail (movie) | 20 | 0.300 | 0.590 | 0.670 | 11 | 0 | 305 | 0 | 0 | 0 |
| item detail (tv work) | 20 | 0.281 | 0.311 | 0.316 | 11 | 0 | 381 | 0 | 0 | 0 |
| facets: genres | 10 | 2.273 | 2.447 | 2.447 | 6 | 729 | 17989 | 0 | 0 | 0 |
| facets: countries | 10 | 2.293 | 2.685 | 2.685 | 6 | 821 | 21854 | 0 | 0 | 0 |
| facets: decades | 10 | 1.801 | 1.966 | 1.966 | 6 | 588 | 12608 | 0 | 0 | 0 |
| facets: watch | 10 | 3.287 | 3.418 | 3.418 | 5 | 1838 | 56008 | 0 | 0 | 0 |
| facets: ratings | 10 | 2.441 | 2.499 | 2.499 | 5 | 1131 | 35821 | 0 | 0 | 0 |
| facets: runtimes | 10 | 2.181 | 2.210 | 2.210 | 6 | 847 | 24128 | 0 | 0 | 0 |
| facets: languages | 10 | 2.281 | 2.322 | 2.322 | 6 | 821 | 21856 | 0 | 0 | 0 |
| facets: resolutions | 10 | 2.403 | 2.492 | 2.492 | 6 | 1014 | 26608 | 0 | 0 | 0 |
| facets: hdr | 10 | 0.686 | 0.745 | 0.745 | 6 | 0 | 4816 | 0 | 0 | 0 |
| facets: stock | 10 | 1.988 | 2.056 | 2.056 | 6 | 630 | 13147 | 0 | 0 | 0 |
| playback authorization roster | 20 | 0.023 | 0.024 | 0.025 | 2 | 4 | 129 | 0 | 0 | 1 |
| playback source selection (no user scope) | 20 | 0.098 | 0.149 | 0.163 | 1 | 0 | 333 | 0 | 0 | 0 |
| playback source selection | 20 | 0.141 | 0.180 | 0.197 | 3 | 4 | 552 | 0 | 0 | 1 |
| integrity status lookup | 20 | 0.013 | 0.013 | 0.014 | 3 | 0 | 43 | 0 | 0 | 0 |
| scan publication (fresh insert) | 1 | 29.244 | 29.244 | 29.244 | 1,610 | 800 | 76,900 | 0 | 0 | 0 |
| scan publication (idempotent republish) | 3 | 29.641 | 30.396 | 30.396 | 1,610 | 800 | 76,900 | 0 | 0 | 0 |
| playback progress write | 50 | 2.667 | 27.637 | 66.755 | 23 | 4 | 984 | 0 | 0 | 1 |
| control: full ledger coverage scan | 1 | 2.564 | 2.564 | 2.564 | n/a | n/a | n/a | 623 | 0 | 0 |

## 3.1 v3 → v4 的变化（同一夹具、同一预算）

v4 只在 DDL 与本轮修复上动过，确定性计数的变化如下（`statements / full-scan steps`）：

| operation | v3 | v4 | 说明 |
|---|---|---|---|
| browse via read lease（生产读路径） | 277 / 25,188（p50 1.46 ms） | **17 / 4**（p50 0.49 ms） | Task 11：读租约不再每次全量校验布局。**语句数 ÷16、扫描步数 ÷6,300、延迟 ÷3** —— 本轮最大的一笔读路径收益，在此之前完全看不见 |
| browse page 1 / deep offset / deep cursor | 6/0 · 6/0 · 5/0 | 不变 | 单表索引读没有回退 |
| facets ×10 维 | 5–6 / 28k–98k | 语句与扫描步数**完全相同** | 但 `facets: hdr` 的 `vm_steps` 从 136,025 涨到 240,562、p50 0.74 → 6.04 ms（8×）：这条维度只读 `library_query_counts`，属于**确定性计数看不到的回归**，单独立项跟进 |
| playback source selection（带用户 scope） | 56.4 ms | **0.056 ms** | 带了 `library_id IN (…)` 的那句源查询计划被修好，约 1,000× |
| scan publication（insert / republish） | 950.8 / 2,140.9 ms | 166.5 / 68.1 ms | 写侧明显变快（republish 31×）；同轮机器负载高，绝对值仍只作参考 |
| control: `list_file_coverage` | 61,999 步 / 122.3 ms | 61,999 步 / 352.3 ms | 扫描步数不变（同一句全表扫描），毫秒差是本轮并发负载 |

**结论**：v4 没有让任何确定性计数回退，并且把读路径最大的两笔固定开销（租约校验、播放源计划）都降下来了。
门禁里 `BROWSE_READ_LEASE` 的预算因此从 277 / 25,188 **收紧到 20 / 8** —— 上限如果还留着旧值，
就无法发现“退回全量校验”的回归（它恰好等于上限）。

### 3.2 Task 19：播放源选择的计划形状（before / after）

§3.1 那一行「56.4 → 0.056 ms」的原因不是常量，是**连接顺序**。同一夹具（REALISTIC，61k 文件）、
同一条 SQL，只多一个用户 scope（`AND f.library_id IN (…)`），计划的第一个表就换了：

```
before（带 scope）：SEARCH f USING INDEX files_presence (library_id=? AND presence_state=?)
                    SEARCH u USING PRIMARY KEY (file_id=?)

after （带 scope）：SEARCH u USING COVERING INDEX coverage_media (media_id=?)
                    SEARCH f USING INDEX files_presence (library_id=? AND presence_state=? AND file_id=?)

不带 scope（两次相同）：SEARCH u USING COVERING INDEX coverage_media (media_id=?)
                        SEARCH f USING INDEX sqlite_autoindex_library_files_1 (file_id=?)
```

差别在 `files_presence` 的**第三列是否被等值约束**：`file_id` 没有绑定值时，这个索引退化成
「按库列出所有 present 文件」（61k / 3 个库 ≈ 2 万行逐个 join units 再按 `media_id` 丢弃），
绑定后是一次三列 seek。也就是说，问题是外层循环选错了表，与索引是否存在无关——`coverage_media`
一直是 `(media_id, season, episode, library_id, file_id)` 的覆盖索引，只是没被选为驱动表。
注意两次计划都带 `TEMP B-TREE`（before 只为 ORDER BY），所以「没有 TEMP B-TREE」不能当判据。

修法：`crates/store/src/playback/source.rs::source_rows` 用
`FROM library_file_units u CROSS JOIN library_files f ON u.file_id = f.file_id`
固定连接顺序（`CROSS JOIN` 在 SQLite 里语义就是「不许重排」）。未加索引、未升
`LIBRARY_USER_VERSION`、结果集不变（`playback_storage_v1` / `playback_resume_v1` / `scoped_coverage` 全绿；
用户无任何可见 Library 时仍先返回空，不是全量）。

两层断言，缺一层就会静默退回：
`playback::source::tests::source_sql_pins_the_media_first_join_order` 钉**真正执行的 SQL**（改回普通
`JOIN` 立刻红，已验证）；`library_query_plans.rs::playback_source_selection_seeks_units_by_media_not_the_whole_library`
钉 SQLite 从这条 SQL 推出的计划第一步与 `files_presence` 的三列 seek（schema 变化让计划翻回去时红）。

## 3.2 `facets: hdr` 的 2× 回退：原因、修复、以及为什么 `vm_steps` 现在也被断言（task-31）

**症状**（v4，REALISTIC）：`facets: hdr` 的确定性计数不变（6 条语句、0 全表扫描步数），但
`vm_steps` 136,025 → 240,562（+77%）、p50 0.74 → 6.04 ms。另外九个维度不变。

**定位方法**：把这句 facet SQL 单独取出来，用**同一个 bundled SQLite（3.50.2）**在冻结的 v4 夹具副本上跑，
分别在有/无 `sqlite_stat1` 两种状态下看 VM 步数和 `EXPLAIN QUERY PLAN`：

| 状态 | 整句 VM 步数 | 四个 `watch` 分档的展开计划（每个 `COUNT(*)`） |
|---|---|---|
| 无 `sqlite_stat1` | **135,746**（≈ v3 的 136,025） | `SEARCH u USING PRIMARY KEY (user_id=? AND library_id=?)` —— covering，30,762 步 |
| `ANALYZE` 之后 | **240,107**（≈ v4 的 240,562） | `SEARCH u USING INDEX user_items_activity (user_id=? AND library_id=?)` —— **不 covering**（缺 `seen`），56,851 步 |

**结论：与 v4 删掉的那几个索引（`terms_media` 等）无关。** 这句 SQL 在 `hdr` 上是"未过滤"形态
（`hdr=true` 在夹具里等于全库，被当恒真项省略），它的开销几乎全部是 `watch` 四个分档对
`user_library_items` 的四次 `COUNT(*)`。`ANALYZE` / `PRAGMA optimize` 写入的 `sqlite_stat1` 让 planner
从"覆盖主键范围扫描"切到"非覆盖索引 + 每行一次主键回查"，每次扫描 30,762 → 56,851 步，四句合计 +104k。
这也解释了为什么确定性计数看不见它：**语句数没变、没有全表扫描，只是同一句语句多访问了一倍的 B 树**。

**修复**（`library_query/members/facets.rs`）：四个分档改成**一次 materialised 聚合**，用三个
`SUM(...)`（`watching = seen - played` 由分档定义直接得出），四个输出都从这一行读：

| 状态 | 修复前（四个 COUNT） | 修复后（一次聚合） |
|---|---|---|
| 无 stats | 135,746 | **127,356**（−6%，已优于 v3） |
| 有 stats | 240,107 | **153,445**（−36%，回到 v3 的 1.13×） |

SMALL 规模同样的趋势：`facets: hdr` 的 `vm_steps` 4,816 → **3,319**（v3 是 3,015）。

**门禁的盲区就此关闭**：十个 facet 维度各有一条 `vm_steps` 预算（`base + files × per_1k / 1000`，
由 424 与 61,000 两个规模拟合、留 ~15% 余量）。它确实会咬——把四处 `COUNT(*)` 放回去，
`cargo test -p store --test perf_baseline` 立刻报：

```
performance gate failed:
  facets: hdr: 4816 vm steps, budget 3816 (row work grew without a new statement)
```

**为什么这里可以断言 `vm_steps`**（而第 1 节说它只记录）：第 1 节的顾虑是"墙钟/计划会随机器和
`ANALYZE` 漂移"。在**固定夹具**上 `vm_steps` 是确定的：十个维度的数值在两次独立运行、两个规模上
逐位重复，且与本文件的预算一致；它不是时长，不含机器速度。剩下唯一的漂移源是 `sqlite_stat1`，
而这次修复正是为了让结果对 `sqlite_stat1` 不敏感（有/无 stats 都优于旧代码）。

**留给 DDL 的一步（未做，按任务要求不上 schema）**：即便修复后，`user_library_items` 的三次聚合在
`ANALYZE` 下仍走非覆盖的 `user_items_activity`（covering 主键 30,762 步 vs 索引 56,851 步）。
一条覆盖索引（`user_items_watch(user_id, library_id, seen, played, favorite)`，或把这三列追加到
`user_items_activity`）可以让"有 stats"也回到 30k 级，即再省 ~1.2×→~4×。这需要一次版本 bump，
由 Lead 决定合并进下一轮 DDL；此处只记录数字与建议。

## 3.3 Task 30：扫描的 Store 锁持有时长（before / after）

**症状**：Task 27 在 `storage_contention` 夹具上看到 10,000 集扫描"持锁 132–138 s / 100 批 ≈ 每 100 文件 1.3 s"。
这个数字决定了"任何有界读等待都会 503"，但先要纠正两点，否则会修错地方：

1. **那不是扫描执行器。** 该夹具的 `run_scan` 直接调用 `publish_files`（每批 100 文件 = 一个事务），debug 构建，
   单个 Media 一路长大。我在 2,000 集上复现：20 批、总 4.5 s、p50 207 / p95 462 / max 476 ms、439.8 文件/秒；
   按批成本随 Media 变大而上升，这正是 10,000 集落到 ~1.3 s/批的原因。**一次 `publish_files` 调用就是一个事务**，
   所以它看起来像"一次扫描持锁"。
2. **真实扫描的持锁是"每阶段"，不是"每批"。** 每个 chunk 固定 6 段：`checkpoints` / `plan` / `record` /
   `scan_media_for` / `enqueue_probes` / `seen`（4 个 chunk = 25 段），不是"每文件"。

**定位**：给 runner 加了 `#[doc(hidden)]`、线程本地、默认关闭的持锁追踪（`runner/hold.rs`；关闭时每段只多一次
thread-local 借用、不读时钟）。逐段测量 400 集、每 chunk 100 文件：

| chunk | Media 里已有文件 | `plan` 持有 | `record` 持有 |
|---|---|---|---|
| 1 | 0 | 162 ms | 37 ms |
| 2 | 100 | 2,144 ms | 66 ms |
| 3 | 200 | 3,176 ms | 95 ms |
| 4 | 300 | 6,247 ms | 196 ms |

`plan` 随 Media 增大而增长（另一份 200 集的隔离运行更干净：chunk 1 = 42 ms，chunk 2 = 2,012 ms——同样的工作量贵 48×），
而 `record`（真正的入账 + 归约）只有 37–196 ms。逐个 micro-measure `plan` 里的每文件调用，只有一个是贵的：
`resolve_scanned_media` 11 ms（Media 100 文件时）/ 94 ms（400 文件时）；`file_coverage_by_path` 60 µs、
`scan_write_context` 44 µs、`library_display_locale` 4 µs。根因：`resolve_scanned_media` → `update_media` →
**`reduce_media_everywhere`**，即**每个被扫描的文件都把该 Media 的整张卡片重建一次**，且发生在 Plan 段的锁内
——adoption 本身什么都没改。

**批大小的杠杆朝上**（400 集、同一批文件，只改 walker 的 chunk 预算）：

| chunk | chunk 数 | 扫描墙钟 | 最长 `plan` 持有 | 事务数 |
|---|---|---|---|---|
| 25 | 16 | **138.2 s** | 0.73 s（且仍随 Media 增长） | 16 |
| 100 | 4 | 48.8 s | 6.25 s | 4 |
| 400 | 1 | **18.1 s** | 0.95 s | 1 |

**直觉在这里是反的：更大的 chunk 既更快、又更短持有。** O(Media 文件数) 的每文件成本在任何 chunk 大小下都要付，
而每个 chunk 还要再付一次固定开销（断点判定、plan 初始化、事务、投影 settle）。所以"缩小批"不是杠杆
（本轮也已明确排除），杠杆是删掉那次每文件的卡片重建。

**修复**（`store/src/media/mod.rs`）：`update_media` / `update_media_if_matches` 在身份已经 current 时
**既不改行、也不重建摘要**。判据与写入共享同一份参数（`media_write_params()` 同时喂 `WRITE_MEDIA_SQL` 与
`PLAN_MEDIA_SQL`；`media_alias_ids()` 是唯一的别名清单），覆盖写入的全部三种效果：行、每张卡片的
`identity_state` 副本、每个外部 id 的 `media_aliases` 行（id 在行上但别名行缺失 ⇒ **不是** no-op）。
比较用 `IS` 而非 `=`（`year` / `original_title` / `aliases_display_json` 可空，`NULL = NULL` 是 NULL）。

| 指标（400 集、chunk 100） | before | after |
|---|---|---|
| `plan` 每 chunk | 162 / 2,144 / 3,176 / **6,247 ms** | 36 / 18 / 17 / **17 ms** |
| 整次扫描的持锁总和 | 13.83 s | **2.13 s** |
| 最长单次**持有** | 6.25 s | **0.17 s**（`record`；0.60 s 的 `enqueue_probes(total)` 是一百次短锁的**调用总计**，不是一次持有） |
| 扫描墙钟 | 48.8 s | **39.5 s**（吞吐 +23%） |

**为什么跳过重建是安全的**：`reduce_media_everywhere` 发布的卡片事实、`library_item_terms`、用户摘要、查询投影
与 NFO 输入清理全部由 `library_files` + metadata 输入重算；它们与本调用共享的输入只有身份列，而那正是判据比较的
列。身份写入与重建在**同一个事务**里，因此"行没变但重建欠账"不可能存在；需要修复的调用方走
`Store::refresh_media_summaries`，本函数不承担修复职责。

**确定性断言**（不靠墙钟）：`Store::with_stats` 下，一次相同身份的 adoption 是 **7 条语句 / 0 行写入**，
一次真实身份变更是 **43 条语句 / 31 行**。`store/src/media/identity_no_op_tests.rs` 断言跳过路径
`statements <= 8`、重建路径 `> 20`，并用 `total_changes` 断言零写入；它还钉住两个"朴素守卫会漏掉"的边界：
**id 在行上但别名行缺失**、**卡片 `identity_state` 过期**——两者都必须照常写入。扫描侧的确定性钉子是结构性的：
`api/src/http/library_scan/runner/hold_tests.rs` 断言锁段数随 **chunk** 增长而不是随文件增长（每 chunk ≤ 8 段），
所以"把锁挪进每文件循环"这类回归不用计时就会失败。

**仍然存在的一段，以及下一步**：`record`（真正的入账 + 归约）现在是唯一真实的持有，170 ms，且随 Media 合法增长
（由 `store/src/scan_probe_obligations.rs::insert_scanned_files` 完成）。等 `store::stats` 的闭包式计数对 `api`
可用后，这里是第一个值得上硬语句预算的位置。

## 4. 硬断言 vs 仅记录

**硬断言**（`crates/store/tests/perf_baseline/gate.rs::limits()`，每个操作一行）：

| 计数器 | 说明 | 谁在用 |
|---|---|---|
| `statements` | 该操作执行的 SQL 语句数，上限 | 全部 instrumented 读路径 |
| `fullscan_steps` | 全表扫描步数，上限 `base + files × per_1k / 1000` | 全部 instrumented 读路径 |
| `ledger_scans` | `Store::list_file_coverage` 被调用的次数（它内部把 `FullscanStep` 累加进这个计数器），上限 0 | 所有热点操作 |
| `media_ledger_reads` | `Store::file_coverage_for_media` 被调用的次数，上限 0 | 所有热点操作 |
| `role_lookups` | 以 `app.db` 角色查询计的授权次数 | 需要授权的操作，上限 1 |
| `vm_steps` | 十个 facet 维度各一条（`base + files × per_1k / 1000`），拟合留 ~15% 余量 | `facets: *`，理由与实测见 §3.2 |

**仅记录**：`p50/p95/max ms`、十个 facet 维度以外的 `vm_steps`、`sql_duration_ns`、`dbstat` 字节数、第 6 节的写侧墙钟。

### 4.1 Task 21：新增的硬断言，以及每个上限为什么是这个数

`Store::with_stats(StatsScope, |…| …)`（`#[doc(hidden)]`、`test-support` 才编进去）把生产入口包起来，
用 SQLite 自己的 trace 报出语句数与 `FULLSCAN_STEP`。它和 `publish_files_with_stats`、
`PENDING_DISPATCH_SQL` 是同一个套路：closure 里调用的是**生产方法本身**，所以测试无法绕过被测查询；
计数来自数据库而不是手写累加器，所以不会出现“路径和计数器互相同意”的假绿。

| 操作 | 断言（实测 → 预算） | 为什么是这个数 |
|---|---|---|
| item detail（movie / tv work） | 语句 11 → **12**，扫描 0 → **0** | 行 + 文件页是一条固定链；12 只给夹具的多版本留 1 条余量。每文件一次查询（电影 2 个版本、剧集 9 集）会立刻超 |
| playback authorization roster | 语句 2 → **3**，扫描 4 → **8**（`role_lookups` 仍 ≤1） | 角色读 `app.db`、可见库读 `library.db`，各一条；4 步扫描来自 1 行的用户表，与规模无关 |
| playback source selection（带 scope） | 语句 3 → **4**，扫描 4 → **8** | roster 两条 + 源查询一条。多一次授权或一次 per-file 查询即判红 |
| playback source selection（无 scope） | 语句 1 → **2**，扫描 0 → **0** | 只有一条按索引的源查询；公开封面路由不该有任何额外读 |
| playback progress write | 语句 23 → **25**，扫描 4 → **8** | 一个事件：写事务 + 事件行 + 每用户摘要 + 计数。25 只容 2 条余量——第二次摘要归约或一次额外往返会超 |
| integrity status lookup | 语句 3 → **4**，扫描 0 → **0** | 三个按键查询（操作行、视图、报告）；扫描必须为 0，否则就是把 `library_operations` 走了一遍 |
| scan publication（insert / republish） | 语句 `1,750 + files×120/1000`，扫描 `900 + files×60/1000` | 发布是唯一随规模增长的写路径：每发布文件 8.05 条语句 / 4 步扫描，在两个尺度上校准（424 → 1,610 / 800；61,000 → 8,050 / 4,000），留 ~12–15% 余量。斜率只能拟合到 `files`（`check` 只知道这一个尺度变量），因此**同时改 `publish_slots` 而不改文件数时需要重新校准** |

这些行的预算都是上限而不是等式：变便宜不需要动 `limits()`；变贵必须在同一个提交里写清为什么。

**一次修复落地后要同步收紧预算**：预算是上限，Task 11 之后 `BROWSE_READ_LEASE` 从 277 / 25,188 收紧到
20 / 8。理由见第 3.1 节——留在旧值等于把旧行为合法化，回归就无从发现。
**永远不要把墙钟数字改成断言，也不要把已有的确定性预算放宽来“修”一次失败**：预算变宽只有一种合法理由——
这次提交确实多了确定性的工作量，并在同一个提交里写清为什么。

## 5. 门禁确实会咬（负向对照）

`crates/store/tests/perf_baseline/gate.rs` 里三个负向对照常驻默认目标，每个都调用**门禁自己用的** `check`：

1. `gate_rejects_a_full_table_scan`：直接测量 `Store::list_file_coverage`（真实生产 API，全表扫描），
   用零预算判它 → 必须判红。
2. `gate_rejects_a_ledger_scan_inside_a_hot_read`：模拟 D1 回归（`delivery.rs` 曾在 Store 锁内对
   `list_file_coverage` 做全表扫描），detail 读本身不变，只多一次 ledger 扫描 → 必须判红。
3. `gate_rejects_a_duplicated_query`：同一页查询每请求跑两次（N+1），全表扫描仍是 0，但语句数 6→12 → 必须判红。
4. `a_lost_index_fails_loudly_instead_of_scanning`：删掉 `library_item_terms` 的索引后，热点查询**不会**
   静默退化成全表扫描，而是因为 `INDEXED BY`（计划勘误 E4）直接报 `no such index` —— 记录这条硬契约。
5. 附带发现（第 6 节测量时撞到的）：删掉**任意**一个 `LIBRARY_INDEXES` 里的索引后，读租约路径会以
   `StoreError::LayoutMismatch("library database schema is missing required index …")` 拒绝服务，而不是降级。
   即“索引名”是硬契约：DDL 与 `objects.rs` 不同步时是先炸开，不会静静地变慢。

实际输出（2026-10-26）：

```
negative control (full table scan) rejected as designed: ["control: full ledger coverage scan: 123 full ledger scans, budget 0"]
negative control (ledger scan in a hot read) rejected as designed: ["detail + full ledger scan: 123 full ledger scans, budget 0"]
negative control (duplicated query) rejected as designed: ["browse page 1 (query run twice): 12 SQL statements, budget 6"]
negative control (lost index) fails loudly as designed: no such index: terms_media
```

## 6. Task 10 的 v3 DDL：写侧成本

计划加了 6 个索引（`operations_library_kind`、`playback_sessions_media`、`playback_logs_media`、
`playback_metrics_media`、`pending_deletions_due`、`metadata_inputs_media`）并把 5 张表改成 `WITHOUT ROWID`。
索引在没有任何查询计划选中它之前都是纯写放大，所以这里回答两个问题：**它让写变慢多少**，以及**有没有任何
被测查询用它**。

方法（`crates/store/tests/perf_baseline/ddl_write_cost.rs`）：同一份夹具内交替 `有索引` / `无索引`，
两次读取测量**相邻且数据完全相同**，中间只差一个索引（用 `sqlite_master` 里它自己的 SQL 重建）。
“分别播种两份夹具再比” 的做法已经被否掉：夹具之间的墙钟差（1,000 文件发布上数百毫秒）远大于索引维护成本。

被测的 19 个幂等读操作（`ops::reads_without_layout_guard`：浏览首页/深 offset/深 cursor、条目详情 ×2、
十个 facet 维度、播放授权/无用户播放源/播放源、完整性状态）在逐个删掉索引后，**确定性计数一个都没有变**：
两次独立运行（同一台机器，一次在高负载下、一次空闲）都得到 “none”。

| 索引 | 删掉后确定性计数变化的读操作 | 发布墙钟 ms/100 文件（有/无，运行 A） | 发布墙钟（有/无，运行 B） |
|---|---|---|---|
| `operations_library_kind` | 无 | 81.0 / 131.9 | 88.2 / 141.0 |
| `playback_sessions_media` | 无 | 99.9 / 88.1 | 77.4 / 85.4 |
| `playback_logs_media` | 无 | 96.1 / 102.3 | 86.3 / 88.4 |
| `playback_metrics_media` | 无 | 654.2 / 1018.2 | 90.7 / 93.3 |
| `pending_deletions_due` | 无 | 133.4 / 138.9 | 105.7 / 97.2 |
| `metadata_inputs_media` | 无 | 112.5 / 410.1 | 106.8 / 107.1 |

进度写（每事件 ms，有/无）在两次运行里都是 0.21–0.23 对 0.21–0.23，即无差别。

**读数（给 Lead 的裁决建议）：**

- **不能据此删任何索引。** 上表 “无” 的含义是 “在这 19 个被测读操作里没有计划用它”，不是 “没人用它”。
  六张被索引的表在这份夹具里几乎是空的，所以它们的读取者根本不会出现：
  `playback_logs`/`playback_metrics`（只有一次会话，几行）、`pending_file_deletions`（0 行）、
  `library_operations`（1 行）、`library_metadata_inputs`（0 行，元数据是走 `set_metadata_overrides` 写的）。
  `library_protocol_people` 同样是 1 页（夹具没有 cast/studio 事实）——这是夹具的已知缺口。
- **写维护成本在噪声以下，不能归因**：`playback_*` / `pending_deletions_due` / `metadata_inputs_media`
  的有/无差异在两个方向上都出现过（`metadata_inputs_media` 一次 +300 ms、一次 +0.3 ms；
  `playback_metrics_media` 一次 +364 ms、一次 +2.6 ms），量级就是本机的 fsync 抖动，不能当成索引成本。
  唯一两次同向的是 `operations_library_kind`（有索引时快 50–60 ms/100 文件），即“保留它更省”，
  方向与“纯写放大”相反。
- **没有任何一条被测查询依赖这 6 个索引**：19 个读操作的确定性计数在两次运行里都没动。
  所以**不能**据此宣布任何一个是“纯写放大、没有计划选中”——这一类结论需要先把被索引的表填满再测（见下）。
- 空间（`dbstat`，REALISTIC 规模 61k 文件）：6 个新索引各 1 页 = 4 KiB（因为上表那些表是空的）。
  真正占空间的是：`library_item_terms` 19.9 MB / 4,865 页、`library_file_units` 8.3 MB / 2,023 页、
  `library_protocol_terms` 8.0 MB / 1,942 页、`library_items` 6.8 MB + 主键 autoindex 1.85 MB、
  `user_library_items` 0.96 MB。前三个正是 v3 改成 `WITHOUT ROWID` 的三张表。
- **建议**：不要在 Task 16 里因为 “本夹具没测到读者” 而删这 6 个索引；要删就先按真实形状补测量：
  装满 `pending_file_deletions` 的到期待删队列、上万行 `playback_sessions/logs/metrics` 的
  `delete_playback_for_media`、上千行 `library_operations` 的完整性状态子查询。

`WITHOUT ROWID` 五张表的前后对比无法事后测量（需要回退 DDL，属于 src 改动，本任务不允许），
这里只记录改完之后的规模与读写计数；它们的收益（少一棵行等宽的 B 树）由 Task 10 自己的测试与上面的
`dbstat` 数字共同体现。

## 7. 测不到的东西，以及为什么

| 操作 | 缺什么 | 现在的证据 | 要怎么办 |
|---|---|---|---|
| ~~item detail（电影/剧集）、播放源选择、播放进度写、发布、完整性状态查询~~ | **已解决（Task 21）** | `Store::with_stats` 把生产入口包起来，用 SQLite trace 报语句数 / `FULLSCAN_STEP`；门禁已按第 4.1 节改成硬断言 | — |
| 会话签发（`issue_play_session`） | 未单独立项：它在门禁里是 `playback progress write` 的前置步骤，而那段成本已经落在 23 条语句里 | 同 `PlaybackSession::open` 的墙钟 | 需要单独门禁时，用同一个 `with_stats` 包一次即可，无需再改 `crates/store/src/**` |
| 写路径的**行数**（不是语句数） | 写放大在行级别只由 `PublishReport`（`inserted/updated/unchanged`）与 `publish_files_with_stats` 的 `written_rows`（SQLite `total_changes`）表达 | 第 6 节的 A/B + 发布形状断言 | 已在 `publish_files_with_stats` 覆盖，无需另立计数器 |
| 深翻页的 cursor 版 | API 仍在用 `BrowsePage::Offset`（Task 14 才切 cursor） | 直接测量 store 层 `BrowsePage::Cursor` 查询，并注明这一点 | Task 14 落地后，把 HTTP 面也纳入 |

## 8. 读数（给 Lead 的发现清单）

1. **读租约的固定开销（D3）—— v4 已修复，收益由此可量化**：v3 时 `browse via read lease` 是
   277 条 SQL 语句 / 25,188 全表扫描步数，而这次请求真正的查询只有 6 条语句 / 0 步：每次浏览请求有
   271 条语句、25k 步花在布局校验上（`validate_reader` 在 prepare 和事后各跑一次全量校验）。
   Task 11 之后是 **17 / 4**（p50 1.46 → 0.49 ms）——语句 ÷16、扫描步数 ÷6,300。门禁的预算已同步
   **收紧**到 20 / 8：上限若还停在 277 / 25,188，恰好等于旧行为，就发现不了“退回全量校验”的回归。
2. **offset 深翻页 vs cursor（D5，Task 14 的直接依据）**：同一深度（offset 5,600）：offset 版 6 条语句 /
   24,338 vm 步 / 每页重算 total；cursor 版 5 条语句 / 2,147 vm 步。11 倍的 VM 差就是 cursor 分页的收益。
3. **facets 是读放大最大的热点**：p50 12–48 ms、全表扫描 28k–98k 步；只有 `hdr` 维度是 0 步
   （它只读 `library_query_counts` 边际表）。其余九个维度在按“每张卡的 terms / watch 行”计数，是
   O(cards) × 10 的读放大 —— 这是 Task 16 审计“`library_query_counts` 到底有没有被用上”的直接证据。
   **v4 新发现**：`hdr` 的 `vm_steps` 从 136,025 涨到 240,562、p50 0.74 → 6.04 ms（8×），而语句数和
   扫描步数都没变 —— 原门禁看不到这一类回归；Task 31 已以单次聚合修复并加入 facet VM-step 预算，见 §3.2。
4. **用户态播放源选择（v3: 56 ms → v4: 0.056 ms，已修复）**：v3 时 61k 文件下 p50 56 ms / p95 59 ms，
   比浏览首页慢约 900 倍。三次测量把它钉住了：
   授权 roster 0.007 ms、**不带用户 scope** 的同一条源查询 0.029 ms、带 scope 的 56.4 ms —— 也就是说
   2000 倍的差距来自 `playable_entries` 里那句 `AND f.library_id IN (…)`：带上库过滤后计划不再按
   `media_id` 定位，而是按库扫 `library_files`。这是真正的热点读回归，**已在 Task 19 修复**：固定连接
   顺序（`CROSS JOIN`）让计划重新从 `media_id` 定位，不加索引、不改结果集；REALISTIC 上
   27.0 → 0.058 ms p50，计划与断言见播放源小节。Task 21 已补上语句计数器，
   现在同时由 EXPLAIN 断言与 scoped 4 statements / 8 fullscan、unscoped 2 / 0 预算保护；墙钟只记录。
5. **发布写侧**：v3 是 950.8 ms / republish 2,140.9 ms；v4 是 166.5 ms / 68.1 ms（同轮机器负载高，
   绝对值只作参考，趋势是明显变快）。v3 时重发布比首次发布还慢 2.25×，`PublishReport` 却报
   `unchanged = 1000`（一行都没写）—— 这条写放大候选在 v4 上已不明显，Task 16 复核时可关掉。
6. **播放进度写**：0.26 ms/事件（`WRITE_PROBE` 规模），会话签发约 1.0 ms。
7. **`list_file_coverage` 控制项**：61k 文件 → 61,999 个全表扫描步数、129 ms。这正是 D1 时代每个请求都在
   做的事；门禁把它的预算是 0。

## 9. 怎么更新基线

1. 跑完整门禁，把第 3 节的表整段替换成新输出（表是程序直接打印的 Markdown，可以整段粘贴）。
2. 只有当**确定性**计数变化时才动 `limits()`，并在提交信息里说明原因；墙钟变化只更新表里的毫秒数。
3. 预算是上限，不是等式：更便宜不需要改文件。
4. 新增热点操作必须同时加预算行，否则 `assert_gate` 会以 “no budget pinned” 判红。
