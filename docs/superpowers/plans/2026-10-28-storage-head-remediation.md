# Storage HEAD Review Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 关闭 `7030c1dd` 对 main 复审的 H1–H6，保留当前存储与协议行为，并完成可复核的性能和并发回归。

**Architecture:** 保留五文件边界、同库事务摘要与现有 blocking/read-pool 机制。先修后台发布的身份屏障，再修遗漏的执行边界和二次算法，最后删除旧输入转换；不以新表、新索引或统计信息掩盖接线问题。六项按独立可验收任务组织，Task 4/6 复用 Task 2 的锁边界原则，其余可独立交付。

**Tech Stack:** Rust 2024 workspace、rusqlite/bundled SQLite 3.50.2、Axum、Tokio、parking_lot；现有依赖。Web 本轮只有验证，无计划内功能改动。

**Spec:** [本次审查 H1–H6 与复现证据](../../audits/2026-10-28-storage-head-review.md)、[存储目标合同](../specs/2026-10-18-library-storage-redesign.md)、[AGENTS](../../../AGENTS.md)。本文件是待执行方案，用户当前只授权 review，所有步骤保持未执行。

## Global Constraints

- 用户要求：“因为我们无需兼容旧版”“一定不要做任何兼容”。不增加导入、迁移阶梯、字段别名、转发或双读写。
- “domain 是 types only — no IO”；Store 可依赖 domain/library，其他 crate 不依赖 api。
- “Tests first at the behavior's public seam (red → green).” 所有新增回归离线运行，不启动 Chromium，不依赖真实 TMDB/Downloader。
- “Run `cargo test -p <crate>` for every crate touched, then `cargo test --workspace` once at the end of the request.”
- `.rs` 文件 hard 800 行；生产函数和测试函数 hard 120 行；按责任拆分。
- “DO NOT require authentication on read-only static image endpoints.” Jellyfin 视频流遵守其协议鉴权。
- 保留精确 total、ACL、稳定坐标决议、文件版本和删除覆盖安全；权威库 synchronous=FULL。
- “Commit on the current branch after the change is green. Do not include unrelated user changes in the commit.” 本计划不授权部署、重建实例或合并。

## Review Focus

1. 后台模型在 Media 转属、换源、冻结后迟到：必须零写拒绝，重开后合并可收敛；Task 1。
2. 外线程持锁、慢文件系统、取消或 panic：timer 与其他 Store 读继续运行，预算由真实 worker 释放；Task 2/4。
3. 空库运行中增加数据，无统计或不同统计：Series 工作量均线性，显式 false 和同时间坐标优先级不变；Task 3。
4. hardlink、未知覆盖、多单元 pack、多版本：重复判定不改变删除安全，轻候选不复制 U² 覆盖；Task 4/6。
5. PATCH 中出现一个旧 kind 或无效成员：请求失败必须整份零写，当前 HDR/title_match 行为与优先级保留；Task 1/5。

---

## 文件结构与接口边界

| 任务 | 生产文件职责 | 测试文件 |
|---|---|---|
| 1 | `crates/store/src/fingerprint_models.rs` 管模型原子发布；`crates/api/src/probe_manager/season.rs` 处理过期 pipeline | 新增 `crates/store/tests/fingerprint_model_identity_barrier.rs`；新增 `crates/api/src/probe_manager/season/tests.rs`（在 season 模块接入） |
| 2 | `http/collections.rs`、`http/library.rs`、`library_statistics.rs` 的 owned SQL offload；Collection 图片观察移锁外 | 新增 `crates/api/tests/executor_remaining_store.rs` |
| 3 | `crates/store/src/library_query/series_scalar.rs` 管三坐标断言查询 | 新增 `crates/store/tests/protocol_series_scalar_work.rs`；扩展 `crates/api/tests/query_page_materialization.rs` |
| 4 | `crates/api/src/http/library_duplicates.rs` 捕获覆盖、锁外观察与集合判重 | 新增 `crates/api/tests/duplicates_work_bounds.rs`；扩展 `crates/api/tests/management/library_duplicates.rs` |
| 5 | `crates/api/src/http/rule_sets.rs` 仅解析当前 atom | 扩展 `crates/api/tests/management/filters.rs` |
| 6 | `media_server_provider/selection/light.rs` 共享覆盖；`selection/{mod,resume}.rs` 锁外纯计算 | 新增 `crates/api/tests/jellyfin_shared_coverage.rs` |

表内 API 源码路径除特别标明外均相对 `crates/api/src/`。触及文件超限时只按该文件职责拆 sibling module，不扩展为全仓重构。O1–O6 不是此六项自动关闭的承诺；尤其大规模设备验收和前端渐进加载另行安排。

