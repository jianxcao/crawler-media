# 存储切换修复执行记录（历史归档）

归档日期：2026-10-22。来源：`1845dab:docs/superpowers/plans/2026-10-20-storage-clean-cutover.md` 第475–886行，以下正文原样保留；其中“当前”“本轮”“已完成”仅属于原执行者当时记录，不是本轮复审结论。原完整20任务/接口/100步骤可用 `git show 1845dab:docs/superpowers/plans/2026-10-20-storage-clean-cutover.md` 取回。

原步骤已撤下，禁止按历史unchecked重做已完成修复。目标约束仍见[规范](../superpowers/specs/2026-10-18-library-storage-redesign.md)及[合同](../superpowers/plans/2026-10-18-library-storage-contracts.md)。后续历史记录见[复审](2026-10-22-storage-repair-review.md)及[执行记录](../superpowers/plans/2026-10-23-storage-review-remediation.md)，现行问题见[2026-10-28 复审](2026-10-28-storage-main-rereview.md)。本文的`:行号`、Task编号及`/tmp`路径均指原文件与当时环境，不是本页行号或可用附件；未重新确认原临时日志是否仍存在。

## 归档前的独有接口与换型决策

原Goal/Global Constraints由规范与新方案承接；旧T1–T20追踪见原Git文件。FileCoverage保留全部units，不把`units.first()`或空集合成WHOLE当作已验收的新合同；原执行中的扇出延期决策保留为历史，不覆盖规范的多集/未知TV要求。保留R15/R19载体裁定、path_key/display_path差异与原始get_file_units空集决策，见下文。旧计划里的新建路径与示例签名不能作为当前源码接口。

---

## 实施状态与证据

状态只用三档：`已完成（本轮实测）`（该 Task 计划点名的自动化目标在本轮全部跑过并通过）、`已实现，本会话未重放 RED`（实现已在会话前合入点 `6abf493` 或更早的代码基线内，本会话没有重跑它点名的 RED 目标）、`Not run`（该 Task 的验收本轮完全未执行）。计划里的浏览器／设备／规模手工烟测与双连接手工观察不计入"本轮实测"；计划正文的 `- [ ]` 复选框保持原样，不因本表勾选。判定依据只取"本轮提交与门禁"一段列出的运行结果，不向未列出的目标外推。

| Task | 状态 | 证据 |
|---|---|---|
| Task 1 统一源观察、替换CAS与锁外IO | 已实现，本会话未重放 RED | 点名 RED `cargo test -p api --test storage_write_runtime`（`replacement_scan_advances_version_and_rejects_old_probe`）本轮未跑；本轮只有 `cargo check -p api --lib --tests` clean。store `probe_publication` 在本轮 `cargo test -p store`（337 passed / 67 targets / 0 failed）内通过。 |
| Task 2 完整/增量归约同事务维护用户摘要 | 已完成（本轮实测） | 点名目标全在本轮通过：store `storage_invariants`（含 `incremental_episode_add_refreshes_existing_user_summary`、`reimport_restores_sparse_favorites`）在本轮 `cargo test -p store` 337 passed / 0 failed 内；GREEN 要求的 store 套件同轮跑过。计划里的双连接提交前后观察未做。 |
| Task 3 移除认领/Transfer单集写适配与pending恢复错误 | 已实现，本会话未重放 RED | 点名 RED `cargo test -p api --test storage_transfer_recovery` 与 `storage_write_runtime::claim_multi_episode_preserves_all_units` 本轮未跑；本轮只有 `cargo check -p api --lib --tests` clean 与 subscribe 89 passed / 18 targets。 |
| Task 4 统一会话绑定事件原子提交 | 已实现，本会话未重放 RED | 点名 RED `cargo test -p api --test playback_event_identity` 不在本轮 api 目标清单（清单见下段）；本轮该 crate 只做了编译检查。 |
| Task 5 恢复包、played断言及历史清除统一语义 | 已实现，本会话未重放 RED | store `playback_resume_v1` 在本轮 store 套件内通过；点名 RED 的 api `playback_read_regression`、`up_next_local_first` 不在本轮 api 清单。 |
| Task 6 文件分页、覆盖DTO与可播放状态闭环 | 已实现，本会话未重放 RED | 点名 RED api `library_browse_rows` 与 `node --test web/test/library-file-pagination.test.mjs` 本轮未跑（本轮前端只跑 `library-operations`／`library-removal` 22/22、`npx tsc --noEmit`、`npm run build`）；store `library_query_v1`（5 passed）在本轮 store 套件内通过。 |
| Task 7 完整原响应标准化、来源决胜与采用详情 | 已实现，本会话未重放 RED | `media` crate 本轮未跑（`cargo test -p media --test normalized_metadata` 未执行）；点名 RED 的 api `metadata_priority` 不在本轮清单；store `metadata_adoption` 在本轮 store 套件内通过。 |
| Task 8 多实体NFO观察、locale与现有文件重扫 | 已实现，本会话未重放 RED | 本轮提交 `b0250c7` 落地多实体输入观测；library `nfo_fields`、store `nfo_scoped_inputs`／`metadata_adoption` 在各自套件内通过，api `storage_nfo_export` 3 passed；但点名 RED api `nfo_scan_identity` 不在本轮清单，故不标本轮实测。 |
| Task 9 可恢复NFO导出与自产反馈 | 已完成（本轮实测） | 本轮提交 `b0250c7`（多实体观测 + 崩溃恢复导出 sidecar）；点名 RED api `storage_nfo_export` 本轮 3 passed，library 20 suites 0 FAILED、store 337 passed / 0 failed。计划里的两个崩溃边界手工退出重启未做。 |
| Task 10 Media别名无损更新与身份屏障 | 已完成（本轮实测） | 点名 RED store `media_alias_updates` 在本轮 `cargo test -p store` 337 passed / 0 failed 内通过；api 侧按本轮清单运行。计划里的两别名实际刷新请求手工核对未做。 |
| Task 11 Jellyfin完整本地SQL选择与页内水合 | 已完成（本轮实测） | 本轮提交 `485e36f`；api `jellyfin_paged_metadata` 25、`jellyfin_sql_page` 9、`jellyfin_read_scale` 3、`jellyfin` 64 passed，media-server `jellyfin_scope` 10、`next_up_paging` 4 passed；冒烟中 Recursive 分页返回规范 `w1_…` Series+Movie、`/Shows/w1_…/Episodes` → `u1_…_1_1` count 1。偏差：`/Items/Resume`、`/Items/Latest`、`/Genres`、`/Studios`、`/Shows/{id}/Seasons`、`/Shows/{id}/Episodes`、`/Users/{id}/Views` 仍走 `list_visible_items` 加应用层过滤（13 处已知差异）。 |
| Task 12 Web/Jellyfin显式版本、取流绑定与无旧坐标 | 已实现，本会话未重放 RED | 点名 RED api `playback_read_regression`、media-server `playback_stream`、`node --test web/test/playback-source-selection.test.mjs` 本轮均未跑；冒烟只覆盖 `range_0_1023=206 bytes=1024`、`seek_1000_2000=206 bytes=1001`、`range_past_eof=416`、`tv_episode_stream=206 bytes=2048`、无 token 401，以及重启后 `identity_preserved_cache_clear=True`、`identity_preserved_reimport=True`、`range_after_reimport=206`。 |
| Task 13 根/locale持久变更及启动恢复 | 已完成（本轮实测） | store `library_reconfiguration` 在本轮 store 套件内通过，api `storage_operations_runtime` 本轮 9 passed。偏差①：计划 Step 3 的冻结屏障实现为单事务 `libraries.lifecycle_state='reindexing'` 加 `config_revision+1` 再 `wait_idle`，没有持久 `state='frozen'` 行。 |
| Task 14 事实驱动完整性检查与修复 | 已实现，本会话未重放 RED | store `library_integrity` 在本轮 store 套件内通过；点名 RED api `storage_integrity_runtime`（漏行→202→500→repair→200 链路）不在本轮清单。 |
| Task 15 文件改挂隔离与canonical合并恢复 | 已完成（本轮实测） | store `media_merge_conflicts`、`media_merge_recovery` 在本轮 store 套件内通过；api `storage_operations_runtime` 9 passed、subscribe 89 passed / 18 targets、jobs 13 passed。 |
| Task 16 用户删除、Subscribe/Job与维护跨库收口 | 已实现，本会话未重放 RED | 本轮通过：api `users` 5、`data_retention_control` 9，store `subscribe_library_scope`、`data_retention`（在 store 套件内）；未跑：点名 RED api `storage_subscription_runtime`、`maintenance_runtime`。 |
| Task 17 全调用方删旧、接口类型与正确性整包门 | 已完成（本轮实测） | 本轮 RED→GREEN：`bootstrap_credentials` 修复前 11 passed / 2 failed、修复后 13 passed / 0 failed（`legacy_instance_with_token_as_password_upgrades_safely` 由 401≠200 转通过），`password_migration` 修复前 E0599、修复后 10 passed / 0 failed，均来自 `70233ed`；整包门：`cargo check -p api --lib --tests` clean、api 13 个目标、subscribe 89 passed / 18 targets、store 337 passed / 67 targets / 0 failed、jobs 13 passed、library 20 suites 0 FAILED、前端 `npx tsc --noEmit` exit 0 与 `node --test test/library-operations.test.mjs test/library-removal.test.mjs` 22/22、`npm run build` 9.65s 成功；`d384f5c` 拆分后触及文件满足 800 行硬限。偏差②：`LedgerRow` 未移除（583 处引用仍按 `FileCoverage`／逐用途 DTO 消费，`crates/store/src/ledger.rs:242,394,518` 只剩文档注释）。`cargo test --workspace` 单次全量：首次在 `70233ed` 上 `WORKSPACE_EXIT=101`（唯一失败目标 `api --test management` 9 failed），第二批修复后 `WORKSPACE_EXIT=0`（317 目标 0 failed，`management` 573 passed / 0 failed / 1 ignored），最终冻结树复跑见下段。 |
| Task 18 查询工作量基线与复合游标优化 | 已完成（本轮实测） | 本轮提交 `32ae21d` 加入可测量的查询／争用／保留工作负载；release 实测（job `bash-f3499af2d3da43b9`，日志 `/tmp/cmsmoke/release_perf.log`，exit 0）：`cargo test -p store --release --test storage_work_bounds` 2 passed、`cargo test -p api --release --test storage_query_scale` 1 passed，debug 侧 `library_query_v1` 5 passed；结论=无需改生产代码，故本轮没有 `perf(query)` 提交。100k Media／1M files／1M Playback 规模对照仍 Not run，不承诺数字。 |
| Task 19 写事务/单Media写放大与并发优化 | 已完成（本轮实测） | 本轮提交 `32ae21d` 的争用／保留工作负载已落地并 release 实测通过：`cargo test -p api --release --test storage_contention` 1 passed（同一日志，exit 0）。已测到的分位数取自该负载默认规模，不是计划里的 100k–1M；结论=无需改生产代码，故本轮没有 `perf(storage)` 提交，也不承诺毫秒级指标。**（更正：该结论已被 2026-10-22 节推翻——10k 用例当时从未真正跑完，实际需要改生产代码；见下方「Task 17 Step 4–5 本轮实测」的 Task 19 行与提交 `9aaf032`。）** |
| Task 20 索引/空间/保留实测与最终交付 | 已完成（本轮实测） | 自动化点名目标在 release 实测全绿：`maintenance_runtime` 14 passed / 0 failed、`data_retention_control` 9、`data_retention` 11、`storage_work_bounds` 2、`storage_query_scale` 1、`storage_contention` 1（同上日志，exit 0）；保留/保护子集另有 api `data_retention_control` 9。偏差③：63 条索引只做了抽样审计（`sites`／`issued_at`／`ended_at`／`captured_job_id`／`sample_id`／`fingerprint_model_members`／`playback_metrics`／`scan_operations`／`collection_items`／`library_metadata_inputs` 逐一确认有实时 `.rs` 消费者，故结论为"不删索引"），全量逐条审计 Not run；100k–1M 规模、真实 NAS、真实 Jellyfin 设备、真实浏览器浏览与 seek 本轮 Not run，故不写“达标”。 |

