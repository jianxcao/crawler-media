# Storage Redesign main Review and Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复本次相对 main 的数据库重构中仍存在的状态读取、并发写入、操作互斥、兼容残留和查询放大问题。

**Architecture:** 保留五文件职责划分，以及 Media、Library 文件事实、采用元数据、Playback 与摘要在 library.db 内同事务更新的设计。读接口统一消费新模型的有效状态；NFO 导出增加明确的路径所有权；操作准入和恢复共享终态定义；异步入口复用现有 blocking/Store 等待边界。

**Tech Stack:** Rust workspace、rusqlite bundled SQLite 3.50.2、parking_lot、Tokio、Axum、React/TypeScript。

**Spec:** 用户本轮四项 review 要求；`CONTEXT.md`；`docs/adr/0008-sqlite-system-of-record.md`；`docs/superpowers/specs/2026-10-18-library-storage-redesign.md`。

## Global Constraints

- 本轮只 review 和提出技术方案；本文件中的实施步骤尚未执行，不修改运行时代码或现有测试。
- 审查基线：`main = 756444f7`；审查 HEAD：`eb79bacd`；分支：`feat/storage-redesign`。审查开始时工作区干净。
- 比较范围：`git diff main...HEAD`，1511 files changed，180736 insertions，65126 deletions；重点是 storage 的行为边界，并非逐行审核全部变更。
- “因为我们无需兼容旧版，所以要保证我们的代码 后端去掉所有旧的逻辑，保证前端代码api全部改用新的，一定不要做任何兼容”。
- “我们只review，并提出优化修复的技术方案，不去修改代码哈”。
- 后续实施遵守 AGENTS.md：公开行为测试先 red，再 green；每个触及 crate 单独运行测试，最后运行 workspace；不使用真实网络、Downloader 或 Chromium。
- 不导入旧数据库、不增加兼容视图、不双读双写；布局变更提升版本并拒绝旧布局，运维切换走现有停写重建流程。
- Jellyfin 协议支持、文件多版本共存、Metadata source 的错误回退是产品行为，不能按“删除旧版兼容”误删。
- 图片静态路由保持 public；Jellyfin 视频保留协议认证。
- `.rs` 文件不超过 800 行；生产函数和测试函数不超过 120 行；按职责拆分，不能只压缩行数。

---

## 审查结论

架构方向合理，无需重新拆分数据库。Media 身份、文件、覆盖、采用元数据、Playback 和派生摘要放入同一 library.db，解决了原来跨库更新无法原子提交的问题。显式 Library/root 归属、文件版本 CAS、延迟复合 FK、可恢复 Media merge 和 Library removal 都值得保留。cache.db 与权威事实分离也合理。

当前还不能认定“新模型切换完整、无兼容残留、无阻塞风险”。下面八项中，F1/F3/F4/F6 的旧 PATCH 接受/F7 有当前 HEAD 的独立运行证据；F8 有项目实际 bundled SQLite 的测量；F2/F5 来自可定位的控制流与锁分析。未发现可以确认的生产 SQLite 永久锁环，不能据此宣称全部生产路径绝无死锁。

| 编号 | 优先级 | 当前问题 | 主要后果 | 对应任务 |
|---|---|---|---|---|
| F1 | P2 | self/Web API 仍读取原始 episode 状态 | 整季标记后列表仍显示未看，resume 自相矛盾，继续观看推荐已看分集 | 1 |
| F2 | P1 | 新 Transfer 设置等 async 入口直接等待同步 Store 锁 | 争用时阻塞 Tokio worker；并发可耗尽 worker | 2 |
| F3 | P2 | failed Scrape 终态历史被当成活跃操作 | 阻止改根、改语言、Integrity、移除 Library | 3 |
| F4 | P2 | 新 NFO 意图删除旧意图，却没撤销旧 worker 写权限 | 磁盘保留旧内容，新任务持续 pending，无法自行恢复 | 4 |
| F5 | P2 | 测试 Barrier 在异常路径没有配对释放 | 一次测试错误转为永久挂住 | 5 |
| F6 | P2 | 旧字段、双形状适配、旧 phase 别名仍被接受 | 不满足本次明确的无兼容约束，掩盖客户端漏改 | 6 |
| F7 | P2 | NFO 正常导出以秒写入毫秒列 | 新历史在下轮保留清理时提前删除 | 7 |
| F8 | P2 | 每次 NFO 建立意图扫描全部 operation 历史 | 随历史增长增加持锁时间和 Scrape 成本 | 8 |

### F1：有效观看状态没有贯穿全部 Web API

位置：`crates/api/src/http/playback.rs:65–76`、`http/playback/episode_listing.rs:58`、`http/playback/watch_state.rs:114–120,244–257`、`http/library.rs:597–610`、`http/playback_views/up_next.rs:197`。

新 `Store::mutate_playback` 只写目标 scope 一行；整季 `(season,-1)`、整部 `(-1,-1)` 都不会预铺 episode 行。`Store::effective_unit_state` 才会叠加父级断言，但 API 的 `effective_watch_state` 名称虽有 effective，实际仍调用 `unit_state`。另外，up-next 把 scope 状态行用于分集选择，未看数也只数已存在的用户行。

真实 HTTP 复现：拥有 S01E01/E02、无分集进度 → POST 整季 played=true → 两个 episodes 接口仍返回 played=false；GET resume 同时返回 played=false 和 resume_status=already_played；up-next 仍推荐 S01E02。Store 的 effective_unit_state 已为 true，证明写入正确而消费者漏切。初始两个已拥有未看分集的 unplayed_count 为 null。

修复统一读取有效状态，保持“较新 assertion 胜出；同时间按更具体 scope 胜出”的现行规则。不能通过重新级联写入每集来掩盖旧读逻辑。批量读时共享该 Media 的断言快照，避免每集额外查询三层断言。

### F2：异步线程内仍等待同步锁

