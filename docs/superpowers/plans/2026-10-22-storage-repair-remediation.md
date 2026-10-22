# 存储修复生产收口 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修复 `1845dab` 复审的25组Spec缺陷及4项工程约束问题，以实际生产入口回归证明，不再用未接线Store方法或替代Provider的绿测关闭问题。

**Architecture:** 保留五文件和领域边界；文件观察、外部响应、磁盘写入在全局Store锁及SQLite事务之外，持久事实以精确FileVersion/config CAS发布。Playback绑定签发目标和物理恢复包；元数据采用、NFO、SQL筛选与展示共享明确的实体/locale/来源决议。根变更、checker、merge和User删除保留明确可恢复阶段，不引入通用event bus或兼容入口。

**Tech Stack:** 现有Rust 2024、rusqlite 0.37 bundled SQLite、Axum 0.8、serde/tracing及Vite/React/TypeScript；优先现有依赖与fixture，不预设连接池或新服务。

**Spec:** [主规范](../specs/2026-10-18-library-storage-redesign.md)、[字段与用例合同](2026-10-18-library-storage-contracts.md)。[本轮复审](../../audits/2026-10-22-storage-repair-review.md)定义R01–R25/S01–S04；[历史修复记录](../../audits/2026-10-22-storage-cutover-execution-history.md)和[benchmark](../../benchmarks/2026-10-18-library-storage.md)只证明各自版本。原20任务可用 `git show 1845dab:docs/superpowers/plans/2026-10-20-storage-clean-cutover.md` 取回，不能机械重做已完成部分。

## Global Constraints

- 无旧数据导入、兼容视图、双读双写、旧ID/DTO/明文密码/Filter DSL fallback。movie/video请求省略季集、内部whole=(-1,-1)；TV S00E00合法，未知TV覆盖为空，不合成whole。
- MediaSourceId=`f1_<file_uuid_simple>_<revision>`，ItemId=`w1_`/`s1_`/`u1_`；裸文件UUID不是逻辑ItemId。UUID公开DTO为string。
- 文件/覆盖/冷事实/卡片/用户摘要在同一library.db短事务发布；外部响应、文件stat/NFO/artwork、解析及磁盘sync/rename均锁外。0是真实config_revision，不是“未指定”。
- checker每批最多100 Media，单Media内部有界游标；扫描发布最多100文件/事务。不得先全量取出再take，精确count允许成本但须如实测量。
- activity/favorite/last_played保留秒，played_changed_at_ms/事件时钟为毫秒，文件stat为纳秒；独立绑定，禁止混用。
- 静态图片public；视频按协议鉴权。保留真正无session客户端：明确的新Start/Playing可切换目标并原子归档旧会话；无法证明新开始的Progress/Stop遇冲突活跃绑定必须零写。首个无ID Progress可只写unit、不造session/log；不能把合法协议当旧数据兼容删掉。
- 每任务RED先在实际公共消费seam取得行为失败；Store单元测试不能替代Router、真实Catalog实现、实际TS adapter。只fake HTTP/Downloader/Probe等外部边界。
- 每触及crate独立 `cargo test -p <crate>`；稳定树最后一次workspace。前端三门依现有package脚本；既有lint失败不得改口为通过。任何临时门禁具名skip必须披露，不宣称全绿。
- `.rs`≤800行、生产/测试函数≤120行；按职责拆，不用删注释凑长度。lib.rs保持模块树/导出。
- 日志：生命周期info，降级warn，失败error完整上下文；禁token/签名STRM URL。恢复失败不能吞掉后返回成功。
- 不访问用户真实DB/媒体，不自动执行cache重建或实例切换。真机/NAS/浏览器未跑写Not run。

## Review Focus

1. E01/E02同物理源：有ID、无ID、旧Stop、不同device；任何冲突拒绝均要求单元/会话/日志零写。
2. SourceChanged响应后发送真实Web Start形状；旧位置/时长/轨道不得重新绑新源。同源轨道刷新只清轨道，保时间轴。
3. pending A重启生成新candidate UUID，恢复返回旧ID，发布A后B继续；每个磁盘/DB登记边界可重启。
4. 已有卡片同批新增同一unit多个版本；增量/全量结果完全一致。配置0→1后旧观察拒绝。
5. NFO provider V1→V2、只改plot、无效XML、删除、locale切换、多Library；checker/merge重算不得偷换来源或恢复过期值。

## 文件责任与依赖

| 责任 | 修改/新增边界 | 依赖 |
|---|---|---|
| 源观察/发布 | `library/src/source_observation.rs`；`store/src/library_write/{publish,source}.rs`；API scan/Probe/claim/Transfer | Task1为共享版本合同 |
| 摘要与Transfer | `reduce_incremental/units.rs`、`worker/transfer/recorder.rs`、claim入口 | Task2/3依Task1 |
| Playback | store event/assertions/mutate，API heartbeat/provider stream，media-server PlaybackInfo | Task4→5 |
| Web文件DTO | libraries.ts、library-file-pages.ts、真实模块测试 | Task6可独立 |
| 元数据/NFO | Catalog实现、adopt/compose/overrides、NFO export/driver与所有Scrape入口 | Task7→8 |
| 根/checker | library_operations roots/locale/integrity、API storage_operations | Task9依Task1/7/8 |
| merge/用户删除 | media_operations、user_removal、typed Job引用 | Task10/11依前述事实合同 |
| 协议查询 | library_query/protocol、selection/item_metadata与协议路由 | Task12依Task5/7 |
| 整体验收 | 现有端到端fixture、运行脚本、文档 | Task13最后 |