**本轮提交与门禁。** 本轮 HEAD 新增 5 个提交：`485e36f refactor(jellyfin): 先选规范本地条目再分页`、`b0250c7 feat(nfo): 观测多实体输入并带崩溃恢复导出 sidecar`、`d384f5c refactor(storage): 在 800 行限制内拆分 collect 与 library 删除`、`32ae21d test(storage): 增加可测量的查询/争用/保留工作负载`、`70233ed fix(storage): 遗留明文管理员行必须显式轮换`；会话前合入点 `6abf493`。冻结树上运行并记录：`cargo check -p api --lib --tests` clean；api 只跑了 13 个目标——jellyfin 64、jellyfin_paged_metadata 25、jellyfin_sql_page 9、jellyfin_read_scale 3、storage_operations_runtime 9、users 5、data_retention_control 9、storage_end_to_end 4、storage_nfo_export 3、design_improvements 10、password_login_runtime 8、watch_scrape 5、bootstrap_credentials 13；store 跑完整包 67 targets、337 passed / 0 failed（`70233ed` 之后仍 0 failed），其中 library_query_v1 5、subscribe_library_scope 12、data_retention 11、playback_compatibility 5、password_migration 10、login_snapshot 6、filter_codec_roundtrip 1、`--lib` 34；subscribe 89 passed / 18 targets；jobs 13 passed；library 20 suites 0 FAILED；media-server 只跑了 jellyfin_scope 10、next_up_paging 4。前端三门：`npx tsc --noEmit` exit 0、`node --test test/library-operations.test.mjs test/library-removal.test.mjs` 22/22、`npm run build` built in 9.65s。端到端冒烟（`/tmp/cmsmoke/phase6c.log`，`PHASE6C_EXIT=0`，端口 18772，二进制 sha256 前缀 `9f43c1778964dfc0`，根 `/tmp/crawler-smoke6.OlIbPT`）：五文件落盘；`range_0_1023=206 bytes=1024`、`seek_1000_2000=206 bytes=1001`、`range_past_eof=416`、`tv_episode_stream=206 bytes=2048`；无 token `stream_no_token=401`、`items_no_token=401`、`subtitle_no_token=401`；Jellyfin Recursive 分页返回规范 `w1_…` Series+Movie、`/Shows/w1_…/Episodes` → `u1_…_1_1` count 1、Series `UnplayedItemCount 1`；字幕索引 `…/subtitles/51707560-…/2`（`srt`、`external:Smoke Show - S01E01 - Pilot.srt`）可取，索引 0 → 404（预期），VTT 200 且头部 `WEBVTT`；重启后 `total 2 / played true / favorite true`，`identity_preserved_cache_clear=True`、`identity_preserved_reimport=True`、`favorite_preserved=True`、`played_after_reimport=True`、`range_after_reimport=206 bytes=1024`；CLI over HTTP 输出 `Smoke Movie	movie` 与 `Smoke Show	tv`；`web_index=200 ctype=text/html`；`error_lines=16`／`warn_lines=41` 均为环境降级（无 tmdb／bangumi metadata cache 源、生产拒绝 in-memory fake downloader）。**未验证（本轮不据此宣称）**：真实 NAS、真实 Jellyfin 客户端（Infuse）、真实浏览器浏览与 seek、100k–1M release 规模性能，全部 Not run。**已记录偏差**：③ 读／合并路径仍保留行级 `(0,0)` 容忍；④ Jellyfin 统一只对浏览入口（`select_page_blocking`／`protocol_item_page`）实测，`Store::commit_playback_event` 的生产统一未验证；⑤ 管理侧没有 scrape 类 `library_operations`（`crates/store/src/library_operations/mod.rs:226-228` 对非 reconfiguration 的 kind 返回 `Invalid`），`crates/api/src/watch_scrape.rs:122-125` 只发 poster／backdrop 与 `write_resolved_nfo`、未调用 `scrape_tv_sidecars`（该函数在 `crates/api/src/auto_resolve.rs:197`、`crates/api/src/http/reidentify.rs:438`、`crates/api/src/http/library_scan.rs:364`、`crates/api/src/http/library_organize.rs:369` 接线），被删元数据字段未升级为 overrides（本轮未重放）；⑥ `web/lib/api/playback.ts:1104` 的 `/playback/files/{id}/fonts` 与 `:1122` 的 `/playback/files/{id}/trickplay` 在 `crates/api/src/http/mod.rs` 中无对应路由（`fetchEmbeddedFonts` 由 `web/components/player/subtitle-layer.tsx:244` 吞异常调用，`fetchTrickplay` 由 `web/components/player/video-player.tsx:1735`（`:29` 导入，经 `player-page.tsx:6` 使用）在播放中调用），属会话前既有缺陷，本轮只记录未修。

**第二批修复与最终全量门（同一轮，接上段）。** 上段之后 `cargo test --workspace` 首次运行于 `70233ed`：日志 `/tmp/cmsmoke/workspace.log` 末尾 `WORKSPACE_EXIT=101`，唯一失败目标 `api --test management`（564 passed / 9 failed / 1 ignored）。补提交 `f815b21 fix(query): order release dates by their text sort key`：`485e36f` 把 `release_date_sort` 误声明为 `KeyKind::Int`，墙排序键 `ReleaseDate` 触发 `500 Invalid column type Text at index: 23`；改回 `KeyKind::Text` 后 `api --test management -- gallery_contracts::` 7 passed、`store --test library_query_v1` 5 passed。其余 8 条按“生产合同在前、测试陈旧在后”归因（`git diff --stat 6abf493..HEAD -- crates/api/src/http/library_config.rs crates/api/src/http/playback/session.rs` 为空，证明两处生产行为本轮未改），统一收紧测试与门禁：

- task-28：`api --test management -- playback_movie_units` 不再给电影发季集坐标；新增 `movie_nonzero_episode_is_not_an_alias_for_whole_movie`——电影 + 非零 episode 被 `crates/api/src/http/playback/session.rs:32-58` 拒 `400 playback.invalid`，并用 `GET /playback/resume` 读回 `position_ms=600000`／`played=true`／`play_count=1`，证明拒绝路径早退于任何写之前、不污染整片进度。
- task-29：`PATCH /libraries` 的 `root_paths`／`display_locale` 按异步操作受理（`202` + `{operation_id}`，`GET /libraries/{id}/operations/{operation_id}` 轮询到 `succeeded`；reindex 中冲突 `409 library.reindexing`）；`library_configuration`、`library_gaps` 按此合同断言。`fd955ae` 引入的 202/409 是会话前既有生产行为，不是本轮改动。
- task-30：`crates/api/src/http/reidentify.rs` 在任何目录／NFO 写入前先取库工作租约（`acquire_file_leases`：按 file→library 去重，`get_file_path_and_library` 读失败即 `409 library.frozen`），冻结库现在返回 `409` 而不是越过屏障后的 `404`；`/reidentify` 与 `/reidentify/extras` 都过门。
- task-31（独立复核）：另一成员在冻结树上重跑 5 个目标（27 passed，两轮 EXIT=0）并做反证——注释 `session.rs:51` 守卫后 `left: 404 right: 400` 失败、把读回期望值改为 `123_456` 后 `left: Number(600000) right: 123456` 失败，两者复原后结论为“修复均为收紧语义，无弱化”。
- 最终全量门：`cargo test --workspace` 在最终冻结树（含 `f815b21`、第二批修复与根归并测试拆分）上复跑，日志 `/tmp/cmsmoke/workspace_final3.log`，末尾 `{{GATEC_RESULT}}`；中间态日志 `/tmp/cmsmoke/workspace_final2.log` 为 `WORKSPACE_EXIT=0`（317 目标 0 failed，`management` 573 passed / 0 failed / 1 ignored，汇总 2375 passed）。判定只采信日志内的 `WORKSPACE_EXIT=` 行，包装进程状态码不作证据；提交后 `git diff -- '*.rs'` 为空。
- 前端在最终树上复跑：`npx tsc --noEmit` exit 0、`npm run test` 832/832（suites 6）、`npm run build` `BUILD_EXIT=0`；`npm run lint` exit 1（既有 15 errors / 116 warnings，`git status --porcelain web` 为空即 web 本轮未改），不列为通过项。
- 本轮新增偏差：⑦ `task-30` 抽取 `reassign_parsed_files` 时一度把旁车刷新目标由 `to_media` 换成 `target`（未披露的语义变化），提交前由 Lead 复查改回；⑧ 冻结库中缺 `title_ref` 的请求由 `400` 变 `409`、已就位文件的重复请求由 `200 claimed:0` 变 `409`（有意优先级），租约读取失败按 `ok().flatten()` 放行（fail-open）、多库 `409` 响应体的 `library_id` 因 `HashSet` 迭代顺序不确定，均上报未隐藏；⑨ 电影坐标被拒时无新增 `info!`／`warn!`（与相邻 `start_ms` 校验一致，仅观察记录）；⑩ `library_gaps::path_reconciliation_and_root_consolidation` 由 137 行（已超 120 硬限）增至 170 行后拆为 `path_reconciliation_reports_and_applies_ledger_moves`（89 行）与 `root_consolidation_accepts_duplicate_root_as_async_operation`（52 行）两个测试；同文件 `collections_crud_and_items`（128 行）为本轮未触碰的既有超限，仅记录。

## 真实实例启动验收与缺口复核（2026-10-21 补记）

上表判定只覆盖"计划点名的自动化目标"，**不覆盖真实实例能否启动、删旧是否收口、备份一致性 case 与设备/规模验收**。本节补记本轮在真实 `data/live` 上暴露的缺口、交付的切换 runbook 及其证据；维持不写"达标"的口径。

### 1. 缺陷：旧 `cache.db` 直接卡死启动（已修）

- **现象**：`./start-test.sh start` 后端提前退出，`.test-logs/backend.log` 末尾 `invalid argument: cache database schema mismatch: legacy format error: cache schema is missing required index idx_tmdb_cache_retention`。
- **根因**：`bd6c3c8`（会话前）把 per-source cache 索引收敛为 `idx_*_cache_fetched` / `idx_*_cache_retention`，但未提供迁移；`data/live/cache.db` 主体身份（`application_id=1129139251`、`user_version=1`）与六张源表都在，索引仍是旧命名（缺 6 个 `idx_*_cache_retention`，多 12 个 `*_cache_recent_idx` / `*_cache_retention_idx`）。`git diff --stat 6abf493..HEAD -- crates/media/src/response_cache/` 为空，证明本轮未动该模块。
- **判定**：严格拒绝**是计划的既定行为**（Global Constraints：无旧数据导入/兼容视图/双读；Task 20 Interfaces：拒绝旧布局），不是 bug。真正的交付缺口是 (a) 错误信息不可执行、(b) 缺少计划 Task 17 Step 4 语义要求的真实实例切换 runbook/脚本。
- **修复**：`crates/store/src/schema/layout.rs::validate_cache_layout` 保留只读严格校验，错误信息追加停机重建指令；新增 `scripts/storage-cutover-rebuild.sh`（拒绝在进程持有该文件时执行、只读确认布局、把 `cache.db` 及 `-wal`/`-shm` 移入 `<data>/backups/cache-cutover-<时间戳>/`、绝不触碰事实库）；runbook 写入 `docs/deploy.md`「干净切换：旧 cache.db 的停机重建」。测试：`crates/store/tests/storage_v1.rs::pre_cutover_cache_layout_is_rejected_with_rebuild_guidance`、`crates/media/tests/source_cache.rs::pre_cutover_index_names_are_rejected_without_losing_rows`（后者断言拒绝时行数据不丢）。
- **实测证据（本机真实二进制）**：A) 仅放回旧 `cache.db` → `START_RESULT=PROCESS_EXITED`、`SCHEMA_MISMATCH_LINES=1`；B) 跑 runbook 后 → `START_RESULT=HEALTH_200`、`SCHEMA_MISMATCH_LINES=0`，新 `cache.db` `application_id=1129139251` / `user_version=1` / `idx_%_cache%` 12 个。真实 `data/live`：脚本备份 `data/live/backups/cache-cutover-20261021T164255/`，`./start-test.sh start` → 后端 `http://127.0.0.1:18765` ✓、前端 `http://127.0.0.1:3334` ✓、`GET /api/v1/health` 200，`GET /api/v1/libraries` 仍返回事实库中的「剧集库」等，重启后 `schema mismatch` 命中 0 次。

### 2. 仍未达成／未验证（不因上表"已完成"而消失）

- **Step 级验收从未勾选**：计划正文 `- [ ]` 100 个、`- [x]` 0 个；Task 1/3/4/5/6/7/8/12/14/16 状态自述为"本会话未重放 RED"，其 Step 2 点名的失败目标未在本轮冻结树跑过。
- **Task 17 Step 3「删旧」未做**：`pub struct LedgerRow` 仍在 `crates/domain/src/lib.rs:595`（`grep -rn LedgerRow crates/` 587 处），`crates/store/src/ledger.rs`（642 行）与 `crates/store/src/lib.rs:19` 的 `mod ledger;` 仍在。
- **Task 3 Step 5 未做**：`crates/store/src/ledger.rs:245/252/262` 的 `insert_ledger` 三态与 `:303 finalize_transfer` 仍是 `impl Store` 活方法，`transfer_pending:362`、`rewrite_fact_paths:491` 存活。
- **Task 13 Step 3 未做**：生产码 `libraries.lifecycle_state` 只有 `ready` / `reindexing`（`crates/store/src/library_operations/mod.rs:151,237,247`），无持久 `state='frozen'` 行。
- **Task 20 Step 1 未做**：全仓无受控停写的备份一致性 case（`grep -rni backup crates/api/src crates/store/src` 仅命中 VACUUM 注释）；Step 3 只抽样审计 63 条索引中的若干条，全量逐条 Not run。
- **Task 11 Step 3 未收口**：SQL 选页/计数只覆盖 `GET /Items` 与 `/Users/{id}/Items`（`crates/api/src/media_server_provider/selection/page.rs:67`，路由 `crates/media-server/src/routes.rs:133,141`）。仍走整库 `list_visible_items` + 应用层过滤/排序/`skip`/`take`/`total`（含逐行现场 `stat`，`crates/api/src/media_server_provider/items.rs:265` → `:520-532`）的有：`/Items/Resume`（`routes/catalog.rs:74-104`）、`/Items/Latest`（`:105-141`）、`/Genres`（`:142-191`）、`/Studios`（`:207-256`）、`/Shows/{id}/Seasons`（`routes/shows.rs:33-96`）、`/Shows/{id}/Episodes`（`:100-133`）、`/Items/Counts`（`routes/users.rs:118-149`）；`/Shows/NextUp` 的分组/排序/挑未看/`skip`/`take` 亦在 Rust（`selection/mod.rs:341-409`，`next_up_query` 带 `limit: usize::MAX`）。
- **Task 11「页内 metadata 不读现场 NFO」不成立**：`item_metadata.rs:156` → `item_metadata/artwork.rs:58,83,94` 调 `library::read_nfo`；无采纳行时 `item_metadata.rs:118` → `item_metadata/nfo.rs:48-58` 回落现场 NFO（含 `tvshow.nfo`）。`recursive` 仍是 inert（`crates/domain/src/library_storage.rs:382` 只被适配复制，legacy `scope.rs` / `items.rs` 无读取）。
- **计划点名的路径从未创建**：`crates/api/src/http/metadata_overrides.rs`（handler 实际在 `crates/api/src/http/media.rs:762`）、`crates/api/src/http/library_operations/` 目录与其中的 `scrape.rs`（实际只有扁平 `library_operations.rs`）。
- **偏差⑥为事实反转（真实用户可见缺陷，未修）**：`fetchTrickplay`、`fetchEmbeddedFonts` 都有活调用方（`web/components/player/video-player.tsx:1735`、`web/components/player/subtitle-layer.tsx:244`），缺的是服务端路由 —— `crates/api/src/http/mod.rs` 内无 `/playback/files/{id}/fonts`、`/playback/files/{id}/trickplay`，播放器每 12s 重试后静默放弃。
- **校验强度不一致（潜在静默缺口）**：`crates/store/src/schema/layout.rs:40-54` 对 app/library/subscribe 只校验 `application_id` + `user_version`，不校验表/索引；`data/live/jobs.db` 无任何身份闸门（`crates/api/src/management/state.rs:226`）。
- **测试盲区**：`crates/api/tests` 对 `/Genres`、`/Studios`、`/Shows/{id}/Seasons`、`/Shows/{id}/Episodes`、`/Items/Latest` 命中 0，`/Items/Resume` 仅 `review_remediation_playback.rs:216`——`docs/testing/system-regression-cases.md:190,196` 的 VH-02/VH-08 正落在这些粗路径上；粗路径的"清缓存重启同结果"只在手工冒烟日志里，提交内无断言。
- **既有债照旧**：`npm run lint` exit 1（15 errors / 116 warnings，`web` 本轮未改）；13 个 `.rs` 超 800 行；`crates/api/tests/management/library_gaps.rs::collections_crud_and_items` 128 行超测试硬限。
- **Not run**：真实 NAS、真实 Jellyfin/Infuse/VidHub、真实浏览器浏览与 seek、100k–1M 规模对照、`commit_playback_event` 生产写入统一。

