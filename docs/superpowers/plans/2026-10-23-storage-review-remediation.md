# Storage Review Remediation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 关闭 main 基线复审确认的锁、身份/文件事实、前端选源及零兼容缺口，并以等价 SQL 减少深游标工作量。

**Architecture:** 保留五文件、显式 ownership、同库事务摘要和冷详情；不建兼容层、第二投影、通用事件流或新服务。先消除进程锁循环与不可逆事实破坏，再修可恢复操作及实际消费者；同向排序直接复用已有复合索引。源码证明的成本问题先度量再缩小更新范围，不先增加索引或连接池。

**Tech Stack:** Rust 2024、rusqlite 0.37 bundled SQLite、Axum、parking_lot、现有 React/TypeScript/Vite；不预设新依赖。

**Spec:** [存储主规范](../specs/2026-10-18-library-storage-redesign.md)、[本轮 review 与复现](../../audits/2026-10-23-storage-main-review.md)。执行者必须同时读；固定审查HEAD=`263114d854bca9515642053f024f3816efa2a9d6`，main=`86122d73a4a23439e1d83234590fdd98b388fc50`。此计划只设计，未授权实施；旧10-22任务不机械重做。

## Global Constraints

以下引自主规范，所有任务隐含遵循：

- “无旧数据导入、兼容视图、双读双写、旧版本回退。”
- “TV 的 S00/E00 是合法分集，不能兼作 whole。”；movie/video whole=`(-1,-1)`，未知TV覆盖为空。
- “所有索引事实、覆盖、terms、卡片和用户摘要在同一 library.db `BEGIN IMMEDIATE` 事务内发布；冷 IO/网络/JSON 解析必须在事务外完成。”
- “扫描以**最多100个文件**为提交窗口”；完整性校验“每批最多100个Media”。
- “Media、原始用户进度、Collection成员不随最后文件删除。”
- “禁止library/file删除通过FK级联删除Media或User观看事实。”
- “静态图片仍公开，Jellyfin 视频按协议鉴权，不能把两者混淆。”
- “规模至少 Media=100k、文件=1M、Playback 单元=1M”；“普通请求 page_size=50”。这是后续验收规模，不是本轮已完成测量。
- “未来变更正常做前向schema版本升级，本次不写旧数据迁移器。”本次未上线布局直接更新目标DDL，不为旧本分支布局增shim。
- 补充仓库约束：公开seam RED→GREEN；每触及crate独立 `cargo test -p <crate>`，稳定树 `cargo test --workspace` 一次。`.rs`≤800行，函数≤120行；`lib.rs`仅模块/导出；关键失败完整结构化日志，不吞错误。
- 不操作用户真实DB/媒体/服务；不自动执行实例重建。实现完成前更新相关现有数据库/接口文档，不创建无关文档。

## Review Focus

1. claim后文件被并发删除：退役应结束且Store仍可获取，不只是“查询返回空”（Task1）。
2. 目标lease在publication内析构：与提前acquire一样会反锁，源删除与跨库整理必须都能结束（Task2）。
3. rename恢复时源消失、目标存在却是外来文件：不能把存在当作属于意图，必须拒绝且不改字节（Task3）。
4. merge选择的provider有快照/无快照、显式空与不同locale：持久有效行/详情/卡片一致且不丢来源（Task5）。
5. 用户没选版本的继续观看与跨集导航：自动恢复保原源，next/prev不继承旧集物理偏好（Task10）。

---

## 文件责任与任务依赖

| 任务 | 生产边界 | 对后续任务提供的合同 |
|---|---|---|
| 1–2 锁 | probe_manager/retry；library_lifecycle；library_organize | owned lookup后退役；lease acquire/drop均在gate外 |
| 3 文件意图 | library_write/rename_durable及新rename_disk模块 | 版本固定、磁盘证据、跨DB阶段恢复 |
| 4 改识别 | library_write/reassign | 只改文件归属，不清原观看事实 |
| 5 元数据身份 | metadata/adopt/overrides；media_operations/references | 合法身份/CAS/目标；同一entity/locale合成 |
| 6 路径 | roots；libraries；library_operations/roots | canonical组件范围、持久重归属 |
| 7 locale | library_operations/locale；metadata/nfo_input | 重算完成才ready；NFO导入locale不漂移 |
| 8 checker | reduce/card；library_operations/integrity | expected Option语义与双向孤儿校验 |
| 9 API选源 | http/playback/selection/session及其他同合同入口 | 显式坏源拒绝，省略才自动 |
| 10 Web | detail/player-page/api/playback | 显式偏好不跨逻辑目标传播 |
| 11 去旧 | Web libraries API、playback坐标合同 | 唯一DTO，无旧数组/假成功导出 |
| 12 优化 | items/page；metadata归约；protocol/facts | 等价seek、依赖范围明确、工作量可测 |