共享domain/schema/HTTP router由集成者单独持有；并行工作不得同文件交叉编辑。新增内部helper仅在任务注明模块，不建立第二套公开模型。

### Task 1: 真实源观察接生产并删除revision零通配（R01/R02，S01）

**Files:** Modify `crates/library/src/source_observation.rs`、`crates/store/src/library_write/{publish,source}.rs`、`crates/store/src/{scan_seen,scan_probe_obligations}.rs`、`crates/api/src/{watch_ledger,fingerprint_job/source,probe_manager/probe}.rs`、`crates/api/src/http/library_scan/runner/{walk,checkpoint}.rs`；Test `crates/api/tests/storage_write_runtime.rs`、`crates/store/tests/library_reconfiguration.rs`、`probe_publication.rs`。

**Interfaces:** 保留 `library::observe_source(&Path)->Result<SourceObservation,LibraryError>`、`Store::replace_file_source(&WriteScope,LedgerId,&FileVersion,&SourceObservation,i64)->Result<FileVersion,StoreError>`及FileVersion。为现有扫描发布参数增加owned artwork与source observation，不在Store discover；修改所有构造点而非加可选兼容默认。先捕获归属/version/config，释放锁观察，再以相同scope发布。相同观察幂等；STRM源hash统一包含内容digest。

- [ ] **Step 1 — 写回归：** 在PresenceFixture真实POST scan中扫描STRM A→同stat B；在Store配置0→1后提交非空旧写。断言：
  ```rust
  assert_eq!(after.revision, before.revision + 1);
  assert_ne!(after.source_version, before.source_version);
  assert!(!old_probe_published);
  assert!(matches!(stale_zero_write, Err(StoreError::Protected(_))));
  assert_eq!(presence_after_refusal, presence_before);
  ```
- [ ] **Step 2 — RED：** `cargo test -p api --test storage_write_runtime actual_rescan_replaces_physical_source`；`cargo test -p store --test library_reconfiguration initial_zero_scope_is_rejected_after_reconfiguration`。当前分别版本不动、旧写成功，应行为失败而非只编译失败。
- [ ] **Step 3 — 实现：** 真实walk/checkpoint/watch调用canonical observation/CAS，删除clear/stat伪版本及Probe `source_version=="v1"`例外。`ensure_config_revision`始终比较。claim/Transfer/Probe/fingerprint携真实版本，零不再通配；将IO从Store mutex和事务移到API观察阶段，所有old worker拒绝无写。
- [ ] **Step 4 — GREEN/烟测：** 跑上述target及library/store/api独立套件。临时TCP实例扫描A→B→重启，旧session/Probe拒绝、新Probe成功；用受控阻塞sidecar/外部fake闸门证明另一Library心跳仍可执行，释放闸门必须在teardown。
- [ ] **Step 5 — 文档/提交：** 按生产烟测更新library字典和R01/R02状态；`git commit -m "fix(storage): connect observed source CAS to production writers"`。

### Task 2: 已有卡片多版本增量保持守恒（R03）

**Files:** Modify `crates/store/src/library_write/publish.rs`、`reduce_incremental/{mod,units}.rs`；Test `crates/store/tests/{file_publication,storage_invariants}.rs`。

**Interfaces:** 保留 `publish_files(&WriteScope,&[FilePublication],i64)->Result<PublishReport,StoreError>`。内部按受影响unit键去重，一次从本批最终事实更新该unit；不得先最终聚合后继续加本批delta。卡片总数按文件一次、episode按unit去重。

- [ ] **Step 1 — 写回归：** E01先建卡；同批E02 A/B、重叠多集、mixed更新/新增/重复请求，回读unit page再完整归约对照。
  ```rust
  assert_eq!((e02.file_count,e02.available_file_count),(2,2));
  assert_eq!(incremental_page, full_reduce_page);
  assert_eq!(retry_page, incremental_page);
  ```
- [ ] **Step 2 — RED：** `cargo test -p store --test file_publication existing_card_batch_two_versions_of_new_episode`，当前(3,3)≠(2,2)。
- [ ] **Step 3 — 实现：** 将unit聚合与文件卡片delta明确分开，避免新unit初始化读最终集合后重复加；涉及prefer file和删除/存在变化一并使用同最终事实，不退回每file全Media归约。
- [ ] **Step 4 — GREEN/测量：** `cargo test -p store --test file_publication --test storage_invariants`、`cargo test -p store`；再跑 `cargo test -p api --release --test storage_contention`，保留10k工作量、正确计数和原性能数字对照，不缩小fixture。
- [ ] **Step 5 — 文档/提交：** benchmark注明9aaf032回归和新对照；`git commit -m "fix(store): reduce shared new units once per publication batch"`。

### Task 3: Transfer恢复身份与真实claim覆盖（R04/R05）