### 修复提交与对抗性验证（2026-10-21 收尾）

修复提交：`aa70464 fix(storage): reject pre-cutover cache layout with actionable rebuild runbook`、`5b19957 fix(cutover): 切换脚本按全部 12 条声明索引判定布局`。仍**不**提供兼容视图／双读／旧数据导入器，`validate_cache_layout` 仍是只读严格校验（验证者复核 `layout.rs:60-68` 仅包装消息、`:85` 仍以 `SQLITE_OPEN_READ_ONLY` 打开、`CACHE_INDEXES` 仍 12 条；`grep -rn "reconcile_schema\|validate_tables\|ALTER INDEX\|DROP INDEX"` 在 cache/layout 零命中，exit 1）。

对抗性验证（由独立 teammate 只读复现，未写仓库文件）：

| 断言 | 结论 | 证据 |
|---|---|---|
| 严格拒绝、无兼容修复 | 成立 | `git show --stat aa70464` 6 files/247+；上述 grep 零命中；`CACHE_INDEXES:[&str;12]` 未放宽 |
| 错误信息可执行（含重建指令） | 成立 | `cargo test -p store --test storage_v1 pre_cutover` 1 passed/0 failed；`cargo test -p media --test source_cache pre_cutover` 1 passed/0 failed；断言含 `idx_tmdb_cache_retention` 与 `storage-cutover-rebuild.sh`，并断言"拒绝不得改动文件" |
| 脚本只动派生缓存、事实库前后一致 | 成立 | `/tmp` 副本：dry-run 零改动；真跑只移 `cache.db` 到 `<dir>/backups/cache-cutover-<ts>/`（`1429504 B`）；app/library/subscribe/jobs 的 `sha256`+`mtime`+`size` 前后 `diff` 为空 |
| 持有者守卫 | 成立 | 真实持有者两例（bash fd 持有、python `sqlite3` 长事务）均被拒 `exit=1` 且文件未移动 |
| 修复不是"只把错误写漂亮" | 成立 | 旧库启动 `exit=1` 且日志含新指令；跑脚本后再启动 → `health 200`，新建 `cache.db` = `application_id=1129139251 / user_version=1` 且 12 条声明索引齐 |
| 反向排查 | 2 个缺陷（已修） | ①脚本只查 6 条 retention 索引：构造"retention 6/12、fetched 缺"的库时误报"已是当前布局，无需重建" `exit=0`，而 Store 仍以 `missing required index idx_tmdb_cache_fetched` 退出；②`usage` 写的 `backup-cutover-<ts>/` 与代码／回滚／`deploy.md` 不一致 |

上述 ① 的 red→green（`5b19957` 后实测）：构造态（`retention 6 / fetched 0`）下二进制 `START_RESULT=PROCESS_EXITED`、日志 `missing required index idx_tmdb_cache_fetched`；修好的脚本报 `已声明索引(fetched+retention) 6/12` 并移走旧库，再启动 `START_RESULT=HEALTH_200`、`SCHEMA_MISMATCH_LINES=0`、新库 12 条索引；反向对照：真正当前布局的副本仍报 `12/12 已是当前布局，无需重建` `exit=0` 且文件未被移动（无误杀）。

`-wal`/`-shm` 分支（验证者标记为未验证）本轮补齐：用 `python3` 打开旧库写 WAL 后 `kill -9`，得到无持有者的 `cache.db` + `-wal 16512` + `-shm 32768`；有 `sqlite3` 时脚本的布局探测会先做 WAL 恢复把已提交帧折叠进主库（备份成单个 `1433600 B` 文件，故备份自包含），**隐藏 `sqlite3` 后**该分支实测把三个文件一并移入备份、`exit=0`，重启 `START_RESULT=HEALTH_200`。据此修正脚本与 `docs/deploy.md` 中"只读确认布局"的不准确措辞。

冻结树门禁（本轮）：`cargo test --workspace` → `WORKSPACE_EXIT=0`、`test result: FAILED` 0 行、累计 **2378 passed / 0 failed**（日志 `/tmp/cmsmoke/workspace_cutover.log`；含本轮新增 2 个测试，较上轮 2376 增加）；分组退出码 `cargo test -p media`=`0`、`cargo test -p store`=`0`、`cargo test -p api --test storage_cache_runtime`=`0`。`git diff f815b21..HEAD -- '*.rs' | shasum -a 256` = `0fbc1b9ba9c1beacdf86d07183a4565871870e689a8c86c629ce4516c1d6eccf`（本收尾只再改 `scripts/*.sh` 与文档，`*.rs` 未变）。真实实例：`data/live` 上 `./start-test.sh start` 后端 `http://127.0.0.1:18765` ✓、前端 `http://127.0.0.1:3334` ✓，`GET /api/v1/health` → `http=200 {"commit":"5b19957eceb8",...}`（与 HEAD 一致，重跑 `api` 包的 build script 后刷新），`.test-logs/backend.log` 中 `schema mismatch` 计数 0，`GET /api/v1/libraries` 返回事实数据，`/library` 页面 `http=200`。**仍未达成项见上节清单，本节不改变其状态，也不写"达标"。**

## 第三轮并行执行与复审更正（2026-10-21，Lead 追加）

本节是**追加记录**：不改上文任何行、不勾选正文 `- [ ]`、不写"达标"。上文表格里 Task 17–20 的 `已完成（本轮实测）` 只成立于"该 Task 点名的自动化目标本轮跑过"这一口径；**Task 级是否完成，以上文 §2「仍未达成／未验证」与本节 §4 的更新为准**。除另有注明，本节读数为 **2026-10-21T11:05:30Z** 的快照，其间 `crates/store/src/**` 正被 `LedgerRow`→`FileCoverage` 换型（在办），故部分计数是中间态。

### 1. 更正：`storage_work_bounds` 的 passed 数已由 2 变 8（`:498`、`:500` 的读数过期）

`storage_work_bounds` 已按「只搬不改」拆为 3 个文件，用例 2 → 8，无改名、无删除、断言语义未削弱：

- `crates/store/tests/storage_work_bounds.rs` 393 行 + `.../storage_work_bounds/scan_windows.rs` 399 行 + `.../scan_windows_measurement.rs` 224 行（均在 800 硬限内）。
- debug（2026-10-21T10:47:59Z–10:48:04Z）：`DEBUG_RAW_EXIT=0`，逐字 `test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.42s`。
- release（2026-10-21T10:48:49Z–10:48:52Z）：`RELEASE_RAW_EXIT=0`，逐字 `test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.08s`。
- 拆分后 md5：父 `d074ec8d0d3409cb0ddf165b811d7c4d`、`scan_windows.rs` `4401d811c249561fc6fe6ba54a07d374`、`scan_windows_measurement.rs` `69ad4a95392eff2e0c7dc068b0146c26`。
- **证据等级**：上列数字为执行者自证；独立复核（只读重算，含 `-- --list` 名字集、20 vs 21 的归约次数、`item_revision` 断言）本轮在办，未落地前本节按"执行者自证"计。
- 该文件中两个既有测试函数（178 行、195 行）超 120 行测试硬限，本轮未动（排在其自身验收之后的独立拆分工作）。
- 相关更正：同一批测量里，先前把测量夹具的归约次数写成 21 是**测试期望错误**，真实 `item_revision` 归约数为 20；已改为按窗口断言（`== round` / `== 10 + round`），并把原先"窗口提交需等待并发写者"的用例按其真实行为更名（`window_commit_under_write_lock_contention`）：60 ms 外锁下 `busy_timeout=5s` 的普通 rusqlite 写者等待 67/76/92/105 ms 后 `Ok`，而 `BEGIN DEFERRED`+读+写 的**读→写升级**在 0 ms 内返回 `SQLITE_BUSY`(+5)，`Store::insert_scanned_files`（`crates/store/src/scan_probe_obligations.rs:66`）同样 0–7 ms 返回 busy —— 即升级路径**绕过**了 `crates/store/src/open.rs:247-253` 的 5 s 超时。该现象已记为 Task 19 Step 3 的待办（改 `BEGIN IMMEDIATE` vs 调用方重试的三组 A/B，**只准备未跑**）。

### 2. 更正：索引基线（比 `:500` 的"抽样审计"更完整，但**删索引决策仍未做**）

- 代码侧 `LIBRARY_INDEXES` 共 **51** 条（`crates/store/src/schema/v1/objects.rs`），`docs/database/library.md` 计 47 个 `CREATE INDEX` + 4 个 `CREATE UNIQUE INDEX` = **51** ⇒ 文档与代码零漂移（对照产物 `/tmp/task52-index-audit.md`）。
- 已从 `crates/store/src/schema/v1/library.sql` 与文档中删除 2 条冗余索引：`sessions_user`、`model_sample`（各去掉 1 条与主键/UNIQUE 重复的声明）；新增 `crates/store/tests/schema_redundant_indexes.rs` 作为回归网。
- 口径：**这不等于 Task 20 Step 3 完成** —— 全量逐条"读/写对照"与"删/留"决策仍未收口；上列两条删除是"重复声明"这一类，不涉及查询路径取舍。

### 3. 序列偏差（必须披露，不得读作已完成）

计划 Task 17 Step 3 的门（旧写入口与旧读模型删净）**未收口**，而 Task 18/19/20 的 Step 1（工作负载断言）与 Step 2（release 测量）已先行落地。逐项状态：

- 已提前建立（仅测试与测量，不含生产改动）：`storage_work_bounds`（8 用例）、`storage_query_scale`、`storage_contention`（原 1074 行超限，已按"只搬不改"拆为 337 + `harness.rs` 323 + `fixtures.rs` 365，4 个测试函数最大 78 行，拆分前 `-- --list` 名字集有 `EXIT=0` 基线存档）、`maintenance_runtime`、`data_retention_control`、`data_retention`、`backup_consistency`。
- **完全未开始**：Task 18 Step 3/4/5、Task 19 Step 3/4/5、Task 20 Step 3/4/5。
- **无任何生产性能改动**：本轮不存在 `perf(query)`／`perf(storage)`／`perf(database)` 提交，Task 17 Step 5 门禁未被绕过（计划 L395–L396 仍有效）。
- 因此上文表格 Task 18/19/20 的"无需改生产代码"结论**只是"未做改动"的记录**，不是"经测量后确认无需改动"。

### 4. 上文 §2 清单的逐条现状更正（仅状态变更，不撤销任何仍成立项）

| 上文行 | 原述 | 现状（2026-10-21T11:05:30Z） |
|---|---|---|
| `:529` | `LedgerRow` 587 处、`crates/store/src/ledger.rs` 642 行 | **已推进未收口**：全仓 `LedgerRow` 434 处（换型在办）；`ledger.rs` 385 行；`domain::LedgerRow`（`crates/domain/src/lib.rs:595`）与 `crates/store/src/lib.rs:19` 的 `mod ledger;` **仍在** |
| `:530` | `insert_ledger` 三态与 `finalize_transfer` 仍是活方法 | **已删除**：`crates/store/src` 内 `insert_ledger` 命中 0；`crates/api/src` 内 `insert_ledger`/`finalize_transfer` 命中 0；`ledger.rs` 由 583 行降至 385 行；`crates/store/tests/fixture_publication_equivalence.rs` 209 → 72 行（删去 legacy 对照用例）。**但 Task 3 Step 5 仍未完成**：`get_ledger_by_unit` 需随读模型替换一并删（在办），`domain::LedgerRow` 与 `mod ledger;` 仍在 |
| `:537` 后半 | `data/live/jobs.db` 无身份闸门 | **已加**：`crates/api/src/management/state.rs:233` 在 `jobs::Queue::open` 之前调用 `store::schema::layout::initialize_jobs_layout`，修掉 `expected application_id 1129139252, user_version 1, found application_id 0, user_version 0` 的启动失败。另修 `CRAWLER_MEDIA_*` 误报（`crates/api/src/config.rs:18` `KNOWN_ENV_VARS`）。`app/library/subscribe` 只校验 `application_id`+`user_version`（不校验表/索引）**仍成立** |
| `:539` 前半 | 13 个 `.rs` 超 800 行 | **11 个**（快照）：`api/tests/jellyfin/catalog.rs` 1833、`api/src/probe_manager.rs` 868、`downloader/src/qbit.rs` 835、`api/tests/management/cli.rs` 820、`api/src/http/mod.rs` 818、`api/src/media_server_provider/items.rs` 817、`api/tests/management/downloaders.rs` 814、`store/tests/library_query_filters.rs` 813、`api/src/http/sites.rs` 813、`store/src/library_write/reduce.rs` 811、`store/tests/data_retention.rs` **801** |

`:531`（Task 13 Step 3 无持久 `frozen` 行）、`:532`（无受控停写备份一致性 case）、`:533`–`:534`（Task 11 未收口 / 页内 metadata 仍读现场 NFO）、`:535`（计划点名的路径未创建）、`:536`（trickplay/fonts 路由缺失）、`:538`（测试盲区）**全部仍然成立**，本轮无变化。

### 5. 仍未跑（本轮 Not run，逐项列出，不用概述代替）

`cargo test --workspace`（最新树，尚未重跑）、release 全量构建、真实 NAS、真实 Jellyfin/Infuse/VidHub 客户端、真实浏览器浏览与拖动 seek、100k–1M 规模对照、`commit_playback_event` 生产写入统一、真实 `data/live` 上换型后的端到端验收（`./start-test.sh restart` 尚未在最新树上执行）、以及上文已列的 Task 18/19/20 Step 3–5 与 Task 17 Step 3/4/5。

**本节不改变任何 Task 的完成状态，也不写"达标"。**

### 6. 第四轮裁定与事实更正（Lead 追加，2026-10-21T11:11Z 前后）

#### 6.1 R15：`MediaItemSnapshot.row: LedgerRow` 裁定为「拆字段」而非「边界投影」