Task1/2/4/8/9可独立公开seam验收；Task3依Task2锁合同，Task5先于7/12元数据优化，Task6先于7组合操作，Task10依9，Task11与9共用一次坐标合同修改，Task12正确性前提依前述不变量。可按锁、持久操作、消费者拆执行批次，但不拆成各自放宽规范的子产品；若分计划，必须保留这些共享接口。共享schema/domain由集成者单独持有，避免交叉编辑。

### Task 1: Probe删除分支释放Store guard（S01）

**Files:** Modify `crates/api/src/probe_manager/retry.rs:42-79`；Create test `crates/api/tests/probe_retry_deleted_file.rs`。

**Interfaces:** Consumes现有 `ProbeManager::new(Arc<Mutex<Store>>)`、`Store::{claim_probe_stage,finish_probe_stage}`；Produces不变的 `dispatch_due(&self,now_ms:i64,limit:usize)->Result<usize,StoreError>`。无新公开API。

- [ ] **Step1 — 写行为回归：** absent UUID的Metadata阶段claim/finish为Failed，取真实deadline后dispatch；以独立子进程外部watchdog观测。再用具名一次性检查点安排claim后删除已发布文件；检查点不成为常驻生产逻辑。
  ```rust
  assert_eq!(dispatch_result.unwrap(), 0);
  assert!(store.try_lock_for(Duration::from_secs(1)).is_some());
  assert!(store.lock().get_probe_stage(&key).unwrap().is_none());
  ```
- [ ] **Step2 — RED：** `cargo test -p api --test probe_retry_deleted_file -- --nocapture`。旧实现由外部watchdog报告子进程未结束，不让整套测试无限等待。
- [ ] **Step3 — 实现：** 保持dispatch_one签名，`let loaded = { self.store.lock().file_coverage_by_id(file_id) };`后再match；所有退役在guard外。不用重入mutex或超时返回绕过死锁。
- [ ] **Step4 — GREEN/烟测：** 同命令通过；独立runner实际dispatch返回0、再读写Store成功；`cargo test -p api`。
- [ ] **Step5 — 文档/提交：** 关闭review S01并登记当前证据；显式stage本任务文件，`git commit -m "fix(probe): release store before retiring missing retries"`。

### Task 2: organize多库lease与publication锁序（S02）

**Files:** Modify `crates/api/src/http/library_organize.rs:156-211`、`crates/api/src/library_lifecycle.rs:231-265,315-325,372-377`（仅确有必要的锁合同）；Test `crates/api/tests/library_work_lifecycle.rs`、`crates/api/tests/management/library_organize_cross.rs`。

**Interfaces:** Consumes `LibraryWorkRegistry::{acquire,freeze}`、`LibraryWorkLease::{control,publish}`。Produces内部 `PreparedOrganizeEntry { from:PathBuf,to:PathBuf,destination_library_id:String }`；`apply_organize(state:&ApiState,library:&store::Library,entries:&[PreparedOrganizeEntry])->Response`。外层拥有 `Vec<LibraryWorkLease>`，不向gate闭包转移所有权。

- [ ] **Step1 — 写回归：** 先持源signal、并发freeze，观察removal持久标记后尝试目标acquire；子进程应正常结束或拒绝。另覆盖目标lease Drop与源freeze交错、实际HTTP跨库organize和源删除。
  ```rust
  assert!(organize_result.finished_before_deadline());
  assert!(freeze_result.finished_before_deadline());
  assert!(registry.acquire(&store, &source_id).is_err());
  assert!(intent_and_disk_are_consistent);
  ```
  finished_before_deadline代表测试channel的外部截止判断，不新增生产方法。