位置：`crates/api/src/http/transfer_settings.rs:42,59–64`。同类位置包括 `library_lifecycle/removal.rs:6–21,50–56,75,119–123` 和 `scan_operations.rs:294,300,325,351`。

GET/PATCH 直接在 async handler 中调用 parking_lot Mutex；PATCH 还同步执行写事务。外层 Store 锁争用不受 SQLite busy_timeout 控制。已有 `http/store_wait.rs` 正是为此提供 blocking pool + try_lock_for。认证阶段已经有隔离，并不能保证认证完成后 Store 不再被别的工作拿走。

这是 executor 阻塞，尚无生产永久锁环证据。后台 drain/Scan 的同类入口也应处理，不能只修一个 HTTP 文件就宣称风险清零。

### F3：终态定义在准入与恢复之间不一致

位置：`crates/store/src/library_operations/integrity_guard.rs:74–82`；对照 `library_operations/mod.rs:41–58`。

ensure_no_operation 用 state!='succeeded' 拦截全部 Library operation，但 owner 模块规定 failed scrape 是终态，恢复也不会继续它。一次 fail_nfo_export 后，历史记录会阻止后续配置和删除；新 Scrape 成功不清理旧 failed 行。正常毫秒时间戳下，可能要等默认 30 天 Operations retention。

公开 API 复现：failed scrape 不在 unfinished_library_operations 中，begin_library_reconfiguration 却报 already has an operation；清理该终态历史后同一请求立即成功。这是状态机逻辑阻塞，并非数据库死锁。failed roots/locale/integrity/default_locale、merge、removal 等仍可能可恢复，不能一概忽略 failed。

### F4：NFO 意图被替换，但锁外写盘仍可继续

位置：`crates/store/src/metadata/export.rs:149–155`、`metadata/export_driver.rs:388–394,450–493`。

begin_nfo_export 删除同 Media/path 的未完成旧操作；write_planned_nfo 仅检查文件 digest。Library work lease 允许多个工作并行，不是路径互斥锁。

确定性交错：A/B 都观察到 sidecar 不存在 → plan A → 修改有效元数据 → plan B 删除 A 意图 → A 仍写盘成功 → A finish 被 superseded 拒绝 → B write 因文件变化拒绝 → 无导出基线时 B 的连续 recovery 都拒绝覆盖。最终磁盘保持 A 的旧内容，B 保持 pending。

修复必须覆盖“观察→plan→写盘→登记”的路径所有权。禁止删除还可能写盘的活跃意图；不能只在最后登记前再加一次检查，那已经晚于磁盘副作用。

### F5：测试异常会使 Barrier 永久等待

位置：`crates/store/tests/fingerprint_source_safety.rs:617–653`。

capture 线程在第二次 Barrier::wait 等待。删除线程在第 648 行因非 Busy 错误或重试耗尽 panic 后，不会执行第 651 行的对应 wait；thread::scope 在 unwind 时 join，永远等不到子线程。注释声称错误路径会释放 Barrier，实际没有实现。

改为带截止时间和取消语义的 channel/gate。先解除等待、汇合工作线程，再断言操作成功；不能把“最多重试 1000 次、每次可能等 5 秒”当成合理的测试时间上界。

### F6：严格切换仍有可定位缺口

| 位置 | 当前形态 | 方案 |
|---|---|---|
| `crates/api/src/http/library_config.rs:208–227` | PATCH 接受 Json<Value>；source/auto_clear_missing/scrape_overrides 实测返回 200 | 强类型当前 PATCH DTO，拒绝未知字段；在写入前完成验证 |
| `web/lib/subscription-pipeline.ts:1–11`、`web/lib/api/subscriptions.ts:418–425` | library_name/name 双字段及优先级 | 以当前后端 readiness 输出的 name 为唯一字段；删除旧字段分支及对应兼容测试 |
| `crates/store/src/library_operations/mod.rs:116–119,538`、`crates/api/src/storage_operations.rs:161–167`、`web/lib/api/libraries.ts:388–390,764` | Library operation 的 phase 与 state 重复，前端 state 缺失时回退 phase | Library operation 只发布/消费 state；真实 Media merge phase 有独立含义，保留 |
| `crates/store/src/media_operations/apply.rs:118–124` | phase_cursor 空值回退 init；planned/frozen/draining/applying 都按 init 处理 | 当前 writer 明确写 init；只接受 init/subscribes/retire/complete，未知或缺失阶段拒绝 |

旧 PATCH 字段被接受是合同宽松，未观察到旧 source 等逻辑仍执行；不能把这两件事混为一谈。现行 schema 确实拒绝旧数据库，已删除的 directory 聚合/旧浏览投影也没有据此被判定为仍在生产运行。这里列出的是当前确实存在的遗漏，不把兼容关键词搜索当成“全部清理完毕”的证明。

### F7：时间单位导致保留窗口失效

位置：`crates/api/src/scrape_metadata/export.rs:106`；`job_loop::unix_now` 最终使用 `store::unix_now` 的 as_secs。导出接口参数及 library_operations 列却都是毫秒；保留 cutoff 也按毫秒计算。startup recovery 使用 playback_clock.now_ms，本问题在正常导出入口。

公开 Store 驱动复现：按该入口传秒完成导出后，立即执行 30 天 cutoff 的 prune_terminal_operations，deleted=1，registration 从 Some 变 None。影响是近期历史提前被删；library_metadata_inputs 上的 export tag 和 export_fields_json 仍存在，不能推断本地编辑基线因此丢失。

### F8：未完成导出查询随终态历史线性增长

位置：`crates/store/src/metadata/export.rs:308–329`，由每次 begin_nfo_export 调用。kind/state 过滤无匹配索引，当前可用索引无法直接选出这一小集合。

使用当前生产 library.sql、当前 rusqlite rlib 和 bundled SQLite 3.50.2；在内存库种入 succeeded scrape 历史，未完成数保持 0。测量真实 StatementStatus：

