# library.db：Media、Library、Playback 与持久文件工作

> 读写架构见 [ADR-0008](../adr/0008-sqlite-system-of-record.md)，优化设计见[存储性能设计](../design/storage-performance.md)。工作量回归源码见 `crates/store/tests/storage_lock_work.rs`。

[返回数据库总览](README.md)

唯一生产 DDL 的文件身份为 `application_id=1129139249`、`user_version=6`。本库保存权威事实，不可按缓存清空。

结构为 **66张业务表（含1张FTS5虚拟表）、668字段、115个显式命名索引、45组FK、133个CHECK、0生产触发器**，不计SQLite自动索引、FTS5的4张内部影子表及虚拟表隐藏列；668字段按 `PRAGMA table_info` 统计。影子表另有10字段，物理表总数为70、物理声明字段678。字段以[生产DDL](../../crates/store/src/schema/v1/library.sql)为准，复核方法见[数据库总览](README.md#结构复核方法)。

Subscribe retention 是孤儿 Media 保留权威；创建、释放、补偿和合并通过 generation fence 及持久阶段跨库恢复，Collection 删除及 orphan 候选按窗口处理。关闭的 fence 和终态记录可能累积。`collections.created_at` 和 `collection_items.added_at` 为 INTEGER 毫秒；集合 cursor 按 `(sort_order, added_at, media_id)` 排序并严格使用数值时间。`items_roster_stats` / `files_roster_stats` 覆盖索引减少回表，精确聚合仍随目标库大小线性增长。

`library_operations.resource_key` 和 `nfo_export_active_path` 保证活跃 NFO 导出独占 sidecar 路径；`nfo_export_pending_age` 限定恢复窗口。`library_integrity_owner` 服务于 Library/Media 写屏障，failed 操作仍冻结以供恢复。Integrity 在 reader 中准备有限输入，释放读租约后用短 writer 事务校验 owner revision 与游标并提交；一次 advance 可只推进当前 Media 的一页。失败保留游标和冻结，显式重试恢复。卡片、收藏和用户统计读取沿用完整性屏障。FTS 准备、最终计数/重叠核对及单 Media 已采用元数据的成本仍取决于输入规模，不把整个 operation 描述为恒定工作量。`hdr_observed` 区分未观察与确认 SDR。终态清理按年龄窗口执行。

删除意图保存 `path_key` 和 `deletion_id`，分别标识固定规范路径与本次删除；`pending_deletions_path_key` 保证每个路径最多一个意图，`pending_file_renames_file` 支持检查文件已有改名工作。

## 实现边界

- Media/Library/Collection/Playback 同库；`store::LedgerRow` 是文件事实的读取 DTO。
- 文件源 CAS、元数据写屏障和用户摘要由发布事务维护；重新识别严格校验已观察的身份。
- `pending_file_renames`保存版本、源/目标磁盘证据、sidecar决议、已完成阶段与重试；organize/housekeeping/启动恢复均使用owned plan将磁盘观察、执行与staging清理移出Store锁，锁内提交DB/CAS。
- 根迁移每窗口≤100文件/Unidentified 候选，持久游标、公平轮转和状态可查询；API driver 批间放锁并在解冻后自动继续。关联 NFO 与文件同批迁移。Unidentified 的索引 seek 不重扫游标前记录。
- 工作量、容量、内存与实际媒体验证方法见[存储性能测试](../testing/storage-performance.md)。

## 字段阅读规则

类型、主键顺位、NOT NULL 与 DEFAULT 均为 SQLite 声明；正文约束使用原 DDL 表达，包含 DEFERRABLE 细节。无 FK 时明确列出，不因字段名带 `_id` 推断约束。JSON 是各消费者的版本化结构，json_valid 只验证 JSON 语法，不保证业务字段完整。

用户和通用设置在 app；Transfer/Watch 与 Scrape 配置在本库；Subscribe 在 subscribe、通用 Job 在 jobs，跨库 ID 无 FK。playback 恢复包不引用 library_files 的 FK；仍有同一 Media 的其他文件时可保留观看事实，但最后文件删除必须按 ADR-0012 清理孤儿 Media、favorites 与播放进度。秒/ms/ns 混用，以实际读写入口为准；`played_changed_at_ms` 与 watch/favorite 时钟不能互换。

## 表目录

- [transfer_settings](#transfer_settings) — 2 字段；Transfer/Watch 的 revisioned 配置单例。
- [scrape_settings](#scrape_settings) — 3 字段；原始 Scrape 配置与单调 revision。

- [library_query_counts](#library_query_counts) — 5 字段；当前查询投影/成员/计数模型。
- [library_query_count_rebuild](#library_query_count_rebuild) — 5 字段；当前查询投影/成员/计数模型。
- [library_query_config](#library_query_config) — 3 字段；当前查询投影/成员/计数模型。
- [library_protocol_terms](#library_protocol_terms) — 4 字段；当前查询投影/成员/计数模型。
- [library_protocol_people](#library_protocol_people) — 9 字段；当前查询投影/成员/计数模型。
- [pending_root_reassignments](#pending_root_reassignments) — 6 个字段
- [pending_file_renames](#pending_file_renames) — 28 字段；文件改名/改挂持久意图：版本/磁盘证据、sidecar决议、跨分区引用恢复及重试。

- [chapter_frame_tasks](#chapter_frame_tasks) — 12 字段；章节帧生成的源版本、目标、逐目标结果和不可变图片身份。
- [collection_items](#collection_items) — 4 字段；以 canonical MediaId 保存成员关系与排序；Collection 成员保留无文件 Media；最后文件删除仅清理无 Collection/Subscribe 保留的孤儿 Media。
- [collections](#collections) — 7 字段；公共系列或用户私有 Collection 定义；可见性与 owner 组合有 CHECK，deleting 控制删除阶段。
- [subscribe_retention](#subscribe_retention)、[subscribe_retention_fences](#subscribe_retention_fences)、[subscribe_compensation_receipts](#subscribe_compensation_receipts)、[pending_orphan_media](#pending_orphan_media)、[reference_recovery_cursors](#reference_recovery_cursors) — Subscribe 引用、补偿与孤儿恢复。
- [file_details](#file_details) — 11 字段；同源版本的冷探测详情；随文件级联删除，不等于可随意清空的浏览缓存。
- [file_probe_obligations](#file_probe_obligations) — 3 字段；随文件登记的持久探测义务；文件删除级联移除。
- [fingerprint_attempts](#fingerprint_attempts) — 12 字段；采样尝试、阶段、错误与工作指标；按作业/文件查询。
- [fingerprint_cache](#fingerprint_cache) — 8 字段；每物理文件当前指纹缓存及算法/时长参数。
- [fingerprint_model_members](#fingerprint_model_members) — 4 字段；模型到样本/物理文件源版本的成员；仅 model_id 声明 FK。
- [fingerprint_samples](#fingerprint_samples) — 12 字段；按文件源版本、profile、kind、窗口去重的采样事实。
- [fingerprint_season_models](#fingerprint_season_models) — 9 字段；季级指纹模型、成员/政策版本与模型体。
- [libraries](#libraries) — 23 字段；Library 配置、显示语言、访问范围、生命周期与完整性状态；操作状态需结合正式operation验收。
- [library_file_units](#library_file_units) — 5 字段；文件到逻辑单元的覆盖事实；一文件可覆盖多集，同集可有多版本。
- [library_files](#library_files) — 35 字段；权威物理文件台账；显式 Library/root/Media/kind 与源版本、在位/发布状态、热质量字段。
- [library_integrity_rows](#library_integrity_rows) — 10字段；Integrity 当前 Media 的分页归约临时事实，仅服务持久操作，完成后分批清理。
- [library_item_terms](#library_item_terms) — 4 字段；公共卡片多值倒排；私有 Collection 不得混入公共 terms/facets；当前主键以 `media_id` 为第二列（每卡读取直接命中键前缀）。
- [library_items](#library_items) — 39 字段；Library/Media 浏览摘要。依赖同事务维护，不保留完整文件数组或来源响应。
- [library_metadata_inputs](#library_metadata_inputs) — 18 字段；Library 独立 NFO 输入及导出来源，按登记身份校验并随文件按批迁移。
- [library_operations](#library_operations) — 11 字段；roots/locale/default_locale/integrity/scrape 持久操作载体。
- [library_removal_roots](#library_removal_roots) — 2 字段；删除操作冻结的根路径范围；与持久文件归属需要一致。
- [library_removals](#library_removals) — 10 个字段
- [library_roots](#library_roots) — 6字段；path_key 唯一；根替换使用持久重配置，嵌套根队列通过持久游标恢复。
- [library_units](#library_units) — 8 字段；Library/Media/单元级去重摘要和首选文件。whole=(-1,-1)，TV S00E00 合法。
- [media](#media) — 9 字段；规范 Media 身份；可无文件存在。retired 墓碑不是可继续写的兼容实体。
- [media_aliases](#media_aliases) — 5 字段；来源别名归属及每来源唯一刷新目标；不把来源 ID 当内部身份。
- [media_episodes](#media_episodes) — 13 字段；已采用分集详情；拥有事实来自文件覆盖。
- [file_markers](#file_markers) — 12 字段；精确文件版本/单元的片头片尾标记及人工锁，文件删除级联清理。
- [media_metadata](#media_metadata) — 17 字段；work 级已采用元数据；effective/compose 合成来源并维护字段 provenance。
- [media_metadata_overrides](#media_metadata_overrides) — 7 字段；实体/locale 人工字段覆盖；写入后同事务重合成/归约，Media merge 预检已有人工锁冲突拒绝。
- [media_metadata_sources](#media_metadata_sources) — 13 字段；每实体/locale/来源的当前规范化快照与选择来源；不是响应缓存或历史表。
- [media_operations](#media_operations) — 10 字段；Media 合并/改挂持久计划与阶段。冲突/冻结在 Store 内落库，HTTP（`POST /media/merges`、`GET /media/operations/{id}`）与 job loop 推进已接线；改挂仍非独立操作类型。
- [pending_file_deletions](#pending_file_deletions) — 10 字段；待删除磁盘文件意图、身份检查与重试状态；不是浏览事件流。
- [playback_logs](#playback_logs) — 23 字段；终态会话历史；按稳定会话/generation 去重，不依赖文件仍存在。
- [playback_metrics](#playback_metrics) — 13 字段；Playback QoE 指标；仅按明确保留策略清理，不作为用户播放次数来源。
- [playback_session_ids](#playback_session_ids) — 16 字段；签发会话的逻辑目标、物理源、生命周期与 generation。`Store::commit_playback_event` 写入前校验 `(user, device, session)` 的 generation/目标单元/文件版本/source_version 与库 `lifecycle_state`+`config_revision`+ACL，不匹配一律 `IgnoredStale` 不落事实；`pending` 会话的首个 progress 视为 Start 原子激活；`Stop` 只关闭上报会话并归档一条 play log，重复 Stop 因会话已 `closed` 而幂等。
- [playback_sessions](#playback_sessions) — 31 字段；每用户/设备活动会话及源绑定、事件顺序、传输/观看统计。
- [playback_units](#playback_units) — 20字段；用户逻辑观看/收藏事实及源绑定恢复包，不FK到物理文件。未完成 Progress 保留既有 played 断言；达到已知源时长90%阈值写 true 及毫秒断言时钟，人工 mark/unmark 写显式 true/false。目标按最新时钟、同时间 episode>season>whole 决议；effective assertion 与协议 SQL 使用同一规则。分集按请求单元/必要祖先分窗读取，在同一快照内归约。清历史清完整 resume 与断言，favorite 保留。
- [probe_job_units](#probe_job_units) — 15 字段；Probe 文件单元、阶段与输出；活动单元/派发索引控制工作窗口。
- [probe_jobs](#probe_jobs) — 17 字段；专用 Probe 作业与单元计数、阶段策略和历史；不同于 jobs.db 的通用 Job。
- [probe_season_comparison_history](#probe_season_comparison_history) — 4 字段；每 Media/季最近比较摘要与发布时间，不是完整历史集合。
- [probe_stage_state](#probe_stage_state) — 11 字段；每文件/上下文/阶段的退避、错误与活动作业状态。
- [scan_operations](#scan_operations) — 11 字段；扫描作用域、generation、全量/定向意图与恢复报告。
- [scan_seen](#scan_seen) — 6 字段；当前扫描 operation 已确认处理的路径版本；与源观察、物理 FileVersion 不是同一语义。
- [scrape_sidecars](#scrape_sidecars) — 3 字段；文件的 NFO/图片 sidecar 路径；登记不等于导出 provenance 合同已完成。
- [unidentified](#unidentified) — 7 个字段
- [user_library_items](#user_library_items) — 23字段；用户/Library/Media 稀疏摘要，卡片删除级联，重导入由仍存在的原始用户事实恢复；支持完整/增量维护，批量发布合并每个 Media 的用户摘要写入。
- [library_removal_files](#library_removal_files) — 9 个字段
- [root_reassignment_state](#root_reassignment_state) — 9 个字段

- [library_protocol_entities](#library_protocol_entities) — 7 字段；协议实体投影/重建状态。

- [library_search_fts](#library_search_fts) — 2 字段；协议实体投影/重建状态。

- [protocol_entity_overlap_counts](#protocol_entity_overlap_counts) — 4 字段；协议实体投影/重建状态。

- [protocol_entity_overlap_rebuild](#protocol_entity_overlap_rebuild) — 5 字段；协议实体投影/重建状态。

- [user_library_continue_units](#user_library_continue_units) — 7 字段；协议实体投影/重建状态。

## chapter_frame_tasks

章节帧生成的源版本、规范化目标、generation/attempt 与逐目标持久结果。`results_json` 保存 index/ready/error 和 candidate/published artifact（attempt + 唯一 id）；成功目标的 published 身份跨重试保留，candidate 在渲染前登记。claim/finish/recover/repair 仅做短 SQL 事务，文件验证与 IO 在所有锁外执行。取消和 superseded 行不得恢复；权威事实再次采用相同 generation 时，同事务分配新 task_id，保留终态历史，每文件最多一个非取消/非 superseded 的当前任务。同事实重复发布幂等复用当前任务；普通 ensure/recover 不分配这种重新采用义务。空目标不登记运行任务。该表没有文件 FK，删除文件仍在同连接显式清理。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 是 | 无 | 记录身份或单例键。 |
| `file_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `source_version` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `targets_json` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `targets_hash` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `generation` | `TEXT` | — | 是 | 无 | file/source/规范化 targets/renderer version 的固定编码摘要。 |
| `attempt` | `INTEGER` | — | 是 | `0` | 非负 claim/recovery fence。 |
| `results_json` | `TEXT` | — | 是 | 无 | 逐目标结果与精确 candidate/published 身份。 |
| `status` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `started_at_ms` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `finished_at_ms` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE chapter_frame_tasks (
 id TEXT PRIMARY KEY NOT NULL, file_id TEXT NOT NULL, source_version TEXT NOT NULL,
 targets_json TEXT NOT NULL CHECK(json_valid(targets_json)), targets_hash TEXT NOT NULL,
 generation TEXT NOT NULL, attempt INTEGER NOT NULL DEFAULT 0 CHECK(attempt>=0),
 results_json TEXT NOT NULL CHECK(json_valid(results_json)),
 status TEXT NOT NULL CHECK(status IN ('queued','running','completed','partial','failed','cancelled','superseded')),
 created_at_ms INTEGER NOT NULL, started_at_ms INTEGER, finished_at_ms INTEGER
);
```

**显式命名索引**

```sql
CREATE INDEX chapter_dispatch ON chapter_frame_tasks(status,created_at_ms,id);
CREATE UNIQUE INDEX chapter_current_file ON chapter_frame_tasks(file_id) WHERE status NOT IN ('cancelled','superseded');
CREATE INDEX chapter_history_identity ON chapter_frame_tasks(file_id,generation,created_at_ms,id);
```

## collection_items

以 canonical MediaId 保存成员关系与排序；Collection 成员保留无文件 Media；最后文件删除仅清理无 Collection/Subscribe 保留的孤儿 Media。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `collection_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `media_id` | `TEXT` | 2 | 是 | 无 | Media 身份。 |
| `sort_order` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `added_at` | `INTEGER` | — | 是 | 无 | Unix 毫秒时间戳。 |

**生产表定义**

```sql
CREATE TABLE collection_items (
 collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE, media_id TEXT NOT NULL REFERENCES media(media_id),
 sort_order INTEGER NOT NULL DEFAULT 0, added_at INTEGER NOT NULL, PRIMARY KEY(collection_id,media_id)
);
```

**显式命名索引**

```sql
CREATE INDEX collection_items_order ON collection_items(collection_id,sort_order,added_at,media_id);
CREATE INDEX collection_media ON collection_items(media_id,collection_id);
```

## collections

公共系列或用户私有 Collection 定义；可见性与 owner 组合有 CHECK。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 是 | 无 | 记录身份或单例键。 |
| `user_id` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `visibility` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `name` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `sort_order` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `created_at` | `INTEGER` | — | 是 | 无 | Unix 毫秒时间戳。 |
| `deleting` | `INTEGER` | — | 是 | `0` | Collection 是否处于删除中。 |

**生产表定义**

```sql
CREATE TABLE collections (
 id TEXT PRIMARY KEY NOT NULL, user_id TEXT, visibility TEXT NOT NULL CHECK(visibility IN ('private','public')),
 name TEXT NOT NULL, sort_order INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL, deleting INTEGER NOT NULL DEFAULT 0 CHECK(deleting IN (0,1)),
 CHECK((visibility='private' AND user_id IS NOT NULL) OR (visibility='public' AND user_id IS NULL))
);
```

**显式命名索引**

```sql
CREATE INDEX collections_deleting ON collections(id) WHERE deleting=1;
CREATE INDEX collections_owner ON collections(user_id,sort_order,id);
```

## file_details

同源版本的冷探测详情；随文件级联删除，不等于可随意清空的浏览缓存。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `file_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `source_revision` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `source_version` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `tracks_version` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `video_json` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `audio_json` | `TEXT` | — | 是 | `'[]'` | 见本表生产定义。 |
| `subtitle_json` | `TEXT` | — | 是 | `'[]'` | 见本表生产定义。 |
| `chapters_json` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `fingerprint_json` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `outro_fingerprint_json` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `format_duration_ms` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE file_details (
 file_id TEXT PRIMARY KEY NOT NULL REFERENCES library_files(file_id) ON DELETE CASCADE,
 source_revision INTEGER NOT NULL, source_version TEXT NOT NULL, tracks_version INTEGER NOT NULL,
 video_json TEXT, audio_json TEXT NOT NULL DEFAULT '[]', subtitle_json TEXT NOT NULL DEFAULT '[]', chapters_json TEXT,
 fingerprint_json TEXT, outro_fingerprint_json TEXT, format_duration_ms INTEGER,
 CHECK(json_valid(audio_json)), CHECK(json_valid(subtitle_json)), CHECK(video_json IS NULL OR json_valid(video_json)), CHECK(chapters_json IS NULL OR json_valid(chapters_json))
);
```

## file_probe_obligations

随文件登记的持久探测义务；文件删除级联移除。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `file_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `reason` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE file_probe_obligations (file_id TEXT PRIMARY KEY NOT NULL REFERENCES library_files(file_id) ON DELETE CASCADE,created_at_ms INTEGER NOT NULL,reason TEXT NOT NULL);
```

**显式命名索引**

```sql
CREATE INDEX probe_obligation_dispatch ON file_probe_obligations(created_at_ms,file_id);
```

## fingerprint_attempts

采样尝试、阶段、错误与工作指标；按作业/文件查询。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `attempt_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `job_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `file_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `kind` | `TEXT` | — | 是 | 无 | 本次指纹采样类别。 |
| `window_start_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `window_end_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `phase` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `started_at_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `finished_at_ms` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `status` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `error_kind` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `metrics_json` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE fingerprint_attempts (attempt_id TEXT PRIMARY KEY NOT NULL,job_id TEXT NOT NULL,file_id TEXT NOT NULL,kind TEXT NOT NULL,window_start_ms INTEGER NOT NULL,window_end_ms INTEGER NOT NULL,phase TEXT NOT NULL,started_at_ms INTEGER NOT NULL,finished_at_ms INTEGER,status TEXT NOT NULL,error_kind TEXT,metrics_json TEXT NOT NULL);
```

**显式命名索引**

```sql
CREATE INDEX attempt_job_file ON fingerprint_attempts(job_id,file_id);
CREATE INDEX attempts_file ON fingerprint_attempts(file_id);
```

## fingerprint_cache

每物理文件当前指纹缓存及算法/时长参数。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `file_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `cache_key` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `algorithm_version` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `sample_duration_secs` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `media_duration_ms` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `intro_json` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `outro_json` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `captured_at_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE fingerprint_cache (file_id TEXT PRIMARY KEY NOT NULL,cache_key TEXT NOT NULL,algorithm_version INTEGER NOT NULL,sample_duration_secs INTEGER NOT NULL,media_duration_ms INTEGER,intro_json TEXT NOT NULL,outro_json TEXT,captured_at_ms INTEGER NOT NULL);
```

## fingerprint_model_members

模型到样本/物理文件源版本的成员；仅 model_id 声明 FK。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `model_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `sample_id` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |
| `file_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `source_version` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE fingerprint_model_members (model_id TEXT NOT NULL REFERENCES fingerprint_season_models(model_id) ON DELETE CASCADE,sample_id TEXT NOT NULL,file_id TEXT NOT NULL,source_version TEXT NOT NULL,PRIMARY KEY(model_id,sample_id));
```

**显式命名索引**

```sql
CREATE INDEX model_file ON fingerprint_model_members(file_id);
```

## fingerprint_samples

按文件源版本、profile、kind、窗口去重的采样事实。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `sample_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `file_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `source_version` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `capture_profile_key` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `kind` | `TEXT` | — | 是 | 无 | 指纹样本的采样类别。 |
| `window_start_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `window_end_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `pcm_duration_ms` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `fingerprint_json` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `captured_job_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `captured_at_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `metrics_json` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE fingerprint_samples (sample_id TEXT PRIMARY KEY NOT NULL,file_id TEXT NOT NULL,source_version TEXT NOT NULL,capture_profile_key TEXT NOT NULL,kind TEXT NOT NULL,window_start_ms INTEGER NOT NULL,window_end_ms INTEGER NOT NULL,pcm_duration_ms INTEGER,fingerprint_json TEXT NOT NULL,captured_job_id TEXT NOT NULL,captured_at_ms INTEGER NOT NULL,metrics_json TEXT NOT NULL,UNIQUE(file_id,source_version,capture_profile_key,kind,window_start_ms,window_end_ms));
```

**显式命名索引**

```sql
CREATE INDEX sample_job ON fingerprint_samples(captured_job_id);
CREATE INDEX sample_lookup ON fingerprint_samples(file_id,kind,source_version,capture_profile_key);
```

## fingerprint_season_models

季级指纹模型、成员/政策版本与模型体。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `model_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `media_id` | `TEXT` | — | 是 | 无 | Media 身份。 |
| `season` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `kind` | `TEXT` | — | 是 | 无 | 季指纹模型类别。 |
| `model_version` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `membership_key` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `policy_key` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `model_json` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE fingerprint_season_models (model_id TEXT PRIMARY KEY NOT NULL,media_id TEXT NOT NULL,season INTEGER NOT NULL,kind TEXT NOT NULL,model_version INTEGER NOT NULL,membership_key TEXT NOT NULL,policy_key TEXT NOT NULL,model_json TEXT NOT NULL,created_at_ms INTEGER NOT NULL);
```

**显式命名索引**

```sql
CREATE INDEX season_model_lookup ON fingerprint_season_models(media_id,season,kind,policy_key,model_id);
```

## libraries

Library 配置、显示语言、访问范围、生命周期与完整性状态；后两类 operation 合同不等于已实现。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `library_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `kind` | `TEXT` | — | 是 | 无 | MediaKind：movie、tv 或 video。 |
| `name` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `lifecycle_state` | `TEXT` | — | 是 | `'ready'` | 见本表生产定义。 |
| `config_revision` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `integrity_state` | `TEXT` | — | 是 | `'unknown'` | 见本表生产定义。 |
| `display_locale` | `TEXT` | — | 是 | `'zh-CN'` | 见本表生产定义。 |
| `is_default` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `sort_order` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `access_mode` | `TEXT` | — | 是 | `'everyone'` | 见本表生产定义。 |
| `admin_visible` | `INTEGER` | — | 是 | `1` | 见本表生产定义。 |
| `member_ids_json` | `TEXT` | — | 是 | `'[]'` | 见本表生产定义。 |
| `detect_intros` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `enable_fingerprint` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `cover_path` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `cover_revision` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `match_rules_json` | `TEXT` | — | 是 | `'[]'` | 见本表生产定义。 |
| `default_filter_id` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `realtime_watch` | `INTEGER` | — | 是 | `1` | 见本表生产定义。 |
| `generate_thumbnails` | `INTEGER` | — | 是 | `1` | 见本表生产定义。 |
| `extract_chapter_images` | `INTEGER` | — | 是 | `1` | 见本表生产定义。 |
| `exclude_from_home` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `auto_series_collections` | `INTEGER` | — | 是 | `1` | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE libraries (
 library_id TEXT PRIMARY KEY NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('movie','tv','video')), name TEXT NOT NULL,
 lifecycle_state TEXT NOT NULL DEFAULT 'ready' CHECK(lifecycle_state IN ('ready','frozen','draining','reindexing','failed')),
 config_revision INTEGER NOT NULL DEFAULT 0, integrity_state TEXT NOT NULL DEFAULT 'unknown' CHECK(integrity_state IN ('unknown','clean','failed')),
 display_locale TEXT NOT NULL DEFAULT 'zh-CN', is_default INTEGER NOT NULL DEFAULT 0 CHECK(is_default IN (0,1)), sort_order INTEGER NOT NULL DEFAULT 0,
 access_mode TEXT NOT NULL DEFAULT 'everyone', admin_visible INTEGER NOT NULL DEFAULT 1,
 member_ids_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(member_ids_json)),
 detect_intros INTEGER NOT NULL DEFAULT 0, enable_fingerprint INTEGER NOT NULL DEFAULT 0,
 cover_path TEXT, cover_revision INTEGER NOT NULL DEFAULT 0, match_rules_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(match_rules_json)),
 default_filter_id TEXT, realtime_watch INTEGER NOT NULL DEFAULT 1, generate_thumbnails INTEGER NOT NULL DEFAULT 1,
 extract_chapter_images INTEGER NOT NULL DEFAULT 1, exclude_from_home INTEGER NOT NULL DEFAULT 0, auto_series_collections INTEGER NOT NULL DEFAULT 1,
 UNIQUE(library_id,kind)
);
```

**显式命名索引**

```sql
CREATE UNIQUE INDEX library_default ON libraries(kind) WHERE is_default=1;
```

## library_file_units

文件到逻辑单元的覆盖事实；一文件可覆盖多集，同集可有多版本。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `file_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `library_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `media_id` | `TEXT` | — | 是 | 无 | Media 身份。 |
| `season` | `INTEGER` | 2 | 是 | 无 | 见本表生产定义。 |
| `episode` | `INTEGER` | 3 | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE library_file_units (
 file_id TEXT NOT NULL, library_id TEXT NOT NULL, media_id TEXT NOT NULL, season INTEGER NOT NULL, episode INTEGER NOT NULL,
 PRIMARY KEY(file_id,season,episode), UNIQUE(library_id,media_id,season,episode,file_id),
 FOREIGN KEY(file_id,library_id,media_id) REFERENCES library_files(file_id,library_id,media_id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
 CHECK((season=-1 AND episode=-1) OR (season>=0 AND episode>=0))
) WITHOUT ROWID;
```

**显式命名索引**

```sql
CREATE INDEX coverage_media ON library_file_units(media_id,season,episode,library_id,file_id);
```

## library_files

权威物理文件台账；显式 Library/root/Media/kind 与源版本、在位/发布状态、热质量字段。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `file_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `library_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `root_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `media_id` | `TEXT` | — | 是 | 无 | Media 身份。 |
| `kind` | `TEXT` | — | 是 | 无 | MediaKind：movie、tv 或 video。 |
| `path_key` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `display_path` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `presence_state` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `publication_state` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `file_revision` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `source_version` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `size_bytes` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `stat_known` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `mtime_ns` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `created_ns` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `source_digest` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `missing_at` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `source_path` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `transfer_mode` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `resolution` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `codec` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `hdr` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `hdr_observed` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `quality_source` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `confidence` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `filter_score` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `release_quality` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `duration_ms` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `tracks_version` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `poster_path` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `backdrop_path` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `still_path` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `has_poster` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `has_backdrop` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `has_still` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE library_files (
 file_id TEXT PRIMARY KEY NOT NULL, library_id TEXT NOT NULL, root_id TEXT NOT NULL, media_id TEXT NOT NULL, kind TEXT NOT NULL,
 path_key TEXT NOT NULL UNIQUE CHECK(substr(path_key,1,1)='/' AND instr(path_key,'//')=0 AND instr(path_key,'/./')=0 AND instr(path_key,'/../')=0 AND path_key NOT LIKE '%/.' AND path_key NOT LIKE '%/..' AND (path_key='/' OR path_key NOT LIKE '%/')), display_path TEXT NOT NULL, presence_state TEXT NOT NULL CHECK(presence_state IN ('present','missing','unknown')),
 publication_state TEXT NOT NULL CHECK(publication_state IN ('pending','published')),
 file_revision INTEGER NOT NULL CHECK(file_revision>0), source_version TEXT NOT NULL,
 size_bytes INTEGER CHECK(size_bytes IS NULL OR size_bytes>=0), stat_known INTEGER NOT NULL DEFAULT 0,
 mtime_ns INTEGER, created_ns INTEGER, source_digest TEXT, missing_at INTEGER,
 source_path TEXT, transfer_mode TEXT CHECK(transfer_mode IS NULL OR transfer_mode IN ('hardlink','copy','move')),
 resolution TEXT, codec TEXT, hdr TEXT, hdr_observed INTEGER NOT NULL DEFAULT 0 CHECK(hdr_observed IN (0,1)),
 quality_source TEXT NOT NULL CHECK(quality_source IN ('probe','release')),
 confidence TEXT NOT NULL CHECK(confidence IN ('high','low')), filter_score INTEGER, release_quality TEXT,
 duration_ms INTEGER CHECK(duration_ms IS NULL OR duration_ms>=0), tracks_version INTEGER NOT NULL DEFAULT 0,
 poster_path TEXT, backdrop_path TEXT, still_path TEXT,
 has_poster INTEGER NOT NULL DEFAULT 0, has_backdrop INTEGER NOT NULL DEFAULT 0, has_still INTEGER NOT NULL DEFAULT 0,
 UNIQUE(file_id,library_id,media_id),
 FOREIGN KEY(root_id,library_id) REFERENCES library_roots(root_id,library_id),
 FOREIGN KEY(library_id,kind) REFERENCES libraries(library_id,kind), FOREIGN KEY(media_id,kind) REFERENCES media(media_id,kind)
);
```

**显式命名索引**

```sql
CREATE INDEX files_kind_path ON library_files(kind,path_key);
CREATE INDEX files_library_file ON library_files(library_id,file_id);
CREATE INDEX files_library_media ON library_files(library_id,media_id,file_id);
CREATE INDEX files_media_library ON library_files(media_id,library_id,file_id);
CREATE INDEX files_missing ON library_files(library_id,missing_at,file_id) WHERE missing_at IS NOT NULL;
CREATE INDEX files_presence ON library_files(library_id,presence_state,file_id);
CREATE INDEX files_root_path ON library_files(root_id,path_key);
CREATE INDEX files_roster_stats ON library_files(library_id,presence_state,publication_state,path_key);
```

## library_integrity_rows

Integrity 当前 Media 的持久中间结果，按 surface 和类型化稳定键分页；不作为业务事实来源或普通浏览入口。游标保存阶段和固定大小累加器，不保存整个文件、单元或用户列表。当前 Media 完成核对后按页清理，再推进到下一个 Media。

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `operation_id` | `TEXT` | 1 | 是 | 无 | 拥有这些中间结果的持久 Integrity 操作；删除操作时级联清理。 |
| `media_id` | `TEXT` | 2 | 是 | 无 | 当前正在分页检查的 Media；引用检查使用 media_id 前缀索引。 |
| `surface` | `TEXT` | 3 | 是 | 无 | 本行对应的卡片、单元、用户、继续观看或术语成员投影类别。 |
| `user_id` | `TEXT` | 4 | 是 | `''` | 用户或继续观看行的用户身份；其他类别为空串。 |
| `season` | `INTEGER` | 5 | 是 | `-1` | 单元坐标；whole 或不适用时为 -1。 |
| `episode` | `INTEGER` | 6 | 是 | `-1` | 单元坐标；whole 或不适用时为 -1。 |
| `key_a` | `TEXT` | 7 | 是 | `''` | 术语、人物或实体的稳定复合键第一部分；卡片和用户单元行为空串。 |
| `key_b` | `TEXT` | 8 | 是 | `''` | 稳定复合键第二部分；不适用时为空串。 |
| `key_c` | `TEXT` | 9 | 是 | `''` | 人物等稳定复合键第三部分；不适用时为空串。 |
| `row_json` | `TEXT` | — | 是 | 无 | 期望投影行的 JSON 对象；解码保留 SQLite 值类型。 |

**生产表定义**

```sql
CREATE TABLE library_integrity_rows (
 operation_id TEXT NOT NULL REFERENCES library_operations(operation_id) ON DELETE CASCADE,
 media_id TEXT NOT NULL REFERENCES media(media_id),
 surface TEXT NOT NULL CHECK(surface IN ('card','unit','user','continue','web_term','protocol_term','person','entity','member')),
 user_id TEXT NOT NULL DEFAULT '', season INTEGER NOT NULL DEFAULT -1, episode INTEGER NOT NULL DEFAULT -1,
 key_a TEXT NOT NULL DEFAULT '', key_b TEXT NOT NULL DEFAULT '', key_c TEXT NOT NULL DEFAULT '',
 row_json TEXT NOT NULL CHECK(json_valid(row_json) AND json_type(row_json)='object'),
 PRIMARY KEY(operation_id,media_id,surface,user_id,season,episode,key_a,key_b,key_c),
 CHECK((surface IN ('user','continue') AND user_id<>'') OR (surface NOT IN ('user','continue') AND user_id='')),
 CHECK((surface IN ('unit','continue') AND ((season=-1 AND episode=-1) OR (season>=0 AND episode>=0))) OR (surface NOT IN ('unit','continue') AND season=-1 AND episode=-1)),
 CHECK(surface NOT IN ('card','unit','user','continue') OR (key_a='' AND key_b='' AND key_c='')),
 CHECK(surface NOT IN ('web_term','protocol_term','entity') OR key_c='')
) WITHOUT ROWID;
```

**显式命名索引**

```sql
CREATE INDEX integrity_rows_media ON library_integrity_rows(media_id);
```

## library_item_terms

公共卡片多值倒排；私有 Collection 不得混入公共 terms/facets。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `library_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `term_kind` | `TEXT` | 3 | 是 | 无 | 见本表生产定义。 |
| `term_value` | `TEXT` | 4 | 是 | 无 | 见本表生产定义。 |
| `media_id` | `TEXT` | 2 | 是 | 无 | Media 身份。 |

**生产表定义**

```sql
CREATE TABLE library_item_terms (
 library_id TEXT NOT NULL, term_kind TEXT NOT NULL CHECK(term_kind IN ('genre','country','resolution','decade','runtime','language','rating','hdr','stock','collection','studio')),
 term_value TEXT NOT NULL, media_id TEXT NOT NULL, PRIMARY KEY(library_id,media_id,term_kind,term_value),
 FOREIGN KEY(library_id,media_id) REFERENCES library_items(library_id,media_id) ON DELETE CASCADE
) WITHOUT ROWID;
```

**显式命名索引**

```sql
CREATE INDEX terms_value ON library_item_terms(term_kind,term_value,library_id,media_id);
```

## library_items

Library/Media 浏览摘要。依赖同事务维护，不保留完整文件数组或来源响应。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `search_rowid` | `INTEGER` | 1 | 否 | 无 | 见本表生产定义。 |
| `library_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `media_id` | `TEXT` | — | 是 | 无 | Media 身份。 |
| `kind` | `TEXT` | — | 是 | 无 | MediaKind：movie、tv 或 video。 |
| `title` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `title_sort` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `year` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `identity_state` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `metadata_revision` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `item_revision` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `protocol_title_key` | `TEXT` | — | 是 | `''` | 见本表生产定义。 |
| `protocol_original_title_key` | `TEXT` | — | 是 | `''` | 见本表生产定义。 |
| `query_projection_revision` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `query_config_revision` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `release_date_sort` | `TEXT` | — | 是 | `''` | 见本表生产定义。 |
| `rating_sort` | `REAL` | — | 是 | `-1` | 见本表生产定义。 |
| `runtime_sort` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `language` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `added_at_sort` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `size_bytes` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `size_sort` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `resolution_rank` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `hdr` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `scraped` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `missing` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `stable_shuffle_key` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `file_count` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `available_file_count` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `missing_file_count` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `unknown_file_count` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `distinct_episode_count` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `distinct_season_count` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `preferred_file_id` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `poster_ref` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `backdrop_ref` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `has_still` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `search_text` | `TEXT` | — | 是 | `''` | 见本表生产定义。 |
| `first_owned_season` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `first_owned_episode` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE library_items (
 search_rowid INTEGER PRIMARY KEY,
 library_id TEXT NOT NULL REFERENCES libraries(library_id), media_id TEXT NOT NULL REFERENCES media(media_id), kind TEXT NOT NULL,
 title TEXT NOT NULL, title_sort TEXT NOT NULL, year INTEGER, identity_state TEXT NOT NULL,
 metadata_revision INTEGER NOT NULL, item_revision INTEGER NOT NULL,
 protocol_title_key TEXT NOT NULL DEFAULT '', protocol_original_title_key TEXT NOT NULL DEFAULT '', query_projection_revision INTEGER NOT NULL DEFAULT 0,
 query_config_revision INTEGER NOT NULL DEFAULT 0,
 release_date_sort TEXT NOT NULL DEFAULT '', rating_sort REAL NOT NULL DEFAULT -1, runtime_sort INTEGER NOT NULL DEFAULT 0,
 language TEXT, added_at_sort INTEGER NOT NULL, size_bytes INTEGER, size_sort INTEGER NOT NULL DEFAULT 0,
 resolution_rank INTEGER NOT NULL DEFAULT 0, hdr INTEGER NOT NULL DEFAULT 0, scraped INTEGER NOT NULL DEFAULT 0,
 missing INTEGER NOT NULL DEFAULT 0, stable_shuffle_key TEXT NOT NULL,
 file_count INTEGER NOT NULL CHECK(file_count>0), available_file_count INTEGER NOT NULL,
 missing_file_count INTEGER NOT NULL DEFAULT 0, unknown_file_count INTEGER NOT NULL DEFAULT 0,
 distinct_episode_count INTEGER NOT NULL DEFAULT 0, distinct_season_count INTEGER NOT NULL DEFAULT 0,
 preferred_file_id TEXT, poster_ref TEXT, backdrop_ref TEXT, has_still INTEGER NOT NULL DEFAULT 0,
 search_text TEXT NOT NULL DEFAULT '',
 first_owned_season INTEGER, first_owned_episode INTEGER,
 UNIQUE(library_id,media_id),
 FOREIGN KEY(preferred_file_id,library_id,media_id) REFERENCES library_files(file_id,library_id,media_id) DEFERRABLE INITIALLY DEFERRED,
 CHECK(available_file_count>=0 AND missing_file_count>=0 AND unknown_file_count>=0),
 CHECK(available_file_count+missing_file_count+unknown_file_count=file_count), CHECK(available_file_count=0 OR preferred_file_id IS NOT NULL),
 CHECK(missing=(missing_file_count>0))
);
```

**显式命名索引**

```sql
CREATE INDEX items_added ON library_items(library_id,added_at_sort,media_id);
CREATE INDEX items_media ON library_items(media_id,library_id);
CREATE INDEX items_protocol_default ON library_items(library_id,added_at_sort DESC,protocol_title_key DESC,media_id ASC);
CREATE INDEX items_protocol_default_explicit ON library_items(library_id,added_at_sort DESC,protocol_title_key DESC,title ASC,media_id ASC);
CREATE INDEX items_protocol_name_asc ON library_items(library_id,protocol_title_key ASC,title ASC,media_id ASC);
CREATE INDEX items_protocol_name_desc ON library_items(library_id,protocol_title_key DESC,title ASC,media_id ASC);
CREATE INDEX items_rating ON library_items(library_id,rating_sort,media_id);
CREATE INDEX items_release ON library_items(library_id,release_date_sort,media_id);
CREATE INDEX items_resolution ON library_items(library_id,resolution_rank,media_id);
CREATE INDEX items_roster_stats ON library_items(library_id,size_bytes);
CREATE INDEX items_runtime ON library_items(library_id,runtime_sort,media_id);
CREATE INDEX items_search_prefix ON library_items(library_id,protocol_title_key,title_sort,media_id);
CREATE INDEX items_search_title ON library_items(title_sort,media_id,library_id,search_rowid);
CREATE INDEX items_shuffle ON library_items(library_id,stable_shuffle_key,media_id);
CREATE INDEX items_size ON library_items(library_id,size_sort,media_id);
CREATE INDEX items_title ON library_items(library_id,title_sort,media_id);
```

## library_protocol_people

协议 person 的规范化精确成员；写入期维护 Actor/Director/Writer 的名称、来源 ID 与 wire ID。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `library_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `media_id` | `TEXT` | 2 | 是 | 无 | Media 身份。 |
| `role` | `TEXT` | 3 | 是 | 无 | 见本表生产定义。 |
| `name_key` | `TEXT` | 4 | 是 | 无 | 见本表生产定义。 |
| `tmdb_id` | `TEXT` | 5 | 是 | 无 | 见本表生产定义。 |
| `wire_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `display_name` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `character` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `image_url` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE library_protocol_people (
 library_id TEXT NOT NULL, media_id TEXT NOT NULL,
 role TEXT NOT NULL CHECK(role IN ('Actor','Director','Writer')),
 name_key TEXT NOT NULL, tmdb_id TEXT NOT NULL, wire_id TEXT NOT NULL,
 display_name TEXT NOT NULL, character TEXT, image_url TEXT,
 PRIMARY KEY(library_id,media_id,role,name_key,tmdb_id),
 FOREIGN KEY(library_id,media_id) REFERENCES library_items(library_id,media_id) ON DELETE CASCADE
) WITHOUT ROWID;
```

**显式命名索引**

```sql
CREATE INDEX protocol_people_image_tmdb ON library_protocol_people(tmdb_id,image_url DESC,library_id,media_id);
CREATE INDEX protocol_people_name ON library_protocol_people(name_key,image_url DESC,library_id,media_id,role,tmdb_id);
CREATE INDEX protocol_people_tmdb ON library_protocol_people(tmdb_id,role,library_id,media_id);
CREATE INDEX protocol_people_wire ON library_protocol_people(wire_id,image_url DESC,library_id,media_id,role,name_key,tmdb_id);
```

## library_protocol_terms

协议 genre/studio 的规范化精确成员；与公共浏览倒排分开。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `library_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `media_id` | `TEXT` | 4 | 是 | 无 | Media 身份。 |
| `term_kind` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |
| `term_value` | `TEXT` | 3 | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE library_protocol_terms (
 library_id TEXT NOT NULL, media_id TEXT NOT NULL,
 term_kind TEXT NOT NULL CHECK(term_kind IN ('genre','studio')), term_value TEXT NOT NULL,
 PRIMARY KEY(library_id,term_kind,term_value,media_id),
 FOREIGN KEY(library_id,media_id) REFERENCES library_items(library_id,media_id) ON DELETE CASCADE
) WITHOUT ROWID;
```

**显式命名索引**

```sql
CREATE INDEX protocol_terms_media ON library_protocol_terms(library_id,media_id,term_kind,term_value);
```

## library_query_config

查询默认语言及配置版本单例。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `INTEGER` | 1 | 否 | 无 | 记录身份或单例键。 |
| `default_locale` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `revision` | `INTEGER` | — | 是 | `0` | 默认语言查询配置修订号。 |

**生产表定义**

```sql
CREATE TABLE library_query_config (
 id INTEGER PRIMARY KEY CHECK(id=1), default_locale TEXT, revision INTEGER NOT NULL DEFAULT 0
);
```

## library_query_count_rebuild

Integrity 修复期间的计数暂存；只有操作提交后才替换公开边际计数。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `operation_id` | `TEXT` | 1 | 是 | 无 | 所属持久操作标识。 |
| `surface` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |
| `dimension` | `TEXT` | 3 | 是 | 无 | 见本表生产定义。 |
| `value` | `TEXT` | 4 | 是 | 无 | 见本表生产定义。 |
| `member_count` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE library_query_count_rebuild (
 operation_id TEXT NOT NULL REFERENCES library_operations(operation_id) ON DELETE CASCADE,
 surface TEXT NOT NULL, dimension TEXT NOT NULL, value TEXT NOT NULL,
 member_count INTEGER NOT NULL CHECK(member_count>=0),
 PRIMARY KEY(operation_id,surface,dimension,value)
);
```

## library_query_counts

按 Library、查询表面、维度、值保存非负精确边际计数。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `library_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `surface` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |
| `dimension` | `TEXT` | 3 | 是 | 无 | 见本表生产定义。 |
| `value` | `TEXT` | 4 | 是 | 无 | 见本表生产定义。 |
| `member_count` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE library_query_counts (
 library_id TEXT NOT NULL REFERENCES libraries(library_id) ON DELETE CASCADE,
 surface TEXT NOT NULL CHECK(surface IN ('browse','protocol_work','protocol_entity')),
 dimension TEXT NOT NULL, value TEXT NOT NULL, member_count INTEGER NOT NULL CHECK(member_count>=0),
 PRIMARY KEY(library_id,surface,dimension,value)
);
```

## library_metadata_inputs

Library 独立 NFO 输入、解析状态与导出来源字段。导出使用持久意图/版本校验、路径独占与执行 lease，保存失败和恢复状态。

**写入方与生命周期**

- `input_key` 指 sidecar，`path_key` 指关联视频，NFO 观察/录入统一使用 canonical 身份。display 保留 IO 拼写；rename 共享图片也按登记身份判断，仍有引用时保留源图片。
- 候选包括视频旁NFO及祖先`tvshow.nfo`等；观察不等于每个候选都会持久一行。Missing删除该输入，Invalid/Unknown保留上一份有效输入；Valid按实体/坐标/locale/input_key写入。
- `entity_scope`来自NFO层级与可确定坐标，不能将未知TV升为work。已有sidecar沿用首次登记的locale；display_locale切换不重标输入语言。实体层级或坐标实际变化时，仅替换该sidecar对应旧行。
- locale由观察调用参数提供，当前没有从NFO声明语言自动推导的保证。卡片/详情按明确语言选择规则读取；不能在无命中时任取一份外语输入。
- 录入事务验证原file/Media/root/path/revision/source与Library config_revision及当前最深根，不符则拒绝这一组观察。共享Store的扫描/Claim采用准备、锁外观察、提交三阶段；`reconcile_nfo_inputs`为包含路径IO的独立Store便捷入口。编辑/Unknown/删除各保留其语义。
- work 输入参与卡片/详情；season/episode 输入用于相应详情和协议读取。输入随文件 batch 迁移/复制，根 operation 和跨批重启需要验证输入守恒。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `library_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `media_id` | `TEXT` | 2 | 是 | 无 | Media 身份。 |
| `entity_scope` | `TEXT` | 3 | 是 | 无 | 见本表生产定义。 |
| `season` | `INTEGER` | 4 | 是 | 无 | 见本表生产定义。 |
| `episode` | `INTEGER` | 5 | 是 | 无 | 见本表生产定义。 |
| `locale` | `TEXT` | 6 | 是 | 无 | 见本表生产定义。 |
| `input_key` | `TEXT` | 7 | 是 | 无 | 见本表生产定义。 |
| `root_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `path_key` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `parse_state` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `source_version` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `fields_json` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `present_fields_json` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `provenance_json` | `TEXT` | — | 是 | `'{}'` | 见本表生产定义。 |
| `export_id` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `export_digest` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `export_fields_json` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `revision` | `INTEGER` | — | 是 | 无 | 该文件元数据输入的修订号。 |

**生产表定义**

```sql
CREATE TABLE library_metadata_inputs (
 library_id TEXT NOT NULL, media_id TEXT NOT NULL REFERENCES media(media_id), entity_scope TEXT NOT NULL,
 season INTEGER NOT NULL, episode INTEGER NOT NULL, locale TEXT NOT NULL, input_key TEXT NOT NULL,
 root_id TEXT NOT NULL, path_key TEXT NOT NULL CHECK(substr(path_key,1,1)='/' AND instr(path_key,'//')=0 AND instr(path_key,'/./')=0 AND instr(path_key,'/../')=0 AND path_key NOT LIKE '%/.' AND path_key NOT LIKE '%/..' AND (path_key='/' OR path_key NOT LIKE '%/')), parse_state TEXT NOT NULL CHECK(parse_state IN ('valid','invalid','unknown','missing')),
 source_version TEXT NOT NULL, fields_json TEXT NOT NULL CHECK(json_valid(fields_json)),
 present_fields_json TEXT NOT NULL CHECK(json_valid(present_fields_json)),
 provenance_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(provenance_json)),
 export_id TEXT, export_digest TEXT, export_fields_json TEXT CHECK(export_fields_json IS NULL OR json_valid(export_fields_json)), revision INTEGER NOT NULL,
 PRIMARY KEY(library_id,media_id,entity_scope,season,episode,locale,input_key),
 FOREIGN KEY(root_id,library_id) REFERENCES library_roots(root_id,library_id),
 CHECK((entity_scope='work' AND season=-1 AND episode=-1) OR (entity_scope='season' AND season>=0 AND episode=-1) OR (entity_scope='episode' AND season>=0 AND episode>=0))
);
```

**显式命名索引**

```sql
CREATE INDEX metadata_input_path ON library_metadata_inputs(root_id,path_key);
CREATE INDEX metadata_inputs_media ON library_metadata_inputs(media_id);
```

## library_operations

roots/locale/integrity/scrape 操作载体，由运行驱动推进并从持久游标恢复；终态以 owning module 为准，failed Scrape 为终态，failed Integrity 等仍可恢复。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `operation_id` | `TEXT` | 1 | 是 | 无 | 所属持久操作标识。 |
| `library_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `kind` | `TEXT` | — | 是 | 无 | Library 操作类别：roots、locale、default_locale、integrity 或 scrape。 |
| `state` | `TEXT` | — | 是 | 无 | 持久操作或 fence 状态。 |
| `expected_revision` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `plan_json` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `cursor_json` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `error` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `updated_at_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `resource_key` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE library_operations (
 operation_id TEXT PRIMARY KEY NOT NULL, library_id TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('roots','locale','default_locale','integrity','scrape')),
 state TEXT NOT NULL CHECK(state IN ('planned','frozen','draining','applying','checking','succeeded','failed')), expected_revision INTEGER NOT NULL, plan_json TEXT NOT NULL CHECK(json_valid(plan_json)),
 cursor_json TEXT, error TEXT, created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL,
 resource_key TEXT CHECK(kind<>'scrape' OR (resource_key IS NOT NULL AND length(resource_key)>0))
);
```

**显式命名索引**

```sql
CREATE INDEX library_integrity_owner ON library_operations(library_id)
 WHERE kind='integrity' AND state<>'succeeded';
CREATE INDEX library_operation_dispatch ON library_operations(state,updated_at_ms,operation_id);
CREATE INDEX library_operations_terminal_age ON library_operations(updated_at_ms,operation_id)
 WHERE state = 'succeeded' OR (state = 'failed' AND kind NOT IN ('roots','locale','integrity','default_locale'));
CREATE UNIQUE INDEX nfo_export_active_path ON library_operations(resource_key)
 WHERE kind='scrape' AND state NOT IN ('succeeded','failed');
CREATE INDEX nfo_export_pending_age ON library_operations(updated_at_ms,operation_id)
 WHERE kind='scrape' AND state NOT IN ('succeeded','failed');
CREATE INDEX operations_library_kind ON library_operations(library_id,kind,updated_at_ms DESC);
```

## library_removal_roots

删除操作冻结的根路径范围；与持久文件归属需要一致。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `operation_id` | `TEXT` | 1 | 是 | 无 | 所属持久操作标识。 |
| `path` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE library_removal_roots (operation_id TEXT NOT NULL REFERENCES library_removals(operation_id),path TEXT NOT NULL,PRIMARY KEY(operation_id,path));
```

## library_removals

Library 删除先原子冻结配置，再按目标 Library 的文件索引逐 100 行捕获快照；snapshot_cursor/snapshot_done 记录进度。API 在快照和清理窗口之间真正释放 Store。立即取消该库所有任务后推进删除；最后文件消失时，仅在无其他文件、Collection 或 Subscribe 保留引用时清理孤儿 Media 及其 favorites、观看状态与播放进度。Library 删除本身保留磁盘字节。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `operation_id` | `TEXT` | 1 | 是 | 无 | 所属持久操作标识。 |
| `library_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `state` | `TEXT` | — | 是 | 无 | 持久操作或 fence 状态。 |
| `config_revision` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `snapshot_cursor` | `TEXT` | — | 是 | `''` | 见本表生产定义。 |
| `snapshot_done` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `total_files` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `error` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `updated_at_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE library_removals (
 operation_id TEXT PRIMARY KEY NOT NULL, library_id TEXT NOT NULL UNIQUE, state TEXT NOT NULL,
 config_revision INTEGER NOT NULL, snapshot_cursor TEXT NOT NULL DEFAULT '', snapshot_done INTEGER NOT NULL DEFAULT 0, total_files INTEGER NOT NULL DEFAULT 0,
 error TEXT, created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL
);
```

## library_roots

根路径配置。path_key唯一；新增嵌套根通过pending_root_reassignments分批重归属。根替换另由library_operations驱动；两条入口不能混作同一恢复协议。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `root_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `library_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `path_key` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `display_path` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `enabled` | `INTEGER` | — | 是 | `1` | 见本表生产定义。 |
| `sort_order` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE library_roots (
 root_id TEXT PRIMARY KEY NOT NULL, library_id TEXT NOT NULL REFERENCES libraries(library_id),
 path_key TEXT NOT NULL UNIQUE CHECK(substr(path_key,1,1)='/' AND instr(path_key,'//')=0 AND instr(path_key,'/./')=0 AND instr(path_key,'/../')=0 AND path_key NOT LIKE '%/.' AND path_key NOT LIKE '%/..' AND (path_key='/' OR path_key NOT LIKE '%/')), display_path TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
 sort_order INTEGER NOT NULL DEFAULT 0, UNIQUE(root_id,library_id)
);
```

**显式命名索引**

```sql
CREATE INDEX roots_library ON library_roots(library_id,sort_order,root_id);
```

## library_units

Library/Media/单元级去重摘要和首选文件。whole=(-1,-1)，TV S00E00 合法。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `library_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `media_id` | `TEXT` | 2 | 是 | 无 | Media 身份。 |
| `season` | `INTEGER` | 3 | 是 | 无 | 见本表生产定义。 |
| `episode` | `INTEGER` | 4 | 是 | 无 | 见本表生产定义。 |
| `file_count` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `available_file_count` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `preferred_file_id` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `unit_revision` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE library_units (
 library_id TEXT NOT NULL, media_id TEXT NOT NULL, season INTEGER NOT NULL, episode INTEGER NOT NULL,
 file_count INTEGER NOT NULL CHECK(file_count>0), available_file_count INTEGER NOT NULL CHECK(available_file_count BETWEEN 0 AND file_count),
 preferred_file_id TEXT, unit_revision INTEGER NOT NULL,
 PRIMARY KEY(library_id,media_id,season,episode),
 FOREIGN KEY(library_id) REFERENCES libraries(library_id), FOREIGN KEY(media_id) REFERENCES media(media_id),
 FOREIGN KEY(library_id,media_id,season,episode,preferred_file_id) REFERENCES library_file_units(library_id,media_id,season,episode,file_id) DEFERRABLE INITIALLY DEFERRED,
 CHECK((season=-1 AND episode=-1) OR (season>=0 AND episode>=0)), CHECK(available_file_count=0 OR preferred_file_id IS NOT NULL)
);
```

**显式命名索引**

```sql
CREATE INDEX units_media_scope ON library_units(media_id,season,episode,library_id);
CREATE INDEX units_protocol_order ON library_units(library_id,season ASC,episode ASC,media_id ASC);
```

## media

规范 Media 身份；可无文件存在。retired 墓碑不是可继续写的兼容实体。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `media_id` | `TEXT` | 1 | 是 | 无 | Media 身份。 |
| `kind` | `TEXT` | — | 是 | 无 | MediaKind：movie、tv 或 video。 |
| `title` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `original_title` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `year` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `identity_state` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `aliases_display_json` | `TEXT` | — | 是 | `'[]'` | 见本表生产定义。 |
| `metadata_revision` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `survivor_id` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE media (
 media_id TEXT PRIMARY KEY NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('movie','tv','video')),
 title TEXT NOT NULL, original_title TEXT, year INTEGER CHECK(year IS NULL OR year BETWEEN 0 AND 9999),
 identity_state TEXT NOT NULL CHECK(identity_state IN ('provisional','confirmed','merging','retired')),
 aliases_display_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(aliases_display_json)),
 metadata_revision INTEGER NOT NULL DEFAULT 0, survivor_id TEXT REFERENCES media(media_id),
 UNIQUE(media_id,kind), CHECK((identity_state='retired')=(survivor_id IS NOT NULL)), CHECK(survivor_id IS NULL OR survivor_id<>media_id)
);
```

**显式命名索引**

```sql
CREATE INDEX media_survivor ON media(survivor_id) WHERE survivor_id IS NOT NULL;
CREATE INDEX media_title ON media(kind,title,year,media_id);
```

## media_aliases

来源别名归属及每来源唯一刷新目标；不把来源 ID 当内部身份。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `source` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `kind` | `TEXT` | 2 | 是 | 无 | MediaKind：movie、tv 或 video。 |
| `external_id` | `TEXT` | 3 | 是 | 无 | 见本表生产定义。 |
| `media_id` | `TEXT` | — | 是 | 无 | Media 身份。 |
| `is_refresh_target` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE media_aliases (
 source TEXT NOT NULL CHECK(source IN ('tmdb','douban','tvdb','bangumi','anilist')),
 kind TEXT NOT NULL, external_id TEXT NOT NULL, media_id TEXT NOT NULL,
 is_refresh_target INTEGER NOT NULL DEFAULT 0 CHECK(is_refresh_target IN (0,1)),
 PRIMARY KEY(source,kind,external_id), FOREIGN KEY(media_id,kind) REFERENCES media(media_id,kind)
);
```

**显式命名索引**

```sql
CREATE INDEX alias_media ON media_aliases(media_id,source);
CREATE UNIQUE INDEX alias_target ON media_aliases(media_id,source,kind) WHERE is_refresh_target=1;
```

## media_episodes

已采用分集详情；拥有事实来自文件覆盖。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `media_id` | `TEXT` | 1 | 是 | 无 | Media 身份。 |
| `season` | `INTEGER` | 2 | 是 | 无 | 见本表生产定义。 |
| `episode` | `INTEGER` | 3 | 是 | 无 | 见本表生产定义。 |
| `locale` | `TEXT` | 4 | 是 | 无 | 见本表生产定义。 |
| `title` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `overview` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `air_date` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `runtime_minutes` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `rating` | `REAL` | — | 否 | 无 | 见本表生产定义。 |
| `still_url` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `details_json` | `TEXT` | — | 是 | `'{}'` | 见本表生产定义。 |
| `field_origins_json` | `TEXT` | — | 是 | `'{}'` | 见本表生产定义。 |
| `revision` | `INTEGER` | — | 是 | 无 | 该集该语言的元数据修订号。 |

**生产表定义**

```sql
CREATE TABLE media_episodes (
 media_id TEXT NOT NULL REFERENCES media(media_id), season INTEGER NOT NULL CHECK(season>=0),
 episode INTEGER NOT NULL CHECK(episode>=0), locale TEXT NOT NULL, title TEXT, overview TEXT, air_date TEXT,
 runtime_minutes INTEGER, rating REAL, still_url TEXT,
 details_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(details_json)),
 field_origins_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(field_origins_json)), revision INTEGER NOT NULL,
 PRIMARY KEY(media_id,season,episode,locale)
);
```

## file_markers

精确文件版本与单元的片头片尾标记及人工锁。所有读取同时核对 library_files 的当前 revision/source_version 与登记覆盖；锁定行也不能跨源版本。Whole 使用 (-1,-1)，不同文件即使覆盖同一集也不共享范围。仅 checked 事务发布，TheIntroDB 未经本文件验证的候选不能写入或用于正式 overlay。

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `file_id` | `TEXT` | 1 | 是 | 无 | 文件身份，FK → library_files，ON DELETE CASCADE。 |
| `file_revision` | `INTEGER` | — | 是 | 无 | 发布时文件版本的 revision。 |
| `source_version` | `TEXT` | — | 是 | 无 | 发布时文件版本的 source_version。 |
| `season` | `INTEGER` | 2 | 是 | 无 | UnitKey season，Whole 为 -1。 |
| `episode` | `INTEGER` | 3 | 是 | 无 | UnitKey episode，Whole 为 -1。 |
| `intro_start_ms` | `INTEGER` | — | 否 | 无 | 片头起始毫秒。 |
| `intro_end_ms` | `INTEGER` | — | 否 | 无 | 片头结束毫秒。 |
| `outro_start_ms` | `INTEGER` | — | 否 | 无 | 片尾起始毫秒。 |
| `outro_end_ms` | `INTEGER` | — | 否 | 无 | 片尾结束毫秒。 |
| `source` | `TEXT` | — | 是 | 无 | 已验证标记的来源。 |
| `locked` | `INTEGER` | — | 是 | `0` | 保留当前文件版本的人工范围。 |
| `updated_at` | `INTEGER` | — | 是 | 无 | Unix 秒。 |

**生产表定义**

```sql
CREATE TABLE file_markers (file_id TEXT NOT NULL REFERENCES library_files(file_id) ON DELETE CASCADE,file_revision INTEGER NOT NULL,source_version TEXT NOT NULL,season INTEGER NOT NULL,episode INTEGER NOT NULL,intro_start_ms INTEGER,intro_end_ms INTEGER,outro_start_ms INTEGER,outro_end_ms INTEGER,source TEXT NOT NULL,locked INTEGER NOT NULL DEFAULT 0,updated_at INTEGER NOT NULL,PRIMARY KEY(file_id,season,episode));
```

## media_metadata

work 级已采用元数据；effective/compose 合成来源并维护字段 provenance。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `media_id` | `TEXT` | 1 | 是 | 无 | Media 身份。 |
| `locale` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |
| `title` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `original_title` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `year` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `overview` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `release_date` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `rating` | `REAL` | — | 否 | 无 | 见本表生产定义。 |
| `runtime_minutes` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `language` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `genres_json` | `TEXT` | — | 是 | `'[]'` | 见本表生产定义。 |
| `countries_json` | `TEXT` | — | 是 | `'[]'` | 见本表生产定义。 |
| `poster_url` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `backdrop_url` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `details_json` | `TEXT` | — | 是 | `'{}'` | 见本表生产定义。 |
| `field_origins_json` | `TEXT` | — | 是 | `'{}'` | 见本表生产定义。 |
| `revision` | `INTEGER` | — | 是 | 无 | 该 Media 该语言的元数据修订号。 |

**生产表定义**

```sql
CREATE TABLE media_metadata (
 media_id TEXT NOT NULL REFERENCES media(media_id), locale TEXT NOT NULL,
 title TEXT NOT NULL, original_title TEXT, year INTEGER, overview TEXT, release_date TEXT,
 rating REAL CHECK(rating IS NULL OR rating BETWEEN 0 AND 10), runtime_minutes INTEGER CHECK(runtime_minutes IS NULL OR runtime_minutes>=0),
 language TEXT, genres_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(genres_json)),
 countries_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(countries_json)),
 poster_url TEXT, backdrop_url TEXT, details_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(details_json)),
 field_origins_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(field_origins_json)),
 revision INTEGER NOT NULL CHECK(revision>=0), PRIMARY KEY(media_id,locale)
);
```

## media_metadata_overrides

实体/locale 人工字段覆盖；写入后同事务重合成/归约，Media merge 预检已有人工锁冲突拒绝。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `media_id` | `TEXT` | 1 | 是 | 无 | Media 身份。 |
| `entity_scope` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |
| `season` | `INTEGER` | 3 | 是 | 无 | 见本表生产定义。 |
| `episode` | `INTEGER` | 4 | 是 | 无 | 见本表生产定义。 |
| `locale` | `TEXT` | 5 | 是 | 无 | 见本表生产定义。 |
| `fields_json` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `revision` | `INTEGER` | — | 是 | 无 | 该 Media 该语言的用户覆盖修订号。 |

**生产表定义**

```sql
CREATE TABLE media_metadata_overrides (
 media_id TEXT NOT NULL REFERENCES media(media_id), entity_scope TEXT NOT NULL, season INTEGER NOT NULL,
 episode INTEGER NOT NULL, locale TEXT NOT NULL, fields_json TEXT NOT NULL CHECK(json_valid(fields_json)), revision INTEGER NOT NULL,
 PRIMARY KEY(media_id,entity_scope,season,episode,locale),
 CHECK((entity_scope='work' AND season=-1 AND episode=-1) OR (entity_scope='season' AND season>=0 AND episode=-1) OR (entity_scope='episode' AND season>=0 AND episode>=0))
);
```

## media_metadata_sources

每实体/locale/来源的当前规范化快照与选择来源；不是响应缓存或历史表。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `media_id` | `TEXT` | 1 | 是 | 无 | Media 身份。 |
| `entity_scope` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |
| `season` | `INTEGER` | 3 | 是 | `-1` | 见本表生产定义。 |
| `episode` | `INTEGER` | 4 | 是 | `-1` | 见本表生产定义。 |
| `source` | `TEXT` | 5 | 是 | 无 | 见本表生产定义。 |
| `locale` | `TEXT` | 6 | 是 | 无 | 见本表生产定义。 |
| `provider_entity_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `provider_work_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `normalized_json` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `present_fields_json` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `selection_origins_json` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `fetched_at` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `source_revision` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE media_metadata_sources (
 media_id TEXT NOT NULL REFERENCES media(media_id), entity_scope TEXT NOT NULL,
 season INTEGER NOT NULL DEFAULT -1, episode INTEGER NOT NULL DEFAULT -1,
 source TEXT NOT NULL CHECK(source IN ('tmdb','douban','tvdb','bangumi','anilist','fanart')),
 locale TEXT NOT NULL, provider_entity_id TEXT NOT NULL, provider_work_id TEXT NOT NULL,
 normalized_json TEXT NOT NULL CHECK(json_valid(normalized_json)),
 present_fields_json TEXT NOT NULL CHECK(json_valid(present_fields_json)),
 selection_origins_json TEXT NOT NULL CHECK(json_valid(selection_origins_json)),
 fetched_at INTEGER NOT NULL, source_revision INTEGER NOT NULL CHECK(source_revision>=0),
 PRIMARY KEY(media_id,entity_scope,season,episode,source,locale),
 CHECK((entity_scope='work' AND season=-1 AND episode=-1) OR
       (entity_scope='season' AND season>=0 AND episode=-1) OR
       (entity_scope='episode' AND season>=0 AND episode>=0))
);
```

## media_operations

Media 合并/改挂持久计划与阶段。冲突/冻结在 Store 内落库，HTTP（`POST /media/merges`、`GET /media/operations/{id}`）与 job loop 推进已接线；改挂仍非独立操作类型。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `operation_id` | `TEXT` | 1 | 是 | 无 | 所属持久操作标识。 |
| `kind` | `TEXT` | — | 是 | 无 | Media 操作类别：merge 或 reassign。 |
| `state` | `TEXT` | — | 是 | 无 | 持久操作或 fence 状态。 |
| `survivor_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `loser_id` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `plan_json` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `phase_cursor` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `error` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `updated_at_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE media_operations (
 operation_id TEXT PRIMARY KEY NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('merge','reassign')),
 state TEXT NOT NULL CHECK(state IN ('planned','frozen','draining','applying','checking','succeeded','failed')), survivor_id TEXT NOT NULL, loser_id TEXT, plan_json TEXT NOT NULL CHECK(json_valid(plan_json)),
 phase_cursor TEXT, error TEXT, created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL
);
```

**显式命名索引**

```sql
CREATE INDEX media_operations_terminal_age ON media_operations(updated_at_ms,operation_id)
 WHERE state = 'succeeded';
```

## pending_file_deletions

待删除磁盘文件意图、身份检查与重试状态；不是浏览事件流。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `path` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `path_key` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `deletion_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `reason` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `attempts` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `next_retry_at` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `last_error` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `file_id` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `file_size` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `file_inode` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE pending_file_deletions (path TEXT PRIMARY KEY NOT NULL,path_key TEXT NOT NULL,deletion_id TEXT NOT NULL,reason TEXT NOT NULL,attempts INTEGER NOT NULL DEFAULT 0,next_retry_at INTEGER NOT NULL DEFAULT 0,last_error TEXT,file_id TEXT,file_size INTEGER,file_inode INTEGER);
```

**显式命名索引**

```sql
CREATE INDEX pending_deletions_due ON pending_file_deletions(next_retry_at);
CREATE UNIQUE INDEX pending_deletions_path_key ON pending_file_deletions(path_key);
```

## playback_logs

终态会话历史；按稳定会话/generation 去重，不依赖文件仍存在。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 是 | 无 | 记录身份或单例键。 |
| `user_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `media_id` | `TEXT` | — | 是 | 无 | Media 身份。 |
| `season` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `episode` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `client` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `device_name` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `device_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `play_method` | `TEXT` | — | 是 | `'local'` | 见本表生产定义。 |
| `started_at` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `ended_at` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `watched_ms` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `start_position_ms` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `end_position_ms` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `duration_ms` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `completed` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `admin_ended` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `play_session_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `generation` | `INTEGER` | — | 是 | 无 | 本次生命周期的 generation。 |
| `file_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `file_revision` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `source_version` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `timeline_mode` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE playback_logs (
 id TEXT PRIMARY KEY NOT NULL, user_id TEXT NOT NULL, media_id TEXT NOT NULL, season INTEGER, episode INTEGER,
 client TEXT, device_name TEXT, device_id TEXT NOT NULL, play_method TEXT NOT NULL DEFAULT 'local',
 started_at INTEGER NOT NULL, ended_at INTEGER NOT NULL, watched_ms INTEGER NOT NULL DEFAULT 0,
 start_position_ms INTEGER NOT NULL DEFAULT 0, end_position_ms INTEGER NOT NULL DEFAULT 0, duration_ms INTEGER,
 completed INTEGER NOT NULL DEFAULT 0, admin_ended INTEGER NOT NULL DEFAULT 0,
 play_session_id TEXT NOT NULL, generation INTEGER NOT NULL, file_id TEXT NOT NULL, file_revision INTEGER NOT NULL, source_version TEXT NOT NULL, timeline_mode TEXT NOT NULL,
 UNIQUE(user_id,device_id,play_session_id,generation)
);
```

**显式命名索引**

```sql
CREATE INDEX logs_ended ON playback_logs(ended_at,started_at,id);
CREATE INDEX logs_started ON playback_logs(started_at DESC,id DESC);
CREATE INDEX logs_user_started ON playback_logs(user_id,started_at DESC,id DESC);
```

## playback_metrics

Playback QoE 指标；仅按明确保留策略清理，不作为用户播放次数来源。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 是 | 无 | 记录身份或单例键。 |
| `user_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `media_id` | `TEXT` | — | 是 | 无 | Media 身份。 |
| `tier` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `engine` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `ttff_ms` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `rebuffer_ms` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `rebuffer_count` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `seek_count` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `dropped_frames` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `total_frames` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `watched_ms` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `recorded_at` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE playback_metrics (
 id TEXT PRIMARY KEY NOT NULL, user_id TEXT NOT NULL, media_id TEXT NOT NULL, tier INTEGER NOT NULL DEFAULT 0,
 engine TEXT, ttff_ms INTEGER, rebuffer_ms INTEGER NOT NULL DEFAULT 0, rebuffer_count INTEGER NOT NULL DEFAULT 0,
 seek_count INTEGER NOT NULL DEFAULT 0, dropped_frames INTEGER, total_frames INTEGER, watched_ms INTEGER NOT NULL DEFAULT 0, recorded_at INTEGER NOT NULL
);
```

**显式命名索引**

```sql
CREATE INDEX metrics_recorded ON playback_metrics(recorded_at,id);
```

## playback_session_ids

签发会话的逻辑目标、物理源、生命周期与 generation。self Playback 必须提交签发 session/sequence，缺失或伪造请求拒绝，过期绑定不发布；Jellyfin 由协议适配层建立绑定。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `user_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `device_id` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |
| `play_session_id` | `TEXT` | 3 | 是 | 无 | 见本表生产定义。 |
| `item_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `media_id` | `TEXT` | — | 是 | 无 | Media 身份。 |
| `season` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `episode` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `file_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `file_revision` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `source_version` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `library_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `config_revision` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `timeline_mode` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `issued_at` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `generation` | `INTEGER` | — | 是 | 无 | 本次生命周期的 generation。 |
| `state` | `TEXT` | — | 是 | 无 | 持久操作或 fence 状态。 |

**生产表定义**

```sql
CREATE TABLE playback_session_ids (
 user_id TEXT NOT NULL, device_id TEXT NOT NULL, play_session_id TEXT NOT NULL, item_id TEXT NOT NULL,
 media_id TEXT NOT NULL, season INTEGER NOT NULL, episode INTEGER NOT NULL, file_id TEXT NOT NULL, file_revision INTEGER NOT NULL CHECK(file_revision>0),
 source_version TEXT NOT NULL, library_id TEXT NOT NULL, config_revision INTEGER NOT NULL CHECK(config_revision>=0), timeline_mode TEXT NOT NULL CHECK(timeline_mode='full_file'),
 issued_at INTEGER NOT NULL, generation INTEGER NOT NULL CHECK(generation>=0), state TEXT NOT NULL CHECK(state IN ('pending','active','closed')),
 PRIMARY KEY(user_id,device_id,play_session_id)
);
```

**显式命名索引**

```sql
CREATE INDEX playback_session_ids_media ON playback_session_ids(media_id);
CREATE INDEX session_generation ON playback_session_ids(user_id,device_id,generation DESC);
CREATE INDEX session_id_expiry ON playback_session_ids(issued_at);
CREATE INDEX session_ids_live_file ON playback_session_ids(file_id) WHERE state <> 'closed';
```

## playback_sessions

每用户/设备活动会话及源绑定、事件顺序、传输/观看统计。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `user_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `device_id` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |
| `media_id` | `TEXT` | — | 是 | 无 | Media 身份。 |
| `season` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `episode` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `client` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `device_name` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `client_version` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `play_method` | `TEXT` | — | 是 | `'local'` | 见本表生产定义。 |
| `position_ms` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `start_position_ms` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `duration_ms` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `paused` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `watched_ms` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `rate_bps` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `bytes_sent` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `connections` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `admin_ended` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `started_at` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `last_report_at` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `file_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `file_revision` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `source_version` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `library_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `config_revision` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `timeline_mode` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `tracks_version` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `play_session_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `generation` | `INTEGER` | — | 是 | 无 | 本次生命周期的 generation。 |
| `last_report_at_ms` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `last_event_sequence` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE playback_sessions (
 user_id TEXT NOT NULL, device_id TEXT NOT NULL, media_id TEXT NOT NULL, season INTEGER NOT NULL, episode INTEGER NOT NULL,
 client TEXT, device_name TEXT, client_version TEXT, play_method TEXT NOT NULL DEFAULT 'local',
 position_ms INTEGER NOT NULL DEFAULT 0, start_position_ms INTEGER NOT NULL DEFAULT 0, duration_ms INTEGER,
 paused INTEGER NOT NULL DEFAULT 0, watched_ms INTEGER NOT NULL DEFAULT 0, rate_bps INTEGER NOT NULL DEFAULT 0,
 bytes_sent INTEGER NOT NULL DEFAULT 0, connections INTEGER NOT NULL DEFAULT 0, admin_ended INTEGER NOT NULL DEFAULT 0,
 started_at INTEGER NOT NULL, last_report_at INTEGER NOT NULL, file_id TEXT NOT NULL, file_revision INTEGER NOT NULL CHECK(file_revision>0),
 source_version TEXT NOT NULL, library_id TEXT NOT NULL, config_revision INTEGER NOT NULL CHECK(config_revision>=0), timeline_mode TEXT NOT NULL CHECK(timeline_mode='full_file'),
 tracks_version INTEGER, play_session_id TEXT NOT NULL, generation INTEGER NOT NULL CHECK(generation>=0),
 last_report_at_ms INTEGER NOT NULL DEFAULT 0, last_event_sequence INTEGER, PRIMARY KEY(user_id,device_id)
);
```

**显式命名索引**

```sql
CREATE INDEX playback_session_file ON playback_sessions(file_id,user_id,device_id);
CREATE INDEX playback_sessions_media ON playback_sessions(media_id);
```

## playback_units

用户逻辑观看/收藏事实及一套源绑定恢复包；不 FK 到物理文件，删文件保留原始事实。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `user_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `media_id` | `TEXT` | 2 | 是 | 无 | Media 身份。 |
| `season` | `INTEGER` | 3 | 是 | 无 | 见本表生产定义。 |
| `episode` | `INTEGER` | 4 | 是 | 无 | 见本表生产定义。 |
| `played` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `has_played_assertion` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `played_changed_at_ms` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `favorite` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `favorite_at` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `play_count` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `watch_activity_at` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `position_ms` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `duration_ms` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `resume_file_id` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `resume_file_revision` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `resume_source_version` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `timeline_mode` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `tracks_version` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `audio_track` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `subtitle_track` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE playback_units (
 user_id TEXT NOT NULL, media_id TEXT NOT NULL REFERENCES media(media_id), season INTEGER NOT NULL, episode INTEGER NOT NULL,
 played INTEGER NOT NULL DEFAULT 0 CHECK(played IN (0,1)), has_played_assertion INTEGER NOT NULL DEFAULT 0 CHECK(has_played_assertion IN (0,1)),
 played_changed_at_ms INTEGER, favorite INTEGER NOT NULL DEFAULT 0 CHECK(favorite IN (0,1)), favorite_at INTEGER NOT NULL DEFAULT 0,
 play_count INTEGER NOT NULL DEFAULT 0 CHECK(play_count>=0), watch_activity_at INTEGER NOT NULL DEFAULT 0,
 position_ms INTEGER NOT NULL DEFAULT 0 CHECK(position_ms>=0), duration_ms INTEGER CHECK(duration_ms IS NULL OR duration_ms>=0),
 resume_file_id TEXT, resume_file_revision INTEGER, resume_source_version TEXT, timeline_mode TEXT CHECK(timeline_mode IS NULL OR timeline_mode='full_file'),
 tracks_version INTEGER, audio_track TEXT, subtitle_track TEXT,
 PRIMARY KEY(user_id,media_id,season,episode), CHECK((season=-1 AND episode=-1) OR (season>=0 AND episode>=-1)),
 CHECK((has_played_assertion=0 AND played=0 AND played_changed_at_ms IS NULL) OR (has_played_assertion=1 AND played_changed_at_ms IS NOT NULL)),
 CHECK((resume_file_id IS NULL AND resume_file_revision IS NULL AND resume_source_version IS NULL AND timeline_mode IS NULL AND position_ms=0 AND duration_ms IS NULL AND tracks_version IS NULL AND audio_track IS NULL AND subtitle_track IS NULL) OR
       (resume_file_id IS NOT NULL AND resume_file_revision IS NOT NULL AND resume_file_revision>0 AND resume_source_version IS NOT NULL AND timeline_mode IS NOT NULL)),
 CHECK((audio_track IS NULL AND subtitle_track IS NULL) OR tracks_version IS NOT NULL)
);
```

**显式命名索引**

```sql
CREATE INDEX playback_activity ON playback_units(user_id,media_id,watch_activity_at DESC,season,episode);
CREATE INDEX playback_favorite_order ON playback_units(user_id,favorite_at DESC,media_id ASC,season DESC,episode DESC) WHERE favorite=1;
CREATE INDEX playback_favorite_winner ON playback_units(user_id,media_id,favorite_at DESC,season DESC,episode DESC) WHERE favorite=1;
CREATE INDEX playback_played_episodes ON playback_units(user_id,media_id,season,episode) WHERE played<>0 AND season>=0 AND episode>=0;
CREATE INDEX playback_unit_media ON playback_units(media_id,user_id,season,episode);
```

## probe_job_units

Probe 文件单元、阶段与输出；活动单元/派发索引控制工作窗口。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `job_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `file_id` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |
| `kind` | `TEXT` | — | 是 | 无 | 该文件单元的探测类别。 |
| `force_fingerprint` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `reuse_fingerprint_cache` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `overwrite_markers` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `status` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `error` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `reuse_media_info_cache` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `stage` | `TEXT` | — | 是 | `'metadata'` | 见本表生产定义。 |
| `sampling_plan_json` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `detection_outcome_json` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `analysis_context_json` | `TEXT` | — | 否 | 无 | 固定政策、源、覆盖及语义证据身份。 |
| `analysis_claim_token` | `TEXT` | — | 否 | 无 | 当前 Analysis 领取令牌。 |
| `analysis_refresh_json` | `TEXT` | — | 否 | 无 | wanted-reference 与未完 keyset 游标。 |

**生产表定义**

```sql
CREATE TABLE probe_job_units (
 job_id TEXT NOT NULL REFERENCES probe_jobs(id) ON DELETE CASCADE,file_id TEXT NOT NULL,kind TEXT NOT NULL,
 force_fingerprint INTEGER NOT NULL DEFAULT 0,reuse_fingerprint_cache INTEGER NOT NULL DEFAULT 0,overwrite_markers INTEGER NOT NULL DEFAULT 0,
 status TEXT NOT NULL,error TEXT,reuse_media_info_cache INTEGER NOT NULL DEFAULT 0,stage TEXT NOT NULL DEFAULT 'metadata',
 sampling_plan_json TEXT,detection_outcome_json TEXT,
 analysis_context_json TEXT CHECK(analysis_context_json IS NULL OR json_valid(analysis_context_json)),
 analysis_claim_token TEXT,analysis_refresh_json TEXT,
 PRIMARY KEY(job_id,file_id)
);
```

**显式命名索引**

```sql
CREATE UNIQUE INDEX probe_unit_active ON probe_job_units(file_id) WHERE status IN ('queued','running');
CREATE INDEX probe_unit_dispatch ON probe_job_units(stage,status,job_id) WHERE status='queued';
CREATE INDEX probe_unit_file ON probe_job_units(file_id,job_id);
CREATE INDEX probe_unit_status ON probe_job_units(job_id,status);
CREATE INDEX probe_analysis_season ON probe_job_units(
 json_extract(analysis_context_json,'$.media_id'),json_extract(analysis_context_json,'$.season'),job_id,file_id
) WHERE stage='analysis' AND status='queued';
```

## probe_jobs

专用 Probe 作业与单元计数、阶段策略和历史；不同于 jobs.db 的通用 Job。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 是 | 无 | 记录身份或单例键。 |
| `kind` | `TEXT` | — | 是 | 无 | 探测任务类别。 |
| `media_id` | `TEXT` | — | 是 | 无 | Media 身份。 |
| `season` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `scope_key` | `TEXT` | — | 是 | 无 | 探测任务作用域键；与任务类别共同隔离同一 Media 的不同探测范围。 |
| `status` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `total` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `completed` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `succeeded` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `failed` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `partial` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `error` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `started_at_ms` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `finished_at_ms` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `analysis_policy_key` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `sampling_mode` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE probe_jobs (
 id TEXT PRIMARY KEY NOT NULL,kind TEXT NOT NULL,media_id TEXT NOT NULL,season INTEGER,scope_key TEXT NOT NULL,status TEXT NOT NULL,
 total INTEGER NOT NULL,completed INTEGER NOT NULL DEFAULT 0,succeeded INTEGER NOT NULL DEFAULT 0,failed INTEGER NOT NULL DEFAULT 0,partial INTEGER NOT NULL DEFAULT 0,error TEXT,
 created_at_ms INTEGER NOT NULL,started_at_ms INTEGER,finished_at_ms INTEGER,analysis_policy_key TEXT,sampling_mode TEXT
);
```

**显式命名索引**

```sql
CREATE UNIQUE INDEX probe_active_scope ON probe_jobs(scope_key) WHERE status IN ('queued','running');
CREATE INDEX probe_media_created ON probe_jobs(media_id,created_at_ms DESC);
CREATE INDEX probe_scope_terminal ON probe_jobs(scope_key,finished_at_ms DESC,id DESC) WHERE status IN ('succeeded','failed','cancelled');
CREATE INDEX probe_terminal ON probe_jobs(finished_at_ms,id) WHERE status IN ('succeeded','failed','cancelled');
```

## probe_season_comparison_history

每 Media/季最近比较摘要与发布时间，不是完整历史集合。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `media_id` | `TEXT` | 1 | 是 | 无 | Media 身份。 |
| `season` | `INTEGER` | 2 | 是 | 无 | 见本表生产定义。 |
| `comparison_digest` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `published_at_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE probe_season_comparison_history (media_id TEXT NOT NULL,season INTEGER NOT NULL,comparison_digest TEXT NOT NULL,published_at_ms INTEGER NOT NULL,PRIMARY KEY(media_id,season));
```

## probe_stage_state

每文件/上下文/阶段的退避、错误与活动作业状态。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `file_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `context_key` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |
| `stage` | `TEXT` | 3 | 是 | 无 | 见本表生产定义。 |
| `status` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `failure_count` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `next_retry_at_ms` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `error_kind` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `error` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `active_job_id` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `dispatched_until_ms` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `updated_at_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE probe_stage_state (file_id TEXT NOT NULL,context_key TEXT NOT NULL,stage TEXT NOT NULL,status TEXT NOT NULL,failure_count INTEGER NOT NULL DEFAULT 0,next_retry_at_ms INTEGER,error_kind TEXT,error TEXT,active_job_id TEXT,dispatched_until_ms INTEGER,updated_at_ms INTEGER NOT NULL,PRIMARY KEY(file_id,context_key,stage));
```

**显式命名索引**

```sql
CREATE INDEX probe_retry ON probe_stage_state(next_retry_at_ms,file_id,context_key,stage) WHERE status='failed' AND next_retry_at_ms IS NOT NULL AND active_job_id IS NULL;
CREATE INDEX probe_stage_active_job ON probe_stage_state(active_job_id) WHERE active_job_id IS NOT NULL;
```

## scan_operations

扫描作用域、generation、全量/定向意图与恢复报告。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `id` | `TEXT` | 1 | 是 | 无 | 记录身份或单例键。 |
| `library_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `scope_signature` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `status` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `generation` | `INTEGER` | — | 是 | 无 | 本次生命周期的 generation。 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `report_json` | `TEXT` | — | 是 | `'{}'` | 见本表生产定义。 |
| `targets_json` | `TEXT` | — | 是 | `'[]'` | 见本表生产定义。 |
| `scope_kind` | `TEXT` | — | 是 | `'full'` | 见本表生产定义。 |
| `dirty_rev` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `consumed_rev` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE scan_operations (
 id TEXT PRIMARY KEY NOT NULL,library_id TEXT NOT NULL,scope_signature TEXT NOT NULL,status TEXT NOT NULL,generation INTEGER NOT NULL,
 created_at_ms INTEGER NOT NULL,report_json TEXT NOT NULL DEFAULT '{}',targets_json TEXT NOT NULL DEFAULT '[]',
 scope_kind TEXT NOT NULL DEFAULT 'full' CHECK(scope_kind IN ('full','targeted')),dirty_rev INTEGER NOT NULL DEFAULT 0,consumed_rev INTEGER NOT NULL DEFAULT 0
);
```

**显式命名索引**

```sql
CREATE INDEX scan_library_status ON scan_operations(library_id,status);
CREATE INDEX scan_operations_terminal_age ON scan_operations(created_at_ms,id)
 WHERE status IN ('completed','partial','cancelled','failed');
```

## scan_seen

当前扫描 operation 已确认处理的路径版本；与源观察、物理 FileVersion 不是同一语义。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `operation_id` | `TEXT` | 1 | 是 | 无 | 所属持久操作标识。 |
| `path` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `path_key` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |
| `mtime_ms` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `file_size` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `content_digest` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE scan_seen (operation_id TEXT NOT NULL,path TEXT NOT NULL,path_key TEXT NOT NULL,mtime_ms INTEGER NOT NULL DEFAULT 0,file_size INTEGER NOT NULL DEFAULT 0,content_digest TEXT,PRIMARY KEY(operation_id,path_key));
```

## scrape_sidecars

文件的 NFO/图片 sidecar 路径；登记不等于导出 provenance 合同已完成。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `file_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `kind` | `TEXT` | 2 | 是 | 无 | 刮削产物的 sidecar 类型。 |
| `path` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE scrape_sidecars (file_id TEXT NOT NULL REFERENCES library_files(file_id) ON DELETE CASCADE,kind TEXT NOT NULL,path TEXT NOT NULL,PRIMARY KEY(file_id,kind));
```

## unidentified

登记的待认领来源身份。unidentified_id 是每次新停放生成的唯一 ID，删除再停放不复用；root 移动保留身份，合并保留目标身份并推进 revision。path 是显示路径，path_key 是持久规范键；列表按 library_id SQL 过滤。Claim 固定原 library/root/revision/config，在同一个文件发布事务中 CAS 删除来源；明确的 IdentifyAndTransfer 可以将 Movie 停放来源转入 TV。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `unidentified_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `library_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `root_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `path` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `path_key` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |
| `confidence` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `revision` | `INTEGER` | — | 是 | `1` | 待认领来源修订号，Claim 与移动/合并使用它执行 CAS。 |

**生产表定义**

```sql
CREATE TABLE unidentified (unidentified_id TEXT NOT NULL,library_id TEXT NOT NULL,root_id TEXT NOT NULL,path TEXT NOT NULL,path_key TEXT NOT NULL,confidence TEXT NOT NULL,revision INTEGER NOT NULL DEFAULT 1,PRIMARY KEY(library_id,path_key),FOREIGN KEY(root_id,library_id) REFERENCES library_roots(root_id,library_id));
```

**显式命名索引**

```sql
CREATE UNIQUE INDEX unidentified_identity ON unidentified(unidentified_id);
CREATE INDEX unidentified_path_seek ON unidentified(path_key,unidentified_id);
CREATE INDEX unidentified_root ON unidentified(root_id,library_id);
```

## user_library_items

用户/Library/Media 稀疏摘要；卡片删除会级联清理，重导入需要从原始用户事实重算。写事务原子维护，重发布与重新归属使用当前事实重算。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `user_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `library_id` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |
| `media_id` | `TEXT` | 3 | 是 | 无 | Media 身份。 |
| `seen` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `played` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `favorite` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `last_played_at` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `favorite_at` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `owned_episode_count` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `completed_episode_count` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `next_season` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `next_episode` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `resume_season` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `resume_episode` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `summary_revision` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `activity_next_season` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `activity_next_episode` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `activity_season` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `activity_episode` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `activity_at` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `activity_present` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `all_watch_at` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `resume_activity_at` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE user_library_items (
 user_id TEXT NOT NULL, library_id TEXT NOT NULL, media_id TEXT NOT NULL,
 seen INTEGER NOT NULL, played INTEGER NOT NULL, favorite INTEGER NOT NULL, last_played_at INTEGER NOT NULL DEFAULT 0,
 favorite_at INTEGER NOT NULL DEFAULT 0, owned_episode_count INTEGER NOT NULL DEFAULT 0, completed_episode_count INTEGER NOT NULL DEFAULT 0,
 next_season INTEGER, next_episode INTEGER, resume_season INTEGER, resume_episode INTEGER, summary_revision INTEGER NOT NULL,
 activity_next_season INTEGER, activity_next_episode INTEGER, activity_season INTEGER, activity_episode INTEGER,
 activity_at INTEGER NOT NULL DEFAULT 0, activity_present INTEGER NOT NULL DEFAULT 0 CHECK(activity_present IN(0,1)),
 all_watch_at INTEGER NOT NULL DEFAULT 0, resume_activity_at INTEGER NOT NULL DEFAULT 0,
 PRIMARY KEY(user_id,library_id,media_id), FOREIGN KEY(library_id,media_id) REFERENCES library_items(library_id,media_id) ON DELETE CASCADE,
 CHECK(completed_episode_count BETWEEN 0 AND owned_episode_count)
) WITHOUT ROWID;
```

**显式命名索引**

```sql
CREATE INDEX user_items_activity ON user_library_items(user_id,library_id,last_played_at DESC,media_id DESC);
CREATE INDEX user_items_home ON user_library_items(user_id,activity_at DESC,media_id,library_id)
 WHERE activity_present=1 AND (activity_next_season IS NOT NULL OR resume_season IS NOT NULL);
CREATE INDEX user_items_media ON user_library_items(library_id,media_id,user_id);
CREATE INDEX user_items_next ON user_library_items(user_id,activity_at DESC,media_id,library_id) WHERE next_season IS NOT NULL;
CREATE INDEX user_items_resume ON user_library_items(user_id,resume_activity_at DESC,media_id,library_id) WHERE resume_season IS NOT NULL;
```

## pending_file_renames

文件改名/改挂持久意图：版本与磁盘证据、sidecar决议、跨分区引用恢复和重试。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `rename_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `file_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `from_path` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `to_path` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `expected_revision` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `expected_source_version` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `from_library_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `from_root_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `to_library_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `to_root_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `sidecars_json` | `TEXT` | — | 是 | `'[]'` | 见本表生产定义。 |
| `phase` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `last_completed_phase` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `source_size` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `source_device` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `source_inode` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `source_mtime_ns` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `dest_size` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `dest_device` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `dest_inode` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `dest_mtime_ns` | `INTEGER` | — | 否 | 无 | 见本表生产定义。 |
| `from_config_revision` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `to_config_revision` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `attempts` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `next_retry_at` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |
| `last_error` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `updated_at_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE pending_file_renames (
 rename_id TEXT PRIMARY KEY NOT NULL,
 file_id TEXT NOT NULL,
 from_path TEXT NOT NULL,
 to_path TEXT NOT NULL,
 expected_revision INTEGER NOT NULL,
 expected_source_version TEXT NOT NULL,
 from_library_id TEXT NOT NULL,
 from_root_id TEXT NOT NULL,
 to_library_id TEXT NOT NULL,
 to_root_id TEXT NOT NULL,
 sidecars_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(sidecars_json)),
 phase TEXT NOT NULL CHECK(phase IN ('planned','disk_applied','library_published','references_applied','failed')),
 last_completed_phase TEXT CHECK(last_completed_phase IN ('planned','disk_applied','library_published','references_applied')),
 source_size INTEGER NOT NULL,
 source_device INTEGER NOT NULL,
 source_inode INTEGER NOT NULL,
 source_mtime_ns INTEGER NOT NULL,
 dest_size INTEGER,
 dest_device INTEGER,
 dest_inode INTEGER,
 dest_mtime_ns INTEGER,
 from_config_revision INTEGER NOT NULL,
 to_config_revision INTEGER NOT NULL,
 attempts INTEGER NOT NULL DEFAULT 0,
 next_retry_at INTEGER NOT NULL DEFAULT 0,
 last_error TEXT,
 created_at_ms INTEGER NOT NULL,
 updated_at_ms INTEGER NOT NULL,
 CHECK((dest_device IS NULL) = (dest_inode IS NULL))
);
```

**显式命名索引**

```sql
CREATE INDEX pending_file_renames_due ON pending_file_renames(next_retry_at, rename_id);
CREATE INDEX pending_file_renames_file ON pending_file_renames(file_id);
```

## pending_root_reassignments

嵌套根迁移的持久待处理队列。文件与 Unidentified 各有游标；Unidentified 用不可变的 `(path_key, unidentified_id)` 排序，归属变更不会倒退次序。dispatch_order 公平轮转冻结根。Unidentified 查询下界是 max(root 范围下界, 已提交游标)，先选最多 100 行再求目标根；游标前出现错归属来源时重置游标；正确新记录不回退已完成窗口。完成时删除队列行，状态留在 root_reassignment_state。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `kind` | `TEXT` | 1 | 是 | 无 | 根目录重新归属任务类别。 |
| `root_key` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |
| `cursor_key` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `unidentified_cursor_key` | `TEXT` | — | 是 | `''` | 见本表生产定义。 |
| `unidentified_cursor_id` | `TEXT` | — | 是 | `''` | 见本表生产定义。 |
| `dispatch_order` | `INTEGER` | — | 是 | `0` | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE pending_root_reassignments (kind TEXT NOT NULL, root_key TEXT NOT NULL, cursor_key TEXT NOT NULL, unidentified_cursor_key TEXT NOT NULL DEFAULT '', unidentified_cursor_id TEXT NOT NULL DEFAULT '', dispatch_order INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(kind, root_key));
```

**显式命名索引**

```sql
CREATE INDEX root_reassignment_dispatch ON pending_root_reassignments(dispatch_order,kind,root_key);
```

## library_removal_files

冻结后的持久文件快照。每行固定 root/path/Media/file revision/source version 与覆盖；清理前逐项校验，完成窗口后删除快照行。工作量不依赖无关 Library 的文件或磁盘路径。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `operation_id` | `TEXT` | 1 | 是 | 无 | 所属持久操作标识。 |
| `file_id` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |
| `root_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `path_key` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `display_path` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `media_id` | `TEXT` | — | 是 | 无 | Media 身份。 |
| `file_revision` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `source_version` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `units_json` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE library_removal_files (
 operation_id TEXT NOT NULL REFERENCES library_removals(operation_id), file_id TEXT NOT NULL,
 root_id TEXT NOT NULL, path_key TEXT NOT NULL, display_path TEXT NOT NULL, media_id TEXT NOT NULL,
 file_revision INTEGER NOT NULL, source_version TEXT NOT NULL, units_json TEXT NOT NULL CHECK(json_valid(units_json)),
 PRIMARY KEY(operation_id,file_id)
);
```

## root_reassignment_state

按 kind/root_key 保留最近一次根归属工作的稳定 operation_id、目标 Library/config revision、状态、单调 revision 和错误；Library DTO 的 root_reassignments 可查询。周期 driver 使用通知和定期兜底，每窗口释放 Store、yield；完成后状态保留 succeeded。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `kind` | `TEXT` | 1 | 是 | 无 | 根目录重新归属任务类别。 |
| `root_key` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |
| `operation_id` | `TEXT` | — | 是 | 无 | 所属持久操作标识。 |
| `library_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `config_revision` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `state` | `TEXT` | — | 是 | 无 | 持久操作或 fence 状态。 |
| `revision` | `INTEGER` | — | 是 | 无 | 根目录重新归属状态修订号。 |
| `error` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |
| `updated_at_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE root_reassignment_state (
 kind TEXT NOT NULL, root_key TEXT NOT NULL, operation_id TEXT NOT NULL UNIQUE,
 library_id TEXT NOT NULL REFERENCES libraries(library_id) ON DELETE CASCADE, config_revision INTEGER NOT NULL, state TEXT NOT NULL,
 revision INTEGER NOT NULL, error TEXT, updated_at_ms INTEGER NOT NULL,
 PRIMARY KEY(kind,root_key)
);
```

**显式命名索引**

```sql
CREATE INDEX root_reassignment_library ON root_reassignment_state(library_id,root_key);
```

## library_protocol_entities

按 Library/Media 保存 genre/studio 协议实体投影。`name_key` 用于排序，`wire_id` 用于协议身份匹配，`poster_file_id` 保存页内选图候选；随 library_items 级联删除。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `library_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `media_id` | `TEXT` | 2 | 是 | 无 | Media 身份。 |
| `entity_kind` | `TEXT` | 3 | 是 | 无 | 见本表生产定义。 |
| `name` | `TEXT` | 4 | 是 | 无 | 见本表生产定义。 |
| `name_key` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `wire_id` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |
| `poster_file_id` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE library_protocol_entities (
 library_id TEXT NOT NULL,media_id TEXT NOT NULL,entity_kind TEXT NOT NULL CHECK(entity_kind IN ('genre','studio')),
 name TEXT NOT NULL,name_key TEXT NOT NULL,wire_id TEXT NOT NULL,poster_file_id TEXT,
 PRIMARY KEY(library_id,media_id,entity_kind,name),
 FOREIGN KEY(library_id,media_id) REFERENCES library_items(library_id,media_id) ON DELETE CASCADE
) WITHOUT ROWID;
```

**显式命名索引**

```sql
CREATE INDEX protocol_entities_media ON library_protocol_entities(media_id,entity_kind,name,library_id);
CREATE INDEX protocol_entities_name ON library_protocol_entities(entity_kind,name_key,poster_file_id DESC,library_id,media_id,name);
CREATE INDEX protocol_entities_wire ON library_protocol_entities(entity_kind,wire_id,poster_file_id DESC,library_id,media_id,name);
```

## library_search_fts

Library 搜索的 contentless FTS5 虚拟表，`search_text` 与 `people` 为两个无显式类型的搜索字段；采用 trigram tokenizer、`content=''`、`contentless_delete=1`。SQLite 自动维护 `_data`、`_idx`、`_docsize`、`_config` 四张影子表，不作为业务表或独立命名索引统计；隐藏列不计入字段总数。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `search_text` | 无 | — | 否 | 无 | 见本表生产定义。 |
| `people` | 无 | — | 否 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE VIRTUAL TABLE library_search_fts USING fts5(search_text,people,content='',contentless_delete=1,tokenize='trigram');
```

## protocol_entity_overlap_counts

按实体种类、名称和所属 Library 集合分组的去重成员计数。`owner_libraries_json` 是归约后的 Library 集合，查询据此避免跨库重叠重复计数；member_count 必须为正。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `entity_kind` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `name` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |
| `owner_libraries_json` | `TEXT` | 3 | 是 | 无 | 见本表生产定义。 |
| `member_count` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE protocol_entity_overlap_counts (
 entity_kind TEXT NOT NULL,name TEXT NOT NULL,owner_libraries_json TEXT NOT NULL CHECK(json_valid(owner_libraries_json)),
 member_count INTEGER NOT NULL CHECK(member_count>0),PRIMARY KEY(entity_kind,name,owner_libraries_json)
) WITHOUT ROWID;
```

## protocol_entity_overlap_rebuild

协议实体重叠计数的重建暂存事实；在相同分组键前增加 operation_id 隔离重建操作，完成后用于替换正式计数。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `operation_id` | `TEXT` | 1 | 是 | 无 | 所属持久操作标识。 |
| `entity_kind` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |
| `name` | `TEXT` | 3 | 是 | 无 | 见本表生产定义。 |
| `owner_libraries_json` | `TEXT` | 4 | 是 | 无 | 见本表生产定义。 |
| `member_count` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE protocol_entity_overlap_rebuild (
 operation_id TEXT NOT NULL,entity_kind TEXT NOT NULL,name TEXT NOT NULL,
 owner_libraries_json TEXT NOT NULL CHECK(json_valid(owner_libraries_json)),member_count INTEGER NOT NULL CHECK(member_count>0),
 PRIMARY KEY(operation_id,entity_kind,name,owner_libraries_json)
) WITHOUT ROWID;
```

## user_library_continue_units

每用户/Library/Media/逻辑单元的继续观看投影。`position_ms` 是恢复位置，`watch_activity_at` 是观看活动排序时钟；通过双重外键随用户卡片或 Library 单元级联清理，resume 索引只包含 position_ms>0 的行。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `user_id` | `TEXT` | 1 | 是 | 无 | 见本表生产定义。 |
| `library_id` | `TEXT` | 2 | 是 | 无 | 见本表生产定义。 |
| `media_id` | `TEXT` | 3 | 是 | 无 | Media 身份。 |
| `season` | `INTEGER` | 4 | 是 | 无 | 见本表生产定义。 |
| `episode` | `INTEGER` | 5 | 是 | 无 | 见本表生产定义。 |
| `position_ms` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |
| `watch_activity_at` | `INTEGER` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE user_library_continue_units (
 user_id TEXT NOT NULL, library_id TEXT NOT NULL, media_id TEXT NOT NULL, season INTEGER NOT NULL, episode INTEGER NOT NULL,
 position_ms INTEGER NOT NULL CHECK(position_ms>=0), watch_activity_at INTEGER NOT NULL,
 PRIMARY KEY(user_id,library_id,media_id,season,episode),
 FOREIGN KEY(user_id,library_id,media_id) REFERENCES user_library_items(user_id,library_id,media_id) ON DELETE CASCADE,
 FOREIGN KEY(library_id,media_id,season,episode) REFERENCES library_units(library_id,media_id,season,episode) ON DELETE CASCADE
) WITHOUT ROWID;
```

**显式命名索引**

```sql
CREATE INDEX user_continue_integrity ON user_library_continue_units(library_id,media_id,user_id,season,episode);
CREATE INDEX user_continue_media ON user_library_continue_units(media_id,library_id,season,episode);
CREATE INDEX user_continue_owner_resume ON user_library_continue_units(user_id,library_id,media_id,watch_activity_at DESC,season DESC,episode DESC) WHERE position_ms>0;
CREATE INDEX user_continue_owner_season_resume ON user_library_continue_units(user_id,library_id,media_id,season,watch_activity_at DESC,episode DESC) WHERE position_ms>0;
CREATE INDEX user_continue_resume ON user_library_continue_units(user_id,watch_activity_at DESC,media_id,library_id,season,episode) WHERE position_ms>0;
CREATE INDEX user_continue_scope ON user_library_continue_units(user_id,media_id,season,episode,library_id);
```

## transfer_settings

Transfer/Watch 配置的唯一权威；配置保存与发布共享 Library writer。PATCH 在单事务内比较 expected_revision，只更新目标字段并递增 DTO 内的 revision。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `singleton` | `INTEGER` | 1 | 否 | 无 | 见本表生产定义。 |
| `value` | `TEXT` | — | 是 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE transfer_settings (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1), value TEXT NOT NULL CHECK(json_valid(value))
);
```

`singleton=1`，`value` 必须是合法 JSON；业务解码使用严格 `TransferSettings` DTO。尚未保存时读取 revision=0 的默认配置。没有额外命名索引或 FK。

## scrape_settings

原始 Scrape 配置单例。API 负责配置合成；Naming template 保存在四个分段字段，不再保存另一份组合模板。


| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `singleton` | `INTEGER` | 1 | 否 | 无 | 见本表生产定义。 |
| `revision` | `INTEGER` | — | 是 | 无 | 本地单调配置修订号；每次保存递增，用于拒绝旧发布计划。 |
| `value` | `TEXT` | — | 否 | 无 | 见本表生产定义。 |

**生产表定义**

```sql
CREATE TABLE scrape_settings (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1), revision INTEGER NOT NULL CHECK(revision>=0), value TEXT
);
```

`singleton=1` 且 `revision>=0`；初始行为 `(1,0,NULL)`。每次保存递增 revision，配置内容改回原值也不会恢复旧 revision，避免旧 Transfer plan 通过 ABA 校验。发布在同一 Library 事务内比较 revision 与配置内容，变化则拒绝旧 plan。没有额外命名索引或 FK。

## pending_orphan_media

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `media_id` | `TEXT` | 1 | 否 | 无 | Media 身份。 |

**生产表定义**

```sql
CREATE TABLE pending_orphan_media(media_id TEXT PRIMARY KEY REFERENCES media(media_id) ON DELETE CASCADE);
```

## reference_recovery_cursors

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `kind` | `TEXT` | 1 | 否 | 无 | 引用恢复流程类别，用于隔离各流程的分页游标。 |
| `cursor` | `TEXT` | — | 是 | 无 | 恢复游标。 |

**生产表定义**

```sql
CREATE TABLE reference_recovery_cursors(kind TEXT PRIMARY KEY,cursor TEXT NOT NULL);
```

## subscribe_compensation_receipts

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `generation` | `TEXT` | 1 | 是 | 无 | 本次生命周期的 generation。 |
| `protected` | `INTEGER` | — | 是 | 无 | 补偿是否保护现有 Media。 |

**生产表定义**

```sql
CREATE TABLE subscribe_compensation_receipts(generation TEXT PRIMARY KEY NOT NULL,protected INTEGER NOT NULL CHECK(protected IN (0,1)));
```

## subscribe_retention

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `subscribe_id` | `TEXT` | 1 | 是 | 无 | Subscribe 身份。 |
| `generation` | `TEXT` | 2 | 是 | 无 | 本次生命周期的 generation。 |
| `media_id` | `TEXT` | 3 | 是 | 无 | Media 身份。 |

**生产表定义**

```sql
CREATE TABLE subscribe_retention(subscribe_id TEXT NOT NULL,generation TEXT NOT NULL,media_id TEXT NOT NULL REFERENCES media(media_id),PRIMARY KEY(subscribe_id,generation,media_id));
```

**显式命名索引**

```sql
CREATE INDEX subscribe_retention_media ON subscribe_retention(media_id,subscribe_id,generation);
```

## subscribe_retention_fences

| 字段 | SQLite 类型 | 主键顺位 | 显式 NOT NULL | DEFAULT | 含义 |
|---|---|---:|---|---|---|
| `subscribe_id` | `TEXT` | 1 | 是 | 无 | Subscribe 身份。 |
| `generation` | `TEXT` | 2 | 是 | 无 | 本次生命周期的 generation。 |
| `state` | `TEXT` | — | 是 | 无 | 持久操作或 fence 状态。 |

**生产表定义**

```sql
CREATE TABLE subscribe_retention_fences(subscribe_id TEXT NOT NULL,generation TEXT NOT NULL,state TEXT NOT NULL CHECK(state IN ('open','closed')),PRIMARY KEY(subscribe_id,generation));
```