- [ ] **Step2 — RED：** `cargo test -p api --test library_work_lifecycle --test management organize -- --nocapture`；直接registry竞态case不使用过滤遗漏，单独按所加名称运行。旧实现超时而非5000ms后恢复。
- [ ] **Step3 — 实现：** 所有库lease先获取并由外层持有；gate只包必须原子防取消的短发布，磁盘IO释放Store；任何lease acquire/drop不能在gate内。freeze仍需阻止旧工作发布，不能只删除cancel锁等待。
- [ ] **Step4 — GREEN/烟测：** 上述两target及 `cargo test -p api`；临时真实Router并发organize/remove，确认响应、意图和字节一致，watchdog停止子进程。
- [ ] **Step5 — 文档/提交：** 在现有生命周期文档记录锁序与析构约束；`git commit -m "fix(api): order organize publication and library leases"`。

### Task 3: rename身份安全与跨DB恢复（R01/R11）

**Files:** Modify `crates/store/src/library_write/rename_durable.rs:59-301,498-528`、`crates/store/src/schema/v1/library.sql:267-288`、`crates/store/src/library_write/rename.rs`及现有调用者；Create `crates/store/src/library_write/rename_disk.rs`；Test `crates/store/tests/file_rename_durable.rs`、`crates/api/tests/source_token_rename.rs`。

**Interfaces:** Consumes Task2 gate/lease合同、`FileVersion`、共享 `library::observe_source`。Produces扩展 `FileRenameRequest`：`source_evidence:RenameSourceEvidence`、`from_config_revision:i64`、`to_config_revision:i64`；新类型 `RenameSourceEvidence { version:FileVersion,size_bytes:u64,device:u64,inode:u64 }`（原生stat不转成内容hash）；保留 `move_rename_bytes(&FileRenameRequest)->Result<(),StoreError>`、`complete_file_rename(&str,i64)->Result<(),StoreError>`并迁移全部构造点。持久phase=`planned|disk_applied|library_published|references_applied|failed`，失败需保留last_completed_phase，不丢恢复位置。

- [ ] **Step1 — 写回归：** begin(A)后原子替换B、源消失但目标为外来C、目标竞争占用、正常同FS及跨FS、library提交后subscribe触发器故障再重启。
  ```rust
  assert!(stale_disk_result.is_err());
  assert_eq!(read(&source), replacement_bytes);
  assert!(!destination.exists());
  assert!(reference_failure.is_err());
  assert!(pending_intent_after_failure);
  assert_eq!(references_after_restart, expected_new_paths);
  assert!(!pending_intent_after_success);
  ```
- [ ] **Step2 — RED：** `cargo test -p store --test file_rename_durable -- --nocapture`。旧实现移动B/删除intent后吞subscribe错误。
- [ ] **Step3 — 实现：** evidence在锁外观察、begin事务验证version/config；disk重新验证源且no-clobber，不信任任意已存在目标；恢复必须有已持久磁盘阶段证据。library提交只推进phase，不ACK；subscribe幂等路径收敛成功再ACK，失败结构化error且保持可重试。拆文件只按磁盘/事务责任；保持每函数≤120行。不得通过读时修复或跨DB假事务代替阶段。
- [ ] **Step4 — GREEN/烟测：** store target、`cargo test -p store`、`cargo test -p api --test source_token_rename`、`cargo test -p api`；实际runner故障注入与重启恢复，旧worker拒绝、新源按证据推进；跨FS未有设备时明确Not run，不用同FS新inode冒充跨FS性能。
- [ ] **Step5 — 文档/提交：** 更新library pending rename字典/故障阶段；`git commit -m "fix(storage): bind rename disk work and reference recovery to durable evidence"`。

### Task 4: 重新识别保留原观看事实（R03）

**Files:** Modify `crates/store/src/library_write/reassign.rs:186-214`；Test `crates/store/tests/media_reassign.rs:308-376`。

**Interfaces:** 保留 `reassign_files(&self,&ReassignFiles,i64)->Result<usize,StoreError>`。关闭相关session但原 `playback_units`整包不变；B不复制A。