| 终态历史行数 | 当前 VM / Fullscan / Sort | 增加候选 pending 部分索引后 |
|---|---|---|
| 1,000 | 7,012 / 999 / 1 | 9 / 0 / 0 |
| 20,000 | 140,012 / 19,999 / 1 | 9 / 0 / 0 |

当前 EQP 为 SCAN library_operations + USE TEMP B-TREE FOR ORDER BY。候选索引见 Task 8。这是查询工作量实测，不是整机 p95、NAS 或并发 Scrape 吞吐承诺。查询只解码未完成行，但仍扫描终态历史；不能误写成“每次解码所有历史 JSON”。

## 文件与职责

| 任务 | 修改/新增位置 | 职责 |
|---|---|---|
| 1 | `crates/api/src/http/playback.rs`、`playback/episode_listing.rs`、`playback/watch_state.rs`、`playback_views/up_next.rs`；`crates/api/tests/storage_review_playback.rs` | 新状态的 HTTP 消费与公开行为回归 |
| 2 | `crates/api/src/http/transfer_settings.rs`、`transfer_settings/tests.rs`；`library_lifecycle/removal.rs`、`scan_operations.rs` | 异步入口的 blocking 与有界等锁 |
| 3 | `crates/store/src/library_operations/{mod,integrity_guard}.rs`；`crates/store/tests/terminal_operation_admission.rs` | 唯一终态口径与操作准入 |
| 4 | `crates/store/src/metadata/{export,export_driver}.rs`；`schema/v1/library.sql`、`schema/layout.rs`、`schema/v1/objects.rs`；`crates/store/tests/nfo_export_ownership.rs` | 持久路径所有权及恢复 |
| 5 | `crates/store/tests/fingerprint_source_safety.rs` | 测试的截止、取消和线程汇合 |
| 6 | `crates/api/src/http/library_config/patch_input.rs`、`library_config.rs`、`storage_operations.rs`；上述 store/web 合同位置及对应 tests | 唯一当前形状与严格拒绝 |
| 7 | `crates/api/src/scrape_metadata/export.rs`、其测试子模块 | 真实导出入口的毫秒时钟 |
| 8 | `crates/store/src/schema/v1/library.sql`、layout/object 校验；`crates/store/tests/nfo_export_work.rs` | 待恢复查询的索引及工作量验证 |

后续任何 schema 变更都必须同时更新 layout 版本、对象校验、数据库字典和 schema 测试。任务分开提交时分别提升版本；交付只支持最终布局，不为中间版本添加迁移。本文不预先占用版本号。

## Task 1：所有 self/Web 观看读取使用有效断言

**Interfaces:** 消费现有 `Store::effective_unit_state(UserId, MediaId, UnitKey) -> Result<Option<UnitState>, StoreError>`；保持现有 HTTP 响应字段和新 assertion 写入模型。若需要批量化，将三个作用域在同一 Media 的 owned snapshot 中加载，不发明第二套断言决胜规则。

- [ ] **Step 1：加入附录 A 的公开 HTTP 回归。** 放到 `crates/api/tests/storage_review_playback.rs`，fixture 与断言按职责拆成小函数。它同时验证两个 episodes API、marks 和 resume。
- [ ] **Step 2：运行 red。** `cargo test -p api --test storage_review_playback -- --nocapture`。当前应出现 `played=false` 而预期 true；复现已观察到该失败。
- [ ] **Step 3：替换原始状态读取。** `effective_watch_state` 的返回表达式改为：

```rust
store.effective_unit_state(
    user_id,
    media_id,
    domain::UnitKey::new(season, episode),
)
```

episode_listing 使用同一个有效状态入口；Store 错误返回错误响应并记录 error，不用空状态吞掉。up-next 以可见 Library 拥有的逻辑 episode 为候选，使用有效 played 排除已看项；`(-1,-1)` 和 `(s,-1)` 仅作为断言，绝不当成下一集坐标。unplayed_count 同样基于可见覆盖去重后的 episode，不能从稀疏用户行数推算。

- [ ] **Step 4：扩充并运行 green。** 在同一公开测试中增加整部、整季、较新单集取消、较新整季覆盖旧单集、相同时间按 specificity、第二个用户、重开 Store。关键断言为：

```rust
assert_eq!(episode_marks["played"], true);
assert_eq!(resume["played"], true);
assert_eq!(resume["resume_status"], "already_played");
assert!(up_next["items"].as_array().unwrap().is_empty());
assert_eq!(initial_season_marks["unplayed_count"], 2);
```

实现若将 0 未看表示为 null，继续遵守当前 DTO 的零值表达；不能把“尚无用户行”的两个未看集也表示为零。

- [ ] **Step 5：独立验证并提交。** `cargo test -p api`；若改动 Store 则 `cargo test -p store`。只 stage 本任务文件，提交 `fix(playback): use effective assertions in self API views`。

## Task 2：异步入口释放执行器，写请求等待实际结果

**Interfaces:** 复用 `store_wait::store_read`、`with_bounded_store`、`busy_response` 和 `BlockingBudget::run_to_completion`；禁止再引入新的 Store 等待封装。

- [ ] **Step 1：在 transfer_settings 测试子模块通过临时 Router 注册真实 GET/PATCH handler。** 在认证完成后的 handler 边界注入争用，避免请求停在已有的 auth bounded wait 而漏测 handler。持锁线程通过 channel 通知 ready，并最多等待 6 秒释放；测试先释放和 join，最后断言。timer 的核心断言：

```rust
let started = std::time::Instant::now();
tokio::time::sleep(std::time::Duration::from_millis(10)).await;
let elapsed = started.elapsed();
// 保存 elapsed；先发 release 并 join holder，再执行这一条 assert。
assert!(elapsed < std::time::Duration::from_millis(200));
```

