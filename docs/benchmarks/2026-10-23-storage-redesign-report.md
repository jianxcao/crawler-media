# 存储重构性能报告（feat/storage-redesign）

只收录日志与基准文档里的真实数字；未测项单独标出。在线页面：https://cursor.com/canvas/store-d2860fd7-7b74-430e-9855-1a3525523fbe/e3dea255-fc03-4210-9944-cbc78ac2e26b

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
- 权威库显式 synchronous=FULL（耐久性修复）。
- 删库清理按 ≤100 文件分批提交，崩溃后可续跑。
- cache NORMAL / mmap / cache_size / temp_store / WITHOUT ROWID：**未做 A/B，默认值不变**。

## O01 深游标 / 100k 完整性检查（早期轮次，未重跑）
- 无关库文件 1k → 1M，vm_steps 恒为 1963。
- 100k 完整性检查 1000 批共 122 s，最慢一批 215 ms，不随进度退化。

## A 系列状态
- A6：嵌套库共享 NFO 复制（fa05e9d）；根迁移 ≤100 分批 + 持久游标续跑（本提交）。
- A7：跨文件系统重命名已测；sidecar IO 移出 Store 锁（95d77c7）；housekeeping 对账路径仍持锁记录（未完成）。
- A8：只有编译期 RED，行为级未复现（未完成）。
- A11：probe 与指纹缓存 API 级存活 / 版本校验（未完成）。
- A12：旧代码上未复现失败（未完成）。
- 真实 NAS、Jellyfin 真机、100k Media / 1M 文件整体规模均未测。

最后一次完整门禁（05a7363）：cargo test --workspace 2652/0（12 忽略），web 842/0，tsc、fmt 干净。之后的提交只跑了受影响 crate。