- [ ] **Step1 — 替换错误断言：** 删除原position=0断言；保存改挂前完整raw恢复包、5000ms位置、120000ms时长、favorite/history，改挂一个文件后核对A/B及旧session。
  ```rust
  assert_eq!(after_a_resume_package, before_a_resume_package);
  assert_eq!(after_a.position_ms, 5_000);
  assert_eq!(after_a.duration_ms, Some(120_000));
  assert!(b_progress.is_none());
  assert_eq!(late_old_session_outcome, EventOutcome::IgnoredStale);
  ```
- [ ] **Step2 — RED：** `cargo test -p store --test media_reassign reassignment_ends_bound_sessions_and_keeps_the_history_binding`，旧实现得到0。
- [ ] **Step3 — 实现：** 删除清raw恢复包UPDATE；保留会话关闭和两侧摘要归约，旧源不再覆盖A时由正常选源返回不可用/source_changed，而非清事实。
- [ ] **Step4 — GREEN/烟测：** `cargo test -p store --test media_reassign`、`cargo test -p store`；实际重新识别Router回读A进度/收藏、B无继承。
- [ ] **Step5 — 文档/提交：** 明确“不可恢复位置不等于删除恢复事实”；`git commit -m "fix(store): preserve original progress when reidentifying files"`。

### Task 5: 元数据身份/CAS与merge合成（R04/R05）

**Files:** Modify `crates/store/src/metadata/{adopt,overrides,effective}.rs`、`crates/store/src/media_operations/references.rs`、`crates/api/src/metadata_service.rs`、`crates/api/src/scrape_metadata/series.rs`；Test `crates/store/tests/{metadata_adoption,media_merge_snapshots,media_merge_recovery}.rs`、`crates/api/tests/management/library_meta.rs`。

**Interfaces:** 保留 `adopt_metadata(&MetadataAdoption,i64)->Result<i64,StoreError>`、`set_metadata_overrides(MediaId,EntityScope,&str,&MetadataFields,i64)->Result<i64,StoreError>`。明确 `MetadataAdoption.expected_revision`是捕获的Media.metadata_revision，生产者必须填Some；source_revision仍是来源快照自身时钟。Consumes现有 `write_effective(&Connection,MediaId,EntityScope,&str)->Result<i64,StoreError>`，merge内部可在冻结状态调用；外部采用/人工写必须拒绝merging/retired。

- [ ] **Step1 — 写回归：** retired/merging写零改变；旧expected revision/旧refresh target网络迟到拒绝；A=p2/A、B=p3/B、选p3；work/season/episode、不同locale、目标无快照、显式空、重启重复advance。
  ```rust
  assert!(matches!(retired_write, Err(StoreError::Protected(_))));
  assert_eq!(facts_after_rejected_adoption, facts_before);
  assert_eq!(composed_title, "Adopted B");
  assert_eq!(persisted_title, composed_title);
  assert_eq!(card_title, composed_title);
  assert_eq!(after_restart_origins, committed_origins);
  assert!(explicit_empty.value().is_none());
  ```
- [ ] **Step2 — RED：** `cargo test -p store --test metadata_adoption --test media_merge_snapshots --test media_merge_recovery`；新断言目前retired写成功、落盘A/合成B。
- [ ] **Step3 — 实现：** 外部写事务先ensure_media_writable，再校验expected/current target与scope归属；调用方锁内捕获→锁外网络→CAS。merge在固定候选/人工锁决议后枚举变动entity/locale，调用write_effective再reduce；无目标快照保留规范pending fallback及真实provenance，不用常规survivor.or(loser)覆盖决议。
- [ ] **Step4 — GREEN/烟测：** store与api相关target、两个crate独立套件；真实merge/override Router拒退役ID、读取列表/详情同值；旧网络结果用可控HTTP fixture迟到。
- [ ] **Step5 — 文档/提交：** 更新metadata revision与merge字典；`git commit -m "fix(metadata): enforce identity CAS and recompose selected merge snapshots"`。

### Task 6: canonical根归属与有界重归属（R02）

**Files:** Modify `crates/store/src/roots.rs:130-213`、`crates/store/src/libraries.rs:397-455`、`crates/store/src/library_operations/roots.rs`、`crates/api/src/http/library_admin.rs`；Test `crates/store/tests/{path_key_normalization,library_reconfiguration}.rs`、`crates/api/tests/library_path_ownership.rs`。

