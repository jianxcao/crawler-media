# 存储重构性能报告（feat/storage-redesign）

本页性能数字为历史阶段测量，本轮未重跑。当前实现/正确性及门禁见[5bd696e修复复核](../audits/2026-10-24-storage-fix-review.md)。只收录日志与基准文档里的真实数字；未测项单独标出。在线页面：https://cursor.com/canvas/store-d2860fd7-7b74-430e-9855-1a3525523fbe/e3dea255-fc03-4210-9944-cbc78ac2e26b

## 结论
- Jellyfin 协议首页（50k 电影）从 >20 分钟未完成降到 148.8 ms；根因是选源子查询走错索引。
- Q4c 写入期索引后，genre / person / search 在 50k 时从 878.7 / 1741.0 / 531.5 ms 降到 221.0 / 6.0 / 58.7 ms。

## Q4 / Q4b Jellyfin 协议分页（store 级 release，7 次中位数，ms）
| 形状（50k） | Q4 前 | Q4 后 | Q4b 后 |
|---|---|---|---|
| first_page | >20 min（中止） | 208.4 | 148.8 |
| deep_offset 40000 | 未测 | 289.8 | 212.7 |
| genre Drama | 未测 | 1269.6 | 878.7 |
| person | 未测 | 1945.7 | 1741.0 |
| search | 未测 | 933.7 | 531.5 |

| 形状（5k） | Q4 前 | Q4 后 |
|---|---|---|
| first_page | 7246.1 | 290.1 |
| deep_offset（空页） | 6815.0 | 17.4 |
| genre Drama | 1279.3 | 383.6 |
| person | 195.4 | 206.8 |
| search | 100.7 | 112.0 |

改动：LIMIT 之后再选源（`sourced AS MATERIALIZED`），一元 `+` 强制索引，`COUNT(*) OVER ()` 一次扫描得到页与总数。5k 的 Q4 后数字来自中间版本。

## Q4c 写入期 genre / person / search 索引（6419c0a）
| 形状 | 5k 前 → 后 | 20k 前 → 后 | 50k 前 → 后 |
|---|---|---|---|
| genre Drama | 82.7 → 21.3 | 325.7 → 85.4 | 878.7 → 221.0 |
| person | 173.5 → 2.5 | 598.3 → 3.2 | 1741.0 → 6.0 |
| search | 55.7 → 2.8 | 181.6 → 21.6 | 531.5 → 58.7 |

- 卡片归约时写入 `library_item_terms`（jellyfin_genre、person_id、person_tmdb_id）与 `library_items.search_text`（全部语言），索引预筛候选后再跑精确语言链谓词；切换显示语言无需刷新。
- 关键索引 `terms_value`；没有它时 50k genre 508、person 472 ms。未用 FTS5。
- 50k first_page 139.7 ms，deep_offset 199.4 ms。
- **遗留**：genre 仍随命中行数线性增长，约 70k 时预计超过 300 ms。

## Q3b 分面聚合（50k，请求级）
前（26 条语句）median 428.5 / p95 488.1 ms → 后（11 条语句）237.0 / 265.1 ms。

## Q3a studio 投影（代理测量，100k）
读整块 JSON 1614 ms / 604.6 MB → json_extract 投影 991 ms / 1.1 MB。

## Q2 用户摘要写入次数（100 文件批次）
40 用户：240.3 ms / 4000 次写入 → 41.4 ms / 40 次；0 用户 40.7 → 39.5 ms。

## Q1 反向索引代价（代理测量）
插入 200k 行 725 → 799 ms（+10%），大小 16.49 → 18.28 MB（+11%），400 次反查 1642.5 → 1.5 ms。

## Q5 长操作与连接策略
- 当轮Store四连接显式synchronous=FULL（含cache）。截至5bd696e，Jobs Queue也已显式FULL并通过OFF→policy→FULL回归；独立media ResponseCache仍未显式设置该PRAGMA。
- 删库清理按 ≤100 文件分批提交，崩溃后可续跑。
- cache NORMAL / mmap / cache_size / temp_store / WITHOUT ROWID：**未做 A/B，默认值不变**。

## O01 深游标 / 100k 完整性检查（早期轮次，未重跑）
- 无关库文件 1k → 1M，vm_steps 恒为 1963。
- 100k 完整性检查 1000 批共 122 s，最慢一批 215 ms，不随进度退化。

## 当前边界（2026-10-24，5bd696e修复复核）

- 原NFO跨批丢失、冻结目标推进cursor、alias NFO首次采用及正常重开默认根的具体反例已修；根operation/旧scope屏障和Unidentified仍开，rename共享图片还有alias反例。
- 启动root恢复已逐batch释放Store；完整rename/Probe/NFO出锁仍未完成，SQL路径normalizer还新增锁内realpath。没有本轮新生产锁环证据。
- fixture新增写timeout和部分panic传播，但完成线程丢handle、最后owner join环和总退出deadline仍未闭环。
- tracks已绑定queued version；实际Marker/capture生产链仍能将旧结果写入换源后的状态。具体复现与方案见[本轮复核](../audits/2026-10-24-storage-fix-review.md)。
- A8竞争写者行为RED、A12 NFO export区分性失败RED仍缺；不能据此直接判当前行为坏。
- genre仍线性；约70k超300ms是外推，非实测。cache NORMAL/mmap/cache_size/temp_store/WITHOUT ROWID未做A/B。
- NAS、Jellyfin真机、100k Media/1M文件整体规模均Not run，历史性能数字没有被本轮验证为当前性能。

历史完整门禁（05a7363）：workspace 2652/0（12忽略）、Web842/0、tsc/fmt干净。**当前5bd696e**：workspace停于jellyfin_sql_page两个FK失败，Store停于shared artwork失败；Jobs55通过/1忽略、Web842通过和typecheck通过。详见本轮复核，不将历史绿测写成当前通过。