current-thread runtime 上并行轮询真实 handler 请求与 timer。分别验证 GET/PATCH；写等待耗尽为 503 + Retry-After，原 revision 和值不变。

- [ ] **Step 2：运行 red。** `cargo test -p api transfer_settings -- --nocapture`。当前同步 lock 使 timer 随 holder 延迟，必须被新测试捕获。
- [ ] **Step 3：GET 复用现有读取边界；PATCH 在 blocking 预算内执行有界锁。** GET 的完整结果分派：

```rust
match super::store_wait::store_read(&state, |store| store.transfer_settings()).await {
    Ok(Ok(settings)) => ok(serde_json::json!(settings)).into_response(),
    Ok(Err(error)) => failure(error),
    Err(error) => error.response(),
}
```

PATCH 将 owned body/patch 和 Store handle 移入 `run_to_completion`，闭包调用现有 `with_bounded_store(handle, |store| store.patch_transfer_settings(expected_revision, patch))`。None 映射 busy_response；Some(Err) 保留 failure 映射；worker 错误复用 browse_work_error。不要给 mutation 套外部 timeout 然后在事务可能继续时向客户端声称失败未应用。

- [ ] **Step 4：以同样边界调整 removal async 驱动和 Scan async 准入/收尾。** Store/同步 cancel 工作移入已有预算或 spawn_blocking；等待 publication gate 时不能持 Store 或 registry。运行 `cargo test -p api --test executor_not_blocked --test executor_store_bounds --test driver_executor_bounds`，再运行本任务 GET/PATCH 回归。
- [ ] **Step 5：运行 `cargo test -p api` 并提交。** 提交 `fix(api): bound Store waits outside async workers`。

## Task 3：操作互斥复用 owner 的未完成谓词

**Interfaces:** 复用现有 `unfinished_operation_predicate() -> String`；修改 Library operation 分支，不改变 Media merge/removal 各自的可恢复定义。

- [ ] **Step 1：加入终态 Scrape 的准入回归。** 使用附录 B 的 Store fixture，建立 NFO plan 后 fail_nfo_export；确认 unfinished 列表不包含它，再用当前 config_revision 发起 locale 变更：

```rust
store.fail_nfo_export("operation-a", "source changed", 2_000).unwrap();
assert!(!store.unfinished_library_operations().unwrap().contains(&"operation-a".into()));
let file = store.file_coverage_by_path(video.to_str().unwrap()).unwrap().unwrap().file;
let request = store::library_operations::LibraryReconfiguration {
    operation_id: "locale-op".into(), library_id: file.library_id,
    expected_revision: store.library_config_revision(&file.library_id.to_string()).unwrap(),
    root_paths: None, display_locale: Some("en-US".into()),
};
assert!(store.begin_library_reconfiguration(&request, 3_000).is_ok());
```

- [ ] **Step 2：运行 red。** `cargo test -p store --test terminal_operation_admission`。当前报 already has an operation。
- [ ] **Step 3：替换该分支。** SQL 由固定 owner 谓词组成，ID 仍参数绑定：

```rust
let active = crate::library_operations::unfinished_operation_predicate();
let library_branch = format!(
    "SELECT 1 FROM library_operations WHERE library_id=?1 AND ({active})"
);
```

把 library_branch 放回现有 EXISTS，保留 removal/media 分支。Integrity owner 的 kind='integrity' 语义不变，不把所有 state!='succeeded' 全局替换成排除 failed。

- [ ] **Step 4：分别验证 roots、locale、Integrity、removal、默认 Metadata language 不受终态 Scrape 拦截；可恢复失败操作仍被拒绝。** 每个场景使用独立 fixture，避免上个操作自身阻塞下一个。运行 `cargo test -p store --test terminal_operation_admission --test operations_retention`。
- [ ] **Step 5：运行 `cargo test -p store`，提交 `fix(store): share operation terminal rules in admission`。**

## Task 4：NFO 导出建立唯一活跃路径所有权

**Interfaces:** 保留 `NfoExportRequest` 和分阶段导出；begin 对不同活跃 owner 返回 StoreError::Conflict。同一 operation 重试只能匹配同一绑定，不允许替换成另一个文件版本/请求。canonical key 来自现有 path_key 规则。

- [ ] **Step 1：以附录 B 写 red。** A plan 后建立不同 operation 的 B plan，预期 B 返回 Conflict 且 A 仍 pending：

```rust
let request_b = store.nfo_export_target(
    &video, &sidecar, domain::EntityScope::Work, "operation-b", "export-b"
).unwrap().unwrap();
assert!(matches!(store.plan_nfo_export(&request_b, 2_000), Err(store::StoreError::Conflict(_))));
assert!(store.nfo_export_pending("operation-a").unwrap());
```

- [ ] **Step 2：运行 `cargo test -p store --test nfo_export_ownership`，确认当前 B 成功且 A 消失，测试失败。**
- [ ] **Step 3：删除 begin_nfo_export 中删除旧活跃意图的循环，增加数据库唯一约束。** 最小方案给 library_operations 增加 `resource_key TEXT`，Scrape 写 canonical sidecar key，并加：

```sql
CREATE UNIQUE INDEX nfo_export_active_path ON library_operations(resource_key)
WHERE kind='scrape' AND state NOT IN ('succeeded','failed');
```

当前布局的 Scrape 必须提供非空 resource_key，可用 CHECK 约束表达 `kind<>'scrape' OR (resource_key IS NOT NULL AND length(resource_key)>0)`。不同 Media 也不能同时拥有同一实际 sidecar；不能只按 Media/path 组合互斥。更新生产 INSERT 和 fixture INSERT 的显式列，所有布局/header/对象校验与字典同步。唯一冲突映射为领域 Conflict，日志记录 owner/path，不静默丢工作。