**Interfaces:** Consumes `canonical_root_key(&str)->Result<String,StoreError>`及 `LibraryReconfiguration`。Produces一套内部canonical组件范围查询；display_path不参与key比较。创建空库可同步完成；会改变现有文件归属的新增根/库通过现有operation及config barrier处理，HTTP返回现有operation receipt而非提前ready。变更该receipt时同任务更新实际Web调用者。

- [ ] **Step1 — 写回归：** alias→real根的A先有nested文件，新增alias/nested B；重启推进operation、嵌套不同kind、未改变字节及原观看事实。
  ```rust
  assert_eq!(persisted_file.library_id.to_string(), strict_owner.id);
  assert_eq!(strict_owner.id, nested_library.id);
  assert_eq!(a_card, None);
  assert_eq!(b_card.file_count, 1);
  assert_eq!(file_bytes_after, file_bytes_before);
  ```
- [ ] **Step2 — RED：** `cargo test -p store --test path_key_normalization nested_symlink_root_reassigns_existing_files`（新增具名用例），旧归属仍A。
- [ ] **Step3 — 实现：** 使用已保存root.path_key计算组件边界范围，索引keyset只取影响文件；事务维护files/coverage/NFO/双方摘要。持久phase/lease阻止旧scope发布，按每批≤100，禁止同kind全量Vec扫描；不增加display-key双读。
- [ ] **Step4 — GREEN/烟测：** 两store targets与crate、api ownership target与crate；实际新增嵌套库到完成并重启，列表/详情/严格解析归属一致。
- [ ] **Step5 — 文档/提交：** 更新创建/新增根operation receipt及字典；`git commit -m "fix(storage): reassign nested roots using canonical bounded ownership"`。

### Task 7: locale重算与NFO语言绑定（R06）

**Files:** Modify `crates/store/src/library_operations/{locale,mod}.rs`、`crates/store/src/metadata/nfo_input/{reconcile,record}.rs`；Test `crates/store/tests/library_reconfiguration.rs`、`crates/api/tests/metadata_priority.rs`。

**Interfaces:** Consumes Task5有效合成、Task6 operation；保留 `begin_library_reconfiguration`/`advance_library_operation`。新增内部cursor `last_locale_media_id:Option<String>`；`apply_locale`按当前batch推进并返回既有BatchOutcome，不允许仅改配置返回Done。reconcile plan携已登记sidecar的locale映射，首次无语言才采用当前display locale。

- [ ] **Step1 — 写回归：** zh标题中文/en标题English，切en并完成后卡片详情同English；无语言NFO初次zh导入，切en→同字节重扫、IO unknown/invalid、重启，仍保zh输入；全新NFO采用en。
  ```rust
  assert_eq!(completed_card.title, "English");
  assert_eq!(detail.metadata.title.value().unwrap(), "English");
  assert_eq!(same_sidecar_locale_after_rescan, "zh-CN");
  assert_eq!(new_sidecar_locale, "en-US");
  assert_eq!(input_after_unknown_io, last_valid_input);
  ```
- [ ] **Step2 — RED：** `cargo test -p store --test library_reconfiguration`；卡片旧中文。NFO fixture必须先断言初次输入真实存在再进入重扫，不能把父审查临时runner的QueryReturnedNoRows当已完成此case。
- [ ] **Step3 — 实现：** 按Media keyset重合成display及terms；reindexing期间不返回混合成功页，完成校验后ready。record删除旧locale规则只用于明确实体/语言变更，不以当前配置替代导入语言。
- [ ] **Step4 — GREEN/烟测：** store/api定向与独立套件；Router切语言、浏览、重扫、重启、清cache后同值。
- [ ] **Step5 — 文档/提交：** 更新locale phase及导入规则；`git commit -m "fix(storage): complete locale rebuild without relabelling imported NFO"`。

### Task 8: checker反向用户孤儿（R07）

**Files:** Modify `crates/store/src/library_write/reduce/card.rs:56-86`、必要时 `library_operations/integrity.rs:302-328`；Test `crates/store/tests/library_integrity_sparse.rs`。

