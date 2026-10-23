# Storage Runtime Correctness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复提交 `572d4fa` 中确认的锁、Playback、根迁移、rename 与 locale 正确性缺口，不改变已有领域语义。

**Architecture:** 保留五文件和同库原子摘要；磁盘/网络观察在锁外，持久意图、版本 CAS 与生命周期屏障衔接 IO 和数据库。用户标记只写目标 scope；跨文件系统移动使用可恢复的持久证据，而非放宽时间戳校验。

**Tech Stack:** Rust 2024、rusqlite、SQLite WAL、Axum、Tokio、parking_lot；复用已有依赖和公开测试 fixtures。

**Spec:** [主规范](<../specs/2026-10-18-library-storage-redesign.md>) §§4–6；[本轮证据及撤回项](<../../audits/2026-10-23-storage-committed-head-verification.md>)。基线 HEAD=`572d4fa8870271033068b9ede0cea71ecbdf922b`，main=`86122d73a4a23439e1d83234590fdd98b388fc50`。**此交付只写文档，不授权实施；未提交代码不属于审查基线。**

## Global Constraints

- “无旧数据导入、兼容视图、双读双写、旧版本回退。”
- “所有索引事实、覆盖、terms、卡片和用户摘要在同一 library.db `BEGIN IMMEDIATE` 事务内发布；冷 IO/网络/JSON 解析必须在事务外完成。”
- “扫描以**最多100个文件**为提交窗口”；完整性校验“每批最多100个Media”。输入有界不代表单 Media 工作耗时有界。
- “Media、原始用户进度、Collection成员不随最后文件删除。”禁止 file/library FK 级联删除观看事实。
- whole=`(-1,-1)`；“TV 的 S00/E00 是合法分集，不能兼作 whole。”full_file 只完成所选分集；merge played 三元组 OR 政策保留。
- 无真实观看不推进 watch_activity_at；恢复包禁止跨源拼接；favorite 与 played 时钟独立。
- `.rs`≤800行、函数/测试函数≤120行；公开 seam RED→GREEN，修改的每个 crate 独立测试，最终稳定树运行一次 `cargo test --workspace`；无真实外部网络/Downloader。
- 静态图片保持 public，Jellyfin 取流保留鉴权；不碰真实数据库/媒体/服务，不把本轮计划当作已修复证据。

## Review Focus

1. publication 内部析构 lease 与 cancel 并发：双方必须退出且新 lease 被拒绝（A1）。
2. 等待库 idle 时 Subscribe 被编辑/删除：重读配置后拒绝旧计划，不靠提前等待的快照发布（A2）。
3. 新源上报省略 duration：不得沿用旧源时长、位置、轨道，重复 Stop 不增加计数（A5）。
4. 源已删、目标是相同 size/mtime 的外来文件：恢复必须拒绝，不能误删/覆盖任一文件（A7/A8）。
5. symlink alias、共享 work NFO、区域语言切换及重启：归属/本地字段不丢，卡片和相同语言请求的详情一致（A6/A9）。

---

说明：下列测试代码块固定输入与结果断言；fixture变量由每步列明的现有测试辅助构造，不要求新增同名生产符号。

## 文件责任与依赖

| 任务 | 文件责任 | 提供的接口/依赖 |
|---|---|---|
| A1 | LibraryWorkRegistry 取消/发布锁合同 | 保留现有 public signatures；独立先行 |
| A2 | Transfer 等待与异步 handler 阻塞边界 | 依 A1；不持 Subscribe/Store 锁等待 idle |
| A3 | Store/Queue/response cache 写事务政策 | 新内部 write_tx；先于 A4–A9 的写入口 |
| A4 | 逻辑 played/favorite 变更 | 唯一 mutate_playback 和有效状态读取 |
| A5 | 源感知 Playback event 事实包 | 依 A4，保留 EventOutcome/序列幂等 |
| A6 | canonical roots 与关联事实迁移 | 新准备后 roots 数据；依 A3 |
| A7 | 视频磁盘意图/receipt | Store 只管 DB、worker 锁外执行 IO |
| A8 | sidecar 执行决议/发布/清理 | 依 A7，不读 begin 时过期决议 |
| A9 | locale 解析与 identity 投影同步 | 依 A6，供查询优化计划复用 |
| A10 | 测试 RAII/server 生命周期 | 可独立；为前述竞态提供可靠失败退出 |
| A11 | Probe/marker/管理命令锁外观察 | 依A1/A3；owned观察后版本校验 |
| A12 | NFO导出准备失败隔离 | 正常与错误明确分支，不绕过持久driver |

共享 DDL 的 A6/A7/A8 由同一集成者串行修改；不得并行改 [library.sql](<../../../crates/store/src/schema/v1/library.sql>)。已超800行的 [units](<../../../crates/store/src/playback/units.rs>)、[event tests](<../../../crates/store/tests/playback_event_binding.rs>)、[reconfiguration tests](<../../../crates/store/tests/library_reconfiguration.rs>) 在相关任务内按用例拆分；不额外创建一个覆盖全库的“规模治理重写”。

### Task A1: cancel 不在 registry 锁内等待 publication（C01）