**Files:** Modify `crates/api/src/worker/transfer/{recorder,run,tests}.rs`、`crates/api/src/{claim,http/library_admin}.rs`、必要时`crates/subscribe/src/collect/transfer.rs`；Test `crates/api/tests/{storage_transfer_recovery,storage_write_runtime}.rs`。

**Interfaces:** `TransferRecorder::prepare`返回的FileCoverage是权威reservation；reserved集合插入返回`file.file_id`，后续发布使用返回version。`ClaimedFacts`构造器显式接收MediaKind和解析Release；unknown TV返回空units、movie/video返回WHOLE，禁止默认whole跨kind。

- [ ] **Step 1 — 写回归：** 真PlannedPublicationRecorder重启fresh UUID恢复A→published A→prepare B；真实两个claim route覆盖E01-E02、未知TV、TV00。
  ```rust
  assert_eq!(recovered.file.file_id, durable_a_id);
  assert_ne!(recovered.file.file_id, fresh_candidate_id);
  assert!(prepare_b.is_ok());
  assert_eq!(named_tv_units, vec![UnitKey::new(1,1),UnitKey::new(1,2)]);
  assert!(unknown_tv_units.is_empty());
  ```
- [ ] **Step 2 — RED：** `cargo test -p api --lib restarted_pending_a_with_fresh_candidate_does_not_block_b`；`cargo test -p api --test storage_write_runtime actual_claim_routes_preserve_tv_coverage`，当前B冲突/覆盖错误。
- [ ] **Step 3 — 实现：** 使用returned identity自写排除，路径/源/mode冲突仍拒绝；两claim入口解析实际文件Release并统一覆盖策略，不能由管理入口静默unidentified whole。用Task1真实scope及owned观察发布。
- [ ] **Step 4 — GREEN/烟测：** api/subscribe/store独立套件；临时Downloader fixture真实Transfer两文件，A持久pending后重启并完成A/B；HTTP认领回读两集可播、未知行展示不可选。
- [ ] **Step 5 — 文档/提交：** 更新对应复审状态；`git commit -m "fix(transfer): preserve recovered reservations and claimed TV coverage"`。

### Task 4: Jellyfin流与无ID事件绑定（R06/R07）

**Files:** Modify `crates/api/src/{media_server_provider,playback_events}.rs`、`media_server_provider/playback.rs`、`http/playback/heartbeat.rs`、`crates/media-server/src/routes/media/playback.rs`；Test `crates/api/tests/{review_remediation_playback,playback_event_identity}.rs`及其`merged_session.rs`。

**Interfaces:** 复用`PhysicalSourceId` encoder/parser、`issue_play_session`及issued binding；`resolve_stream_source_bound(user,id,source_id,session_id,device_id)`严格消费绑定，不新增第二session注册表。无IDStart/Playing可签发新binding并按Task5归档旧目标；无IDProgress/Stop仅同活跃逻辑/物理绑定可复用，无活跃绑定的首Progress保留只写unit、不造session/log的合同，冲突目标零写。

- [ ] **Step 1 — 写回归：** 真实ApiServerProvider PlaybackInfo→f1→Range；替换源/关闭sid/不同unit返回404、绑定device不符403，均不返回媒体字节或STRM重定向。E02活跃无ID E01 progress/stop零写；首无IDProgress只写unit；同目标复用；无IDStart A→B成功归档A、新建B，Web与Jellyfin均有正控制。
  ```rust
  assert_eq!(advertised_source, physical_source.to_wire_string());
  assert_eq!(stale_stream.status(),StatusCode::NOT_FOUND);
  assert_eq!(wrong_device_stream.status(),StatusCode::FORBIDDEN);
  assert!(stale_stream.headers().get("location").is_none());
  assert_ne!(stale_body.as_ref(),replacement_media_bytes);
  assert_eq!(facts_after_conflicting_no_id, facts_before);
  assert_eq!(valid_b_range_bytes, b"BBBB");
  ```
- [ ] **Step 2 — RED：** `cargo test -p api --test review_remediation_playback production_jellyfin_stream_uses_issued_binding`、`cargo test -p api --test playback_event_identity merged_no_id_other_unit_is_ignored`，当前206 XXXX、E01被写。
- [ ] **Step 3 — 实现：** Advertise/parse f1，不收裸UUID旧shape；显式sid检查user/device/logical unit/file revision/source/config/ACL及revocation，不暗选默认。无IDProgress/Stop先比完整活跃目标，同file不同unit冲突不得进入unbound writer；无IDStart/Playing明确签发新会话并按Task5原子归档前会话，不受迟到事件拒写规则阻断。
- [ ] **Step 4 — GREEN/烟测：** api/media-server/playback/store独立套件；临时TCP实际PlaybackInfo与Range、替换源和跨集迟到，保存状态码/字节/零写事实；不能用自定义StreamProvider替代被测provider。
- [ ] **Step 5 — 文档/提交：** 更新ADR0005实际绑定边界；`git commit -m "fix(playback): enforce issued bindings on protocol streams and events"`。

### Task 5: 恢复包、会话切换、时间和played统一（R08–R13）