**Interfaces:** 保留 `refresh_affected_user_summaries(&Connection,LibraryId,MediaId)->Result<(),StoreError>`；候选用户=`raw media users UNION current library/media summary users`；`write_library_summary`由facts算Option为空删除，不把旧值作输入。

- [ ] **Step1 — 写回归：** favorite建立摘要后外部DELETE raw用户行；check→repair→check；同时已有跨库不相交季case不误报。
  ```rust
  assert_eq!(before.totals.orphan_user_items, 1);
  assert!(after_summary.is_none());
  assert!(!recheck.has_issues());
  assert_eq!(unrelated_facts_after, unrelated_facts_before);
  ```
- [ ] **Step2 — RED：** `cargo test -p store --test library_integrity_sparse orphan_summary_without_raw_user_is_repaired`，旧check假clean。
- [ ] **Step3 — 实现：** UNION使用现有playback_media与user_items_media索引；只当前Library/Media的额外derived用户，合法稀疏默认不补行，cap/totals保持。
- [ ] **Step4 — GREEN/烟测：** sparse及report-cap targets、`cargo test -p store`；真实Store注入孤儿后公开checker可发现并稳定修复。
- [ ] **Step5 — 文档/提交：** 记录双向校验边界；`git commit -m "fix(store): repair summaries with no remaining playback facts"`。

### Task 9: 自有播放请求严格选源（R08，R12坐标）

**Files:** Modify `crates/api/src/http/playback/{selection,session}.rs`、同合同 `crates/api/src/http/playback.rs`/heartbeat输入；Create内部 `crates/api/src/http/playback/request.rs`；Test `crates/api/tests/storage_http_cutover.rs`、`crates/api/tests/management/playback_review.rs`。

**Interfaces:** 新内部 `PlaybackSelection { media_item_id:MediaId,season_number:Option<u32>,episode_number:Option<u32>,file_id:Option<LedgerId>,start_ms:Option<i64> }`；`parse_selection(&Value)->Result<PlaybackSelection,SelectionError>`，错误enum=`Invalid|Unavailable`；`resolve_playback_for(&Store,&PlaybackSelection,UserId)->Result<(LibraryFile,Media,ResolvedResume),SelectionError>`。只解析共享选择字段，保留既有device/codec等session参数，不用全body deny_unknown误拒合法字段；显式拒绝season/episode旧键。

- [ ] **Step1 — 写回归：** B可播、显式A坏类型/UUID/不存在/missing/pending/无权/错unit各拒绝；无file_id可选B，TV S00E00成功，movie显式坐标及旧键400。拒绝前后session表不变。
  ```rust
  assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
  assert_eq!(unavailable.status(), StatusCode::NOT_FOUND);
  assert_eq!(sessions_after_rejection, sessions_before);
  assert_eq!(automatic["file_id"], b_id);
  assert_eq!(obsolete_coordinate.status(), StatusCode::BAD_REQUEST);
  ```
- [ ] **Step2 — RED：** `cargo test -p api --test storage_http_cutover`，新case旧实现200换源；更新已有旧坐标依赖fixture为新合同。
- [ ] **Step3 — 实现：** 省略/null与显式无效分离，显式源完整校验授权/发布/在位/覆盖；无权与不存在同错误，不泄漏。所有自有选择入口用同解析；不改Jellyfin标准参数。
- [ ] **Step4 — GREEN/烟测：** cutover/management targets及api独立套件；真实Router错误响应无新session，自动与显式有效选择正确。
- [ ] **Step5 — 文档/提交：** 更新self API合同，`git commit -m "fix(api): reject invalid explicit playback sources and obsolete coordinates"`。

### Task 10: Web逻辑目标与显式源偏好分离（R09/R10）

**Files:** Modify `web/components/player/player-page.tsx:117-179`、`web/components/library-item-detail-view.tsx:571-574,955-964`、`web/lib/api/playback.ts:870-895`及对应resume handler；Create行为测试 `web/test/player-source-transitions.test.mjs`；Extend `crates/api/tests/storage_http_cutover.rs`。