所有 driver 入口必须携带同一持久 operation 身份，只有该 owner 能进入写盘阶段；同 owner 的重试只允许重放已持久化的同一 body，不能以同一 ID 替换新请求。现有恢复在启动期、实例租约独占时运行，应维持这项调度约束。若以后引入运行期恢复，必须让同 owner 的初次执行和恢复串行。活跃意图不允许被另一个请求删除或更换，约束在整个锁外写盘期间成立；仅终态或确认停止工作后才能释放路径所有权。

- [ ] **Step 4：运行交错/重启回归。** A 持 owner 时 B 被拒绝；A 完成后重新观察并导出 B 得到最新内容；A 写盘后进程退出，重开后 recovery 登记 A 再允许 B；同路径不同 Media、路径别名、两个 Store handle 都验证唯一 owner；人工编辑仍保留。运行 `cargo test -p store --test nfo_export_ownership --test nfo_scoped_inputs_layers` 和 `cargo test -p api --test storage_nfo_export`。
- [ ] **Step 5：运行 Store/API crate 测试，提交 `fix(storage): retain exclusive NFO path ownership`。**

## Task 5：并发测试错误路径有界退出

**Interfaces:** 使用标准库 mpsc 通知 ready/release/done；所有接收使用 recv_timeout，发送失败视为对方取消。重排现有并发行为测试，不向生产 Store 添加测试专用同步接口。

- [ ] **Step 1：增加故障注入用例。** 删除闭包返回非 Busy 错误，capture 必须退出；另测 worker 未发 ready、父方提前退出。用 channel 的 recv_timeout 判断外层回合完成，不能再用无限 join 作为失败检测。
- [ ] **Step 2：运行 red。** 旧回合的错误路径在外层截止时间内不能完成；外层 watchdog 必须能结束子进程，不能让 Cargo 自己挂住。
- [ ] **Step 3：替换 Barrier。** 每个回合保存 delete_result，先通知 release，再汇合并断言。最小控制流：

```rust
let delete_result = deleting.delete_ledger_path(&row.path);
let _ = release_tx.send(()); // 任意错误都先释放 capture。
let worker_result = done_rx.recv_timeout(std::time::Duration::from_secs(10));
assert!(worker_result.is_ok(), "capture did not stop before deadline");
delete_result.unwrap();
```

capture 的 ready/release 接收也都用 recv_timeout；channel 断开即退出整个 rows 循环。Busy 重试用绝对 Instant 截止，不能每次重新延长 5 秒。上例是回合顺序，不是让每个 file 都重新 spawn Store 或线程。

- [ ] **Step 4：运行正常交错和注入失败。** `cargo test -p store --test fingerprint_source_safety -- --nocapture`；分别运行新增取消/超时场景。验证不会把初始删除错误替换为无信息 hang。
- [ ] **Step 5：运行 `cargo test -p store` 并提交 `test(store): bound fingerprint rendezvous failures`。**

## Task 6：仅接受当前数据库与 HTTP 形状

**Interfaces:** Library PATCH 使用 deny_unknown_fields 的 PatchLibrary；前端 LibraryUpdatePayload 不包含不可改的 kind；readiness 只用 name；LibraryOperation 只用 state；Media merge 仍保留有业务意义的 phase。

- [ ] **Step 1：加入 API red 用例与前端当前形状测试。** 公开 PATCH 测试逐个发送旧字段，并用 name 哨兵确认没有部分写入：

```rust
for body in [
    serde_json::json!({"name":"must-not-write", "source":"local"}),
    serde_json::json!({"auto_clear_missing":true}),
    serde_json::json!({"scrape_overrides":{"language":"en-US"}}),
] {
    let response = app.clone().oneshot(common::request(
        "PATCH", &format!("/api/v1/libraries/{library}"),
        Some("management-secret"), body,
    )).await.unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
}
```

- [ ] **Step 2：运行 red。** 复用附录 A fixture 加上述独立测试；当前返回 200。前端测试改为只接受 `{name,kind,is_default}`，删除“双字段谁优先”的兼容测试。
- [ ] **Step 3：在 PATCH 写入前进行强类型解析。** 新 DTO 包含现行 name/root_paths/display_locale/toggles/ACL/match_rules/default_filter_id，保持 absent/null 的当前语义；用 `StrictJson<PatchLibrary>` 或明确映射反序列化错误到 400，不能让未知字段变成成功 no-op。验证完整请求后才调用 apply_library_settings。default_filter_id 继续区分“不修改”和“显式清空”。

删除 readiness 的 library_name 和 Library operation 的 phase/fallback。前端名称选择核心变为：

```ts
const name = pipeline.name.trim() || "未命名媒体库";
```

operation adapter 缺少 state 时报告合同错误，不再伪造 planned。Media merge 读取 phase_cursor 不再接受 planned/frozen/draining/applying 别名或 None；当前 preflight 已明确写 init，损坏阶段必须拒绝且不修改事实。

- [ ] **Step 4：验证当前前端全部消费者。** `npm --prefix web run typecheck`、`npm --prefix web test`、`npm --prefix web run build`；API 测试包括 PATCH 字段合法/非法类型、未知字段、完整当前 payload、kind 拒绝、null/absent，Store 测试包括合法 merge 每阶段恢复及未知/缺失 phase 拒绝。搜索确认旧适配不再存在：

```bash
rg -n 'library_name|r\.phase|phase_cursor.*unwrap_or' web/lib/subscription-pipeline.ts web/lib/api/libraries.ts web/lib/api/subscriptions.ts crates/store/src/media_operations
```

搜索输出需逐项解释，不以匹配“phase”本身误删真实 merge 阶段。

- [ ] **Step 5：运行触及 crate 测试，只提交本任务文件，提交 `refactor(api): enforce current storage contracts only`。**

## Task 7：正常 NFO 导出使用毫秒时钟

**Interfaces:** 复用现有 `ApiState::with_playback_clock` 和 `PlaybackClock::now_ms`，不新增生产时钟。