`crates/media-server/src/provider/mod.rs:54` 的 `pub row: LedgerRow` 是 `LedgerRow` 在仓库里最后一个生产消费字段。split-worker 提出两条路：(a) 把它换成 `unit: Option<UnitKey>` 并改 media-server 约 15 个消费点；(b) 保留 `row_view(file, unit) -> LedgerRow` 作为「media-server DTO 边界投影」，让 `grep LedgerRow` 只余 1 处生产构造点。

**裁定 (a)。** 依据 L71「迁移全部消费者后删除…**不保留别名**」与 task-62 验收「生产代码 `grep LedgerRow` 归零」；(b) 就是保留旧载体，且会让 task-62 永久无法收口。

接口冻结（已下发给两个 owner，逐字一致）：

```rust
pub struct MediaItemSnapshot {
    pub file: LibraryFile,      // 已存在，不动
    pub unit: Option<UnitKey>,  // 取代 pub row: LedgerRow
    ...
}
```

- 构造规则：`row.season`/`row.episode` 同有同无 ⇒ 皆 `Some(s,e)` → `Some(UnitKey::new(s as i32, e as i32))`；否则 `None`。**必须保留**旧「退化 TV 行 → 整个作品单元、绝不编造 `(0,0)`」语义（`provider/mod.rs` `item_identity()`）。
- 派工：**task-74** = media-server 片（owner playback-event-worker，写范围 `crates/media-server/src|tests`，已从 task-62 提前拉出）；**api 侧构造点**（`crates/api/src/media_server_provider/items.rs:17/616/753`，其中 `row_view` helper 整体删除）并入 task-59（split-worker）。**两侧必须同时落地才编译得过**，窗口期 `cargo check -p media-server` / `-p api` 红属 R14 预期。
- **未决风险（不静默统一）**：旧 `row.path` 取自 `display_path`，而 `crates/store/src/library_query/files.rs` 的 30 列投影用 `path_key`（本文件前述已记录的不一致）。`row.path` → `file.path` 前必须由 split-worker 给出同夹具逐字节 seam 证据（含大小写）；**若不等，先回报 Lead**，作为行为收窄进偏差清单，不许单方面换口径。
- 同时新建 **task-73**（Task 17 Step 3 删旧入口对账台账，只写 `/tmp`，零仓库写、不跑 cargo，owner playback-event-worker，task-74 之后做）。

#### 6.2 R16：`crates/store/tests/data_retention.rs` 801 行是**在飞编辑**，已回到硬限内

上文 `:4` 更正表里「`store/tests/data_retention.rs` **801**」的口径更正：该 801 行是 nfo-worker 于 11:05:30Z 快照时刻**正在写**的中间态（当时未压缩），不是硬限违约。

nfo-worker 实测（自证，非 Lead 复核）：`cargo test -p store --release --test data_retention -- --list` EXIT=0 @ 11:08:50Z → 11 个用例名逐字列出（`attempts_are_deleted_in_the_same_transaction_as_the_job` … `zero_limit_is_rejected`），与基线 `11 passed` 数目一致；`git diff -U0 -- <file> | grep -E '^-.*(fn |#\[test\])'` 输出为空（无删改用例）；`sed` 后 **791 行**；绿跑 `cargo test -p store --release --test data_retention -- --nocapture` **raw EXIT=0** @ 11:09:03Z，逐字 `test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.80s`，新增行 `MEASURE task20/store preview_zero_write db=4096->4096 wal=786952`。声明删掉的注释：`churn_cache` 的 2 行 doc 注释合并为 1 行（语义保留），断言一条未删、未放宽、数值未改。**未拆子模块**，因此没有「拆分前 `--list` 基线」，以名字集合 + 无删改 diff 代替——这一点如实记在此处，不作为等强度替代。

#### 6.3 R17：jellyfin-worker 的 `storage_contention` 拆分行数更正（推翻上一轮「最大 78 行」）

jellyfin-worker 主动更正：上一轮报的「4 个测试 fn 最大 78 行」**错误**；按两种独立方法（大括号配平 / 顶层 `^}` 扫描）交叉验证，拆分后实测为 **68 / 81 / 108 / 28，最大 108**。结论：**文件限 800 未超、函数硬限 120 未超**（拆分判定不变），但第 2、3 个函数超 80 **软限**。Lead 裁定：**现在不重构**（遵守「只搬不改」），超软限记为债；上文任何引用「最大 78 行」的说法以本条为准作废。

同时披露一处证据缺口：该拆分是**原地替换**，旧 870 行版从未提交也无备份，所以「断言条数不变」**没有可复算证据**，可复算的只有名字集 / 行数 / 数值常量；当前 `grep -c assert` = 36。后续同类拆分必须先 `cp` 旧文件存档。

#### 6.4 R18：store 观测计数器 doc 注释的名称漂移（交 ops-worker 顺手改）

Lead 于 11:11:07Z 自查：`crates/store/src/lib.rs:197/201/223/227/238` 的 doc 注释仍在写 `list_ledger` / `ledger_for_media`，但这两个方法已随 task-58 更名（`list_file_coverage` / `file_coverage_for_media`）。**仅是注释漂移**，计数器行为未动；交 ops-worker 在 task-58 内顺手改正（不改数值语义）。

#### 6.5 Lead 自查快照（2026-10-21T11:11:07Z，`crates/store/src`，grep 计数）

`ledger_by_path` 0、`get_ledger` 0、`ledger_for_media_batch` 0、`list_ledger_with_mode` 0、`rename_ledger_path` 0、`get_ledger_by_unit` 0、`map_ledger` 0、`insert_ledger` 0、`finalize_transfer` 0；新 shape `file_coverage_by_path` 6、`file_coverage_by_id` 2、`file_coverage_for_media` 3、`list_file_coverage` 4、`rename_file_path` 1、`map_library_file` 3、`map_file_unit` 6。`crates/store/src/scan_probe_obligations.rs:17` `ScannedLedger` 字段实测为 `file_id/media_id/path/units/resolution/codec/hdr/quality_source/confidence/filter_score/source_version/source_digest/size_bytes/created_ns/modified_ns/observations`（`row`/`season`/`episode` 已删）。`crates/store/src/ledger.rs` 385 行；`crates/store/src/lib.rs:19 mod ledger;` **仍在**（task-62 处理）；全仓 `LedgerRow` 404 处（换型在办）；`domain::LedgerRow`（`crates/domain/src/lib.rs:595`）**仍在**。

#### 6.6 窗口期红归因扩大（R14 追加证据）

jellyfin-worker 11:06:28Z 独立观测 `cargo test -p store` **`STORE_EXIT=0`**；同一时刻起 `cargo test -p api --test storage_contention` 仍 `EXIT=101`，**360 条 error 全在 `crates/api/src/**`**（FileCoverage 迁移缺字段 `media_id`/`path`/`id`/`season`），其自身文件 0 处（raw 存档 `/tmp/t60/list_after_err.txt`）。R14 的「窗口期红只在 `crates/api/src/**`」归因成立并扩大到这个量级；两个 owner 均正确地拒绝改他人文件。

**本节同样不改变任何 Task 的完成状态，也不写"达标"。**

### 7. 第五轮裁定：R19 七片并行换型（Lead 追加，2026-10-21T12:31Z）

**触发**：12:10Z 前后 Lead 自查发现约 56 分钟无仓库写入，而 board 上 6 个任务处于 `in_progress`、8 名成员全部 `inactive`——`crates/api/src` 的 `LedgerRow → FileCoverage` 换型被写成了「一条长串行链」，链上任何一人停下，其余人无活可干。**处置**：不再按文件串行，改为**按目录切片并行**，每片一人、写范围互斥（advisory 但本轮实测无人越界），接口先由 Lead 冻结再各自开工。规则全文 `/tmp/lead-rules-ledgerrow-to-filecoverage.md`（只写 `/tmp`，零仓库写）。

#### 7.1 切片表（写范围以 board 为准，逐字）

| 片 | task | owner | 写范围 | 12:31Z 状态 |
|---|---|---|---|---|
| S1 | task-59 | split-worker | `crates/api/src/media_server_provider`、`watch_ledger.rs`、`playback_read.rs`、`claim.rs`、`library_presence.rs`、`auto_resolve.rs`、`watch_intake.rs`、`library_cover_service.rs`、`management`、`fs_watcher` | 在办；`crates/media_server_provider/src` 独立 crate 已 0 命中 |
| S3 | task-75 | jellyfin-worker | `crates/api/src/probe_manager.rs`、`crates/api/src/probe_manager`、`crates/api/src/http/library/probe.rs` | 在办 |
| S4 | task-79 | nfo-worker | `crates/api/src/scrape_metadata`、`fingerprint_job{,.rs}`、`marker_resolver.rs`、`scan_operations`、`fanart_artwork.rs` | 在办 |
| ST | task-76 | ops-worker | `crates/store/tests`、`crates/store/src/test_support.rs`、`.../user_summary/tests.rs`、`.../probe_unit_retirement/tests.rs`、`crates/store/src/library_write/transfer.rs` | 在办 |
| H1 | task-77 | media-alias-worker | `crates/api/src/http/playback.rs`、`http/playback`、`http/playback_views` | 在办 |
| H2 | task-78 | meta-split-worker | `crates/api/src/http/subscriptions`、`subscription_depth{,.rs}`、`http/library`、`library_scan{,.rs}` | 在办 |
| H3 | task-80 | lead | `http/{library_chapters,reidentify,library_organize,library_duplicates,collections,library_housekeeping,file_delete}.rs` | **本轮已完成，见 7.4** |
| MS | task-74 | playback-event-worker | `crates/media-server/src`、`crates/media-server/tests` | 在办；12:31Z `crates/media-server/src` 已 0 命中 |

#### 7.2 换型约定的六条铁律（R19 A–F，逐片下发，逐字一致）

- **A 载体（carrier）**：任何原先「一行 = 一份文件 + 一集」的 `LedgerRow` 载体换成 `domain::FileCoverage { file: LibraryFile, units: Vec<UnitKey> }`；文件级事实统一读 `coverage.file.x`，集级判定统一读 `coverage.units`。
- **B 文件级事实**：`path/id/media_id/resolution/codec/hdr/...` 一律来自 `.file`，**不得**再从集级字段读。
- **C 单集**：`row.season == Some(s) && row.episode == Some(e)` → `coverage.units.iter().any(|u| u.season == s && u.episode == e)`；整作品单元 = `UnitKey::WHOLE`(`(-1,-1)`)/`is_whole()`，**绝不写 `(0,0)`**；`u32::try_from(unit.season).ok() == Some(query)` 复现旧 `row.season == Some(s)`（负数 ⇒ `None` ⇒ 不匹配）。
- **D 多集**：生产端按 unit 扇出；消费端若只吃单集，用 `units.first()` 并 `tracing::warn!`（**不许静默丢弃**）。活库实测 files=121 / units=121 / multi-unit=**0**，故扇出暂缓，但约定先冻结。
- **E 退化默认值逐字保留**：`row.season.unwrap_or(1)` → `FileCoverage::season_or_one()`；`row.season.is_some()` → `FileCoverage::is_episodic()`。**禁止**在换型时顺手「修正」这些历史口径。
- **F 接口名稳定、只换类型**：`pub`/`pub(crate)` 函数的**参数名与结构体字段名保持旧标识符**（如 `row`、`rows`、`ProbeUnit.row`、`EpisodeCaptureRequest.row`），类型改 `domain::FileCoverage`；文件内局部绑定可改名（`coverage`）以求可读。目的：把「换型」与「改名」两件事分开，让 diff 与调用点可机械核对。

#### 7.3 已冻结接口（下发即生效，改签需 Lead 追加更正）

```rust
// S1/S4 共用
pub async fn resolve_item_chapters(state: &ApiState, row: &domain::FileCoverage,
                                   media: &Media, theintrodb: Option<TheIntroDbClient>) -> Vec<ChapterMarker>;
pub async fn force_refresh_item_chapters(state: &ApiState, row: &domain::FileCoverage, /* … */) -> Vec<ChapterMarker>;
// S4
pub(crate) fn show_root(path: &Path, coverage: &domain::FileCoverage) -> PathBuf;
pub(crate) async fn scrape_tv_sidecars(state: &ApiState, media: &Media, show_root: &Path,
                                       coverages: &[domain::FileCoverage]) -> …;
fn nfo_from_episode(coverage: &domain::FileCoverage, /* … */) -> …;
fn live_groups(coverages: &[domain::FileCoverage]) -> …;
// S3
pub struct ProbeUnit { pub row: domain::FileCoverage, /* … */ }      // 字段名保留 row
impl CaptureGate { fn work_control(&self, row: &domain::FileCoverage) -> …; }
pub struct EpisodeCaptureRequest { pub row: domain::FileCoverage, /* … */ }
// MS（R15 已冻结）
pub struct MediaItemSnapshot { pub file: LibraryFile, pub unit: Option<UnitKey>, /* … */ }
```

**每集载体约定（冻结）**：`ProbeUnit.row`、`EpisodeCaptureRequest.row`、指纹采样 `Vec<(FileCoverage, Vec<u32>)>` 里的 coverage 一律是**单集载体**（`units` 恰一个；整作品 = `[WHOLE]`）。多集扇出**推迟**，消费端退化路径必须 `warn!`。

#### 7.4 H3（task-80，Lead 自做）本轮完成情况

实测（12:27–12:31Z）：7 个文件 `LedgerRow` 命中**全部为 0**（`grep -c` 逐文件：`collections.rs` 0、`file_delete.rs` 0、`library_housekeeping.rs` 0、`library_duplicates.rs` 0、`reidentify.rs` 0、`library_chapters.rs` 0、`library_organize.rs` 0；换型前共 16 处）。行数 626 / 512 / 586 / 176 / 354 / 349 / 183，**均在 800 硬限内**；md5 依次 `5972c953144d76c1d35ea0de0cb7ab75`、`7ab7d1c02a1e89ac30a8e0bfbb70be0b`、`b54a8eb98897aeee060b40daa42391de`、`9a09624170b6805154be5e51ae1ab1ed`、`9932ed58b17c8091ee245af578ab7ab1`、`e05eb573158ed65620277258a97db9cf`、`cde26e15fe77bfb6524868b17708bd23`。