**Interfaces:** Consumes Task9唯一请求。活动 `PlaybackUnit`包含当前逻辑坐标和可选file_id；初始化可携URL显式源，next/prev/autoplay整体替换成目标unit。保持displayFile单独；file_id只来自当前unit有效的显式selectedTrackFileId。resume读返回源感知 `{ file_id:string,start_ms:number,duration_ms:number|null,resume_status:"compatible"|"source_changed"|"no_progress"|"already_played" }`，与sessions共用Store::resolve_playback，不写session；更新全部真实resume消费者。

- [ ] **Step1 — 写回归：** 实际组件交互或项目现有可执行UI harness：E01/A→Next E02/B→Prev；合并A:E01/E02→E03/B；A在列表首、B进度3600000，未选版本Continue与显式选A分别检查请求/起点。
  ```js
  assert.equal(nextSessionRequest.file_id, undefined);
  assert.equal(prevSessionRequest.file_id, undefined);
  assert.equal(untouchedContinue.file_id, undefined);
  assert.equal(autoResume.file_id, sourceB);
  assert.equal(autoResume.start_ms, 3600000);
  assert.equal(explicitA.resume_status, "source_changed");
  assert.equal(explicitA.start_ms, 0);
  ```
- [ ] **Step2 — RED：** `cd web && node --test test/player-source-transitions.test.mjs`；必须运行真实状态/事件而非assert源码或只测playHref转发。
- [ ] **Step3 — 实现：** 一份active target状态，切集清物理偏好；展示默认不当显式选择；resume请求携同一optional preference并由canonical resolver解释，不从raw进度推不同源位置。
- [ ] **Step4 — GREEN/实际UI：** Web测试/typecheck/build及api相关回归；临时服务浏览器按Next/Prev/自动下一集/Continue/显式换版本，观察请求file_id、实际session源与位置，关闭tab。未做实际surface不能写完成。
- [ ] **Step5 — 文档/提交：** self API更新只读resume字段，`git commit -m "fix(web): scope physical source preferences to the active playback target"`。

### Task 11: 删除剩余自有兼容层（R12）

**Files:** Modify `web/lib/api/libraries.ts:1944-2024,2119-2132`及专属旧DTO；Test `web/test/library-dto-contract.test.mjs`；必要时 `docs/api-contracts/self.md`。Task9已拥有坐标删除，勿重复。

**Interfaces:** `refreshItemChapters(...)->Promise<RefreshedItemChapters>`只接收canonical对象；保留真正使用的新 `listLibraryDuplicates/deleteDuplicateFile`。删除 `listTrashedFiles,purgeTrashedFiles,restoreTrashedFiles,listDuplicateFiles,startDuplicateScan,resolveDuplicates,resolveAllDuplicates`及仅为它们存在的类型/常量，不提供别名re-export。

- [ ] **Step1 — 写回归：** 实际API adapter输入canonical chapter对象保留其queue/chapters字段；旧数组返回明确合同错误，不变形成成功对象。所有现存UI只走新duplicates调用。
  ```js
  assert.deepEqual(canonicalResult, canonicalPayload);
  await assert.rejects(oldArrayResponse, /contract|schema/i);
  ```
- [ ] **Step2 — RED：** `cd web && node --test test/library-dto-contract.test.mjs`；旧实现接受数组。
- [ ] **Step3 — 实现：** 统一DTO并删除假成功导出；先查真实引用，迁移仍在使用者后清除，不把预先未使用代码称运行时回归。另为fonts/trickplay现存consumer逐项核对真实后端能力：只保留有真实source-bound API的调用与显式URL；无端点的假API及猜数字URL分支删除，并准确显示能力缺失，不在本计划偷偷新增字体抽取/缩略图子系统。
- [ ] **Step4 — GREEN/烟测：** Web test/typecheck/build；实际章节刷新/重复文件管理UI请求新接口、旧数组拒绝。读取部署路由确认无声称存在却实际404的asset API；静态图片public不受影响。
- [ ] **Step5 — 文档/提交：** 删旧合同和能力假描述；`git commit -m "refactor(web)!: remove obsolete library API compatibility adapters"`。

### Task 12: 等价SQL与依赖范围优化（O01–O03）

**Files:** Modify `crates/store/src/library_query/items/page.rs:108-131`；Test `crates/store/tests/storage_work_bounds/seek_scale.rs`；测量后按证据 Modify `metadata/{adopt,overrides}.rs`、`library_write/reduce/card.rs`、`playback/user_summary/mod.rs`、`library_query/protocol/facts.rs`与对应tests。如拆helper Create `library_write/reduce/display.rs`。