- [ ] **Step 1：在 scrape_metadata/export 的测试子模块测试真实 finish_sidecar_export。** 从附录 B 的文件事实构造 ApiState，注入固定时钟；begin_sidecar_export 得到已知 operation_id 的请求，调用 finish 后执行保留。核心断言：

```rust
let now_ms: i64 = 1_800_000_000_000;
let cutoff_ms = now_ms - 30 * 86_400_000;
assert_eq!(store.prune_terminal_operations(cutoff_ms, 100, false).unwrap().deleted, 0);
assert!(store.nfo_export_registration(&operation_id).unwrap().is_some());
assert_eq!(store.prune_terminal_operations(now_ms + 1, 100, false).unwrap().deleted, 1);
```

固定时钟实现已有 trait，`fn now_ms(&self) -> i64 { self.0 }`。应通过 API glue 的实际入口测试，不能只在 Store 测试里把参数改成正确毫秒而漏掉错误调用者。

- [ ] **Step 2：运行 red。** `cargo test -p api scrape_metadata::export -- --nocapture`，当前新历史立即被清理。
- [ ] **Step 3：仅替换错误取时入口。**

```rust
let now_ms = state.playback_clock.now_ms();
```

不修补旧数据、不保留秒/毫秒双解释分支。

- [ ] **Step 4：运行 green 和 `cargo test -p api --test storage_nfo_export`。** 确认保留清理不改 library_metadata_inputs 的 export baseline。
- [ ] **Step 5：运行 `cargo test -p api`，提交 `fix(scrape): timestamp NFO operations in milliseconds`。**

## Task 8：待恢复 NFO 查询不扫描终态历史

**Interfaces:** 保持 scrape_operations 的返回集合和顺序；索引 predicate 与现行 SQL 完全一致；不通过取消恢复、删除活跃行或缩减结果语义来优化。

- [ ] **Step 1：增加工作量 red 回归。** 使用生产 schema，未完成数固定为 0/1/20，终态 operation 从 1k 增至 20k；测实际 statement 的 FullscanStep/VmStep/Sort，同时断言返回待恢复 operation 集合与顺序。不要只断言索引名称。
- [ ] **Step 2：运行 `cargo test -p store --test nfo_export_work`。** 现状 0 个 pending 时从 7012 VM 增至 140012；判定门槛可以设终态历史增长后 VM 不超过预热基线 2 倍，并要求 fullscan=0、sort=0。
- [ ] **Step 3：增加候选部分索引，同步布局校验及版本。**

```sql
CREATE INDEX nfo_export_pending_age
ON library_operations(updated_at_ms, operation_id)
WHERE kind='scrape' AND state NOT IN ('succeeded','failed');
```

保留绑定 kind='scrape' 的当前 SQL，并在 bundled SQLite 实测 planner 使用该部分索引。Task 4 的路径唯一索引用途不同，不因都为 partial 就删掉其中一个。

- [ ] **Step 4：运行 green 并量写入代价。** `cargo test -p store --test nfo_export_work --test operations_retention --test schema_version_guard`；比较插入/完成 100 个导出的写入工作量和索引页数，确认收益不只是空集合特例。
- [ ] **Step 5：运行 `cargo test -p store` 并提交 `perf(store): index unfinished NFO export recovery`。**

## 额外优化与设计收敛项

以下项与上面可复现缺陷分开，不声称已经取得性能收益：

1. **游标作用域补齐。** `library_query/items/page.rs:51–55` 的 PageCursor 只有 keys/media_id，不包含 Library/User/order/direction/filter 指纹；不像 UnitCursor 会检查 scope。应将新游标绑定这些上下文，换库/换排序/换过滤时返回 400；不接受旧 JSON 游标。当前 ACL 查询仍限制在授权 Library，不能将此说成已证实跨库泄漏。
2. **item-index 与 episodes 的锁边界和放大。** `http/library_scan.rs:489–563` 在 async 中持 Store 遍历 Library 的排序结果；`http/library.rs:597–610` 对完整 episodes 逐集查状态。前者是按库增长的工作，后者存在查询放大。把索引计算放到有界 reader snapshot；分集有效状态批量化。测试固定当前库、增长无关库，以及单 Media 的 10k 分集，记录 SQL 数/VM/锁时长/解码行，不能靠 LIMIT 返回行数推断成本。
3. **连接设置统一。** Store cache 连接设 NORMAL、页缓存/mmap/temp tuning；独立 `media::response_cache::open` 只设 WAL/timeout/FK。统一连接角色策略前先测缓存写入 fsync、读写竞争和内存；不把 authoritative 库降到 NORMAL。`temp_store=MEMORY` 不构成排序内存硬上限，journal_size_limit 也不是有活跃 reader 时的 WAL 硬上限。
4. **Schema 身份验证加强。** Store/cache 当前主要校验 header 和对象名，Jobs 会更严格校验定义。可以用生产 DDL 创建期望 schema，对列、PK/FK/CHECK/index 定义建立规范化指纹；当前版本但约束缺失时拒绝打开。此项是防止错误布局被接受的加固，不是发现了现行启动自动迁移旧库。
5. **先测真实访问路径再删索引。** protocol terms 的主键顺序与额外 media 索引有进一步合并的可能，但必须保留 membership、计数、repair、每卡读取的所有路径。当前已有专门排序索引和稀疏用户摘要，不能按“索引多”或“有 JOIN”直接判设计错误。

## 验证记录与交付门槛

本轮已经执行：