rustfmt（`--edition 2024 --check`，逐文件对比 HEAD）：`library_storage.rs` HEAD 6 处差异 / 改后 6 处（**我的新增方法 0 处新差异**，未顺手重排既有陈旧格式）；`library_organize.rs` HEAD 4 / 改后 4（同一批 hunk，仅行号平移）；`library_chapters.rs` HEAD 0 / 改后 4（**我的新代码引入**）→ 已 `rustfmt` 收敛，改后 `--check` **EXIT=0**；`reidentify.rs` HEAD 0 / 改后 0。

H2 已落地 `crates/api/src/http/library/assets.rs:16 pub(crate) fn coverages_in_library(...)`，H3 的 `library_organize.rs` 与 `library_chapters.rs` 改用该函数（`files_in_library` 只返回 `LibraryFile`、会丢 units，故不再使用）。

**H3 判定点（只报不猜）**：① `is_episodic()` 对**空 units** 返回 false（视同整作品），与旧「无 season 即整作品」一致但新状态（有文件无单元行）此前不可表示；② `season_or_one()` 多集时取最大 season（与 `preferred_coverage` 的偏好一致），仅在新多集状态与逐行旧行为分叉（活库 multi-unit=0）；③ `library_housekeeping.rs` 的复校验现在比较**完整 FileCoverage（含 units）**，比旧的「比文件事实」更严；④ `library_duplicates.rs` 的分组键由 `(season, episode)` 变为 `coverage.units`（整作品 → `(null,null)`，**空单元集也归为整作品**，保证不丢文件）；⑤ `remove_rows` 的路径转换对非 UTF-8 路径是 lossy 的；⑥ 兄弟单元查询出错时**失败关闭**（拒绝删除）；⑦ `extras` 走 typed `LedgerId::from_str` 后能命中 DB 里的带连字符键，而旧的 compact 字符串在这里曾漏配；⑧ `reidentify.rs` 现在能区分「id 畸形」（`LedgerId` 解析失败）与「文件不存在」（旧代码两者都报 not found）。

**本节不改变任何 Task 的完成状态，也不写"达标"；api crate 在 S1/H1/H2/S3/S4 全部落地前仍是红的（预期）。**

#### 7.5 跨片裁定与 store 收口确认（Lead 追加，2026-10-21T12:33Z–12:40Z）

**边界裁定（防两片同改一文件）**
- `crates/api/src/marker_resolver.rs` → **task-79（nfo-worker）**。board 上 task-59 的写范围不含它、task-79 含它；split-worker 曾声明归属，已按 board 纠正并通知两人（若 split-worker 之前已改过该文件，按「越界自报 + Lead 兜」处理，不许自行回滚，避免与 S4 打架）。
- `row_view` 的 3 处外部调用（`crates/api/src/http/playback.rs:720`、`http/playback/selection.rs:80`、`:151`）→ **task-77（media-alias-worker，H1）**，按 R15 改 `&file` + `unit: Option<UnitKey>`，**不许补任何 shim**（L71）。
- `crates/api/src/http/library/probe.rs:16/25/59` → **task-75（jellyfin-worker，S3）**；它是当前最扎眼的跨范围红（S1/H2 都在等它编译通过）。
- `crates/api/src/library_cover_service.rs:192` 的 `CoverSnapshot.rows: Vec<domain::LedgerRow>` → **S1**；最小改法即 `Vec<domain::LibraryFile>`（与 H2 的 `http/library/cover.rs::render_cover_jpeg(files: &[domain::LibraryFile])` 形状一致）。
- `crates/api/src/http/playback_views/{gallery,favorites,up_next}.rs` 的 `preferred_coverage: Option<domain::LedgerRow>` → **H1**，改用 `assets.rs:125` 的 `preferred_coverage<'a, R: Borrow<domain::FileCoverage>>` 泛型版本，**不要复制实现**。

**`path_key` vs `display_path`（R6 的最终裁定）**：**不统一**。split-worker 已给出 seam 证据——`data/live/library.db` 121 行上 `display_path <> path_key` = **0**、大小写不敏感不等 = 0；legacy `data/live_legacy_backup/library.db` 的 `ledger` 121 行 = 118 双非空 + 3 双 NULL、partial **0** 行。故 `LedgerRow.path`（取 `display_path`）→ `LibraryFile.path` 的换型在**活库上逐字节等价**。唯一真实收窄：新 schema 的 `CHECK((season=-1 AND episode=-1) OR (season>=0 AND episode>=0))` 不再表示 legacy 的 `(Some(season), None)` 类 partial 行——本库 0 例，登记为「schema 层不再表示」的偏差，不做兼容（L17）。

**`ScannedLedger` 定型（供 task-59/61 逐字使用）**：16 字段、无 `Default`：
`file_id: LedgerId`、`media_id: MediaId`、`path: String`、`units: Vec<UnitKey>`、`resolution/codec/hdr: Option<String>`、`quality_source: QualitySource`、`confidence: Confidence`、`filter_score: Option<i32>`、`source_version: String`、`source_digest: Option<String>`、`size_bytes/created_ns/modified_ns: Option<i64>`、`observations: Vec<crate::metadata::NfoObservation>`。注意 `file_id`（旧 `id`）、`path: String`、`units`（整片 = `UnitKey::WHOLE`）。

**task-58 / task-76 收口（Lead 独立复核，非转述）**：Lead 于 12:33:44Z 自己跑 `cargo test -p store --lib` → **EXIT=0**，逐字 `test result: ok. 34 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.19s`；Lead 自查 `grep -rn LedgerRow --include=*.rs crates/store` = **0**（`src` 与 `tests` 均 0，HEAD 曾有 33 个文件）。ops-worker 自报 `cargo test -p store --no-fail-fast` EXIT=0 @20:32:02（本地时）= passed **356** / failed 0 / ignored 1 / 70 个 `test result:` 行 / 69 target，与改动前基线**逐字相等**，唯一差异是 task-58 R3 改名 `ledger_for_media_batch_…` → `file_coverage_for_media_batch_…`。

**ops-worker 自查出的真实语义破口（值得单独记）**：`crates/store/src/library_deletion/mod.rs:164` 早先把 `self.get_file_units(row.id)?` 误换成 `observed.units`。旧 `get_file_units` 读**原始 `library_file_units` 行**（无单元 = 空集），而 `list_file_coverage()` 对无单元文件走 LEFT JOIN 合成 `UnitKey::WHOLE`，于是 snapshot coverage 由 `[]` 变 `[(-1,-1)]`，`validate_removal_rows` 的 `current_units != snapshot_units` 成立并抛 `Protected("删除快照台账身份或归属已改变")`；复现：`crates/store/tests/ledger_presence.rs:198` 由 6 passed 掉到 5 passed/1 failed。已回改为 `get_file_units(file.file_id)?`，修后该 target EXIT=0 `ok. 6 passed`，未放宽断言。**结论：`FileCoverage` 的「无单元 ⇒ WHOLE」合成语义与「原始单元行空集」是两件事，任何把 `get_file_units` 换成 coverage 合成路径的改法都必须单独证明等价。**

**H2（task-78）片内归零**：`crates/api/src/http/{subscriptions,subscriptions/**,subscription_depth.rs,subscription_depth/**,library/**(probe.rs 除外),library_scan.rs,library_scan/**}` 的 `LedgerRow` = **0**（12:33Z 实测 `grep -rc … | grep -v ':0$'` 只剩 `http/library/probe.rs:4`，属 S3）。`library_scan.rs::scrape_library_metadata` 保持 `coverages_in_library`（= `list_file_coverage()` + 库归属过滤），**未**换 `file_coverage_for_media`（保住 Task 18/19/20 的 `root_ledger_scans` vs `media_ledger_reads` 缝）；两处 S4 冻结调用在 task-79 落地前报红，代码内已标「归属 S4」。

**本节同样不改变任何 Task 的完成状态，也不写"达标"。**

#### 7.6 收敛测量、漏切发现与延期裁定（Lead 追加，2026-10-21T12:41Z–12:50Z）

**收敛测量的口径（Lead 单点，唯一权威）**：`cargo check -p api --lib --message-format short`，计数用 `grep -oE '^crates/[^:]+:[0-9]+:[0-9]+: error' | cut -d: -f1 | sort | uniq -c`（**行数**，不是「错误个数」）。每次测量前后各贴一次 `pgrep -fl -x cargo`，两次都为空才认；并发窗口内的数据一律作废（此前 `verify-ops-task58-*`、`verify-ops-task43-*` 即因此作废）。

| 时刻（UTC） | 退出码 | error 行数 | 涉及文件 | 日志 |
|---|---|---|---|---|
| 12:37:01 | 101 | 276 | 55 | `/tmp/lead-h3-check.log` |
| 12:47:17 | 101 | 150 | 28 | `/tmp/lead-r19-check-1247.log` |

12:47:17Z 的 28 文件分布（逐文件 error 行数）：`http/playback_views/up_next.rs` 17、`http/library_artwork.rs` 13、`fs_watcher.rs` 10、`http/library_admin.rs` 10、`http/playback/session.rs` 10、`playback_views/gallery.rs` 9、`playback_views/favorites.rs` 9、`library_presence.rs` 7、`http/subscription_depth.rs` 7、`http/playback.rs` 7、`playback_read.rs` 6、`auto_resolve.rs` 6、`management/subscribes/run.rs` 5、`worker/transfer/run.rs` 4、`http/playback/heartbeat.rs` 4、`http/playback/chapter_cache.rs` 4、`http/playback_logs.rs` 3、`fs_watcher/presence.rs` 3、`claim.rs` 3、`watch_ledger.rs` 2、`watch_intake.rs` 2、`library_cover_service.rs` 2、`http/title_ref.rs` 2、`media_server_provider/enrichment.rs` 1、`http/playback_activity.rs` 1、`http/playback/episodes.rs` 1、`http/library/assets.rs` 1、`delivery.rs` 1。**`probe_manager*` 与 `http/library/probe.rs` 为 0**（§7.1 的 S3 片已收口）。

**§7.1 切片表的覆盖漏洞（本轮真实发现，已补片）**：按 12:37Z 的 55 文件清单逐文件比对 §7.1 的 S1/S3/S4/ST/H1/H2/H3/MS 归属，**有 14 个文件不属于任何一片**，合计 ~67 条 error：`http/library_artwork.rs`、`http/library_admin.rs`、`subscribe_library_read.rs`、`watch_scrape.rs`、`http/search.rs`、`http/media.rs`、`worker/transfer/run.rs`、`visible_scope.rs`、`http/playback_logs.rs`、`http/library_delete.rs`、`scan_probe_dispatch.rs`、`http/title_ref.rs`、`http/playback_activity.rs`、`delivery.rs`。**这是 §7.1 切片划分本身的遗漏，不是执行偏差**：原表按「任务编号」切片，而 `LedgerRow` 的调用点分布在按业务命名的零散文件里，任何按任务圈定的范围都会漏。已立 **task-83（该 14 文件，owner ops-worker）** 补齐；`docs` 侧不改 §7.1 原文（保持计划原样），以本节为补丁记录。

**task-74（media-server，playback-event-worker）收口**：Lead 于 12:46:38Z 独立复跑 `grep -rn "LedgerRow" crates/media-server/` → **无输出、exit=1**（自报同为 0，含 tests）。自测 37 项（6+10+3+4+1+3+3+1+3+3）逐字 `test result: ok. …; 0 failed`，`cargo check -p media-server` EXIT=0。**一条曾被怀疑的风险已排除**：`dto.rs:77/112` 的 `unit.map(|u| u.season as u32)` 在 `Some(UnitKey::WHOLE)` 下会产出 `S4294967295E…`，但 `crates/api/src/media_server_provider/items.rs:29 snapshot_unit()` 把任何负坐标折成 `None`，media-server 侧不可能收到 `Some(WHOLE)`。此结论依赖该函数是唯一构造入口，已要求补证。

**task-75（S3：`probe_manager*` + `http/library/probe.rs`，jellyfin-worker）收口**：12:41:24Z 该 scope 0 error、`grep -rn LedgerRow <S3 scope>` exit=1。三项偏差登记：
- **F 偏差（改名）**：`enqueue_probes_for_rows(state, &[LedgerRow])` → `enqueue_probes_for_files(state, &[LibraryFile])`，并新增 `enqueue_probes_for_paths`。R19-F 严格读法要求旧标识符不变；此处载体由「行」变为「已发布文件」，改名成立，但**按 F 偏差登记**（同文件 `enqueue_probes_for_paths` 未改名）。
- **静默丢弃修复**：新实现经 `file_coverage_by_path(path.display())` 读回，读不到即 `filter_map(ok().flatten())` **静默跳过**，旧实现用调用方内存行、无此丢弃。已要求加 `tracing::warn!`（打 path + media_id），**只加日志、不改控制流**（AGENTS.md「不得写静默代码」）。4 个调用点 `claim.rs:386`、`worker/transfer/run.rs:565`、`management/subscribes/run.rs:596`、`http/library_admin.rs:421` 均在 `drop(store)` / `commit_*` 返回之后，**不构成自锁**，已要求 owner 逐点自核。
- **`probe_manager.rs` 868 → 944 行**（越 800 硬限）：本轮新增的 5 个 `projected_*` + `ledger_id` 一族 + `#[cfg(test)] synthetic_coverage` 先**搬出**主文件（`probe_manager/projection.rs`）使主文件回到 ≤868（与 HEAD 持平）；真正的 868→≤800 切分立 **task-84**，**硬门：api `--lib` 归零后**才动（红树里做结构切分无法编译验证）。同理 `probe_manager/probe.rs:353`（125 行）与 `probe_manager.rs:333 unused_mut` 为 HEAD 同形的既有债，本轮只搬不改。

**函数长度口径作废声明（全组强制）**：旧口径（先前的子代理自报「最长生产函数 78 行 / 34 行」）**作废**——它不剥离注释与字符串、也不配平 `json!` 与闭包的 `{}`，实测同一文件真实最长生产函数为 **143 行**。新口径：**剥离注释/字符串字面量，按 `{}` 配平（闭包、`json!` 感知）后计行**；报告里必须附测量命令。

**H3 的 `E0432` 根因（值得单记）**：`crates/api/src/http/library.rs:23` 声明的是**私有** `mod assets;`，同目录兄弟模块 `library_organize.rs` 无法用 `crate::http::library::assets::*` 直达。修法是在 `:26-29` 加 `pub(crate) use assets::{…}` 白名单再导出（现 29 个名字），**不是**把 `mod assets` 改成 `pub`（那会放宽面）。任何新增消费者（如 H1 用 `preferred_coverage` 泛型版 `assets.rs:125`）必须先确认该名字在白名单里，否则又是一次 E0432。