**Files:** Modify [library_lifecycle.rs:437–444](<../../../crates/api/src/library_lifecycle.rs#L437-L444>)；Test [library_work_lifecycle.rs](<../../../crates/api/tests/library_work_lifecycle.rs>)。

**Interfaces:** 保留 `LibraryWorkRegistry::cancel(&self, library_id: &str)`、`LibraryWorkLease::publish<T>(&self, impl FnOnce()->Result<T,String>)->Result<T,String>`；消费现有 `library::WorkControl`，不是自造 Arc<WorkControl>。

- [ ] **Step 1 — 写失败测试** `cancel_during_publication_allows_nested_lease_drop`：复用现有 watched 子进程 watchdog；一线程 gate 内等待调度检查点后获取/析构目标 lease，另一线程 cancel 源。断言两个完成 channel 在截止前返回、旧 control cancelled、新 acquire 源失败。另测无 entry 的 cancel 也禁止随后 acquire；不要只比较返回空值。
  ```rust
  assert!(publisher_finished && cancellation_finished);
  assert!(old_control.is_cancelled());
  assert!(registry.acquire(&store, &source_id).is_err());
  ```
- [ ] **Step 2 — RED**：`cargo test -p api --test library_work_lifecycle cancel_during_publication_allows_nested_lease_drop -- --exact --nocapture`；旧实现由外部 watchdog 报进程未退出，不能只给被卡住的 Tokio future 加 timeout。
- [ ] **Step 3 — 实现**：cancel 在 entries 锁内建 entry、设置 cancelled=true、克隆 `Vec<library::WorkControl>`；结束作用域后 cancel 全部 controls。保留 freeze/block 现有合同；不能先放锁再置 cancelled。
- [ ] **Step 4 — GREEN**：上述新测试及 `cargo test -p api --test library_work_lifecycle`、`cargo test -p api`。保留 target lease 在 publication 内 acquire/drop 的已有回归，不用“禁止一切嵌套 lease”掩盖问题。
- [ ] **Step 5 — Commit**：显式 stage 两个任务文件，`git commit -m "fix(api): cancel publication controls outside registry lock"`。

### Task A2: 不在执行器上同步等长锁，不持 Subscribe 锁等 idle（C02）

**Files:** Modify [worker/transfer/run.rs:418–506](<../../../crates/api/src/worker/transfer/run.rs#L418-L506>)、[subscriptions/patch.rs](<../../../crates/api/src/http/subscriptions/patch.rs>)、[subscriptions.rs](<../../../crates/api/src/http/subscriptions.rs>)、[subscription_missing.rs](<../../../crates/api/src/http/subscription_missing.rs>)、[subscriptions/create.rs](<../../../crates/api/src/http/subscriptions/create.rs>)；Create test `crates/api/tests/subscribe_wait_liveness.rs`。

**Interfaces:** 保留 `transfer_one(&ApiState, SubscribeId)->Result<(),String>`，明确仅在阻塞工作上下文调用；保留 `ApiState::wait_library_idle` 和 `http_work.run`。新增内部 `prepare_transfer_after_idle(state:&ApiState, id:SubscribeId)->Result<Option<PreparedTransfer>,String>`；`PreparedTransfer` 在 Transfer 模块内拥有现有 plan/context 与 Library lease，不向 API 暴露。这个 helper 必须完成“等待→重新锁定/重新读取→取得目标 lease”，不是把旧数据包装返回。

- [ ] **Step 1 — 写失败测试**：`single_worker_patch_does_not_starve_transfer_drain` 用单 worker runtime、可控库 lease 与假 Downloader，发 patch 后由独立异步 task 释放 lease；健康请求/释放 task 在2秒内被 poll。`changed_target_while_waiting_is_revalidated` 等待期间改目标库/删 Subscribe，断言不会对旧库发布文件，删除后不重建 Subscribe。
  ```rust
  assert!(health_request_finished_before_deadline);
  assert_eq!(files_published_to_old_target, 0);
  assert!(deleted_subscribe_after.is_none());
  ```
- [ ] **Step 2 — RED**：`cargo test -p api --test subscribe_wait_liveness -- --nocapture`，外部 watchdog 负责真正卡死进程；旧实现不能完成调度/重验证断言。
- [ ] **Step 3 — 实现**：库 idle 等待在任何 Subscribe/Store 锁外；重新取得 guard 后重读 Subscribe、pending、目标/config_revision，再获取 lease，目标变更则释放 guard 重新等待新目标。handler 的同步 guard/Store/catalog 调用整体进已有 http_work，禁止在 async 核心线程直接 parking_lot 等待；不要只把单个 `.lock()` 放入 spawn_blocking 后把 guard 跨 await 传回。
- [ ] **Step 4 — GREEN**：新 target、现有 Transfer tests、`cargo test -p api`；确认 timeout/busy 返回既有结构化错误且无半发布。
- [ ] **Step 5 — Commit**：stage 本任务命名的文件与新测试，`git commit -m "fix(api): release subscription locks while draining library work"`。

### Task A3: 写事务 IMMEDIATE 与可重用 cache 连接（C03）

**Files:** Modify [store/open.rs](<../../../crates/store/src/open.rs>)、[ledger_presence.rs](<../../../crates/store/src/ledger_presence.rs>)、[data_retention.rs](<../../../crates/store/src/data_retention.rs>)、[metadata/adopt.rs](<../../../crates/store/src/metadata/adopt.rs>)、[metadata/nfo_input/reconcile.rs](<../../../crates/store/src/metadata/nfo_input/reconcile.rs>)、[schema/layout.rs](<../../../crates/store/src/schema/layout.rs>)；Modify [jobs/store.rs](<../../../crates/jobs/src/store.rs>)、[jobs/retention.rs](<../../../crates/jobs/src/retention.rs>)、[jobs/retention_cursor.rs](<../../../crates/jobs/src/retention_cursor.rs>)、[jobs/media_refs.rs](<../../../crates/jobs/src/media_refs.rs>)；Modify [response_cache/mod.rs](<../../../crates/media/src/response_cache/mod.rs>)、[fanart.rs](<../../../crates/media/src/fanart.rs>) 和 [Scrape work](<../../../crates/api/src/scrape_metadata/work.rs>)/[series](<../../../crates/api/src/scrape_metadata/series.rs>)。Create tests `crates/store/tests/write_transaction_contention.rs`、`crates/media/tests/cache_writer_contention.rs`。

**Interfaces:** Store 内新增 `pub(crate) fn write_tx(conn:&rusqlite::Connection)->Result<rusqlite::Transaction<'_>,StoreError>`，使用 new_unchecked/Immediate；jobs/media 用各自错误类型的同等私有 helper，不反向依赖 api/store 私有符号。保持只读 `BEGIN`；跨数据库仍分阶段，不嵌套为假原子事务。

- [ ] **Step 1 — 写失败测试**：A/B 独立连接同一临时库，屏障控制 A 读→B 提交→A 写，先确认原 DEFERRED 返回 `DatabaseBusy` 扩展码 snapshot；再经公开 prune/presence/queue 用例并发 writer，断言操作成功或有界 busy 可重试、无半摘要。cache 初始化与 put 并发不丢 schema/数据。
  ```rust
  assert_eq!(partial_summary_rows, 0);
  assert_eq!(committed_cache_value, expected_cache_value);
  assert!(writer_outcome.is_ok() || is_bounded_busy(&writer_outcome));
  ```
- [ ] **Step 2 — RED**：`cargo test -p store --test write_transaction_contention`；`cargo test -p media --test cache_writer_contention`。不把 SQLite 一定调度到某交错作为依赖；低层探针用双连接屏障确定复现，公开用例验证结果。
- [ ] **Step 3 — 实现**：替换本任务列举的先读后写 unchecked_transaction，记录其他写入口 inventory 并逐个迁移，不全局字符串替换只读事务。write_tx 不套已存在 playback_write 事务。Fanart 缓存连接在现有服务生命周期内共享，schema 初始化仅首次 open 执行；API key 更新可重建轻 client，不按每个 Media 重跑 DDL。busy_timeout 维持5000ms，不靠无限重试/加大超时遮盖长事务。
- [ ] **Step 4 — GREEN**：新 targets；`cargo test -p store`、`cargo test -p media`、`cargo test -p jobs`、`cargo test -p api`。增加“另一个 writer 超过预算仍返回 busy”的断言，不承诺消灭所有 BUSY。
- [ ] **Step 5 — Commit**：分 crate 可各提交一个已绿变更，消息 `fix(storage): reserve writes before taking WAL read snapshots`；提交精确 inventory 中的文件，不包含用户未提交内容。

### Task A4: 唯一 Playback 标记写入口与有效状态（C04/C05）

**Files:** Modify [playback/mutate.rs](<../../../crates/store/src/playback/mutate.rs>)、[assertions.rs](<../../../crates/store/src/playback/assertions.rs>)、[units.rs](<../../../crates/store/src/playback/units.rs>)、[user_summary/mod.rs](<../../../crates/store/src/playback/user_summary/mod.rs>)、[http/playback.rs](<../../../crates/api/src/http/playback.rs>)、[watch_state.rs](<../../../crates/api/src/http/playback/watch_state.rs>)、[media_server_provider/user_marks.rs](<../../../crates/api/src/media_server_provider/user_marks.rs>)；Test [playback_assertions.rs](<../../../crates/store/tests/playback_assertions.rs>)、[playback_scope.rs](<../../../crates/store/tests/playback_scope.rs>)、[management/playback_clock.rs](<../../../crates/api/tests/management/playback_clock.rs>)。

**Interfaces:** `Store::mutate_playback(&self, media:MediaId, change:&PlaybackMutation, now_ms:i64)->Result<(),StoreError>` 是唯一逻辑标记入口。`effective_unit_state(&self,UserId,MediaId,UnitKey)->Result<Option<UnitState>,StoreError>` 在 own 行存在但无 played assertion 时仍返回 own，只有全无事实返回 None。`resolve_playback` 使用有效断言决定 AlreadyPlayed，物理恢复包仍只来自 own 行。

- [ ] **Step 1 — 写失败测试**：整季 true@1_700_000_000_000ms 后单集 false@1_700_000_001_000ms，再整季 true@1_700_000_002_000ms；分别核 episode/season/whole 最新决胜、同刻 specificity、S00E00、不相关季/用户不变。仅 favorite 的行读出 favorite=true；mark 前后 activity/position/duration/source binding 完全相同。取消收藏后 favorite_at=0；整项取消继续清同 Media 下收藏（保留当前 UI 既有语义，不影响 played）。
  ```rust
  assert!(!episode_after_later_false.played);
  assert!(episode_after_latest_season_true.played);
  assert_eq!(activity_after_mark, activity_before_mark);
  assert_eq!(resume_package_after_mark, resume_package_before_mark);
  assert_eq!(unfavorited.favorite_at, 0);
  ```
- [ ] **Step 2 — RED**：`cargo test -p store --test playback_assertions --test playback_scope`；`cargo test -p api --test management playback_clock`。HTTP→Store 的真实时钟用例要失败，不只测 unused mutate。
- [ ] **Step 3 — 实现**：所有 self API/协议 mark 调 mutate 并传 now_ms；played_changed_at_ms 毫秒，favorite_at 在赋 true 时 now_ms/1000、false 时0、None保留；watch_activity_at不动。played 只写目标 scope，不级联覆盖子断言。迁完全部调用者后删除旧 set_unit_marks 级联路径，不留兼容 wrapper。空变更无写入；层级范围按现有覆盖枚举，不能靠已有稀疏摘要决定范围。会话代数/事件序列校验继续由既有绑定事件入口负责，本任务不为标记请求推断device或扩展会话协议。
- [ ] **Step 4 — GREEN**：以上 targets，加 `cargo test -p store --test playback_resume_v1`、store/api 独立套件；同一组 episode 的浏览完成数、详情 played、NextUp 一致，真实观看活动不被 mark 伪造。清历史与取消已看不是同一操作，不顺便清 position。
- [ ] **Step 5 — Commit**：按标记/有效读取职责拆出超限文件，提交 `fix(playback): unify scoped marks and separate activity clocks`。

### Task A5: 省略时长的 Progress/Stop 不丢恢复事实（C06）

**Files:** Modify [playback/event.rs:58–104、233–237、376–435](<../../../crates/store/src/playback/event.rs#L376-L435>)；Create test `crates/store/tests/playback_event_duration.rs`，复用 [playback_event_binding.rs](<../../../crates/store/tests/playback_event_binding.rs>) 的 fixture，按事件用例拆分其超限文件。

**Interfaces:** 保留 `commit_playback_event` 和 `BoundPlaybackEvent`。新增私有 `EventResumeSnapshot { file_id:LedgerId, revision:i64, source_version:String, timeline:TimelineMode, duration_ms:Option<i64> }`，从 playback_units 同一行一次读取；新增内部 `effective_event_duration(event:&BoundPlaybackEvent, previous:Option<&EventResumeSnapshot>)->Option<i64>`。只有 previous 的 file/revision/source_version/timeline 与事件相同才允许用其 duration；目标源的权威时长若存在按现有同源读取策略使用，不从另一源推断。`UnitState` 不含源绑定，不能用它代替此快照。

- [ ] **Step 1 — 写失败测试** `stop_without_duration_keeps_same_source_completion`：先同源时长100_000、进度50_000，再 Stop position95_000/duration=None，断言 duration仍100_000、played=true、play_count仅+1；重复 Stop 不再+1。`new_source_without_duration_does_not_inherit_old_duration`：旧源100_000、换源未知时长，事件后新恢复包 duration=None，不自动完成、不复用旧轨道。
  ```rust
  assert_eq!(same_source_after_stop.duration_ms, Some(100_000));
  assert!(same_source_after_stop.played);
  assert_eq!(play_count_after_duplicate_stop, play_count_after_first_stop);
  assert_eq!(new_source_package.duration_ms, None);
  ```
- [ ] **Step 2 — RED**：`cargo test -p store --test playback_event_duration`；现实现同源停播会抹时长。记录真实失败断言，不凭注释判红。
- [ ] **Step 3 — 实现**：在同一写事务写事实前读取一次 previous，先做源兼容/绑定校验，求 effective duration，将同一值传给完播阈值与计数；SQL 写入完整新恢复包。拒绝事件不触碰 facts/session/summary；非正时长不能导致0阈值自动完成。
- [ ] **Step 4 — GREEN**：新测试、`cargo test -p store --test playback_event_binding`、`cargo test -p api --test playback_event_identity`，两个 crate 独立套件。多集 full_file 只完成所选分集，作为非回归断言保留。
- [ ] **Step 5 — Commit**：`git commit -m "fix(playback): retain duration only within the bound physical source"`。

### Task A6: canonical roots、关联输入与别名隔离（C07–C09）

**Files:** Modify [roots.rs](<../../../crates/store/src/roots.rs>)、[library_operations/roots.rs](<../../../crates/store/src/library_operations/roots.rs>)、[path_key.rs](<../../../crates/store/src/path_key.rs>)、[libraries.rs](<../../../crates/store/src/libraries.rs>)；Create `crates/store/src/library_operations/root_keys.rs`；Test [path_key_normalization.rs](<../../../crates/store/tests/path_key_normalization.rs>)、[library_reconfiguration_roots.rs](<../../../crates/store/tests/library_reconfiguration_roots.rs>)、[nfo_scoped_inputs.rs](<../../../crates/store/tests/nfo_scoped_inputs.rs>)、[file_rename_playback_summary.rs](<../../../crates/store/tests/file_rename_playback_summary.rs>)。

**Interfaces:** 新内部 `PreparedRoot { display_path:PathBuf, path_key:String }`，`prepare_roots(paths:&[PathBuf])->Result<Vec<PreparedRoot>,StoreError>` 在锁外 canonicalize。操作持久保存 canonical key 与 display，恢复不重新用变化后的 symlink 解释已受理请求。保持 `reassign_files` 当前双方 reduce；保留 `refresh_affected_user_summaries` 已有 raw users UNION derived users，禁止重复新增刷新循环。

- [ ] **Step 1 — 写失败测试**：父库文件带 work/season/episode NFO、favorite/progress，创建 symlink 嵌套子库后检查 file/root/coverage/input均到正确库，旧 card/summary消失、目的库从原事实生成摘要、原 playback_units不变。共享 tvshow.nfo 下仍留父库文件时，父库有效输入不能被全量搬走。另测 root重配同 alias、兄弟前缀root-x、两内存Store相同 display alias 指向不同 real root、根事务回滚后另一连接结果不变。
  ```rust
  assert_eq!(moved_file.library_id, nested_library_id);
  assert_eq!(moved_nfo.locale, original_import_locale);
  assert!(shared_parent_nfo_after.is_some());
  assert_eq!(raw_playback_after, raw_playback_before);
  assert_eq!(other_store_key_after_rollback, other_store_key_before);
  ```
- [ ] **Step 2 — RED**：`cargo test -p store --test path_key_normalization --test library_reconfiguration_roots --test nfo_scoped_inputs`；旧实现分别会丢输入/拒合法 symlink/共享别名串扰；新测试按这三项断言拆开。
- [ ] **Step 3 — 实现**：所有 key 比较消费 PreparedRoot，不在事务内 canonicalize；同一事务先按路径/实体迁移或为共享作用域重建 NFO 归属，保留 provenance/export digest/原导入 locale，再改 file/coverage，最后 reduce 双方。Unidentified 单独按受影响 root 范围迁移，不能依 file join 漏掉未登记文件。移除全局按 conn.path keyed 的可变 TABLES，规范化函数使用连接局部快照；写事务显式传 prepared canonical keys，别名缓存仅提交后发布/失效，同库其他连接在下一操作读取已提交 root revision，不共享未提交映射。
- [ ] **Step 4 — GREEN**：上述 targets、`cargo test -p store --test file_rename_playback_summary`、`cargo test -p api --test library_path_ownership`、store/api 独立套件。新增会影响既有文件的根变更使用现有持久 operation，每批≤100，freeze/config CAS覆盖相关库，重启续同一cursor，不能把影响范围collect全量再单事务处理。
- [ ] **Step 5 — Commit**：按 roots key 与关联事实两个独立已绿提交；消息 `fix(storage): preserve scoped inputs across canonical root operations`。更新现有数据库/根变更文档，不创建新的历史兼容层。

### Task A7: rename 磁盘证据与数据库推进解耦（C10）

**Files:** Modify [rename_durable.rs](<../../../crates/store/src/library_write/rename_durable.rs>)、[rename_disk.rs](<../../../crates/store/src/library_write/rename_disk.rs>)、[library_organize_apply.rs](<../../../crates/api/src/http/library_organize_apply.rs>)、[library_housekeeping.rs](<../../../crates/api/src/http/library_housekeeping.rs>)、[library.sql](<../../../crates/store/src/schema/v1/library.sql>)、[objects.rs](<../../../crates/store/src/schema/v1/objects.rs>)；Create `crates/store/src/library_write/rename_receipt.rs`；Test [file_rename_durable.rs](<../../../crates/store/tests/file_rename_durable.rs>)、[source_token_rename.rs](<../../../crates/api/tests/source_token_rename.rs>)。

**Interfaces:** 新 `RenameDiskReceipt { rename_id:String, destination:RenameSourceEvidence }`；锁外 `execute_rename_disk(&FileRenameRequest)->Result<RenameDiskReceipt,StoreError>`；锁内 `Store::record_rename_disk(&self, receipt:&RenameDiskReceipt, now_ms:i64)->Result<(),StoreError>`；`complete_file_rename(&self,&str,i64)->Result<(),StoreError>` 保留。新 receipt 保存实际目标 native identity，而非要求源/目标 mtime 精度一致。删除执行 IO 的 Store::apply_file_rename_disk 入口，全部调用者改走 driver，不保留旧兼容入口。

- [ ] **Step 1 — 写失败测试**：同FS、模拟 CrossesDevices+目标mtime截断、set_modified失败、from消失且外来to有相同size/mtime、copy后/DB记录前崩溃重启、目标被并发替换。断言旧/外来字节不误删，成功receipt重复应用不增加版本，source_changed仍递增版本；人为阻塞磁盘动作时 Store 另一查询2秒内可完成。真实跨FS无法提供时标 Not run，不能用同FS路径冒充。
  ```rust
  assert_eq!(foreign_destination_bytes_after, foreign_destination_bytes_before);
  assert_eq!(replayed_receipt_revision, first_receipt_revision);
  assert!(independent_store_read_finished);
  ```
- [ ] **Step 2 — RED**：`cargo test -p store --test file_rename_durable`；`cargo test -p api --test source_token_rename`；故障通过已有注入模式或新私有 DiskOps fixture，不能依赖 CI 必有第二挂载盘。
- [ ] **Step 3 — 实现**：登记意图后锁外 copy 到 rename_id 私有 staging，fsync 文件，观察并持久保存目标 staging native identity，然后 no-clobber 发布目标、同步目录；只有证据已可恢复才 unlink 源。崩溃恢复比较持久身份/receipt，不信 size+mtime 或近似容差，不删除来源不明目标。IO之前/之后短事务校验 config/file版本，receipt阶段幂等；去掉 API+Store 双重move。新增中间phase属于此未上线目标布局，不写旧版本 plan JSON 兼容。
- [ ] **Step 4 — GREEN**：两 targets、store/api 独立套件；断点覆盖“copy未登记/receipt已登记/目标已发布/源已删/DB路径已发布/跨库引用未完成”，恢复可收敛；foreign source/destination拒绝且保持意图供诊断。
- [ ] **Step 5 — Commit**：精确 stage 本任务文件/DDL/测试/字典，`git commit -m "fix(storage): persist rename disk receipts outside store critical sections"`。

### Task A8: sidecar 持久决议、目标竞争与锁外观察（C11）

**Files:** Modify [rename_sidecars.rs](<../../../crates/store/src/library_write/rename_sidecars.rs>)、[rename_durable.rs:222–240、340–391](<../../../crates/store/src/library_write/rename_durable.rs#L222-L240>)、A7 driver；Test [file_rename_shared_artwork.rs](<../../../crates/store/tests/file_rename_shared_artwork.rs>)、[file_rename_durable.rs](<../../../crates/store/tests/file_rename_durable.rs>)。

**Interfaces:** 新内部 `SidecarDecision { from:PathBuf,to:PathBuf,action:SidecarAction }`；`SidecarAction = Move | Copy | KeepSource | AlreadyIdentical`。字节比较在锁外观察，文件身份/CAS与源共享关系在短事务重校验；决议按 rename intent 持久化。publish/references 只从 DB 重新加载该决议，不用 begin 返回的 Vec。

- [ ] **Step 1 — 写失败测试**：目标字节不同→目标胜出且源保留，DB artwork/input引用与实际保留结果一致；begin后新 sibling使 Move变Copy→源不入删除队列；目标相同→可清 sole-owned source；观察后目标竞争出现→不覆盖；两文件均读失败→Unknown，不能 `None==None` 当相同。每项验证字节、DB引用和清理队列，不只返回 Ok。
  ```rust
  assert_eq!(destination_bytes, original_competitor_bytes);
  assert_eq!(retained_source_bytes, original_source_bytes);
  assert!(!pending_deletion_paths.contains(&retained_source_path));
  ```
- [ ] **Step 2 — RED**：`cargo test -p store --test file_rename_shared_artwork --test file_rename_durable`，新增“recheck后目标竞争出现但仍为Move”的用例应失败；begin后新sibling的旧用例是非回归，不能冒称它当前失败。
- [ ] **Step 3 — 实现**：用流式/有界字节比较替代事务中整文件 read；持久 KeepSource/Copy 决议控制后续删除；目标发布 no-clobber，不能 fs::rename 覆盖竞争者。unknown IO保最后有效输入并返回可诊断失败。load_file_rename 不因 JOIN 文件已删而无限重试；文件消失时明确终止/保留审计意图。pending选择遵守 next_retry_at，采用 now_ms/seconds既有列单位并测试。
- [ ] **Step 4 — GREEN**：上述 targets、`cargo test -p store`、`cargo test -p api`；恢复仅做同一持久决议，源共享关系变化时重新预检，不使用 stale decision。
- [ ] **Step 5 — Commit**：`git commit -m "fix(storage): publish sidecars from durable execution decisions"`。

### Task A9: 同语言解析规则与身份字段投影闭环（C12/C13）

**Files:** Modify [reduce.rs:361–469](<../../../crates/store/src/library_write/reduce.rs#L361-L469>)、[details/metadata.rs](<../../../crates/store/src/library_query/details/metadata.rs>)、[details/mod.rs](<../../../crates/store/src/library_query/details/mod.rs>)、[media/mod.rs](<../../../crates/store/src/media/mod.rs>)；Create `crates/store/src/metadata/locale.rs`；Test [library_reconfiguration.rs](<../../../crates/store/tests/library_reconfiguration.rs>)、[metadata_adoption.rs](<../../../crates/store/tests/metadata_adoption.rs>)、[media_alias_updates.rs](<../../../crates/store/tests/media_alias_updates.rs>)。

**Interfaces:** 新纯函数 `select_locale(available:&[String], chain:&[String], fallback:LocaleFallback)->Option<String>`，`LocaleFallback = AdoptedFirst | UnboundInputOnly`。每个 chain candidate先exact再语言prefix，平局稳定按locale排序；NFO只允许链内与空locale桶，adopted才可最后取已采用首locale。卡片用已持久的非空 library display_locale；详情用 requested→library→instance。显式 requested 不同时允许不同展示，不许宣称所有详情必须等于默认卡片。

- [ ] **Step 1 — 写失败测试**：库zh，已采用en-US/zh-CN→卡片和默认详情均中文；ja-JP NFO导入后库改zh-CN→旧NFO不串语言，原输入locale不变；显式en-US请求仍英文。update_media仅改基础title/year/alias后未采用元数据卡片/terms同事务变化；有人工锁/NFO时不越权覆盖。
  ```rust
  assert_eq!(default_card_title, default_detail_title);
  assert_eq!(explicit_english_detail_title, "English");
  assert_eq!(imported_nfo_locale_after_switch, "ja-JP");
  assert_eq!(updated_base_card_title, expected_base_title);
  ```
- [ ] **Step 2 — RED**：`cargo test -p store --test library_reconfiguration --test metadata_adoption --test media_alias_updates`；旧card可任取foreign NFO或en-US，基础更新卡片可能停旧值。
- [ ] **Step 3 — 实现**：两个消费层共用纯选择器，不在 library写事务中读取 app设置/外部 IO；需要实例默认时在外层读取并固定操作上下文，不能借另一库形成假事务。write_media调用者在同一写事务内完成别名写入后 reduce_media_everywhere；更新CAS/身份保护保留，不推进无真实语义变化的用户activity。
- [ ] **Step 4 — GREEN**：上述 targets及 `cargo test -p api --test metadata_priority`、store/api 独立套件；locale operation中途重启、清cache后结果不变；库默认非空条件由schema/配置验证保持。
- [ ] **Step 5 — Commit**：两个独立已绿变更可分开提交；消息 `fix(metadata): align scoped locale and identity projections`。

### Task A10: 测试失败也可退出（C18）

**Files:** Modify [poster_cache_capacity.rs](<../../../crates/api/tests/poster_cache_capacity.rs>)、[up_next_local_first.rs](<../../../crates/api/tests/up_next_local_first.rs>)、[download_cleanup_pin.rs](<../../../crates/api/tests/management/download_cleanup_pin.rs>)、[delivery/fixture.rs](<../../../crates/api/tests/management/delivery/fixture.rs>)、[mock_qbit.rs](<../../../crates/api/tests/subscription_e2e/mock_qbit.rs>)、[mock_transmission.rs](<../../../crates/api/tests/subscription_e2e/mock_transmission.rs>)、[downloader owned_removal.rs](<../../../crates/downloader/tests/owned_removal.rs>)、[qbit.rs](<../../../crates/downloader/tests/qbit.rs>)、[transmission.rs](<../../../crates/downloader/tests/transmission.rs>)。

**Interfaces:** 测试私有 `ReleaseOnDrop` 拥有 Arc<Gate>，Drop 释放并 notify_all；TCP fixture拥有 stop标志、listener/thread join 生命周期，单连接 read_timeout=5s，accept loop可由Drop唤醒退出。不建生产API。

- [ ] **Step 1 — 写失败测试**：子进程故意在 gate.release 前 panic，断言 watchdog观察子进程5秒内结束；假server先建空闲连接、第二连接发送完整请求，后者正常得到响应且Drop后线程退出。
  ```rust
  assert!(panicking_child_exited_before_deadline);
  assert_eq!(second_connection_response.status(), 200);
  assert!(server_thread_joined);
  ```
- [ ] **Step 2 — RED**：定向运行两个新 fixture 生命周期测试，当前版本因 gate/server未退出失败；故意panic在子进程观测，不污染测试运行器。
- [ ] **Step 3 — 实现**：复用已有 GateRelease 设计，所有成功/错误/取消路径RAII释放；按连接并发或非阻塞server，读超时后处理预期fixture错误；accept和join均有终止机制，不以detach线程假装通过。
- [ ] **Step 4 — GREEN**：`cargo test -p api --test poster_cache_capacity --test up_next_local_first --test management --test subscription_e2e`；`cargo test -p downloader`；`cargo test -p api`。测试计时仅用宽截止判断存活，不对正常微秒延迟作易抖动断言。
- [ ] **Step 5 — Commit**：`git commit -m "test: release gates and stop fake servers on every exit path"`。

### Task A11: Probe/marker/管理观察全部离开 Store 锁（C19）

**Files:** Modify [source_guard.rs](<../../../crates/store/src/library/markers/source_guard.rs>)、[markers.rs](<../../../crates/store/src/library/markers.rs>)、[probe_manager/season.rs](<../../../crates/api/src/probe_manager/season.rs>)、[probe_manager/probe.rs](<../../../crates/api/src/probe_manager/probe.rs>)、[adaptive/capture.rs](<../../../crates/api/src/fingerprint_job/adaptive/capture.rs>)、[store/library.rs](<../../../crates/store/src/library.rs>)、[library_admin.rs](<../../../crates/api/src/http/library_admin.rs>)、[subscription_depth.rs](<../../../crates/api/src/http/subscription_depth.rs>)；Create test `crates/api/tests/source_observation_liveness.rs`；Test [probe_publication.rs](<../../../crates/store/tests/probe_publication.rs>)。

**Interfaces:** 新 owned `SourceObservation { file_id:LedgerId, expected:FileVersion, presence:FilePresence }`，结果由现有锁外源观察接口生成；checked marker publication显式消费 `members:&[SourceObservation]` 与现有 replacements，在事务内仅核对file/版本/成员/Job与生命周期，不执行try_exists。所有调用点与测试同任务迁移，不保留会自行IO的旧Store入口。

- [ ] **Step 1 — 写失败测试** `slow_source_observation_does_not_block_store`：注入慢metadata/STRM观察gate，观察卡住期间另一个API/Store查询2秒内完成；观察后file version变化则发布拒绝且旧markers未改；Unknown保最后有效facts，Missing按既有明确缺失政策处理。
  ```rust
  assert!(independent_read_finished_before_deadline);
  assert!(stale_publication.is_err());
  assert_eq!(markers_after_rejection, markers_before);
  assert_eq!(unknown_observation.presence, FilePresence::Unknown);
  ```
- [ ] **Step 2 — RED**：`cargo test -p api --test source_observation_liveness -- --nocapture`；慢gate用A10的RAII释放，watchdog避免旧实现挂整个套件。
- [ ] **Step 3 — 实现**：锁内只提取owned文件/版本快照；锁外预算池做metadata、STRM/read_dir；重新锁定按expected版本、成员与lease校验再发布。管理页大小优先用持久size_bytes，必须现场观察的管理命令锁外执行。文件系统与DB无原子事务，不声称能排除不受控外部文件在最后观察后变化；保留源版本/lifecycle屏障与后续正常reconcile。不能简单删文件存在性校验而丧失陈旧结果保护。
- [ ] **Step 4 — GREEN**：新target、`cargo test -p store --test probe_publication`、`cargo test -p api --test probe_retry_deleted_file`、store/api独立套件；慢IO时健康查询可服务且错误仍有structured日志。
- [ ] **Step 5 — Commit**：`git commit -m "fix(storage): observe sources outside store and transaction guards"`。

### Task A12: NFO 导出失败不得降级为无保护写入（C21）

**Files:** Modify [scrape_metadata/export.rs](<../../../crates/api/src/scrape_metadata/export.rs>)、[library_scan.rs:410–430](<../../../crates/api/src/http/library_scan.rs#L410-L430>)、[library_organize.rs](<../../../crates/api/src/http/library_organize.rs>)；Test [management/library_sidecar_cancellation.rs](<../../../crates/api/tests/management/library_sidecar_cancellation.rs>)、[management/rename_grace_sidecars.rs](<../../../crates/api/tests/management/rename_grace_sidecars.rs>)；Create `crates/api/tests/nfo_export_failure_isolation.rs`。

**Interfaces:** `begin_sidecar_export(&ApiState,&Media,&Path,EntityScope)->Result<ExportPreparation,store::StoreError>`，新内部 `ExportPreparation = Ledgered(NfoExportRequest) | NotLedgered`。未知IO/意图失败/local-edit捕获失败返回Err，不混同NotLedgered；合法显式未登记路径Scrape是另一用例，不称为旧格式兼容。

- [ ] **Step 1 — 写失败测试**：已登记video，外部修改NFO后注入读取权限错误/建立意图失败/local edits读取失败；调用实际scan或organize导出，断言NFO字节与本地有效输入不变、有结构化错误、不能走原地write_nfo。另测明确NotLedgered的合法路径Scrape按原授权政策工作，不误把正常用例删掉。
  ```rust
  assert_eq!(nfo_bytes_after_failed_prepare, nfo_bytes_before);
  assert_eq!(effective_input_after_failed_prepare, effective_input_before);
  assert!(export_result.is_err());
  ```
- [ ] **Step 2 — RED**：`cargo test -p api --test nfo_export_failure_isolation -- --nocapture`；旧None分支可能改字节，不能仅测begin返回None。
- [ ] **Step 3 — 实现**：三个结果分别匹配，Ledgered只走现有CAS/digest持久driver；Err记录完整错误并停止本文件导出；NotLedgered仅显式允许用例进入锁外授权写，不被登记文件失败命中。消除“丢编辑比sidecar陈旧更可取”的错误降级，不用warning后继续覆盖。
- [ ] **Step 4 — GREEN**：新target、`cargo test -p api --test management library_sidecar`、`cargo test -p api`；正常导出/删除本地覆盖/重启恢复测试继续通过。
- [ ] **Step 5 — Commit**：`git commit -m "fix(scrape): preserve sidecars when guarded export preparation fails"`。

## 自查与交付门禁

- 覆盖：C01–C13/C18/C19/C21 各有唯一 owning task；A6不重修已存在的 raw/derived user UNION 与FK级联；A7/A8分担视频证据与sidecar决议，不交叉发明两套phase。
- 类型：所有新类型是目标设计，基线未存在；复用现有 ID/FileVersion/UnitState，跨文件调用在同任务更新导出与构造者。A7 receipt 是A8唯一输入来源，不同时保留旧Store磁盘入口。
- 五项 Review Focus 已分别落实 A1/A2/A5/A7–A9，未使用测试通过来证明实际跨FS已测。
- 最终实现验收：所有实际触及crate单测后，一次稳定树 `cargo test --workspace`；更新原有数据库/接口文档，记录失败注入和Not run项目。未获实施授权前，所有任务维持未勾选。