**Files:** Modify `crates/store/src/playback/{event,mutate,assertions,units,sessions,session_ids}.rs`、`user_summary/mod.rs`、`crates/api/src/http/playback{.rs,/heartbeat.rs,/selection.rs}`、`http/playback_views/up_next.rs`、`media_server_provider/{playback,user_marks,items}.rs`；Test `crates/api/tests/{playback_watch_time,review_remediation_playback,playback_event_identity}.rs`、`crates/store/tests/{playback_resume_v1,playback_assertions}.rs`。

**Interfaces:** 保留BoundPlaybackEvent/PlaybackMutation/resolve_playback；新内部公共于同crate的断言决议输入为`Option<PlayedAssertion>`三scope，按`(changed_at_ms,specificity)`max，SQL必须同序。时间轴匹配file/version/source/timeline；轨道匹配另看tracks_version。Start从已解析issued source恢复决议取position/duration，切源时物理包整包替换。

- [ ] **Step 1 — 写回归：** A播放10s→B start，B计时0且A日志一次；假时钟1700000010000→activity1700000010；90min@60min→45min源的真实无position Start=0/2700000；fresh Jellyfin probe120000完成；season false@2000压过episode true@1000，tie episode胜；movie00拒绝、TV00成功。
  ```rust
  assert_eq!(b_session.watched_ms,0);
  assert_eq!(unit.updated_at,1_700_000_010);
  assert_eq!((new_resume.position_ms,new_resume.duration_ms),(0,Some(2_700_000)));
  assert_eq!((fresh.played,fresh.play_count,fresh.duration_ms),(true,1,Some(120_000)));
  assert!(!effective_episode.played);
  assert_eq!(same_source_track_refresh.start_ms,300_000);
  ```
- [ ] **Step 2 — RED：** `cargo test -p api --test playback_watch_time`、`cargo test -p api --test review_remediation_playback`及新增`playback_read_regression`真实Start case；`cargo test -p store --test playback_assertions --test playback_resume_v1`。更换固化错误轨道重置/零坐标兼容的断言，不只新增旁路case。
- [ ] **Step 3 — 实现：** event同事务归档previous自己的binding后新session；仅同sid/generation承接计时，更新start列。seconds/ms各自参数。duration来自精确源probe，旧用户值仅同源有效。Start不得raw unit回填；清旧物理轨道，保持逻辑played/favorite。所有公开mark/read切统一时间决议，不推进观看活动；删movie00 aliases和up-next fallback。
- [ ] **Step 4 — GREEN/烟测：** store/api/media-server独立套件；实际Web无字段Start、Jellyfin首次完整观看、切媒体和season mark三协议读回同值；历史清除since晚于事件不得删该事件前状态。浏览器回归需真实surface，未跑不关闭。
- [ ] **Step 5 — 文档/提交：** 更新时间/断言/恢复字段合同；`git commit -m "fix(playback): preserve source resume and session accounting invariants"`。

### Task 6: 实际Web文件adapter干净DTO（R14）

**Files:** Modify `web/lib/api/libraries.ts`、`web/lib/library-file-pages.ts`、必要时`web/components/library-item-detail-view.tsx`；Test `web/test/library-file-pagination.test.mjs`。

**Interfaces:** 首详情`next_file_cursor`，续页`next_cursor`；`LibraryItemFile`必须完整保留units/presence/publication。`isPlayableFile`仅published+present；缺字段按合同错误/不可选处理，不以旧响应兼容fail-open。

- [ ] **Step 1 — 写回归：** 用library-operations既有transpile loader模式加载实际libraries.ts，并注入真实collector，仅HTTP request fake。50/100/101/201、重复cursor、完整多集/unknown/pending。
  ```javascript
  assert.equal(detail.files.length,101);
  assert.equal(requests.length,3);
  assert.deepEqual(detail.files[0].units,[{season:1,episode:1},{season:1,episode:2}]);
  assert.equal(isPlayableFile(detail.files[0]),false);
  ```
- [ ] **Step 2 — RED：** `node --test web/test/library-file-pagination.test.mjs`，新增真实adapter case当前100≠101/状态undefined。
- [ ] **Step 3 — 实现：** 续页读next_cursor，fileFrom完整映射，移除对缺字段的旧兼容。EOF由cursor而非猜页长，不恢复已修exact50额外空请求。
- [ ] **Step 4 — GREEN/页面：** `npm --prefix web run typecheck`、`npm --prefix web run test`、`npm --prefix web run build`；浏览器实际101文件最后版本/合并集、missing不可选并关闭tab。
- [ ] **Step 5 — 文档/提交：** DTO及复审R14更新；`git commit -m "fix(web): preserve complete file pages and availability state"`。

### Task 7: 生产完整响应采用与统一有效字段（R15/R16）

**Files:** Modify `crates/api/src/{catalog,metadata_service,catalog_refresh,auto_resolve}.rs`、生产fanout/source Catalog实现、`scrape_metadata/{work,series}.rs`、`http/{library_scan,library_organize,media}.rs`；`crates/store/src/metadata/{adopt,compose,overrides}.rs`、`library_query/details/{mod,metadata,episodes}.rs`及`library_write/reduce`实际元数据模块。Test `crates/api/tests/metadata_priority.rs`、`crates/store/tests/metadata_adoption.rs`、`crates/media/tests/normalized_metadata.rs`。