**H2 残留（回到 H2 owner 收口，不新开片）**：`http/subscription_depth.rs:272-279` 7 条（`row.media_id/season/episode/resolution/codec` 读的是 `FileCoverage`）+ `http/library/assets.rs:161` 1 条（`poster_beside(path: &str, …)`，`crates/api/src/poster.rs:6` 签名**不动**，改调用侧 `&file.path.to_string_lossy()`）。`subscription_depth.rs:238-380 wanted_json_with_air_dates` = **143 行**且与 HEAD 逐字相同 ⇒ 预存在债，**本轮不拆**，立 **task-82**，硬门同在 Task 17 Step 5 之后。单元谓词统一用 `units.iter().any(|u| u32::try_from(u.season).ok() == Some(season) && …)`（R19-C），**不得用 `units.first()`**；`404/409`、`read shape` 的语义等价理由逐点写在回报里。

**task-81 目录归属裁定（option (a) 通过）**：`crates/api/tests/management/**` 与 `subscription_e2e/**` 里约 55–60 处旧读访问器调用点**属于 task-81**（与 `crates/api/tests/*.rs` 同批文件、同一条读缝、同一次 §1-F 改名）。四个必报类别：① `.len()` 类（新 `list_file_coverage().len()` = **文件数**，旧 = file×unit 行数；夹具 1:1 ⇒ 应逐点等值，不等值即停下报 Lead，**不许改断言数值**）；② 索引/`remove(0)`/取序类（`transfer_retry/scenarios.rs:25/61/69`、`download_ownership_review.rs:707`、`subscription_e2e/golden_path.rs:36`、`harness.rs:416`，逐处给出取到的**哪个文件**为证）；③ `row.path: String` → `coverage.file.path: PathBuf`（比较/排序用 `to_string_lossy()`，非 UTF-8 下序可能不同，全 ASCII 夹具等价需注明）；④ `wait_for_restored_ledger`（`strm_directory_deletion.rs:487`）返回 `domain::FileCoverage`。`get_ledger_by_unit` **无替代**，遇到即报、不得自造 helper；禁止任何 alias/投影/兼容包装（L71）。

**本节不改变任何 Task 的完成状态，也不写"达标"；api crate 仍未绿（12:47:17Z：150 条 / 28 文件），S1/H1/H2(残留)/task-83 落地前保持红是预期。**

#### 7.7 第二轮收敛测量与四片收口（Lead 追加，2026-10-21T12:51Z–13:05Z）

**收敛测量续（Lead 单点，同一口径，前后 `pgrep -fl -x cargo` 均为空）**：

| 时刻（UTC） | 退出码 | error 行数 | 涉及文件 | 日志 |
|---|---|---|---|---|
| 12:51:33 | 101 | 103 | 24 | `/tmp/lead-r19-check-2102.log` |
| 12:57:56 | 101 | **47** | **7** | `/tmp/lead-r19-check-1258.log` |

12:57:56Z 的 7 个文件**全部属于 H1**：`http/playback_views/up_next.rs` 17、`playback_views/gallery.rs` 9、`playback_views/favorites.rs` 9、`http/playback.rs` 6、`playback/heartbeat.rs` 4、`playback/session.rs` 1、`playback/episodes.rs` 1。**S1(task-59)、S3(task-75)、S4(task-79)、H2(task-78)、H3(Lead)、task-83(ops-worker) 六个切片在同一窗口内全部 0。** 独立旁证（不依赖 cargo）：`grep -rn 'LedgerRow' crates/api/src --include=*.rs` = **22 条命中，逐条落在 H1 的 7 个文件里**（`episodes.rs:4/58/77/104`、`heartbeat.rs:120/228/485`、`gallery.rs:39/40/103/114`、`favorites.rs:54/75/84/172/215`、`up_next.rs:106/304/305/343/360`），其余 api/src 文件 0。**即：api/src 的可编译性与 `LedgerRow` 清零现在与 H1 单一 owner 完全重合，其余 7 个写入者已无 api/src 侧待办。** 并发窗口作废规则不变：meta-split-worker 12:59:23Z 自测读到 43 条（其两文件 0），与 Lead 12:57:56Z 的 47 条差值落在 H1 与 `media_server_provider/enrichment.rs`（后者由 S1 在两次测量间修掉 1 条），两窗口各自自洽、互不作废。

**task-73（Task 17 Step 3 预备：删旧入口逐入口对账台账，playback-event-worker）收口**：产物 `/tmp/task17-step3-deletion-ledger.md`（114 行），对 13 项旧入口（`ledger_by_path`/`get_ledger`/`ledger_for_media`/`ledger_for_media_batch`/`list_ledger`/`list_ledger_with_mode`/`rename_ledger_path`/`get_ledger_by_unit`/`insert_ledger*`/`finalize_transfer`/`MediaItemSnapshot.row`/`domain::LedgerRow`/`crates/store/src/lib.rs 的 mod ledger;`）逐项给出旧签名、旧调用点分布、换型后新入口。**明确结论：当前删除前置条件不满足**（api/src 仍有 H1 的 47 条 + `crates/api/tests/**` 46 处 `LedgerRow`），并列出若此刻强删 `domain::LedgerRow` 的跨 crate 级联报错预估。零仓库写入、未跑 cargo。该台账内含一份**自发的方法论警告**：grep 零匹配可能来自 `use domain::LedgerRow as FileRow` 别名、同名非目标函数（如 `crates/api/src/cli/lists.rs:249 list_ledger`）、宏遮蔽与文档注释干扰 ⇒ 单靠 grep 不能证明删除安全，已立 **task-86** 做独立复核。

**task-78（H2 收口）**：meta-split-worker 12:59:23–12:59:33Z 自测 `http/subscription_depth.rs` 与 `http/library/assets.rs` **各 0 error**，`LedgerRow` exit=1，`wc -l` 650/169，两文件 `73 insertions(+), 41 deletions(-)`。**一处 Lead 处方被证伪并纠正（记在 Lead 头上）**：我在 §7.6 对 H2 残留写的单元谓词是 `u32::try_from(u.season).ok() == Some(season)`，但 `crates/api/src/open_coverage.rs:9` 的 `season`/`episode` **本身就是 `Option<u32>`**，`Some(season)` 造成 `E0308 expected u32, found Option<u32>`（`:274:66`、`:275:74`）。执行者改为 `u32::try_from(u.season).ok() == season` 系唯一正确形式；等价性由 `sync_coverage`（`crates/store/src/library_write/publish.rs:635`）整片写 `(-1,-1)` ⇒ `try_from(-1).ok()==None` 只匹配 `(None,None)`，与 HEAD 整片行 NULL 一一对应所保证。**R19-C 的统一写法须按接收端形参类型二选一，不得机械套用 `Some(...)`。**
**并发副作用登记**：`wanted_json_with_air_dates` 由 HEAD 的 143 行变为 **145 行**（+2，仅 `.find` 谓词容纳 `units.iter().any` 内层闭包，无语句移动）。**task-82 的验收目标据此更新为 145 → ≤120**，并须证明字段名/顺序/值与 `null` 语义逐字不变；本轮**不**在红树里做这次重构。

**task-75（S3）二次收口与偏差 G 的更正**：`crates/api/src/http/library/probe.rs` 原不在 task-75 的 `writeScopes` 内，但它是 Lead 12:31Z 追加进该 owner 的指令范围 ⇒ **看板已 amend**（`task-75` rev 4，writeScopes 增列 `crates/api/src/http/library/probe.rs`、标题同步）。这不是执行者越界，是 Lead 派工单与指令不一致，责任在派工侧。同步确认其静默丢弃修复落地形状：新增 `committed_coverage(store, path, media_id) -> Option<FileCoverage>`，`Ok(None)` 与 `Err` 两分支各一条 `tracing::warn!`，`enqueue_probes_for_files`/`enqueue_probes_for_paths` 的锁作用域、迭代顺序、`.count()` 尾与旧 `.ok().flatten()` 一致；4 个调用点（`claim.rs:388`、`worker/transfer/run.rs:571`、`management/subscribes/run.rs:600`、`http/library_admin.rs:454`）逐个自核为「不持锁」。`probe_manager.rs` 866→**865**（HEAD 868），`projection.rs` 78 行、`test_support.rs` 133 行为新文件，搬迁后 `pub(super)` → `pub(crate)` + `pub(crate) use` 再导出（`pub(super)` 会 `E0364`），有效可见性仅 crate 内部，**不构成 L17 的兼容入口**。

**task-84 的开工门被 Lead 放宽（裁定，非执行者请求）**：原写「api `--lib` 归零后才动」，现改为**立即开工**。理由：task-84 是纯搬迁 + `probe_manager.rs` 865→≤800 的**尺寸**整改，与被搬迁代码能否编译无关；其 owner 已用「`cargo check -p api --lib` + 只 grep 自身路径」方法在红树里连续取得两次可信的「我范围内 0 error」窗口。等待全绿只会把尺寸整改排到关键路径之后，而 AGENTS.md 的硬限要求「下次触碰就拆」。附加约束：全程**只允许一次**收尾 `cargo check -p api --lib`，跑前 `pgrep -fl -x cargo` 必须空、忙则重试而非并发；`probe_manager/probe.rs:353 probe_tracks_and_store`（125 行 >120，HEAD 同形）本轮**只做纯抽取**降到 ≤120；报告只写「我范围内 N error」，**禁止写成 api 绿**。

**task-81 的收敛窗口被 Lead 关闭（裁定）**：`cargo check -p api --tests` 必须先编过 lib，而 lib 现有 47 条 error（全 H1）⇒ 此刻开窗只能得到「47 条 lib 错误 + 级联噪声」的**不可信窗口**，并长时间占 target 锁与 H1 自证冲突。**窗口改在 H1 归零后由 Lead 单点开**，落在 `crates/api/tests/management/**` + `subscription_e2e/**` 的错误逐条回派；该两目录 `--tests` 为 0 后 meta-split-worker 才 CAS complete，cargo 项写「未跑」。**口径更正**：Lead 先前给的访问器基线「55 行 / 24 文件」**作废**，以执行者实测 **46 行 / 22 文件**为准（Lead 那份很可能把 `crates/api/tests/support/**`——metadata-worker 的 seam——算进来了，属 Lead 口径污染）。

**task-86 立（独立写范围与证据审计，playback-event-worker）**：只读、零仓库写入、禁一切 cargo。三部分：① `git status --porcelain` 现有 **316 条**逐文件映射到各 task 的 `writeScopes`（看板为权威），输出「无主 / 多主 / 有主」并反向核对每个 `in_progress` task 的 scope 是否真被写过；② 每个 completed task 点名的 `/tmp` 证据是否存在且非空；③ 「达标」措辞扫描 + 计划复选框计数（应为 `100 / 0`）。**动因**：本轮 8 个写入者并发改同一工作树，L17「零跨范围改动」与 L71「无别名/投影/兼容桥」目前**没有任何独立证据**，只有各作者自述。

**本节同样不改变任何 Task 的完成状态，也不写"达标"；api crate 仍未绿（12:57:56Z：47 条 / 7 文件，全在 H1）。**

**本节新增的「未跑」项**：`cargo check -p api --tests`（Lead 单点，等 H1 归零）、`cargo test -p api`、`cargo test --workspace`、task-84 的收尾自证、task-82 的拆分与前后对照、task-86 的全部 cargo 类验证（按任务书不跑）、task-60 的 Step 2 baseline（继续 parked，等 api `--lib` 绿且无并发 cargo）。

## §7.8 第三轮收敛、S1/task-84/task-81 收口与 `FixtureFile` 裁定（13:09Z–13:14Z）

**③ 收敛测量（Lead 单点 + 第三方独立窗口，`pgrep -fl -x cargo` 前后皆空）**

| 时刻（UTC） | 执行者 | 命令 | 退出码 | 结果 | 日志 |
|---|---|---|---|---|---|
| 13:09:33 | split-worker | `cargo check -p api --lib --message-format short` | 101 | **19 error** / 2 文件 | `/tmp/t59_r5_check8.log` |
| 13:11:36 | Lead | `grep -rn LedgerRow crates/api/src` | 1（无匹配）… **7 命中** | 2 文件 | 本会话 |
| 13:08:12→13:08:23 | jellyfin-worker | `cargo check -p api --lib` | 101 | 19 error / 2 文件 | `/tmp/task84` 证据 |

三者互证且同源：19 条 error **全部**落在 `crates/api/src/http/playback_views/up_next.rs`（17）与 `favorites.rs`（2）；Lead 的 grep 在同两文件读到 7 条 `LedgerRow` 类型标注（`up_next.rs:106/304/305/343/360`、`favorites.rs:180/223`）——**19 条 error 与 7 条类型标注是一一因果**（7 处标注换型后，其余 12 条字段访问/集合元素/Borrow/`&LibraryFile` 期望点级联自消）。12:57:56Z 窗口中 `gallery.rs` 9 / `heartbeat.rs` 4 / `playback.rs` 6 / `session.rs` 1 / `episodes.rs` 1 共 21 条**已由 H1 在同一轮修掉**。**api crate 仍未绿（19 条 / 2 文件，全 H1）。**