- `npm --prefix web run typecheck`：退出码 0。
- `npm --prefix web test`：849 passed，0 failed。
- `npm --prefix web run build`：退出码 0；已有大 chunk 提示，未作为本次数据库缺陷处理。
- `cargo test --workspace --no-fail-fast`：退出码 0；466 个测试程序/文档测试结果段汇总 3095 passed、0 failed、24 ignored。忽略项不计为已验证。
- 临时 HTTP 回归：当前 HEAD 1 failed，失败符合 F1 的正确业务预期；同时记录旧 PATCH 字段 200。它没有加入仓库测试树。
- 独立 Store 公共 API 复现：F3/F4/F7 都复现且复现断言通过。
- bundled SQLite 3.50.2 statement status 实验：F8 的当前与候选结果见上表。
- 仓库 Rust 文件行数检查：没有超过 800 行的 .rs 文件。没有据此承诺所有函数均符合 120 行限制。

没有运行真实 NAS/磁盘掉电、真实 TMDB/Downloader/Chromium、浏览器端到端交互、生产实例迁移或重建；没有执行本文修复步骤。微基准不代表整个系统的吞吐或延迟。

后续实施完成后，每任务跑触及 crate，最后 `cargo test --workspace --no-fail-fast` 一次，加前端 typecheck/test/build。复现必须从 red 变为 green；正常路径通过不能替代故障注入、重启恢复和错误路径有界退出。

## 自检

- 代码/数据库合理性：架构评价、F3/F4/F7/F8 与额外优化 2–5。
- 无旧版兼容：F1 的旧语义读路径、F6 的双形状/别名/宽松字段，Task 6 的唯一当前合同。
- 生产与测试死锁：F2 executor、F3 状态机阻塞、F4 写入竞争、F5 真正的条件性测试永久等待分别命名；未混称 SQLite 死锁。
- 数据库优化：F8 有当前 bundled 引擎证据，其余优化明确为待测。
- 当前交付是 review 和方案；所有任务复选框保持未执行。


## Appendix A: HTTP reproduction run against current HEAD

This standalone HTTP regression produced 1 expected failure. For a permanent test, split setup, request helpers and assertions by responsibility, and separate the PATCH regression from the playback regression. The common module path below is relative to the proposed crates/api/tests/storage_review_playback.rs file.

```rust
use std::{collections::HashMap, sync::Arc};
use axum::http::StatusCode;
use domain::{Media, MediaId, MediaKind, UserId, UnitKey};
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::{Value,json};
use tower::ServiceExt;
use store::test_support::{FixtureFile,publish_fixture_file};
#[path="management/common.rs"]
mod common;
#[tokio::test]
async fn scoped_mark_reaches_all_self_api_views() {
    let tmp=tempfile::tempdir().unwrap();
    let state=common::state(tmp.path(), Arc::new(common::Fixtures {requests:Mutex::new(Vec::new()),bodies:HashMap::new()}),Arc::new(MemoryDownloader::new(tmp.path().join("stage"))));
    let app=api::router(state.clone());
    let media_id=MediaId::new();
    let root=tmp.path().join("data/library/tv/Test Work");
    std::fs::create_dir_all(&root).unwrap();
    let user:UserId;
    let library:String;
    {
      let handle=state.store(); let store=handle.lock();
      user=store.admin_user_id().unwrap();
      let media=Media {id:media_id,kind:MediaKind::Tv,title:"Test Work".into(),year:Some(2024),original_title:None,aliases:vec![],tmdb_id:None,douban_id:None,tvdb_id:None,bangumi_id:None,anilist_id:None};
      store.insert_media(&media).unwrap();
      library=store.default_library(MediaKind::Tv).unwrap().unwrap().id;
      for ep in 1..=2 {
        let path=root.join(format!("Work S01E{ep:02}.mkv")); std::fs::write(&path,b"video").unwrap();
        publish_fixture_file(&store,&FixtureFile {media_id,path:path.to_string_lossy().to_string(),season:Some(1),episode:Some(ep),..Default::default()}).unwrap();
      }
    }
    let initial=app.clone().oneshot(common::request("GET",&format!("/api/v1/playback/marks?media_item_id={media_id}&season_number=1"),Some("management-secret"),Value::Null)).await.unwrap();
    eprintln!("initial season marks (two owned unwatched units): {}",common::json_data(initial).await);
    let patch=app.clone().oneshot(common::request("PATCH",&format!("/api/v1/libraries/{library}"),Some("management-secret"),json!({"source":"local","auto_clear_missing":true,"scrape_overrides":{"language":"en-US"}}))).await.unwrap();
    eprintln!("retired Library PATCH keys -> status={} body={}",patch.status(),common::json_body(patch).await);
    let response=app.clone().oneshot(common::request("POST","/api/v1/playback/marks",Some("management-secret"),json!({"media_item_id":media_id,"season_number":1,"played":true}))).await.unwrap();
    assert_eq!(response.status(),StatusCode::OK);
    let result=common::json_data(response).await;
    eprintln!("mark result={result}");
    {
      let handle=state.store(); let store=handle.lock();
      assert_eq!(store.effective_unit_state(user,media_id,UnitKey::new(1,1)).unwrap().unwrap().played,true);
      assert!(store.unit_state(user,media_id,1,1).unwrap().is_none());
    }
    let mut results=vec![];
    for path in [format!("/api/v1/playback/marks?media_item_id={media_id}&season_number=1&episode_number=1"),format!("/api/v1/playback/items/{media_id}/episodes?season_number=1"),format!("/api/v1/libraries/{library}/items/{media_id}/episodes?season=1"),format!("/api/v1/playback/resume?media_item_id={media_id}&season_number=1&episode_number=1"),String::from("/api/v1/playback/up-next"),format!("/api/v1/playback/marks?media_item_id={media_id}&season_number=1")] {
      let response=app.clone().oneshot(common::request("GET",&path,Some("management-secret"),Value::Null)).await.unwrap();
      assert_eq!(response.status(),StatusCode::OK,"{path}");
      let data=common::json_data(response).await;
      eprintln!("{path} -> {data}"); results.push(data);
    }
    assert_eq!(results[0]["played"],true,"scoped mark must affect episode marks");
    assert_eq!(results[1]["episodes"][0]["played"],true);
    assert_eq!(results[2][0]["played"],true);
    assert_eq!(results[3]["played"],true);
}
```