**Interfaces:** 实现现有`Catalog::normalized_metadata(kind,id,scope,&MetadataPreferences)->Result<Option<NormalizedMetadata>,String>`各真实来源与fanout分发，从validated原响应解析。有效合成输出沿用ComposedMetadata，不新建ItemMeta中转。共享内部`compose(entity,locale,current_snapshots,overrides,nfo,base)`按同scope/locale逐字段决定值与origin；snapshot成功Missing退出旧值，失败不替换快照。

- [ ] **Step 1 — 写回归：** 真TmdbCatalog+假CatalogGet通过真实refresh route；禁用mirror_nfo仍得到完整字段；PUT override200→详情/卡片/筛选同值；TMDB V1 overview→V2 Missing应取Douban；交换采用顺序值及origins完全同。
  ```rust
  assert!(real_catalog_normalized.is_some());
  assert_eq!(detail.overview,Some("MANUAL".into()));
  assert_eq!(persisted.overview,Some("BACKUP".into()));
  assert_eq!(detail_fields,composed.fields);
  assert_eq!(origins["overview"].source,"douban");
  ```
- [ ] **Step 2 — RED：** `cargo test -p api --test metadata_priority real_catalog_refresh_adopts_complete_fields`；`cargo test -p store --test metadata_adoption successful_missing_recomposes_from_remaining_sources`，当前None/旧值/展示不一致。
- [ ] **Step 3 — 实现：** 所有刷新入口统一metadata_service，删除旧ItemMeta/EpisodeMeta有损重建采用器；preferences取真实配置不硬编码zh-CN。snapshot校验source/entity/work/locale/revision，整次无效拒写；同事务重合成effective/来源/卡片，override也触发归约，详情不再短字段白名单。拆compose加载与合成，满足120行。
- [ ] **Step 4 — GREEN/烟测：** media/store/api独立套件；真实HTTP刷新→override→断网/清cache→重启，详情、卡片、Jellyfin同scope/locale字段及来源一致。
- [ ] **Step 5 — 文档/提交：** 更新字典/采用规则，只标已接线来源；`git commit -m "fix(metadata): compose production reads from validated current snapshots"`。

### Task 8: NFO锁外事务协议与局部编辑（R17/R18，S01）

**Files:** Modify `crates/api/src/scrape_metadata/{export,work,series}.rs`、`http/{library_scan,library_organize}.rs`、`watch_scrape.rs`、`storage_operations.rs`；`crates/store/src/metadata/{export,export_driver}.rs`及`nfo_input/{record,reconcile}.rs`；`crates/library/src/nfo/export.rs`；Test `crates/api/tests/storage_nfo_export.rs`。

**Interfaces:** NfoExportRequest沿现有字段；`begin_nfo_export`只存已验证意图，`complete_nfo_export`同事务完成输入/tag/provenance+终态，不先succeeded再register。API编排：短锁snapshot→锁外digest/parse/render→短锁begin CAS→锁外原子写→短锁ACK CAS。新增owned磁盘观察结构放export模块，包含observed digest/Valid/Invalid/Unknown/Missing，不让Store自行读盘。export_fields_json为三方比较baseline，编辑保存对应Library/input的局部差异，不升级全局manual。

- [ ] **Step 1 — 写回归：** 原NFO无效/unknown/外部编辑，正常refresh也必须保字节；provider titleV2且用户只改plot，title跟V2；删除本NFO解除局部覆盖，不影响另一库；begin前/replace后/ACK前逐边界重启，stale source/config/mismatch digest零覆盖。
  ```rust
  assert_eq!(disk_after_refused,disk_before);
  assert_eq!(after.title.value().map(String::as_str),Some("TITLE V2"));
  assert_eq!(after.overview.value().map(String::as_str),Some("USER PLOT"));
  assert_eq!(other_library_after,other_library_before);
  assert!(completed_input.export_digest.is_some());
  ```
- [ ] **Step 2 — RED：** `cargo test -p api --test storage_nfo_export normal_refresh_preserves_invalid_external_sidecar`及`provider_title_changes_when_only_plot_was_edited`，当前覆盖XML/锁旧title。
- [ ] **Step 3 — 实现：** 删除caller先write_nfo再finish的双写路径。持久意图先于首次磁盘写；校验file/source/root/media/config/lifecycle精确绑定。锁外写前复核digest，仍存在不可完全原子防第三方并发写的FS边界，明确保守冲突/重试策略，不声称SQLite+FS原子。capture对上次export字段diff，显式删除/Empty区别，保存局部输入与实际locale；内容不变不重新翻译语言。恢复仅在所有登记成功后终态；无效plan版本拒绝。
- [ ] **Step 4 — GREEN/烟测：** library/store/api独立套件；临时真实文件故障注入三个边界重启，核对内容/字段/来源；慢文件闸门下无全局Store持锁。Watch、movie/TV work/season/episode全部同用例。
- [ ] **Step 5 — 文档/提交：** 生命周期与FS并发边界写合同；`git commit -m "fix(nfo): persist exports before lock-free writes and retain local edit provenance"`。