**task-59（S1）收口（rev 6 completed）**：12 文件切片在 check7/check8 两个窗口 error=0，`LedgerRow` grep=0；仅 2 条 HEAD 既存 warning（`library_presence.rs:8`、`management/subscribes/run.rs:14`）。三项交付按 Lead 要求补齐：① **读缝确认**：`library_cover_service.rs:194 files_in_library` 位于 `:188 pub fn snapshot(state, id)` 体内、**不在任何循环**，4 个调用点（`:86/119/132/153`）全为单库；被调链 `coverages_in_library`（`http/library/assets.rs:8/16`，H2）内 `list_file_coverage()` **恰好 1 次**。现状 `crates/store/src/ledger.rs:137` 与 `HEAD:crates/store/src/ledger.rs:176` 是**同一公式** `root_ledger_scans.fetch_add(max(FullscanStep,1))` ⇒ **`root_ledger_scans` 不漂，Task 18/19/20 的该断言不受本次迁移影响**（此结论推翻「换型可能改变全表扫描计数」的担忧）。② **`LedgerPresenceObservation.row → file` 全仓引用点表**：定义/消费在 `crates/store/src/ledger_presence.rs`（task-76），构造点在 S1 的 `api/src/library_presence.rs:188/209/234` 与 `api/src/fs_watcher/presence.rs:22/23/78/91`；`grep -rn "obs\.row\|observation\.row" crates/` **无命中**；其余 `row:` 字面量属 `ProbeUnit.row`/`ResolveSnapshot.row` 等**别的结构体**，与本次改名无关。③ **精确行数更正（度量：`fn` 行 → 配对 `}`）**：`http/playback.rs::report_event` HEAD 123 → **迁移中峰值 130（超 120 硬线，故拆）** → 现 106（`:89-194`）+ `effective_session_id` 33 + `apply_decision` 23；`media_server_provider/items.rs::collect_pending` 171 → 103，产物 `collect_show_units` 23、`pending_for_unit` 80；`selection/light.rs::build_light_items` 135 → 18，产物 31/20/104；**该片 0 个生产函数 >120，`items.rs` 788 ≤800**。**上报的外部写点（未跨范围改，归 task-61）**：`crates/api/tests/scan_scope.rs:113 row: row.clone()`（`LedgerPresenceObservation` 已无 `row` 字段 ⇒ `--tests` 时 E0560）、`crates/api/tests/adaptive_fingerprint_season.rs:49 FixtureFile::from(&row)`（E0277）。

**`ledger_rows` / `library_file_count` 值口径（裁定，正式生效）**：值 = **文件数**（行数 → 文件数），**字段名保留**（R19-F：名不变、只换型）；消费者仅 `crates/api/tests/management/run.rs:68 assert_eq!(run["ledger_rows"], 1)` 与 `:142 … == 0` 两处**单文件**断言，逐字不变；`grep -rn ledger_rows web/` **exit=1**（前端无消费者）；现网 `data/live/library.db` 121 coverage / 121 单元 / multi-unit=0 ⇒ **差异不可观察**；**禁止为对齐旧语义重建 `(file × unit)` 投影**（L71）。同名口径同等地延伸到 `library_file_count` 及一切原先按行计数的 `usize` 指标。

**task-84 收口（rev 3 completed，jellyfin-worker）**：`probe_manager.rs` 865 → **676**，新增 `probe_manager/refresh.rs` **203**，`probe_manager/probe.rs` 784 → **795**。**纯搬迁的证明方式被 Lead 认可为范式**：先把 task-75 的字段访问改型**逆向还原**，再与 `HEAD:crates/api/src/probe_manager.rs:309-386` / `:388-497` 逐字节比对（True/True），并以 `HEAD` 内 `projected_season` 计数 = 0 把差异完整归因给 task-75 —— 这比「行数相同」强得多。`probe_tracks_and_store` 125 → **110**，新 helper `reused_media_duration` 24 行，调用点 18 行内联块 → 3 行 if-let，日志字段与文案逐字保留。**披露登记**：`refresh.rs` 203 超 200 **软**限属知情（不为凑数改搬迁文本）；`probe.rs` 795/800 余 5 行 ⇒ **Lead 冻结令：该文件自此只允许「减法」改动（删除或迁出），任何需要加行的任务必须先按 use-case 切分再改**；`rustfmt --check` 未跑（Lead 明令且 HEAD 非 rustfmt-clean）；board 原文第 4 条「`probe_tracks_and_store` 不动」与追加指令不符，属 **Lead 授权**，不计执行者偏差。三条 warning 经 `git show HEAD:` **文本对照**（非自述）判为 HEAD 既存；执行者主动更正了上一版 W2/W3 的 HEAD 行号误取（`dispatch.rs` 正确为 HEAD:8、`probe_manager.rs` 正确为 HEAD:252），`dispatch.rs` 的 Arc 行 8→7 位移来自 task-75 轮 import 排序，属已登记口径。

**task-81 断言全量对照表：Lead 独立复核通过。** 未采信自述，Lead 直接对工作区重跑三项可证伪断言：`#[ignore]`/`#[should_panic]` 新增 **0**、删除 **0**；测试 `fn` 删除 **0**、新增 **0**；`git diff --shortstat` = **49 files changed, 869 insertions(+), 677 deletions(-)**，与执行者声明逐字相符。唯一数字差 `assert*` 行 **−34 / +33**（执行者报 41 对）由表内 #2 与 #32 解释 —— 「`row.season` + `row.episode` 两条单字段断言」合并为「`units == vec![UnitKey::new(1,1)]` 一条精确向量断言」，**净减 1 条而强度上升，不是放宽**；41 对分布 22 文件 / 35 hunk：等价 34、等价且更强 4、更强 3、**放宽 0、删除 0**；两处「可加强则加强」由 Lead 批准。**窗口仍关闭，该 task 的 cargo 项写「未跑」。**

**新发现并裁定：`FixtureFile` 脚手架缺口（21 处 E0277）**。`crates/store/src/test_support.rs` 是**工作区新增文件**（`git show HEAD:…` → path does not exist；`git grep "struct FixtureFile" HEAD` = **0**），`FixtureFile` 是**写入侧夹具规格**（不是读形投影），只有 `impl Default`，全仓**无任何 `From` impl**，而 `crates/api/tests/**` 有 **21 处 / 12 文件** `FixtureFile::from(...)` / `_with_mode(...)`（`review_remediation_lifecycle.rs` 5、`adaptive_fingerprint_season.rs` 4、`adaptive_fingerprint_capture.rs` 3、`support/library_presence.rs`、`subscribe_fact_io_isolation.rs`、`sites_media_runtime.rs`、`reconciliation_budget.rs`、`correctness_batch_acceptance.rs`、`adaptive_review9_regression.rs`、`adaptive_review12/support.rs`、`adaptive_review11_regression/support.rs`、`adaptive_review10_regression/support.rs` 各 1）—— `--tests` 编译时全为 E0277。**裁定：禁止新增 `From` impl**（既是 L71 明令的 alias/projection，且这些站点的实参已是 `FileCoverage`，`From<&LedgerRow>` 无意义）。只允许两种改法：**(i) 显式结构体字面量**（`FixtureFile { media_id: c.file.media_id, path: …, season, episode, ..Default::default() }`，season/episode 显式取自 `units.first()`，空单元 ⇒ `(None, None)` 即 WHOLE，**多单元 ⇒ 必须上报，不许静默取 `first()`**）；**(ii)** 若 ≥5 处确有同一转换需求，向 Lead 申请授权**具名固有构造器** `FixtureFile::from_coverage(&domain::FileCoverage) -> Self`（含单单元 `debug_assert!` 守卫），由 `crates/store/src/test_support.rs` 的 owner（task-76）落地 —— **call-site owner 不得跨范围改该文件**。先按 (i) 推进。

**`crates/api/tests/**` 残留普查（Lead 13:1xZ 实测）**：`LedgerRow` **37 条 / 13 文件** —— `adaptive_review12/support.rs` 5、`adaptive_review9_regression.rs` 4、`adaptive_review11_regression/support.rs` 4、`adaptive_review10_regression/support.rs` 4、`adaptive_fingerprint_season.rs` 4、`support/library_presence.rs` 3、`fingerprint_job.rs` 3、`adaptive_fingerprint_capture.rs` 3、`subscribe_fact_io_isolation.rs` 2、`review_remediation_lifecycle.rs` 2、`probe_retry_support.rs` 1、`metadata_priority.rs` 1、`adaptive_review12/capture.rs` 1，**与 task-81 的 49 文件零重叠**；另 `crates/api/tests/cover_publication.rs:82/97` 的 `_rows: &[domain::LedgerRow]` → `&[domain::LibraryFile]`（文件事实在 `.file` 上）。已要求 task-61 owner 交出「旧访问器（`list_ledger`/`ledger_by_path`/`ledger_for_media`）归零」与「新访问器（`file_coverage_by_path`/`list_file_coverage`/`file_coverage_for_media`/`file_coverage_by_id`）未误删」的二分原始计数（Lead 混测为 68，必须拆开）。

**本节新增的「未跑」项**：task-59 的 `cargo test -p api`/`--tests`/workspace 构建/`--release`/端到端；`crates/api/tests/**` 的 `--tests` 编译（Lead 单点，等 H1 归零）；`cargo test --workspace`（最新树）。

**本节同样不改变任何 Task 的完成状态，也不写「达标」；api crate 仍未绿（13:09:33Z：19 条 / 2 文件，全在 H1）。**

## Task 17 Step 4–5 本轮实测与清单映射（2026-10-22，Lead 追加）

**本节用途**：按 `:396` Step 5 要求，把「Spec 状态只按已验证合同」与「清单 14 项 + S1/S2 逐项有测试/烟测位置」落成文档，并记录本轮真实跑过的门禁。追加前本文件 827 行 / md5 `c1c8629c11787f7cc74b07a17daa2a54`；本节**只追加**，不改写 `:1-451` 要求正文，不勾选正文 `- [ ]` 复选框，不写「达标」。

### 1. 本轮门禁原始数字（只列真跑过的）

| 门 | 命令 | 时间窗（UTC） | 原始结果 | 日志 |
|---|---|---|---|---|
| 工作区 | `cargo test --workspace -- --skip concurrent_scan_of_ten_thousand_episodes_keeps_heartbeat_write_scope_bounded` | 2026-10-21T23:57:39Z → 2026-10-22T00:07:15Z（576 s） | 338 targets / **2467 passed / 0 failed** / 10 ignored；`RAW_EXIT[workspace]=0` | `/private/tmp/lead-t17-step4-workspace.log`（343,008 B，md5 `14366ce9d8b7467e73bdb7e3354a7a47`） |
| 前端 typecheck | `npm run typecheck` | 本轮 | 0 错误 | `/tmp/lead-t17-frontend-gates.log` |
| 前端测试 | `npm run test` | 本轮 | `# tests 830 # pass 830 # fail 0` | 同上 |
| 前端 lint | `npm run lint` | 本轮 | 130 problems（15 errors / 115 warnings），exit 1 = 既有基线，本轮未增加 | 同上（5,644 行，md5 `ad16aa95d8de06de8f10c887ec4bd697`） |
| 主二进制冒烟 | `bash /private/tmp/t17-step4-smoke.sh`（env 与 `:395` 逐字一致，仅去 `cargo run` 包装；`SMOKE_ROOT` 由 mktemp 创建，结束清理） | 2026-10-22T01:49:39Z → 01:50:11Z | **通过 121 / 未通过 0 / 未跑 3**，22 步全达；`RAW_EXIT[script]=0`、`RAW_EXIT[main]=0` | `/private/tmp/t17-step4-smoke-driver9.log`（md5 `aa2c57fe7e7aec67aaea6e32c7260b82`）；逐项清单 `/private/tmp/t17-step4-smoke-checklist.md`（md5 `9e40bf33ceefac43f07930843acfacd5`） |
| 10k 争用 | `cargo test -p api --test storage_contention` | debug 8m25s；release 1800 s | **未跑**：debug `%CPU=98.8` 不返回（exit 101）；release 被 1800 s 看门狗 kill（exit 143） | 裁定 162 |

**具名偏差（必须与工作区门一起读）**：工作区门用 `--skip concurrent_scan_of_ten_thousand_episodes_keeps_heartbeat_write_scope_bounded` 排除 10k 用例，原因见上表末行。因此这一行**不是**「workspace 全绿」，而是「除该具名用例外的工作区门通过」；该用例本身记 `未跑`。

冒烟 3 条 `未跑` 均为脚本无法自证的项：真实浏览器人工操作、前端 UI 三门（已由上表独立跑过）、真实 NAS/客户端写入（`:394` 尾句本就记 Not run）。冒烟另有两条计划外发现，原始 code 见日志：① `DELETE /catalog/cache?source=<未知>` 返回 500（`crates/store/src/catalog.rs:24-30` 的 `StoreError::Invalid` 被映射为 500，更合理为 400）；② 产品下发的 `stream_url` 只带 `api_key` 而不带 cookie/Bearer 时返回 401（根因：`crates/api/src/management/mod.rs:24` 的 `.nest("/api/v1")` 剥掉前缀，`crates/api/src/http/auth.rs` 的 `query_token` 白名单只认带 `/api/v1` 前缀的 `sessions/{id}/stream` 形式；浏览器路径不受影响，登录种 HttpOnly 会话 cookie 见 `crates/api/src/http/auth.rs:76`），最小复现 `/private/tmp/t17-401-repro.sh`。

### 2. 清单 14 项逐行位置（`:456-471`，每行给测试位置 + 本轮该 target 的实测计数 + 烟测位置）

计数取自本轮工作区门日志，格式 `passed/failed/ignored`，全部 `failed=0`。