**Interfaces:** 保留 `library_item_page_with_stats(&LibraryQuery)->Result<(ItemPage,QueryWorkStats),StoreError>`及seek_clause签名。同向spec用tuple，混合方向保留经验证的lexicographic形式。元数据优化内部 `reduce_media_display(&Connection,MediaId)->Result<usize,StoreError>`只更新work展示/terms，不触coverage/users；season/episode按真实依赖处理。心跳最小membership换HashSet；任何增量算法仍必须保留season/whole断言与同源恢复合同。

- [ ] **Step1 — 写工作量回归：** 同库100k卡片、page50，第50000项后深cursor，重复title与NULL映射、ASC/DESC、watch tier混合方向；测公开Store stats，不只FullscanStep。
  ```rust
  assert_eq!(deep.items[0].title, "Movie 050000");
  assert_eq!(deep.items.len(), 50);
  assert!(deep_stats.vm_steps <= first_stats.vm_steps * 2);
  assert_eq!(before_ids, after_ids);
  ```
- [ ] **Step2 — RED：** `cargo test -p store --release --test storage_work_bounds -- --nocapture`；新增同库深seek case应在旧实现351645 vs首屏1538失败。现有无关Library增长case不足以证明这个条件。
- [ ] **Step3 — 实现tuple：** 相同方向 `(sort_expr,media_id) >/< (?,?)`，复用现有索引；保持当前过滤、授权、NULL映射、tie-break及cursor编码，不额外夹带游标协议重设计。混合watch-tier排序保留等价且经测量的lexicographic形式。
- [ ] **Step4 — GREEN/等价测量：** 相同projection/join/51行输出比较、EXPLAIN/VmStep/Sort与公开Store；期望完整复合范围SEARCH，深度工作≤首屏2倍；精确count/facets/混合排序单独报成本，不套该阈值。
- [ ] **Step5 — 度量其余热点：** release下episode元数据采用100/1000、当前用户同Media facts100/10000、PersonIds/genre过滤增长scope。记录SQL/解码行字节、VM、写集合、事务时长、WAL、吞吐/延迟；先保存基线。只改确认冗余部分：去重复episode采用、work展示归约不重建coverage/users、HashSet membership、有效locale/NFO协议term写时维护和LIMIT前索引筛选。不改变人物/流派匹配或权限，不增加第二全量投影；未证实收益则记录不改。
- [ ] **Step6 — 完整验证/提交：** `cargo test -p store`及涉及api独立套件、稳定树 `cargo test --workspace`；Web变更执行项目三门及Task10实际浏览器。运行默认10k扫描+心跳，不缩fixture；100k/1M规模和真实NAS未跑明确记Not run。更新现有benchmark及本轮review状态，`git commit -m "perf(storage): bound deep cursor work and narrow metadata reductions"`。

## 自审与交付门

- 覆盖：S01→1，S02→2，R01/R11→3，R03→4，R04/R05→5，R02→6，R06→7，R07→8，R08→9，R09/R10→10，R12→9/11，O01–03→12。Review Focus五项均有所属回归。
- 类型一致：保留FileVersion/MetadataAdoption/LibraryReconfiguration；新增类型限定所属模块，无旧API别名。测试代码块是具名行为场景的断言摘录，fixture变量来自该任务场景，不声称可独立复制编译。
- 字节/DB跨阶段安全、合法稀疏摘要、元数据snapshot/pending fallback和真实Web行为分别验收，不能用其中一种绿测替代其余。
- 工作量测量只证明具体fixture，未授权删功能达标；当前200倍仅SQL VM差，不写API速度承诺。
- 完成任务即时更新现有文档/状态，按文件显式stage并小提交；最终复核用户四项要求及全链路，不只通过编译。
- **执行暂停点：** 本轮保存review/计划供人工审阅，不实施任何修复。若随后获准，推荐Subagent-driven：12个任务存在持久恢复/身份/消费者接口耦合，独立审查可在进入下一依赖前拦截事实破坏。确认后才加载相应执行技能。