### Task 9: 根/locale一致重配置与完整有界checker（R19/R20，S02/S03）

**Files:** Modify `crates/store/src/library_operations/{mod,roots,locale,integrity}.rs`、`crates/api/src/{storage_operations,http/library_operations}.rs`；Create sibling `crates/store/src/library_operations/integrity/{enumerate,compare}.rs`（职责拆分，原入口保留）；Test `crates/store/tests/{library_reconfiguration,library_integrity}.rs`、`crates/api/tests/{storage_operations_runtime,storage_integrity_runtime}.rs`。

**Interfaces:** 现有begin/advance/status保持；持久cursor区分阶段、Media keyset及单Media内部位置/观察revision。roots+locale在同operation分别推进并最终一起验证。failed保留恢复phase和freeze，不将部分变更提交后ready；预检拒绝不合法root/locale，确认目标集合语义后才解冻。 `IntegrityReport::has_issues(&self)->bool`为新增方法，返回有界issue集合非空；`IntegrityIssue`枚举分别携带Missing/Orphan/Mismatch的表类别、Media/unit/User键及观察revision，记录总数和续页cursor，不持久化全库问题数组。

- [ ] **Step 1 — 写回归：** roots+locale组合、纯unidentified无file、根移除/嵌套根、每batch故障；checker删unit/term/user、错计数/preferred/reverse orphan，修复前后新写竞态。
  ```rust
  assert_eq!(finished_locale,"en-US");
  assert_eq!(finished_roots,requested_roots);
  assert_eq!(failed_library.lifecycle_state,"reindexing");
  assert!(damaged_report.has_issues()); // Task内新增报告方法，基于明确非空issue集合
  assert_eq!(repaired_facts,expected_from_raw);
  assert!(examined_media <= 100);
  ```
- [ ] **Step 2 — RED：** `cargo test -p store --test library_reconfiguration roots_and_locale_commit_together`；`cargo test -p store --test library_integrity missing_units_and_terms_are_detected`，当前语言未变/clean。
- [ ] **Step 3 — 实现：** 根变更按受影响原始关系单独迁file/NFO/unidentified，不依赖file遍历发现所有关系，root排序同步更新；未匹配文件归属须按规范选择合法库或预检拒绝，不伪报新根完成。checker SQL有界union keyset，双向比较items/units/terms/用户字段，不只存在性；单Media修复重读版本，最终复核用户也在内且可分页；发现即failed，全部复核后clean。报告新增显式issue种类/键，避免无限累积完整详情进cursor。
- [ ] **Step 4 — GREEN/烟测：** store/api独立套件；实际HTTP组合PATCH→202→重启→完成；漏行注入→checker→500→repair→200。100k库batch1测examined/VM/锁时间，单Media大覆盖也有界，不靠调用次数证明。
- [ ] **Step 5 — 文档/提交：** 更新operation/完整性保证；`git commit -m "fix(library): resume coherent reconfiguration and bounded integrity verification"`。

### Task 10: 合并快照决议、冲突与恢复；别名识别（R21/R22）

**Files:** Modify `crates/store/src/media_operations/{preflight,apply,references}.rs`、`crates/store/src/media/{mod,alias}.rs`、`crates/api/src/{storage_operations,job_loop}.rs`；Test `crates/store/tests/{media_merge_conflicts,media_merge_recovery,media_alias_updates,scanned_media_identity}.rs`、`crates/api/tests/storage_operations_runtime.rs`。

**Interfaces:** MediaMergeRequest/OperationStatus保留；持久plan补version、每source/scope/locale胜出实体/快照及逐字段pending fallback决议。begin固定选择并CAS；advance失败保存恢复phase，不让failed被unfinished/holder排除。alias ownership查完整alias集合，refresh target只决定刷新，不决定既有别名是否合法。

- [ ] **Step 1 — 写回归：** 同实体新/同时间快照、Missing/Empty、异实体选p2/p3无snapshot、人工锁冲突、每阶段故障重启；非target100 target999重新scan100。
  ```rust
  assert_eq!(chosen.fetched_at,2000);
  assert_eq!(tie_chosen,original_survivor_snapshot);
  assert!(conflicting_override_begin.is_err());
  assert!(failed_operation_is_recoverable);
  assert_eq!(resolved_non_target.id,canonical_id);
  assert!(pending_origin.pending_target_fallback);
  ```
- [ ] **Step 2 — RED：** `cargo test -p store --test media_merge_recovery newer_same_entity_snapshot_wins`；`cargo test -p store --test media_alias_updates registered_non_target_alias_resolves_scanned_media`，当前1200/Protected。
- [ ] **Step 3 — 实现：** 同实体选整份最新快照，tie survivor；异实体明确目标且校验季集归属，不保错目标snapshot；人工锁差异预检拒绝不静默删。按规范X/Empty fallback保真实origin/pending，刷新成功才退出；所有新引用/在途工作冻结排空，typed Job/Subscribe收敛后核对retire。失败维持同operation持锁可续，新operation不得偷接管。scan以alias归属判冲突而非flat target差异。
- [ ] **Step 4 — GREEN/烟测：** store/api/subscribe/jobs独立套件；真实HTTP每阶段故障/重启/重试相同operation，交换来源输入次序同结果，清cache/checker不改pending决议。
- [ ] **Step 5 — 文档/提交：** ADR0001及R21/R22逐矩阵证据；`git commit -m "fix(media): persist deterministic merge decisions and preserve alias ownership"`。