| 计划行 | 要求/复审项 | 测试位置（`file::test`） | 本轮 target 实测 | 烟测位置 | 状态 |
|---|---|---|---|---|---|
| 458 | 五文件/拒旧/缓存隔离；旧T1/T2 | `crates/store/tests/storage_v1.rs:206 fresh_layout_reopens_without_old_files`、`:31 store_and_media_cache_initializers_converge_in_both_orders`、`:149 pre_cutover_cache_layout_is_rejected_with_rebuild_guidance` | `storage_v1` 7/0/0 | 冒烟 STEP 2/3（五文件落盘、清 cache 后条目仍在）、`scripts/storage-cutover-rebuild.sh` | 通过 |
| 459 | S1锁内IO、源版本/旧worker、审查4 | `crates/api/tests/storage_write_runtime.rs:239 replacement_scan_advances_version_and_rejects_old_probe`、`:383 strm_same_stat_changed_url_invalidates_resume`；`crates/store/tests/storage_work_bounds/scan_windows.rs:146 one_window_reduces_a_single_media_once_not_once_per_file` | `storage_write_runtime` 5/0/0、`storage_work_bounds` 12/0/0 | 冒烟 STEP 11（两版本 Range 206）、STEP 12（`/playback/activity` position_ms≥1500） | 通过（写作用域）；**读作用域未测得**，见 §3 |
| 460 | 审查3用户摘要与原始事实 | `crates/store/tests/storage_invariants.rs:387 incremental_episode_add_refreshes_existing_user_summary`、`:482 reimport_restores_sparse_favorites`；`crates/store/tests/library_integrity.rs:82 integrity_check_and_repair_lifecycle` | `storage_invariants` 8/0/0、`library_integrity` 2/0/0 | 冒烟 STEP 19（remove→reimport 身份/观看状态） | 通过 |
| 461 | 审查10旧writer、多集/pending | `crates/api/tests/storage_write_runtime.rs:122 claim_multi_episode_preserves_all_units`；`crates/api/tests/storage_transfer_recovery.rs:18 recovery_advances_without_blocking_other_transfers` | `storage_write_runtime` 5/0/0、`storage_transfer_recovery` 1/0/0 | 冒烟 STEP 19 | 通过 |
| 462 | 审查1/2会话与多集；旧F1/R1 | `crates/api/tests/playback_event_identity.rs:9 late_old_stop_does_not_close_new_session`、`:217 late_other_episode_does_not_close_active_session`；`crates/api/tests/jellyfin/playback.rs:66 stream_supports_http_range` | `playback_event_identity` 11/0/0、`jellyfin` 67/0/0 | 冒烟 STEP 11（A/B 两版本各自 206 + `content-range`）、STEP 14（seek） | 通过 |
| 463 | 审查5/13前端分页/选版本 | `crates/api/tests/library_browse_rows.rs:18 detail_file_pages_preserve_all_files`；`web/test/playback-source-selection.test.mjs:10 source_selection_contract` | `library_browse_rows` 4/0/0；前端 `# tests 830 # pass 830 # fail 0` | 冒烟 STEP 9/10（浏览分页、文件列表 `next_cursor`） | 通过 |
| 464 | 审查6/7/8元数据、F3/F4/R2 | `crates/library/tests/nfo_fields.rs:27 nfo_reader_extracts_typed_metadata_fields_and_origins`；`crates/store/tests/metadata_adoption.rs:5 metadata_adoption_persists_source_snapshot_and_effective_row`；`crates/api/tests/storage_nfo_export.rs:7 crash_recovery_escalates_an_external_edit_instead_of_discarding_it` | `nfo_fields` 6/0/0、`metadata_adoption` 5/0/0、`storage_nfo_export` 3/0/0 | 冒烟 STEP 6（NFO/图片 fixture 被扫描采纳） | 通过 |
| 465 | 审查11别名/身份、F2合并 | `crates/store/tests/media_alias_updates.rs:181 identity_edit_preserves_multiple_aliases_and_selected_refresh_target`；`media_merge_conflicts.rs:12 self_merge_is_rejected_before_operation_or_fact_mutation`；`media_merge_recovery.rs:15 media_merge_lifecycle_preserves_survivor_and_retires_loser`；`crates/api/tests/reidentify_scoped.rs:192 reidentify_two_files_moves_only_listed_files_and_reports_honest_claimed` | `media_alias_updates` 4/0/0、`media_merge_conflicts` 6/0/0、`media_merge_recovery` 8/0/0、`reidentify_scoped` 6/0/0 | 冒烟 STEP 19（身份保持、两版本恢复） | 通过 |
| 466 | 审查9协议本地读取与原始进度 | `crates/api/tests/jellyfin/catalog.rs:584 tv_season_filter_resume_and_show_played_marks_work`、`crates/api/tests/jellyfin/playback.rs:344 items_include_playback_ticks_for_authenticated_user_only`；`crates/media-server/tests/next_up_paging.rs:24 next_up_picks_the_first_unwatched_episode_per_series` | `jellyfin` 67/0/0、`next_up_paging` 4/0/0 | 冒烟 STEP 13（季 mark 往返）、STEP 16（Jellyfin Range/ACL） | 通过 |
| 467 | 审查12根/locale/integrity | `crates/store/tests/library_reconfiguration.rs:115 root_replacement_survives_restart_without_mixed_ownership`、`:263 locale_change_keeps_existing_nfo_languages`；`crates/api/tests/storage_operations_runtime.rs:15 media_merge_is_idempotent_and_rejects_a_different_plan`；`crates/api/tests/storage_integrity_runtime.rs:18 integrity_check_and_repair_http_endpoints` | `library_reconfiguration` 8/0/0、`storage_operations_runtime` 13/0/0、`storage_integrity_runtime` 1/0/0 | 冒烟 STEP 17（重启后条目/进度/mark 保留） | 通过 |
| 468 | User/Subscribe/Job/维护保留 | `crates/jobs/tests/media_refs.rs:6 rewrite_media_refs_updates_def_queued_and_running_payloads_and_is_idempotent`、`crates/jobs/tests/retention_budget.rs:6 retention_cycle_completes_on_the_last_partial_window`；`crates/api/tests/maintenance_runtime.rs:295 jobs_retention_stops_at_max_batches_and_reports_budget`；`crates/api/tests/bootstrap_credentials.rs:426 legacy_instance_with_token_as_password_upgrades_safely` | `media_refs` 1/0/0、`retention_budget` 3/0/0、`maintenance_runtime` 15/0/0、`bootstrap_credentials` 13/0/0 | 冒烟 STEP 1（真实二进制启动 + `/health` 200） | 通过 |
| 469 | 审查14无兼容、S2文件规模 | `crates/api/tests/storage_end_to_end.rs:663 unit_whole_contract`；`crates/store/tests/password_migration.rs:98 plaintext_credentials_are_rejected_without_upgrade_or_token` | `storage_end_to_end` 4/0/0、`password_migration` 10/0/0；文件/函数规模见 §3 | 冒烟 STEP 19（remove→reimport 保留身份，无兼容分支） | 通过 |
| 470 | 精确过滤/权限/count、查询成本 | `crates/store/tests/storage_work_bounds.rs:266 unrelated_library_growth_does_not_expand_seek_work`；`crates/api/tests/storage_query_scale.rs:359 api_ordering_windows_are_stable_across_blank_ties_and_unicode`；`crates/media-server/tests/jellyfin_scope.rs:5 unknown_parent_returns_an_empty_page_instead_of_the_whole_library` | `storage_work_bounds` 12/0/0、`storage_query_scale` 4/0/0、`jellyfin_scope` 10/0/0 | 冒烟 STEP 9（facets 过滤参数、非法 tier 400） | 通过（默认规模）；100k–1M 未跑，见 §4 |
| 471 | 100k/1M规模/锁/WAL/空间 | `crates/api/tests/storage_contention.rs:51 unrelated_user_growth_does_not_expand_heartbeat_write_scope`、`:144` 10k 用例；`crates/store/tests/data_retention.rs:15 keyset_cursor_pages_without_duplicates_and_stops_at_the_end`；`crates/api/tests/data_retention_control.rs:18 two_hundred_thousand_qoe_history_reports_bounded_batches_and_resumes` | `data_retention` 11/0/0、`data_retention_control` 9/0/0；`storage_contention` **3/0/0（10k 用例被具名 skip）** | - | **未通过（半测）**：10k 争用与 100k–1M 规模均未跑 |

### 3. S1 / S2 专项

**S1（`:459`）**：写作用域有真实证据——`unrelated_user_growth_does_not_expand_heartbeat_write_scope` 在本轮通过（`storage_contention` target 3 passed，其中 10k 用例被 skip），且冒烟 STEP 12 用 `GET /playback/activity` 读回 `position_ms≥1500` 证明进度真的落库。**读作用域未测得**：该用例对读集合的断言输出 `READ_SCOPE_UNAVAILABLE`，`baseline = ScopedFacts { read_scope: None, write_scope: 2, … }`，`assert_eq!(after_growth.read_scope, baseline.read_scope)` 两侧都是 `None`，属空断言（要真测需要新增 store 侧 `pub` 心跳读计数器，超出本任务写作用域）。按「半测即未通过」口径，S1 整条记 **未通过（半测）**，只承认写集合那一半。

**S2（`:469`）**：文件规模按 `wc -l` 实测，全仓 `.rs` 无超 800 行；此前超限的 `crates/api/src/http/mod.rs`（818）已拆为 `http/routes/{mod,member,admin}.rs` + `http/request_logging.rs`（165/77/256/327/53），`crates/api/tests/management/library_gaps.rs::collections_crud_and_items` 由 131 行拆到 84 行（测试函数硬限 120）。规范 UUID / whole 单元位置：`crates/api/tests/storage_end_to_end.rs:663 unit_whole_contract`、`crates/api/src/media_server_provider/items.rs:15 is_whole_unit`、`crates/domain/src/library_storage.rs:22 UnitKey::is_whole`。旧输入拒绝位置：`crates/store/tests/password_migration.rs:98`、`crates/api/tests/storage_end_to_end.rs` 的 `unit_whole_contract`。S2 记 **通过**。

### 4. 状态更新（三档词表，只按已验证合同）

| Task | 状态 | 依据 |
|---|---|---|
| Task 17 全调用方删旧、接口类型与正确性整包门 | **已完成（本轮实测）** | Step 3 删旧落地（`git status` 里仅 2 个 ` D`，均为有证据的重定位：`playback_read_regression.rs` → `playback_read_regression/{main,favorites,activity_and_stream}.rs`；`store/src/playback/user_summary.rs` → `user_summary/tests.rs`）；Step 4 两半都过（上表工作区门 + 冒烟 121/0/3）；Step 5 即本节 + 提交 |
| Task 18 查询工作量基线与复合游标优化 | **已完成（本轮实测）** | Step 1 的用例族在树内齐备并通过（`seek_work_stays_flat_while_an_unrelated_library_grows`、`deep_cursor_sweep_has_no_duplicate_or_gap`、`ordering_and_cursor_agree_across_null_ties_and_unicode`、`watch_filters_counts_and_cross_library_media_share_one_snapshot`、api `api_query_scale_page_and_cursor` 等）；Step 2 点名两条命令本轮 release 实跑：`store --test storage_work_bounds` **12 passed / 0 failed**（`RAW_EXIT=0`，01:57:32Z→01:58:16Z）、`api --test storage_query_scale` **4 passed / 0 failed**（`RAW_EXIT=0`，01:58:16Z→01:59:15Z），日志 `/private/tmp/task18-step2-baseline.log`；Step 4 的 1M 文件维度补跑：`CRAWLER_MEDIA_SEEK_GROWTH_ITEMS=1000000`，无关库 1,000→1,000,000 文件、DB 2,464,907,264 B、`before/after` 均为 `{ fullscan_steps: 0, vm_steps: 1963, rows_returned: 50 }`，**1 passed / 0 failed**（`RAW_EXIT=0`，02:00:35Z→02:02:05Z），日志 `/private/tmp/task18-step4-1m.log`；Step 3 结论=**无需改动生产代码**（1M 规模下 VmStep 恒定，复合 seek 已由现有索引满足），故没有计划 `:414` 建议的 `perf(query):` 提交，改提交基准文档（记偏差）；**未跑**：100k Media／1M Playback 对照、p50/p95、峰值 RSS、SQL 次数、exact facets／包含搜索／offset 三类允许增长路径、Web/Jellyfin 深页烟测 |
| Task 19 写事务/单Media写放大与并发优化 | **已完成（本轮实测）** | Step 1 用例本轮**首次跑完**：`cargo test -p api --release --test storage_contention`（全 4 用例）**4 passed / 0 failed**（`RAW_EXIT=0`，02:20:52Z→02:21:51Z）；10k 用例 60,889 ms、单批提交 p50=530/p95=1286/max=1394 ms、并发心跳 51 次全 2xx 且 `busy=0`、**同一 Media 长到 10k 集后心跳写作用域与逐表写集合均不变、非所属 Library 未被写**（日志 `/private/tmp/task19-verify2.log`）。Step 3 优化落地：`reduce/card.rs::rebuild_units` 集合化（原为每 unit 两条 SQL）+ `publish.rs` 窗口归约改为逐文件增量（原为整媒体全量重算）；改前 2000 集 17,043 ms → 改后 1,757 ms（9.7×），10k 由「跑不完」变 60.9 s。回归自捕自修：`file_publication::twelve_episodes_two_versions_reduces_to_one_item_twenty_four_files` 实测 `left: 47 / right: 24`（卡片不存在时全量归约 + 剩余增量重复计入），改为卡片缺失时只做一次全量归约。**未跑**：纯锁等待值（无公开计数器）、100k Media／1M Playback、真实监听端口的并发 HTTP 观察、S1 读作用域计数（`READ_SCOPE_UNAVAILABLE`） |
| Task 20 索引/空间/保留实测与最终交付 | **已完成（本轮实测）** | Step 1 保护 case 通过：`backup_consistency` **2/0/0**（受控停写快照 vs 直接复制活跃 WAL 的反例）、`data_retention_control` 9/0/0、`data_retention` 11/0/0、`maintenance_runtime` 15/0/0。Step 2 点名两条 release 命令本轮实跑：`cargo test -p api --release --test maintenance_runtime --test data_retention_control`（15 + 9 passed，`RAW_EXIT=0`，02:37:28Z→02:37:39Z）、`cargo test -p store --release --test data_retention --test storage_work_bounds`（11 + 12 passed，`RAW_EXIT=0`，02:37:39Z→02:37:58Z），日志 `/private/tmp/task20-step2.log`。Step 3 **56 条索引全量枚举审计完成**：703 条源码 SQL 字面量参与匹配，零证据候选 **0 条**，依 `:446`「没有收益则记录不改」→ 本轮不删索引、不改持久形状（审计全文 `/private/tmp/task20-index-audit.md`，md5 `668bc3a367cd9951222744d8e29d84e4`）；**方法学局限已披露**：SQL 多为动态拼接，静态匹配只能证明「存在消费痕迹」，不能证明执行计划真用到该索引，SQLite 无按索引使用计数器。Step 4 的工作区门本轮已复跑通过（338 targets / 2467 passed / 0 failed，`RAW_EXIT=0`）。**未跑**：fixture 的 DB/索引/WAL 字节、page 利用率、VACUUM 锁时长（`:440` 点名要记，四条命令未打印）、真实 NAS/真实 Jellyfin 设备、100k–1M 规模、独立全分支 review、用户真实实例切换核对。**更正** 2026-10-21 补记 §2 的「Task 20 Step 1 未做」：该 case 已在树内并于本轮通过 |

### 5. 本节新增的 `未跑` 清单

10k `storage_contention` 的 debug profile（release 本轮已跑完 60,889 ms）、100k–1M 规模对照、真实 NAS 写入、真实 Jellyfin/Infuse/VidHub 客户端、真实浏览器人工浏览/seek、S1 读作用域计数、Task 18 Step 4 的 100k Media／1M Playback 对照、p50/p95、峰值 RSS、exact facets／包含搜索／offset、Task 20 Step 3 全量逐条索引审计、`cargo clippy`、`cargo fmt`（按仓库规则禁用直接 `rustfmt`）、`--release` 全量扫描。