## Appendix B: Store public API reproduction

This exact program reproduced the NFO interleaving against current HEAD. Its reproduction assertions all passed. Tasks 3 and 4 reuse its fixture setup, then assert the corrected behavior specified in each task. All databases and files are temporary.

```rust
use domain::{EntityScope, FieldValue, Media, MediaId, MediaKind, MetadataFields};
use store::Store;
use store::test_support::{FixtureFile,publish_fixture_file};
use store::metadata::export_driver::{write_planned_nfo, resume_nfo_export_phased};
fn main() {
 let tmp=tempfile::tempdir().unwrap(); let root=tmp.path().join("movies"); std::fs::create_dir(&root).unwrap();
 let store=Store::open(tmp.path().join("data")).unwrap();
 store.create_library(MediaKind::Movie,"Movies",&[root.to_str().unwrap()],"everyone",true,&[]).unwrap();
 let media=Media {id:MediaId::new(),kind:MediaKind::Movie,title:"Original".into(),year:None,original_title:None,aliases:vec![],tmdb_id:None,douban_id:None,tvdb_id:None,bangumi_id:None,anilist_id:None};
 store.insert_media(&media).unwrap(); let path=root.join("Movie.mkv"); std::fs::write(&path,b"movie").unwrap();
 publish_fixture_file(&store,&FixtureFile {media_id:media.id,path:path.to_string_lossy().into_owned(),..Default::default()}).unwrap();
 let sidecar=path.with_extension("nfo");
 let a=store.nfo_export_target(&path,&sidecar,EntityScope::Work,"operation-a","export-a").unwrap().unwrap();
 let a=store.plan_nfo_export(&a,1000).unwrap();
 let mut fields=MetadataFields::default(); fields.title=FieldValue::Value("Updated".into());
 store.set_metadata_overrides(media.id,EntityScope::Work,"zh-CN",&fields,0).unwrap();
 let b=store.nfo_export_target(&path,&sidecar,EntityScope::Work,"operation-b","export-b").unwrap().unwrap();
 let b=store.plan_nfo_export(&b,1002).unwrap();
 assert_ne!(a.body,b.body);
 assert!(!store.nfo_export_pending("operation-a").unwrap());
 let action=write_planned_nfo(&a).unwrap();
 let result=store.finish_planned_nfo_export(&a,action,None,1003); println!("A finish: {result:?}"); assert!(result.is_err());
 let result=write_planned_nfo(&b); println!("B write: {result:?}"); assert!(result.is_err());
 for n in 0..2 {let result=resume_nfo_export_phased(|| &store,&b,1004+n);println!("B resume: {result:?}");assert!(result.is_err());}
 assert!(store.nfo_export_pending("operation-b").unwrap());
 assert_eq!(std::fs::read_to_string(&sidecar).unwrap(),a.body);
 println!("CONFIRMED: superseded writer left A bytes while B remains pending and cannot recover");
}
```

## Appendix C: Query measurement conditions

The temporary Rust program links the current rusqlite rlib, loads production schema/v1/library.sql, inserts 1000 or 20000 succeeded scrape operations and runs ANALYZE. It measures StatementStatus::VmStep, FullscanStep and Sort for the following query with scrape bound to ?1, then adds the Task 8 index and repeats. Pending count remains zero.

```sql
SELECT operation_id, plan_json FROM library_operations
WHERE kind = ?1 AND state NOT IN ('succeeded', 'failed')
ORDER BY updated_at_ms, operation_id;
```

Temporary evidence sources: /tmp/storage_review_pending_query.rs, /tmp/storage_review_failed_scrape.rs, /tmp/storage_review_nfo_race.rs, /tmp/storage_review_nfo_time.rs. These are review artifacts, not runtime dependencies. Reproduction scenarios and portable source are recorded above.

The exact bundled SQLite experiment:

```rust

use rusqlite::{Connection, params, StatementStatus};
fn measure(db:&Connection,n:usize,label:&str) {
 let sql="SELECT operation_id, plan_json FROM library_operations WHERE kind = ?1 AND state NOT IN ('succeeded', 'failed') ORDER BY updated_at_ms, operation_id";
 let mut stmt=db.prepare(sql).unwrap();
 let rows=stmt.query_map(["scrape"],|r|r.get::<_,String>(0)).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
 println!("{label}: history={n}, returned={}, vm={}, fullscan={}, sort={}",rows.len(),stmt.get_status(StatementStatus::VmStep),stmt.get_status(StatementStatus::FullscanStep),stmt.get_status(StatementStatus::Sort));
 let mut plan=db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
 for row in plan.query_map(["scrape"],|r|r.get::<_,String>(3)).unwrap(){println!("  {}",row.unwrap());}
}
fn main(){
 println!("bundled sqlite={}",rusqlite::version());
 for n in [1000,20000] {
  let db=Connection::open_in_memory().unwrap();
  db.execute_batch(include_str!("/Users/jianxcao/work/fun/crawler-media/crates/store/src/schema/v1/library.sql")).unwrap();
  {let tx=db.unchecked_transaction().unwrap();
   for i in 0..n {tx.execute("INSERT INTO library_operations(operation_id,library_id,kind,state,expected_revision,plan_json,created_at_ms,updated_at_ms) VALUES (?1,'lib','scrape','succeeded',1,'{}',1700000000000,1700000000000)",params![format!("op-{i}")]).unwrap();}
   tx.commit().unwrap();}
  db.execute_batch("ANALYZE").unwrap();
  measure(&db,n,"current");
  db.execute_batch("CREATE INDEX scrape_pending_age ON library_operations(updated_at_ms,operation_id) WHERE kind='scrape' AND state NOT IN ('succeeded','failed')").unwrap();
  measure(&db,n,"candidate");
 }
}
```