### Task 11: 用户删除按持久阶段恢复且失败可见（R25/S04）

**Files:** Modify `crates/api/src/{user_removal,job_loop,http/users}.rs`、`crates/api/src/http/routes/admin.rs`、`crates/store/src/users.rs`及typed Job user引用处；Modify `web/lib/api/{members,auth}.ts`、`web/components/members-section.tsx`；Test `crates/api/tests/{users,storage_subscription_runtime}.rs`，Create `web/test/user-removal.test.mjs`（复用实际TS模块loader）。

**Interfaces:** `DELETE /api/v1/users/{id}`先在app事务保存`UserRemovalRecord { version:u32,operation_id:String,user_id:String,phase:String,cursor:Option<String>,error:Option<String>,updated_at:i64 }`及禁用/revoke，提交后立即202，body=`{operation_id,user_id,phase:"disabling",state:"pending",error:null}`，后台驱动而非HTTP同步清完。管理员新增`GET /api/v1/users/removals/{operation_id}`，200返回同DTO，state=pending/failed/succeeded；未知404，会员403。phase=disabling/cleaning/finalizing/succeeded；失败保phase/error，同operation显式重试与重启可续；重复DELETE若已有未完或终态记录返回同operation，不生成新ID；User不存在且无记录404。终态record以`user_removal.result.<operation_id>`留app.settings，pending `user_removal.<user_id>`原子移除；成功结果保留7天，由现有settings维护路径按终态时间清理，活动/失败记录不得被清。Web两个适配器返回该DTO，成员页只有status=succeeded才移除行并提示完成，pending/failed显示状态及错误。

- [ ] **Step 1 — 写回归：** app禁用后/subscribe提交后/library清后/User已删setting未清时恢复，外部写锁导致失败，另一User无变化。
  ```rust
  assert_eq!(retry.operation_id,accepted.operation_id);
  assert_eq!(disabled_login.status(),StatusCode::UNAUTHORIZED);
  assert!(recovery_error.is_some());
  assert_eq!(transferred_subscribe.user_id,admin_id);
  assert!(!pending_setting_remains_after_success);
  assert_eq!(status_after_user_deleted.state,"succeeded");
  assert_eq!(repeated_delete.operation_id,accepted.operation_id);
  ```
- [ ] **Step 2 — RED：** `cargo test -p api --test users interrupted_removal_reuses_operation_and_reports_failure`，当前吞错/新ID或User missing停留。
- [ ] **Step 3 — 实现：** 分离受理与驱动，初始record+disable/revoke同app事务，提交后逐库幂等；恢复消费版本化record不重新start。每阶段成功写cursor，失败保存error/日志且后台backoff重试；新增管理员status handler和route，成功按同app事务删User、移pending为terminal result；User已缺失的finalizing幂等完成。清理前排空session及typed Job；保持Subscribe/私有Collection转admin。终态保留只清成功7天，不能删除待恢复记录。
- [ ] **Step 4 — GREEN/烟测：** store/api/jobs/subscribe独立套件及Web三门；临时HTTP DELETE→202→库阻塞→GET status失败可见/token拒绝→释放→重启同operation完成，User删除后status仍succeeded；实际成员页pending不假报完成、failed可见、成功移行。`node --test web/test/user-removal.test.mjs`覆盖两个实际adapter与轮询状态转换。
- [ ] **Step 5 — 文档/提交：** self.md写明DELETE受理/status/7天终态与重试语义，删除两个旧void/deleted:boolean适配合同；`git commit -m "fix(users): recover observable durable removal phases"`。

### Task 12: 协议查询与显示同一有效语义（R12/R23/R24）

**Files:** Modify `crates/store/src/library_query/protocol/{mod,sql,predicates,facts,order}.rs`、`crates/api/src/media_server_provider/{selection/mod,selection/page,item_metadata}.rs`及`crates/media-server/src/routes/catalog.rs`；Test `crates/api/tests/{jellyfin_sql_page,jellyfin_paged_metadata}.rs`及其子模块。

**Interfaces:** ProtocolItemQuery保留既有filters/count/offset/递归/权限能力。输入PersonIds进入SQL，不保live Catalog旁路；语义casefold统一Rust Unicode并注册确定性SQLite函数（确需时启用现有rusqlite functions特性，不加全量Rust过滤）；genre显示原串与比较fold分离、UUID只基于协议原始规范键。SQL同scope/locale effective字段与水合一致，eligibility在count/LIMIT之前确定。

- [ ] **Step 1 — 写回归：** HTTP `/Genres`→返回ID→Items、ÉCOLE搜索、A/a DateCreated逆序、全看完Series、最新season断言、episode简介/两locale、missing排首Limit1、已采纳人物断网。
  ```rust
  assert_eq!(unicode_page.total,1);
  assert_eq!(genre_roundtrip_page.total,1);
  assert_eq!(case_tie_ids,vec![lower_a_id,upper_a_id]);
  assert_eq!(filtered_series_userdata.played,true);
  assert_eq!(offline_person_page,online_person_page);
  assert_eq!(items.len(),1); // 两项中仅B可播，Limit1，不允许水合后丢A造成空页
  ```