### Task 1: 指纹模型原子身份与成员屏障（H1）

**Interfaces:** 保留 `Store::put_fingerprint_model(&self, model: &StoredFingerprintModel, members: &[StoredFingerprintModelMember]) -> Result<(), StoreError>`。消费现有 `library_write::publish::ensure_media_writable(&Connection, MediaId)`、文件版本与写 scope 校验；对 merging/retired 或过期成员返回 Protected/版本冲突，禁止静默重定向。`persist_season_model` 仍返回 `()`，拒绝记录结构化 warn，正常 SQL 错误保留错误上下文。

- [ ] **Step 1: 写公开回归。** 以 `playback_identity_barrier::interrupted_merge_rejects_late_marks_and_resumes_after_reopen` 的失败注入模式改为模型；按证据附录种一份 TV 文件。新增 `interrupted_merge_rejects_late_model_and_resumes_after_reopen`、`frozen_and_retired_models_are_zero_write`、`invalid_member_keeps_previous_model_and_members`、`current_members_publish_normally`。核心断言：

  ```rust
  assert!(store.put_fingerprint_model(&late_model, &late_members).is_err());
  assert!(store.list_fingerprint_models(&loser.to_string(), 1).unwrap().is_empty());
  // 移除一次失败注入并重开后：
  assert_eq!(reopened.advance_media_merge(&operation, 1300).unwrap().state, "succeeded");
  ```

  无效成员分别构造改挂 Media、换 source_version、非 published/非有效 scope；先有模型更新失败后，用 `list_fingerprint_models` 及 fixture 连接读取成员事实与之前完全相等。正常/合法空成员是否允许沿当前合同保留，不为修复自行新增模型最小人数规则。
- [ ] **Step 2: 跑 red。** `cargo test -p store --test fingerprint_model_identity_barrier`；预期当前迟到写成功，零写/恢复断言失败。
- [ ] **Step 3: 实现事务屏障。** 第一条模型/成员变更前验证 model Media 和全部成员；校验与 INSERT/替换在同一 IMMEDIATE 事务，不逐成员独立提交。API 最终校验显式比较当前 Media 与 pipeline Media，再调用该公开事务；过期模型丢弃。在 season 模块测试注入“计算后文件转属”，断言发布不落到 loser，也不自动写 survivor。
- [ ] **Step 4: 跑 green。** `cargo test -p store --test fingerprint_model_identity_barrier`、`cargo test -p api --lib probe_manager::season`，再运行完整 `cargo test -p store` 与 `cargo test -p api`；退出0。
- [ ] **Step 5: 仅提交本任务文件。** `git commit -m "fix(storage): reject stale fingerprint model publication"`。

### Task 2: 余下 Library/Collection SQL 阶段移出执行器（H2）

**Interfaces:** 保留 HTTP 路由/响应。消费 `http::store_wait::store_read`、`with_bounded_store` 和 `state.http_work.run_to_completion`，不新增另一套锁预算。Collection gallery 返回 owned DB 事实，使用 `FileStatRead` 锁外观察；读请求不能持 Store 跨 await，写请求拿锁后调用现有 writable/admission 检查。

- [ ] **Step 1: 写 Router red 回归。** 在运行时外构建 ApiState，使用真实 User token。在 `remaining_roster_and_collection_routes_keep_executor_responsive` 对 GET `/libraries`、`/collections`、`/collections/{id}/items`、`/collections/{id}/gallery` 与 POST/PATCH/DELETE Collection 路径逐个施加独立线程500ms持锁；10ms timer 断言 `< Duration::from_millis(200)`，保留成功状态/事实断言。gallery 注入阻塞 FileStatRead，独立 Store 读通过 channel 在200ms内完成。holder 独立释放、recv_timeout、join 后再断言，避免测试自己锁死。
- [ ] **Step 2: 跑 red。** `cargo test -p api --test executor_remaining_store`；预期当前 timer 大于500ms或慢观察阻止独立读。
- [ ] **Step 3: 实现表内四个入口文件。** 整段同步 SQL 捕获 owned 结果后 offload；`collect_statistics_for_roster` 保留当前签名与统计值，只将 owned roster 的查询阶段移出执行器。Collection owner/ACL 与写入在同一持锁阶段。图库磁盘观察使用预算，后续角色/可见性重验沿现有授权合同。另逐条审计 async 直接锁调用者，记录已覆盖路由；新增实际同类遗漏时必须给独立回归，不以批量替换完成审计。
- [ ] **Step 4: 跑 green 与失败路径。** 定向测试加等锁超预算503/Retry-After、取消/panic预算释放和等待期间维护准入变化零写；`cargo test -p api --test executor_remaining_store`、`cargo test -p api --test metadata_write_gate`、`cargo test -p api` 全部退出0。
- [ ] **Step 5: 提交。** `git commit -m "fix(api): offload remaining library and collection store work"`。

