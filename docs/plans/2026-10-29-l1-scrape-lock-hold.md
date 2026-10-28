# L1a：缩短 Scrape 周期任务的共享 Store 锁持有时间

日期：2026-10-29 · 关联：[存储锁审计 L1](../audits/2026-10-28-storage-lock-review.md#l1--p1共享锁仍把读取排在扫描后面) · 2026-10-29 E2E 测试 §6（报告未入库）

## 现象

E2E 中 Scrape 任务每 10 分钟扫 `tv/anime`，约 35 s 内所有库的详情请求被挡住（32–41 s，store_wait 5 s 预算耗尽返回 503）。读仍走共享 `Arc<Mutex<Store>>`（L1 未修）；本计划不做 T1/T2 的架构改造，只消除 Scrape 路径上**与目录规模或全库规模成正比的单次持锁**。

## 持锁点（`crates/api/src/watch_scrape.rs`）

| # | 位置 | 单次持锁内的工作 | 规模 |
|---|---|---|---|
| A | `attach_missing_posters` | `list_file_coverage()`：**全库**所有文件 + 全部 unit 行，再在内存按目录过滤 | O(全库文件 + 全库单元)；仙逆缺陷时 ≈10 万单元 |
| B | `scrape_dir` 规划块 | 对目录内**每个**文件 `plan_paths`（reuse 查询 / resolve_scanned_media / scan_write_context） | O(目录文件) |
| C | `scrape_dir` 记录块 | `observed.record` 一次写入全部文件 | O(目录文件) |
| D | `resolve_scanned_files` | 逐文件 `file_coverage_by_path` + `get_media`，整个循环一把锁 | O(目录文件 × 单元) |

## 方案

1. **A → 目录范围、无单元的查询。** store 新增 `file_media_under(dir)`：`library_files` 上按 `path_key` 前缀范围（`>= dir/` 且 `< dir0`，走 UNIQUE 索引）只取 `(media_id, display_path)`。不再加载单元，也不扫全库。
2. **B/C → 分批。** 每批 `SCRAPE_BATCH = 64` 个文件：批内取锁 → `validate_scope` + `library_accepts_work` + lease 检查 → plan；锁外 `observe`；再取锁重验 → `record`。每批都重验，与现有单批语义一致（任一批发现 scope 变化即按原规则中止/跳过），不引入新的半成功形态：每批 record 本身仍是原子的，失败只影响未提交的后续批，下次 Scrape 会补上。
3. **D → 逐文件短锁。** 每个文件单独取锁读 coverage/media，读完立即释放。

## 不做的事（留给 T1/T2）

- 读路径不经共享锁、按数据库的写调度器、reader 等候队列。
- 扫描（`library_scan` runner）与 Probe 发布路径的持锁；它们已有各自的分批/短事务实现，本轮不改。

## 验收

- store 测试：`file_media_under` 只返回目录下（含子目录）文件，不返回同名前缀的兄弟目录（`/a/b` vs `/a/bc`），不返回单元。
- api 测试：Scrape 处理 > `SCRAPE_BATCH` 个文件的目录时全部入库，与单批结果一致；分批期间锁被释放（另一线程能在两批之间拿到 Store 锁）。
- 现有 watch/scrape 测试全部通过。
