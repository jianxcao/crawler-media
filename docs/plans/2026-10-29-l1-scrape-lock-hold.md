# L1：读写锁分离（读走只读连接 + 写锁最小化，含 L1a Scrape 持锁）

日期：2026-10-29 · 关联：[存储锁审计 L1](../audits/2026-10-28-storage-lock-review.md#l1--p1共享锁仍把读取排在扫描后面) · 2026-10-29 E2E 测试 §6（报告未入库）

## 现象

E2E 中 Scrape 任务每 10 分钟扫 `tv/anime`，约 35 s 内所有库的详情请求被挡住（32–41 s，store_wait 5 s 预算耗尽返回 503）。读仍走共享 `Arc<Mutex<Store>>`（L1 未修）；根因是**读与写共用一把进程级 `Mutex<Store>`**：任何长写批都会把所有读排在后面。缩短某一处持锁只是治标，因此本计划做完整的读写分离（T1 + T2），L1a 作为写侧的一部分保留。

## 全貌

| 层 | 目标 | 做法 |
|---|---|---|
| T1 读侧 | 所有读端点（含详情页）永不碰共享写锁 | `Store::open_reader()`：同四个库文件的只读 `Store` 门面（`SQLITE_OPEN_READ_ONLY` + `query_only`，注册 casefold / `norm_path_key`，root alias 按文件共享）。`ApiState` 持有一个有界只读 Store 池；`store_wait::store_read` 改为从池里借只读 Store，不再锁 `state.store`。WAL 下读者看到最近已提交快照，不被写事务阻塞。误写的读路径会被 SQLite 拒绝（显式失败，不会偷偷争写锁）——这些调用点要么改成真正的写路径（`with_bounded_store`），要么去掉写。已有的 `library_reads`（library.db 读池）与 `app_reader` 保持不变。 |
| T2 写侧 | 写锁只覆盖单行或小批，写完即放 | 扫描 / 探测 / 刮削按批（`SCRAPE_BATCH=64`）或逐文件取锁；IO 一律在锁外；见下方 L1a。 |
| L1a | 消除 Scrape 路径上与目录 / 全库规模成正比的单次持锁 | 下表 A–D。 |

验收：后台长时间 scrape/scan/probe 写入期间，详情页延迟接近空闲时（以 `store_read` 不再等写锁为结构保证，并用测试锁定：写锁被占用时读端点仍在预算内返回）。

## L1a 写侧持锁点

（`crates/api/src/watch_scrape.rs`）

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