### Task 3: Series 三坐标精确查找（H3）

**Interfaces:** 保留 `Store::protocol_series_scalar(user: UserId, media: MediaId, parent: &ProtocolParent) -> Result<ProtocolSeriesScalar, StoreError>` 与 `series_scalar_on` 签名、episodes/seasons/completed/latest_created_ns 语义。完整读池观察消费已有 `LibraryReadLease::execute_observed` 和 `QueryWorkStats.vm_steps`。

- [ ] **Step 1: 写公开回归。** 新增 `series_work_is_linear_with_and_without_statistics`，按附录的合法文件/coverage/Playback夹具，E=100/1,000/5,000，统计前后分别执行公开 scalar 和协议 `Limit=1` 的 lease。断言 episodes/completed=E、协议只返回该一个 Work；同口径 VM `vm_1000 <= 12 * vm_100` 且 `vm_5000 <= 6 * vm_1000`。公开结果 oracle 含 whole=true→season=false→episode=true、新旧时间、同时间 episode>season>whole、没有断言的 own 值、其他 User/不可见 Library 不污染。测量不能只包含页选择而遗漏 scalar。
- [ ] **Step 2: 跑 red。** `cargo test -p store --test protocol_series_scalar_work`；当前无统计分支约95倍 VM，将违反12倍预算。
- [ ] **Step 3: 修改 `series_scalar_on` 的 asserted 子查。** 拆为 exact episode、season、whole 三个等值主键查询，以 UNION ALL 合成至多三候选再按原时间/坐标排序；own fallback 不变。不新建索引，不将固定预算写成“必须使用某 EXPLAIN 文本”。
- [ ] **Step 4: 跑 green。** 定向 Store 与 `cargo test -p api --test query_page_materialization`、`cargo test -p store`、`cargo test -p api` 全部退出0；记录统计前后 VM 及写入成本未变（无新 DDL）。
- [ ] **Step 5: 提交。** `git commit -m "perf(store): bound series assertion lookups by exact coordinates"`。

### Task 4: 重复文件列表复用覆盖与锁外集合判重（H2/H4）

**Interfaces:** GET/DELETE duplicates 响应形状和删除条件保持。GET 捕获已有 `Vec<FileCoverage>`，不经 `files_in_library` 丢掉 units；文件观察消费 `FileStatRead::metadata(&Path) -> io::Result<Metadata>`，由同份 metadata 推导实体和 size。非 Unix 保留当前 canonical identity 语义并在锁外执行。

- [ ] **Step 1: 写公开 red 回归。** `duplicates_keep_executor_and_store_available` 复现附录GET争用、慢metadata期间独立Store读。`duplicates_reuse_captured_units` 以100/10,000无重复文件比较完整捕获 SQL：语句数允许按有界批次增长，禁止每file额外一次unit查询。`duplicate_entities_preserve_hardlink_and_pack_semantics` 对独立副本/hardlink/未知覆盖/多单元pack/消失文件验证分组与现有DELETE安全结果。每个覆盖文件只允许一次metadata观察（此处bug就是重复出站IO，因此允许断言次数）。
- [ ] **Step 2: 跑 red。** `cargo test -p api --test duplicates_work_bounds`；当前timer/覆盖SQL/IO次数至少一项失败。
- [ ] **Step 3: 实现 `library_duplicates.rs`。** owned覆盖+批量Media事实在有界blocking Store阶段捕获；释放锁后，限额观察并缓存每file实体/size，HashSet判重；返回全组文件。不改删除证明的集合包含、源版本重验与持久删除意图。GET纯计算也放blocking预算内；补同单元100/1,000/5,000独立实体的比较/分配或CPU规模证据，证明不再N²。
- [ ] **Step 4: 跑 green。** `cargo test -p api --test duplicates_work_bounds`、`cargo test -p api --test management library_duplicates`、`cargo test -p api`；全部退出0，现有删除负例继续通过。
- [ ] **Step 5: 提交。** `git commit -m "perf(api): reuse duplicate coverage and observe files off lock"`。

### Task 5: 删除旧 Filter atom 输入（H5）

**Interfaces:** `RuleSetInput`/`AtomInput` 的当前标准字段不变；`atom_from_input` 对三个旧 kind 走现有 unknown 分支，HTTP400 `rule_set.invalid`；无需改变 canonical atom 存储格式。