- [ ] **Step 2 — RED：** `cargo test -p api --test jellyfin_sql_page --test jellyfin_paged_metadata`，新增discriminating cases当前Unicode/genre0、多键逆序、Catalog500。删除“本地人物必须Catalog失败500”的错误测试，不重钉。
- [ ] **Step 3 — 实现：** entity/locale先决议再filter/order/count；Name只贡献fold主键，所有请求sort键后才stable ID tie。Series有效played从授权本地episode归约，与UserData同一政策；时间断言用Task5。person从采用事实；无候选selected row必须选择明确非可播DTO或在SQL排除，不能静默continue不一致页。offset能力及精确count不缩水。
- [ ] **Step 4 — GREEN/烟测：** store/api/media-server独立套件；真实TCP上上述协议roundtrip，断网/清cache/重启同ID/total，release同数据性能对照，报告count/复杂搜索允许增长成本。
- [ ] **Step 5 — 文档/提交：** 更新ADR0005/benchmark，仅实际测得才填数字；`git commit -m "fix(jellyfin): align scoped SQL predicates with hydrated protocol facts"`。

### Task 13: 全生产入口验收与旧记录收口

**Files:** Extend现有 `crates/api/tests/storage_end_to_end.rs`及其support，Web真实adapter tests；Modify `docs/audits/2026-10-22-storage-repair-review.md`、数据库/ADR/benchmark及当前计划状态；如需新临时TCP runner在系统temp，不提交debug开关。

**Interfaces:** R01–R25/S01–S04逐ID状态=未修/已修未验/已验证/Not run，必须引用公开入口与实际结果；不能凭单一target通过关闭整组矩阵。历史性能/设备门仍独立，不因重写文档视作完成。

- [ ] **Step 1 — 补跨入口回归：** 真实scan→claim/Transfer→Scrape→Web/Jellyfin source/session→marks/history→根/locale/checker/merge→重启清cache/remove-reimport，包含另库/另User不变；旧记录两项unknown cache source与api_key-only stream重新精确验证，若仍失败加对应consumer回归。
  ```rust
  assert_eq!(reimported_media_id,original_media_id);
  assert_eq!(other_user_after,other_user_before);
  assert_eq!(offline_detail,before_cache_clear_detail);
  assert_eq!(unknown_cache_source.status(),StatusCode::BAD_REQUEST);
  assert_eq!(issued_api_key_stream.status(),StatusCode::PARTIAL_CONTENT);
  ```
- [ ] **Step 2 — 定向门：** `cargo test -p api --test storage_end_to_end`及前述所有新增精确case；修复前记录实际红因，不用旧代码文本零匹配代替行为。
- [ ] **Step 3 — 最小收尾：** 若历史两项仍坏，未知source映射400、嵌套路由query_token按实际URI匹配，不放宽所有query鉴权；迁完全部CLI/Web调用者、删obsolete helpers/兼容测试；S03函数按职责拆。不得扩展成其他独立产品任务。
- [ ] **Step 4 — 稳定树全门/真实运行：** 每触及crate独立测试后一次 `cargo test --workspace`；10k若debug不可接受，具名skip与独立release命令同时明确，不称workspace全量绿。前端typecheck/test/build，lint单列。`SMOKE_ROOT=$(mktemp -d)`隔离启动主二进制，真实HTTP/Range/鉴权/重启，浏览器浏览/切版本/seek/字幕并关闭；设备/NAS未跑仍Not run。基准按同fixture记录，不沿用优化前二进制。
- [ ] **Step 5 — 独立审查/提交：** 要求独立review复查生产接线、错误测试是否保留、完整场景和旧文档链接；更新全部R/S状态及实际命令/退出码；`git commit -m "test(storage): verify production repair contracts end to end"`。用户实例切换另需明确路径/停写/授权，绝不自动删库。

## 覆盖与自审

| 复审项 | 任务 |
|---|---|
| R01/R02，S01观察侧 | 1 |
| R03 | 2 |
| R04/R05 | 3 |
| R06/R07 | 4 |
| R08–R13 | 5；R12协议决议再由12验证 |
| R14 | 6 |
| R15/R16 | 7 |
| R17/R18，S01导出侧 | 8 |
| R19/R20，S02/S03 | 9；S03 compose在7 |
| R21/R22 | 10 |
| R25/S04 | 11 |
| R23/R24 | 12 |
| 历史cache400、query api_key401，最终所有入口 | 13 |

自审：全部finding有独立测试边界；共享FileVersion、SourceObservation、BoundPlaybackEvent、ComposedMetadata沿现有类型，新增持久plan版本显式解码拒未知。Review Focus五项分别落Task1/2/3/4/5/8/9/10。每任务列exact文件/接口/红绿命令及consumer断言；示例变量来自该任务已描述的现有fixture步骤，不宣称为可独立复制编译的完整测试函数。未将暂未运行的源码证明写成实测，也未把历史已修项目重新安排实现。文档完成不授权执行；先人工review，再选Subagent-driven或Native。