- [ ] **Step 1: 写 Router red 回归。** `obsolete_filter_atoms_are_rejected_without_writes` 遍历 `platforms_block/hdr_block/release_group_block` 的 POST和PATCH；混入一个合法atom后也必须400，Store `list_filters` 与原filter逐项相等。`canonical_exclusion_atoms_round_trip` 断言 title_match/hdr+exclude 的POST201/PATCH200、kind/value/priority/exclude往返一致。
- [ ] **Step 2: 跑 red。** `cargo test -p api --test management obsolete_filter_atoms`；当前返回201/200，失败。
- [ ] **Step 3: 删除旧转换分支及注释。** 不添加标准化函数或隐式回退；Web已用标准值，不改表单投影。同步 API 文档列当前值与旧值拒绝行为。
- [ ] **Step 4: 跑 green。** `cargo test -p api --test management`、`cargo test -p api`、`cd web && pnpm test && pnpm typecheck` 全部退出0。
- [ ] **Step 5: 提交。** `git commit -m "fix(api): reject obsolete filter atom kinds"`。

### Task 6: NextUp/Resume 共享单文件覆盖（H6）

**Interfaces:** `LightItem.coverage` 改为 `Arc<FileCoverage>`；`light_item` 接受 `&Arc<FileCoverage>`，每个文件在 `push_units` 循环之前只建立一次 Arc，候选仅clone句柄。`selected_light_item` 对页内覆盖单独建立一次共享句柄；`heavy_snapshot` 消费共享内容，输出协议DTO不变。read_scope返回owned数据后释放Store，再build/dedupe/sort；不把Arc构造放在逐单元内部复制原Vec。

- [ ] **Step 1: 写 red 回归。** `nextup_and_resume_share_multiunit_coverage` 通过公开provider分别请求NextUp/Resume，单文件U=100/1,000/5,000、limit1，验证同一个返回单元/total；为该独立test binary提供线程/请求限定的分配测量，输入构造与SQLite夹具在测量外。10倍U时分配字节≤15倍；5倍时≤8倍。测量需包含全部轻候选，不能只计算heavy计数。另在selection模块测试断言同file候选 `Arc::ptr_eq`，作为共享存储合同而非唯一行为证据。覆盖played/favorite、ACL和多版本。
- [ ] **Step 2: 跑 red。** `cargo test -p api --test jellyfin_shared_coverage -- --test-threads=1`；旧覆盖复制将超过线性分配预算。
- [ ] **Step 3: 按接口改共享覆盖和锁边界。** 创建Arc位于每file一次的边界；NextUp和Resume共同使用；纯计算期间不持Store。页内重元数据仍消费同份覆盖与原用户状态，不重读后产生混合快照。
- [ ] **Step 4: 跑 green。** 新回归、`cargo test -p api --test jellyfin`、`cargo test -p api --test jellyfin_page_perf`、`cargo test -p api` 全部退出0，记录分配增长。不得把增加请求时限作为解决方案。
- [ ] **Step 5: 提交。** `git commit -m "perf(api): share multiunit coverage in playback candidates"`。

### Task 7: 文档与最终门禁

**Interfaces:** 本审查报告是finding与验收状态唯一入口；旧版本数字保留版本标识。H1–H6只有对应行为/规模门通过才关闭；O1–O6逐项保持明确状态。

- [ ] **Step 1: 更新当前合同与结果。** 更新本审查的各项状态、实际命令/结果和未跑范围，数据库字典只改真实接线/结构变化；归档本计划实施状态，修复本地文档链接。
- [ ] **Step 2: 完成最终门。** 每个触及crate完整`cargo test -p <crate>`后，最终只在代码稳定时执行一次`cargo test --workspace --no-fail-fast`；Web `pnpm test`、`pnpm typecheck`、`pnpm build`。保留失败/ignored，不用单测重跑抵消全量失败；若出现环境中断，说明并重跑完整门。
- [ ] **Step 3: 检查范围并提交文档。** `git diff --check`；按AGENTS计数Rust文件/函数，检查源码无旧kind转换/旧API消费者；只暂存本任务文件，`git commit -m "docs: record storage remediation acceptance"`。不自动合并。

## 自审与交接

H1–H6分别对应Task1–6；用户的设计/数据库、无兼容、锁与测试、规模性能、文档五方面在审查及Task7有落点。Task1保留模型接口，Task3保留scalar接口，Task6明确共享签名，其他任务保持HTTP合同；测试同时覆盖显式false、失败回滚、取消、统计变化和多单元文件。

本轮仅提交该方案，尚未实施，不请求切换执行方式。后续仅在用户另行授权实现时按任务执行；合并结论以届时最新提交和门禁为准。
