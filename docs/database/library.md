# library.db：Media、Library、Playback 与持久文件工作

[返回数据库总览](README.md)

基线：`feat/storage-redesign@a3dab1b`。`application_id=1129139249`，`user_version=1`。不可按缓存清空。

本字典将 [生产 DDL](../../crates/store/src/schema/v1/library.sql) 在临时 SQLite 内存连接执行后读取结构：**45 张业务表、519 字段、53 个显式命名索引、32 组 FK、0 个触发器**。不计 SQLite 自动索引。此方法核对结构，不声称运行了完整 Store/API 或检查了用户实例。

## 实现边界

- `media`、Library 配置、Collection、Playback 已放同库；不存在旧 `ledger/file_meta/browse_*` 持久表。旧 `LedgerRow` 写适配和电影 `(0,0)` 迁移仍是待移除代码，不是支持的布局。
- 文件事实的复合 FK 与局部公共摘要归约已存在；文件变化后的用户摘要、源版本更新、Transfer 多集收尾仍有不变量缺口。
- `library_operations`、`integrity_state`、NFO `export_*` 是已建结构，不代表完整根/locale/checker/导出恢复实现。
- `a3dab1b` 增加扫描变化时清 stat/durable 版本，未新增表或列；不等于物理 FileVersion、统计摘要及 Probe 版本合同已统一。
- 元数据人工锁/来源冲突、合并冻结/恢复、协议生产接线的修复合同见 [实施计划](../superpowers/plans/2026-10-19-storage-cutover-remediation.md)。

## 字段阅读规则

类型、主键顺位、NOT NULL 与 DEFAULT 均为 SQLite 声明；正文约束使用原 DDL 表达，包含 DEFERRABLE 细节。无 FK 时明确列出，不因字段名带 `_id` 推断约束。JSON 是各消费者的版本化结构，json_valid 只验证 JSON 语法，不保证业务字段完整。

用户/设置在 app、Subscribe 在 subscribe、通用 Job 在 jobs，跨库 ID 无 FK。playback 恢复包不引用 library_files 的 FK，以保留删除后的观看事实。秒/ms/ns 混用，以实际读写入口为准；`played_changed_at_ms` 与 watch/favorite 时钟不能互换。

## 表目录

- [chapter_frame_tasks](#chapter_frame_tasks) — 10 字段；章节帧生成的源版本、目标与持久执行状态。
- [collection_items](#collection_items) — 4 字段；以 canonical MediaId 保存成员关系与排序，不随单库文件删除而删除 Media 事实。
- [collections](#collections) — 6 字段；公共系列或用户私有 Collection 定义；可见性与 owner 组合有 CHECK。
- [file_details](#file_details) — 11 字段；同源版本的冷探测详情；随文件级联删除，不等于可随意清空的浏览缓存。
- [file_probe_obligations](#file_probe_obligations) — 3 字段；随文件登记的持久探测义务；文件删除级联移除。
- [fingerprint_attempts](#fingerprint_attempts) — 12 字段；采样尝试、阶段、错误与工作指标；按作业/文件查询。
- [fingerprint_cache](#fingerprint_cache) — 8 字段；每物理文件当前指纹缓存及算法/时长参数。
- [fingerprint_model_members](#fingerprint_model_members) — 4 字段；模型到样本/物理文件源版本的成员；仅 model_id 声明 FK。
- [fingerprint_samples](#fingerprint_samples) — 12 字段；按文件源版本、profile、kind、窗口去重的采样事实。
- [fingerprint_season_models](#fingerprint_season_models) — 9 字段；季级指纹模型、成员/政策版本与模型体。
- [libraries](#libraries) — 23 字段；Library 配置、显示语言、访问范围、生命周期与完整性状态；后两类 operation 合同不等于已实现。
- [library_file_units](#library_file_units) — 5 字段；文件到逻辑单元的覆盖事实；一文件可覆盖多集，同集可有多版本。
- [library_files](#library_files) — 34 字段；权威物理文件台账；显式 Library/root/Media/kind 与源版本、在位/发布状态、热质量字段。
- [library_item_terms](#library_item_terms) — 4 字段；公共卡片多值倒排；私有 Collection 不得混入公共 terms/facets。
- [library_items](#library_items) — 34 字段；Library/Media 浏览摘要。依赖同事务维护，不保留完整文件数组或来源响应。
- [library_metadata_inputs](#library_metadata_inputs) — 18 字段；Library 独立 NFO 候选、解析状态与导出来源字段；自产反馈/恢复目前未完整接通。
- [library_operations](#library_operations) — 10 字段；roots/locale/integrity/scrape 操作载体。当前建表不代表完整运行处理器已交付。
- [library_removal_roots](#library_removal_roots) — 2 字段；删除操作冻结的根路径范围；与持久文件归属需要一致。
- [library_removals](#library_removals) — 8 字段；Library freeze/drain/cleanup 操作、快照和恢复状态；多集及嵌套根清理仍有缺陷。
- [library_roots](#library_roots) — 6 字段；根路径配置。path_key 唯一；新增嵌套根后的持久重归属尚待完成。
- [library_units](#library_units) — 9 字段；Library/Media/单元级去重摘要和首选文件。whole=(-1,-1)，TV S00E00 合法。
- [media](#media) — 9 字段；规范 Media 身份；可无文件存在。retired 墓碑不是可继续写的兼容实体。
- [media_aliases](#media_aliases) — 5 字段；来源别名归属及每来源唯一刷新目标；不把来源 ID 当内部身份。
- [media_episodes](#media_episodes) — 13 字段；已采用分集详情；拥有事实来自文件覆盖。
- [media_markers](#media_markers) — 10 字段；Media/单元的片头片尾标记及人工锁；不是观看或文件覆盖事实。
- [media_metadata](#media_metadata) — 17 字段；work 级已采用元数据；当前采用入口尚未完整实现来源合成与生产接线。
- [media_metadata_overrides](#media_metadata_overrides) — 7 字段；实体/locale 人工字段覆盖。当前合并冲突保护未完整实现，不能以有表作为保证。
- [media_metadata_sources](#media_metadata_sources) — 14 字段；每实体/locale/来源的当前规范化快照与选择来源；不是响应缓存或历史表。
- [media_operations](#media_operations) — 10 字段；Media 合并/改挂持久计划与阶段。Store 合并实现仍有冲突/冻结/接线缺口。
- [media_seasons](#media_seasons) — 10 字段；已采用季详情；不是新的 Media 身份，不代表拥有该季。
- [pending_file_deletions](#pending_file_deletions) — 8 字段；待删除磁盘文件意图、身份检查与重试状态；不是浏览事件流。
- [playback_logs](#playback_logs) — 23 字段；终态会话历史；按稳定会话/generation 去重，不依赖文件仍存在。
- [playback_metrics](#playback_metrics) — 13 字段；Playback QoE 指标；仅按明确保留策略清理，不作为用户播放次数来源。
- [playback_session_ids](#playback_session_ids) — 16 字段；签发会话的逻辑目标、物理源、生命周期与 generation。生产消费者仍未完整消费绑定字段。
- [playback_sessions](#playback_sessions) — 31 字段；每用户/设备活动会话及源绑定、事件顺序、传输/观看统计。
- [playback_units](#playback_units) — 20 字段；用户逻辑观看/收藏事实及一套源绑定恢复包；不 FK 到物理文件，删文件保留原始事实。
- [probe_job_units](#probe_job_units) — 12 字段；Probe 文件单元、阶段与输出；活动单元/派发索引控制工作窗口。
- [probe_jobs](#probe_jobs) — 17 字段；专用 Probe 作业与单元计数、阶段策略和历史；不同于 jobs.db 的通用 Job。
- [probe_season_comparison_history](#probe_season_comparison_history) — 4 字段；每 Media/季最近比较摘要与发布时间，不是完整历史集合。
- [probe_stage_state](#probe_stage_state) — 10 字段；每文件/上下文/阶段的退避、错误与活动作业状态。
- [scan_operations](#scan_operations) — 11 字段；扫描作用域、generation、全量/定向意图与恢复报告。
- [scan_seen](#scan_seen) — 5 字段；当前扫描 operation 已确认处理的路径版本；与源观察、物理 FileVersion 不是同一语义。
- [scrape_sidecars](#scrape_sidecars) — 3 字段；文件的 NFO/图片 sidecar 路径；登记不等于导出 provenance 合同已完成。
- [unidentified](#unidentified) — 4 字段；带 Library/root 的低置信度待认领文件；删除 Library 前需显式清理，不能删磁盘字节。
- [user_library_items](#user_library_items) — 15 字段；用户/Library/Media 稀疏摘要；卡片删除会级联清理，重导入需要从原始用户事实重算。当前此写链存在漏维护。

## chapter_frame_tasks

章节帧生成的源版本、目标与持久执行状态。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `id` | `TEXT` | 1 | 是 | 无 |
| `file_id` | `TEXT` | — | 是 | 无 |
| `source_version` | `TEXT` | — | 是 | 无 |
| `targets_json` | `TEXT` | — | 是 | 无 |
| `targets_hash` | `TEXT` | — | 是 | 无 |
| `status` | `TEXT` | — | 是 | 无 |
| `error` | `TEXT` | — | 否 | 无 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 |
| `started_at_ms` | `INTEGER` | — | 否 | 无 |
| `finished_at_ms` | `INTEGER` | — | 否 | 无 |

**键与约束**

- 主键：`(id)`。
- `UNIQUE(file_id,source_version,targets_hash)`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

```sql
CREATE INDEX chapter_dispatch ON chapter_frame_tasks(status,created_at_ms,id);
```

## collection_items

以 canonical MediaId 保存成员关系与排序，不随单库文件删除而删除 Media 事实。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `collection_id` | `TEXT` | 1 | 是 | 无 |
| `media_id` | `TEXT` | 2 | 是 | 无 |
| `sort_order` | `INTEGER` | — | 是 | `0` |
| `added_at` | `TEXT` | — | 是 | 无 |

**键与约束**

- 主键：`(collection_id, media_id)`。
- `collection_id: NOT NULL REFERENCES collections(id) ON DELETE CASCADE`。
- `media_id: NOT NULL REFERENCES media(media_id)`。
- `PRIMARY KEY(collection_id,media_id)`。

**显式命名索引**

```sql
CREATE INDEX collection_media ON collection_items(media_id,collection_id);
```

## collections

公共系列或用户私有 Collection 定义；可见性与 owner 组合有 CHECK。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `id` | `TEXT` | 1 | 是 | 无 |
| `user_id` | `TEXT` | — | 否 | 无 |
| `visibility` | `TEXT` | — | 是 | 无 |
| `name` | `TEXT` | — | 是 | 无 |
| `sort_order` | `INTEGER` | — | 是 | `0` |
| `created_at` | `TEXT` | — | 是 | 无 |

**键与约束**

- 主键：`(id)`。
- `visibility: NOT NULL CHECK(visibility IN ('private','public'))`。
- `CHECK((visibility='private' AND user_id IS NOT NULL) OR (visibility='public' AND user_id IS NULL))`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

```sql
CREATE INDEX collections_owner ON collections(user_id,sort_order,id);
```

## file_details

同源版本的冷探测详情；随文件级联删除，不等于可随意清空的浏览缓存。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `file_id` | `TEXT` | 1 | 是 | 无 |
| `source_revision` | `INTEGER` | — | 是 | 无 |
| `source_version` | `TEXT` | — | 是 | 无 |
| `tracks_version` | `INTEGER` | — | 是 | 无 |
| `video_json` | `TEXT` | — | 否 | 无 |
| `audio_json` | `TEXT` | — | 是 | `'[]'` |
| `subtitle_json` | `TEXT` | — | 是 | `'[]'` |
| `chapters_json` | `TEXT` | — | 否 | 无 |
| `fingerprint_json` | `TEXT` | — | 否 | 无 |
| `outro_fingerprint_json` | `TEXT` | — | 否 | 无 |
| `format_duration_ms` | `INTEGER` | — | 否 | 无 |

**键与约束**

- 主键：`(file_id)`。
- `file_id: PRIMARY KEY NOT NULL REFERENCES library_files(file_id) ON DELETE CASCADE`。
- `CHECK(json_valid(audio_json))`。
- `CHECK(json_valid(subtitle_json))`。
- `CHECK(video_json IS NULL OR json_valid(video_json))`。
- `CHECK(chapters_json IS NULL OR json_valid(chapters_json))`。

**显式命名索引**

无；主键/UNIQUE 自动索引不重复列出。

## file_probe_obligations

随文件登记的持久探测义务；文件删除级联移除。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `file_id` | `TEXT` | 1 | 是 | 无 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 |
| `reason` | `TEXT` | — | 是 | 无 |

**键与约束**

- 主键：`(file_id)`。
- `file_id: PRIMARY KEY NOT NULL REFERENCES library_files(file_id) ON DELETE CASCADE`。

**显式命名索引**

```sql
CREATE INDEX probe_obligation_dispatch ON file_probe_obligations(created_at_ms,file_id);
```

## fingerprint_attempts

采样尝试、阶段、错误与工作指标；按作业/文件查询。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `attempt_id` | `TEXT` | 1 | 是 | 无 |
| `job_id` | `TEXT` | — | 是 | 无 |
| `file_id` | `TEXT` | — | 是 | 无 |
| `kind` | `TEXT` | — | 是 | 无 |
| `window_start_ms` | `INTEGER` | — | 是 | 无 |
| `window_end_ms` | `INTEGER` | — | 是 | 无 |
| `phase` | `TEXT` | — | 是 | 无 |
| `started_at_ms` | `INTEGER` | — | 是 | 无 |
| `finished_at_ms` | `INTEGER` | — | 否 | 无 |
| `status` | `TEXT` | — | 是 | 无 |
| `error_kind` | `TEXT` | — | 否 | 无 |
| `metrics_json` | `TEXT` | — | 是 | 无 |

**键与约束**

- 主键：`(attempt_id)`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

```sql
CREATE INDEX attempt_job_file ON fingerprint_attempts(job_id,file_id);
```

## fingerprint_cache

每物理文件当前指纹缓存及算法/时长参数。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `file_id` | `TEXT` | 1 | 是 | 无 |
| `cache_key` | `TEXT` | — | 是 | 无 |
| `algorithm_version` | `INTEGER` | — | 是 | 无 |
| `sample_duration_secs` | `INTEGER` | — | 是 | 无 |
| `media_duration_ms` | `INTEGER` | — | 否 | 无 |
| `intro_json` | `TEXT` | — | 是 | 无 |
| `outro_json` | `TEXT` | — | 否 | 无 |
| `captured_at_ms` | `INTEGER` | — | 是 | 无 |

**键与约束**

- 主键：`(file_id)`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

无；主键/UNIQUE 自动索引不重复列出。

## fingerprint_model_members

模型到样本/物理文件源版本的成员；仅 model_id 声明 FK。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `model_id` | `TEXT` | 1 | 是 | 无 |
| `sample_id` | `TEXT` | 2 | 是 | 无 |
| `file_id` | `TEXT` | — | 是 | 无 |
| `source_version` | `TEXT` | — | 是 | 无 |

**键与约束**

- 主键：`(model_id, sample_id)`。
- `model_id: NOT NULL REFERENCES fingerprint_season_models(model_id) ON DELETE CASCADE`。
- `PRIMARY KEY(model_id,sample_id)`。

**显式命名索引**

```sql
CREATE INDEX model_file ON fingerprint_model_members(file_id);
CREATE INDEX model_sample ON fingerprint_model_members(sample_id);
```

## fingerprint_samples

按文件源版本、profile、kind、窗口去重的采样事实。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `sample_id` | `TEXT` | 1 | 是 | 无 |
| `file_id` | `TEXT` | — | 是 | 无 |
| `source_version` | `TEXT` | — | 是 | 无 |
| `capture_profile_key` | `TEXT` | — | 是 | 无 |
| `kind` | `TEXT` | — | 是 | 无 |
| `window_start_ms` | `INTEGER` | — | 是 | 无 |
| `window_end_ms` | `INTEGER` | — | 是 | 无 |
| `pcm_duration_ms` | `INTEGER` | — | 否 | 无 |
| `fingerprint_json` | `TEXT` | — | 是 | 无 |
| `captured_job_id` | `TEXT` | — | 是 | 无 |
| `captured_at_ms` | `INTEGER` | — | 是 | 无 |
| `metrics_json` | `TEXT` | — | 是 | 无 |

**键与约束**

- 主键：`(sample_id)`。
- `UNIQUE(file_id,source_version,capture_profile_key,kind,window_start_ms,window_end_ms)`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

```sql
CREATE INDEX sample_job ON fingerprint_samples(captured_job_id);
CREATE INDEX sample_lookup ON fingerprint_samples(file_id,kind,source_version,capture_profile_key);
```

## fingerprint_season_models

季级指纹模型、成员/政策版本与模型体。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `model_id` | `TEXT` | 1 | 是 | 无 |
| `media_id` | `TEXT` | — | 是 | 无 |
| `season` | `INTEGER` | — | 是 | 无 |
| `kind` | `TEXT` | — | 是 | 无 |
| `model_version` | `INTEGER` | — | 是 | 无 |
| `membership_key` | `TEXT` | — | 是 | 无 |
| `policy_key` | `TEXT` | — | 是 | 无 |
| `model_json` | `TEXT` | — | 是 | 无 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 |

**键与约束**

- 主键：`(model_id)`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

```sql
CREATE INDEX season_model_lookup ON fingerprint_season_models(media_id,season,kind);
```

## libraries

Library 配置、显示语言、访问范围、生命周期与完整性状态；后两类 operation 合同不等于已实现。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `library_id` | `TEXT` | 1 | 是 | 无 |
| `kind` | `TEXT` | — | 是 | 无 |
| `name` | `TEXT` | — | 是 | 无 |
| `lifecycle_state` | `TEXT` | — | 是 | `'ready'` |
| `config_revision` | `INTEGER` | — | 是 | `0` |
| `integrity_state` | `TEXT` | — | 是 | `'unknown'` |
| `display_locale` | `TEXT` | — | 是 | `'zh-CN'` |
| `is_default` | `INTEGER` | — | 是 | `0` |
| `sort_order` | `INTEGER` | — | 是 | `0` |
| `access_mode` | `TEXT` | — | 是 | `'everyone'` |
| `admin_visible` | `INTEGER` | — | 是 | `1` |
| `member_ids_json` | `TEXT` | — | 是 | `'[]'` |
| `detect_intros` | `INTEGER` | — | 是 | `0` |
| `enable_fingerprint` | `INTEGER` | — | 是 | `0` |
| `cover_path` | `TEXT` | — | 否 | 无 |
| `cover_revision` | `INTEGER` | — | 是 | `0` |
| `match_rules_json` | `TEXT` | — | 是 | `'[]'` |
| `default_filter_id` | `TEXT` | — | 否 | 无 |
| `realtime_watch` | `INTEGER` | — | 是 | `1` |
| `generate_thumbnails` | `INTEGER` | — | 是 | `1` |
| `extract_chapter_images` | `INTEGER` | — | 是 | `1` |
| `exclude_from_home` | `INTEGER` | — | 是 | `0` |
| `auto_series_collections` | `INTEGER` | — | 是 | `1` |

**键与约束**

- 主键：`(library_id)`。
- `kind: NOT NULL CHECK(kind IN ('movie','tv','video'))`。
- `lifecycle_state: NOT NULL DEFAULT 'ready' CHECK(lifecycle_state IN ('ready','frozen','draining','reindexing','failed'))`。
- `integrity_state: NOT NULL DEFAULT 'unknown' CHECK(integrity_state IN ('unknown','clean','failed'))`。
- `is_default: NOT NULL DEFAULT 0 CHECK(is_default IN (0,1))`。
- `member_ids_json: NOT NULL DEFAULT '[]' CHECK(json_valid(member_ids_json))`。
- `match_rules_json: NOT NULL DEFAULT '[]' CHECK(json_valid(match_rules_json))`。
- `UNIQUE(library_id,kind)`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

```sql
CREATE UNIQUE INDEX library_default ON libraries(kind) WHERE is_default=1;
```

## library_file_units

文件到逻辑单元的覆盖事实；一文件可覆盖多集，同集可有多版本。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `file_id` | `TEXT` | 1 | 是 | 无 |
| `library_id` | `TEXT` | — | 是 | 无 |
| `media_id` | `TEXT` | — | 是 | 无 |
| `season` | `INTEGER` | 2 | 是 | 无 |
| `episode` | `INTEGER` | 3 | 是 | 无 |

**键与约束**

- 主键：`(file_id, season, episode)`。
- `PRIMARY KEY(file_id,season,episode)`。
- `UNIQUE(library_id,media_id,season,episode,file_id)`。
- `FOREIGN KEY(file_id,library_id,media_id) REFERENCES library_files(file_id,library_id,media_id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED`。
- `CHECK((season=-1 AND episode=-1) OR (season>=0 AND episode>=0))`。

**显式命名索引**

```sql
CREATE INDEX coverage_media ON library_file_units(media_id,season,episode,library_id,file_id);
```

## library_files

权威物理文件台账；显式 Library/root/Media/kind 与源版本、在位/发布状态、热质量字段。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `file_id` | `TEXT` | 1 | 是 | 无 |
| `library_id` | `TEXT` | — | 是 | 无 |
| `root_id` | `TEXT` | — | 是 | 无 |
| `media_id` | `TEXT` | — | 是 | 无 |
| `kind` | `TEXT` | — | 是 | 无 |
| `path_key` | `TEXT` | — | 是 | 无 |
| `display_path` | `TEXT` | — | 是 | 无 |
| `presence_state` | `TEXT` | — | 是 | 无 |
| `publication_state` | `TEXT` | — | 是 | 无 |
| `file_revision` | `INTEGER` | — | 是 | 无 |
| `source_version` | `TEXT` | — | 是 | 无 |
| `size_bytes` | `INTEGER` | — | 否 | 无 |
| `stat_known` | `INTEGER` | — | 是 | `0` |
| `mtime_ns` | `INTEGER` | — | 否 | 无 |
| `created_ns` | `INTEGER` | — | 否 | 无 |
| `source_digest` | `TEXT` | — | 否 | 无 |
| `missing_at` | `INTEGER` | — | 否 | 无 |
| `source_path` | `TEXT` | — | 否 | 无 |
| `transfer_mode` | `TEXT` | — | 否 | 无 |
| `resolution` | `TEXT` | — | 否 | 无 |
| `codec` | `TEXT` | — | 否 | 无 |
| `hdr` | `TEXT` | — | 否 | 无 |
| `quality_source` | `TEXT` | — | 是 | 无 |
| `confidence` | `TEXT` | — | 是 | 无 |
| `filter_score` | `INTEGER` | — | 否 | 无 |
| `release_quality` | `TEXT` | — | 否 | 无 |
| `duration_ms` | `INTEGER` | — | 否 | 无 |
| `tracks_version` | `INTEGER` | — | 是 | `0` |
| `poster_path` | `TEXT` | — | 否 | 无 |
| `backdrop_path` | `TEXT` | — | 否 | 无 |
| `still_path` | `TEXT` | — | 否 | 无 |
| `has_poster` | `INTEGER` | — | 是 | `0` |
| `has_backdrop` | `INTEGER` | — | 是 | `0` |
| `has_still` | `INTEGER` | — | 是 | `0` |

**键与约束**

- 主键：`(file_id)`。
- `path_key: NOT NULL UNIQUE`。
- `presence_state: NOT NULL CHECK(presence_state IN ('present','missing','unknown'))`。
- `publication_state: NOT NULL CHECK(publication_state IN ('pending','published'))`。
- `file_revision: NOT NULL CHECK(file_revision>0)`。
- `size_bytes: CHECK(size_bytes IS NULL OR size_bytes>=0)`。
- `transfer_mode: CHECK(transfer_mode IS NULL OR transfer_mode IN ('hardlink','copy','move'))`。
- `quality_source: NOT NULL CHECK(quality_source IN ('probe','release'))`。
- `confidence: NOT NULL CHECK(confidence IN ('high','low'))`。
- `duration_ms: CHECK(duration_ms IS NULL OR duration_ms>=0)`。
- `UNIQUE(file_id,library_id,media_id)`。
- `FOREIGN KEY(root_id,library_id) REFERENCES library_roots(root_id,library_id)`。
- `FOREIGN KEY(library_id,kind) REFERENCES libraries(library_id,kind)`。
- `FOREIGN KEY(media_id,kind) REFERENCES media(media_id,kind)`。

**显式命名索引**

```sql
CREATE INDEX files_library_media ON library_files(library_id,media_id,file_id);
CREATE INDEX files_media_library ON library_files(media_id,library_id,file_id);
CREATE INDEX files_presence ON library_files(library_id,presence_state,file_id);
CREATE INDEX files_root_path ON library_files(root_id,path_key);
```

## library_item_terms

公共卡片多值倒排；私有 Collection 不得混入公共 terms/facets。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `library_id` | `TEXT` | 1 | 是 | 无 |
| `term_kind` | `TEXT` | 2 | 是 | 无 |
| `term_value` | `TEXT` | 3 | 是 | 无 |
| `media_id` | `TEXT` | 4 | 是 | 无 |

**键与约束**

- 主键：`(library_id, term_kind, term_value, media_id)`。
- `term_kind: NOT NULL CHECK(term_kind IN ('genre','country','resolution','collection'))`。
- `PRIMARY KEY(library_id,term_kind,term_value,media_id)`。
- `FOREIGN KEY(library_id,media_id) REFERENCES library_items(library_id,media_id) ON DELETE CASCADE`。

**显式命名索引**

```sql
CREATE INDEX terms_media ON library_item_terms(library_id,media_id,term_kind,term_value);
```

## library_items

Library/Media 浏览摘要。依赖同事务维护，不保留完整文件数组或来源响应。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `library_id` | `TEXT` | 1 | 是 | 无 |
| `media_id` | `TEXT` | 2 | 是 | 无 |
| `kind` | `TEXT` | — | 是 | 无 |
| `title` | `TEXT` | — | 是 | 无 |
| `title_sort` | `TEXT` | — | 是 | 无 |
| `year` | `INTEGER` | — | 否 | 无 |
| `identity_state` | `TEXT` | — | 是 | 无 |
| `aliases_json` | `TEXT` | — | 是 | `'{}'` |
| `metadata_revision` | `INTEGER` | — | 是 | 无 |
| `item_revision` | `INTEGER` | — | 是 | 无 |
| `release_date_sort` | `TEXT` | — | 是 | `''` |
| `rating_sort` | `REAL` | — | 是 | `-1` |
| `runtime_sort` | `INTEGER` | — | 是 | `0` |
| `language` | `TEXT` | — | 否 | 无 |
| `added_at_ns` | `INTEGER` | — | 否 | 无 |
| `added_at_sort` | `INTEGER` | — | 是 | 无 |
| `size_bytes` | `INTEGER` | — | 否 | 无 |
| `size_sort` | `INTEGER` | — | 是 | `0` |
| `resolution_rank` | `INTEGER` | — | 是 | `0` |
| `hdr` | `INTEGER` | — | 是 | `0` |
| `scraped` | `INTEGER` | — | 是 | `0` |
| `has_nfo` | `INTEGER` | — | 是 | `0` |
| `missing` | `INTEGER` | — | 是 | `0` |
| `stable_shuffle_key` | `TEXT` | — | 是 | 无 |
| `file_count` | `INTEGER` | — | 是 | 无 |
| `available_file_count` | `INTEGER` | — | 是 | 无 |
| `missing_file_count` | `INTEGER` | — | 是 | `0` |
| `unknown_file_count` | `INTEGER` | — | 是 | `0` |
| `distinct_episode_count` | `INTEGER` | — | 是 | `0` |
| `distinct_season_count` | `INTEGER` | — | 是 | `0` |
| `preferred_file_id` | `TEXT` | — | 否 | 无 |
| `poster_ref` | `TEXT` | — | 否 | 无 |
| `backdrop_ref` | `TEXT` | — | 否 | 无 |
| `has_still` | `INTEGER` | — | 是 | `0` |

**键与约束**

- 主键：`(library_id, media_id)`。
- `library_id: NOT NULL REFERENCES libraries(library_id)`。
- `media_id: NOT NULL REFERENCES media(media_id)`。
- `file_count: NOT NULL CHECK(file_count>0)`。
- `PRIMARY KEY(library_id,media_id)`。
- `FOREIGN KEY(preferred_file_id,library_id,media_id) REFERENCES library_files(file_id,library_id,media_id) DEFERRABLE INITIALLY DEFERRED`。
- `CHECK(available_file_count>=0 AND missing_file_count>=0 AND unknown_file_count>=0)`。
- `CHECK(available_file_count+missing_file_count+unknown_file_count=file_count)`。
- `CHECK(available_file_count=0 OR preferred_file_id IS NOT NULL)`。
- `CHECK(missing=(missing_file_count>0))`。
- `CHECK(json_valid(aliases_json))`。

**显式命名索引**

```sql
CREATE INDEX items_added ON library_items(library_id,added_at_sort,media_id);
CREATE INDEX items_media ON library_items(media_id,library_id);
CREATE INDEX items_rating ON library_items(library_id,rating_sort,media_id);
CREATE INDEX items_release ON library_items(library_id,release_date_sort,media_id);
CREATE INDEX items_resolution ON library_items(library_id,resolution_rank,media_id);
CREATE INDEX items_runtime ON library_items(library_id,runtime_sort,media_id);
CREATE INDEX items_shuffle ON library_items(library_id,stable_shuffle_key,media_id);
CREATE INDEX items_size ON library_items(library_id,size_sort,media_id);
CREATE INDEX items_title ON library_items(library_id,title_sort,media_id);
```

## library_metadata_inputs

Library 独立 NFO 候选、解析状态与导出来源字段；自产反馈/恢复目前未完整接通。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `library_id` | `TEXT` | 1 | 是 | 无 |
| `media_id` | `TEXT` | 2 | 是 | 无 |
| `entity_scope` | `TEXT` | 3 | 是 | 无 |
| `season` | `INTEGER` | 4 | 是 | 无 |
| `episode` | `INTEGER` | 5 | 是 | 无 |
| `locale` | `TEXT` | 6 | 是 | 无 |
| `input_key` | `TEXT` | 7 | 是 | 无 |
| `root_id` | `TEXT` | — | 是 | 无 |
| `path_key` | `TEXT` | — | 是 | 无 |
| `parse_state` | `TEXT` | — | 是 | 无 |
| `source_version` | `TEXT` | — | 是 | 无 |
| `fields_json` | `TEXT` | — | 是 | 无 |
| `present_fields_json` | `TEXT` | — | 是 | 无 |
| `provenance_json` | `TEXT` | — | 是 | `'{}'` |
| `export_id` | `TEXT` | — | 否 | 无 |
| `export_digest` | `TEXT` | — | 否 | 无 |
| `export_fields_json` | `TEXT` | — | 否 | 无 |
| `revision` | `INTEGER` | — | 是 | 无 |

**键与约束**

- 主键：`(library_id, media_id, entity_scope, season, episode, locale, input_key)`。
- `media_id: NOT NULL REFERENCES media(media_id)`。
- `parse_state: NOT NULL CHECK(parse_state IN ('valid','invalid','unknown','missing'))`。
- `fields_json: NOT NULL CHECK(json_valid(fields_json))`。
- `present_fields_json: NOT NULL CHECK(json_valid(present_fields_json))`。
- `provenance_json: NOT NULL DEFAULT '{}' CHECK(json_valid(provenance_json))`。
- `export_fields_json: CHECK(export_fields_json IS NULL OR json_valid(export_fields_json))`。
- `PRIMARY KEY(library_id,media_id,entity_scope,season,episode,locale,input_key)`。
- `FOREIGN KEY(root_id,library_id) REFERENCES library_roots(root_id,library_id)`。
- `CHECK((entity_scope='work' AND season=-1 AND episode=-1) OR (entity_scope='season' AND season>=0 AND episode=-1) OR (entity_scope='episode' AND season>=0 AND episode>=0))`。

**显式命名索引**

```sql
CREATE INDEX metadata_input_path ON library_metadata_inputs(root_id,path_key);
```

## library_operations

roots/locale/integrity/scrape 操作载体。当前建表不代表完整运行处理器已交付。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `operation_id` | `TEXT` | 1 | 是 | 无 |
| `library_id` | `TEXT` | — | 是 | 无 |
| `kind` | `TEXT` | — | 是 | 无 |
| `state` | `TEXT` | — | 是 | 无 |
| `expected_revision` | `INTEGER` | — | 是 | 无 |
| `plan_json` | `TEXT` | — | 是 | 无 |
| `cursor_json` | `TEXT` | — | 否 | 无 |
| `error` | `TEXT` | — | 否 | 无 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 |
| `updated_at_ms` | `INTEGER` | — | 是 | 无 |

**键与约束**

- 主键：`(operation_id)`。
- `kind: NOT NULL CHECK(kind IN ('roots','locale','integrity','scrape'))`。
- `state: NOT NULL CHECK(state IN ('planned','frozen','draining','applying','checking','succeeded','failed'))`。
- `plan_json: NOT NULL CHECK(json_valid(plan_json))`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

```sql
CREATE INDEX library_operation_dispatch ON library_operations(state,updated_at_ms,operation_id);
```

## library_removal_roots

删除操作冻结的根路径范围；与持久文件归属需要一致。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `operation_id` | `TEXT` | 1 | 是 | 无 |
| `path` | `TEXT` | 2 | 是 | 无 |

**键与约束**

- 主键：`(operation_id, path)`。
- `operation_id: NOT NULL REFERENCES library_removals(operation_id)`。
- `PRIMARY KEY(operation_id,path)`。

**显式命名索引**

无；主键/UNIQUE 自动索引不重复列出。

## library_removals

Library freeze/drain/cleanup 操作、快照和恢复状态；多集及嵌套根清理仍有缺陷。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `operation_id` | `TEXT` | 1 | 是 | 无 |
| `library_id` | `TEXT` | — | 是 | 无 |
| `state` | `TEXT` | — | 是 | 无 |
| `snapshot_json` | `TEXT` | — | 是 | 无 |
| `removed_ledger_count` | `INTEGER` | — | 是 | `0` |
| `error` | `TEXT` | — | 否 | 无 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 |
| `updated_at_ms` | `INTEGER` | — | 是 | 无 |

**键与约束**

- 主键：`(operation_id)`。
- `library_id: NOT NULL UNIQUE`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

无；主键/UNIQUE 自动索引不重复列出。

## library_roots

根路径配置。path_key 唯一；新增嵌套根后的持久重归属尚待完成。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `root_id` | `TEXT` | 1 | 是 | 无 |
| `library_id` | `TEXT` | — | 是 | 无 |
| `path_key` | `TEXT` | — | 是 | 无 |
| `display_path` | `TEXT` | — | 是 | 无 |
| `enabled` | `INTEGER` | — | 是 | `1` |
| `sort_order` | `INTEGER` | — | 是 | `0` |

**键与约束**

- 主键：`(root_id)`。
- `library_id: NOT NULL REFERENCES libraries(library_id)`。
- `path_key: NOT NULL UNIQUE`。
- `UNIQUE(root_id,library_id)`。

**显式命名索引**

```sql
CREATE INDEX roots_library ON library_roots(library_id,sort_order,root_id);
```

## library_units

Library/Media/单元级去重摘要和首选文件。whole=(-1,-1)，TV S00E00 合法。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `library_id` | `TEXT` | 1 | 是 | 无 |
| `media_id` | `TEXT` | 2 | 是 | 无 |
| `season` | `INTEGER` | 3 | 是 | 无 |
| `episode` | `INTEGER` | 4 | 是 | 无 |
| `file_count` | `INTEGER` | — | 是 | 无 |
| `available_file_count` | `INTEGER` | — | 是 | 无 |
| `preferred_file_id` | `TEXT` | — | 否 | 无 |
| `quality_json` | `TEXT` | — | 是 | `'{}'` |
| `unit_revision` | `INTEGER` | — | 是 | 无 |

**键与约束**

- 主键：`(library_id, media_id, season, episode)`。
- `file_count: NOT NULL CHECK(file_count>0)`。
- `available_file_count: NOT NULL CHECK(available_file_count BETWEEN 0 AND file_count)`。
- `quality_json: NOT NULL DEFAULT '{}' CHECK(json_valid(quality_json))`。
- `PRIMARY KEY(library_id,media_id,season,episode)`。
- `FOREIGN KEY(library_id) REFERENCES libraries(library_id)`。
- `FOREIGN KEY(media_id) REFERENCES media(media_id)`。
- `FOREIGN KEY(library_id,media_id,season,episode,preferred_file_id) REFERENCES library_file_units(library_id,media_id,season,episode,file_id) DEFERRABLE INITIALLY DEFERRED`。
- `CHECK((season=-1 AND episode=-1) OR (season>=0 AND episode>=0))`。
- `CHECK(available_file_count=0 OR preferred_file_id IS NOT NULL)`。

**显式命名索引**

```sql
CREATE INDEX units_media_scope ON library_units(media_id,season,episode,library_id);
```

## media

规范 Media 身份；可无文件存在。retired 墓碑不是可继续写的兼容实体。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `media_id` | `TEXT` | 1 | 是 | 无 |
| `kind` | `TEXT` | — | 是 | 无 |
| `title` | `TEXT` | — | 是 | 无 |
| `original_title` | `TEXT` | — | 否 | 无 |
| `year` | `INTEGER` | — | 否 | 无 |
| `identity_state` | `TEXT` | — | 是 | 无 |
| `aliases_display_json` | `TEXT` | — | 是 | `'[]'` |
| `metadata_revision` | `INTEGER` | — | 是 | `0` |
| `survivor_id` | `TEXT` | — | 否 | 无 |

**键与约束**

- 主键：`(media_id)`。
- `kind: NOT NULL CHECK(kind IN ('movie','tv','video'))`。
- `year: CHECK(year IS NULL OR year BETWEEN 0 AND 9999)`。
- `identity_state: NOT NULL CHECK(identity_state IN ('provisional','confirmed','merging','retired'))`。
- `aliases_display_json: NOT NULL DEFAULT '[]' CHECK(json_valid(aliases_display_json))`。
- `survivor_id: REFERENCES media(media_id)`。
- `UNIQUE(media_id,kind)`。
- `CHECK((identity_state='retired')=(survivor_id IS NOT NULL))`。
- `CHECK(survivor_id IS NULL OR survivor_id<>media_id)`。

**显式命名索引**

```sql
CREATE INDEX media_title ON media(kind,title,year,media_id);
```

## media_aliases

来源别名归属及每来源唯一刷新目标；不把来源 ID 当内部身份。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `source` | `TEXT` | 1 | 是 | 无 |
| `kind` | `TEXT` | 2 | 是 | 无 |
| `external_id` | `TEXT` | 3 | 是 | 无 |
| `media_id` | `TEXT` | — | 是 | 无 |
| `is_refresh_target` | `INTEGER` | — | 是 | `0` |

**键与约束**

- 主键：`(source, kind, external_id)`。
- `source: NOT NULL CHECK(source IN ('tmdb','douban','tvdb','bangumi','anilist'))`。
- `is_refresh_target: NOT NULL DEFAULT 0 CHECK(is_refresh_target IN (0,1))`。
- `PRIMARY KEY(source,kind,external_id)`。
- `FOREIGN KEY(media_id,kind) REFERENCES media(media_id,kind)`。

**显式命名索引**

```sql
CREATE INDEX alias_media ON media_aliases(media_id,source);
CREATE UNIQUE INDEX alias_target ON media_aliases(media_id,source,kind) WHERE is_refresh_target=1;
```

## media_episodes

已采用分集详情；拥有事实来自文件覆盖。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `media_id` | `TEXT` | 1 | 是 | 无 |
| `season` | `INTEGER` | 2 | 是 | 无 |
| `episode` | `INTEGER` | 3 | 是 | 无 |
| `locale` | `TEXT` | 4 | 是 | 无 |
| `title` | `TEXT` | — | 否 | 无 |
| `overview` | `TEXT` | — | 否 | 无 |
| `air_date` | `TEXT` | — | 否 | 无 |
| `runtime_minutes` | `INTEGER` | — | 否 | 无 |
| `rating` | `REAL` | — | 否 | 无 |
| `still_url` | `TEXT` | — | 否 | 无 |
| `details_json` | `TEXT` | — | 是 | `'{}'` |
| `field_origins_json` | `TEXT` | — | 是 | `'{}'` |
| `revision` | `INTEGER` | — | 是 | 无 |

**键与约束**

- 主键：`(media_id, season, episode, locale)`。
- `media_id: NOT NULL REFERENCES media(media_id)`。
- `season: NOT NULL CHECK(season>=0)`。
- `episode: NOT NULL CHECK(episode>=0)`。
- `details_json: NOT NULL DEFAULT '{}' CHECK(json_valid(details_json))`。
- `field_origins_json: NOT NULL DEFAULT '{}' CHECK(json_valid(field_origins_json))`。
- `PRIMARY KEY(media_id,season,episode,locale)`。

**显式命名索引**

无；主键/UNIQUE 自动索引不重复列出。

## media_markers

Media/单元的片头片尾标记及人工锁；不是观看或文件覆盖事实。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `media_id` | `TEXT` | 1 | 是 | 无 |
| `season` | `INTEGER` | 2 | 是 | 无 |
| `episode` | `INTEGER` | 3 | 是 | 无 |
| `intro_start_ms` | `INTEGER` | — | 否 | 无 |
| `intro_end_ms` | `INTEGER` | — | 否 | 无 |
| `outro_start_ms` | `INTEGER` | — | 否 | 无 |
| `outro_end_ms` | `INTEGER` | — | 否 | 无 |
| `source` | `TEXT` | — | 是 | 无 |
| `locked` | `INTEGER` | — | 是 | `0` |
| `updated_at` | `INTEGER` | — | 是 | 无 |

**键与约束**

- 主键：`(media_id, season, episode)`。
- `PRIMARY KEY(media_id,season,episode)`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

无；主键/UNIQUE 自动索引不重复列出。

## media_metadata

work 级已采用元数据；当前采用入口尚未完整实现来源合成与生产接线。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `media_id` | `TEXT` | 1 | 是 | 无 |
| `locale` | `TEXT` | 2 | 是 | 无 |
| `title` | `TEXT` | — | 是 | 无 |
| `original_title` | `TEXT` | — | 否 | 无 |
| `year` | `INTEGER` | — | 否 | 无 |
| `overview` | `TEXT` | — | 否 | 无 |
| `release_date` | `TEXT` | — | 否 | 无 |
| `rating` | `REAL` | — | 否 | 无 |
| `runtime_minutes` | `INTEGER` | — | 否 | 无 |
| `language` | `TEXT` | — | 否 | 无 |
| `genres_json` | `TEXT` | — | 是 | `'[]'` |
| `countries_json` | `TEXT` | — | 是 | `'[]'` |
| `poster_url` | `TEXT` | — | 否 | 无 |
| `backdrop_url` | `TEXT` | — | 否 | 无 |
| `details_json` | `TEXT` | — | 是 | `'{}'` |
| `field_origins_json` | `TEXT` | — | 是 | `'{}'` |
| `revision` | `INTEGER` | — | 是 | 无 |

**键与约束**

- 主键：`(media_id, locale)`。
- `media_id: NOT NULL REFERENCES media(media_id)`。
- `rating: CHECK(rating IS NULL OR rating BETWEEN 0 AND 10)`。
- `runtime_minutes: CHECK(runtime_minutes IS NULL OR runtime_minutes>=0)`。
- `genres_json: NOT NULL DEFAULT '[]' CHECK(json_valid(genres_json))`。
- `countries_json: NOT NULL DEFAULT '[]' CHECK(json_valid(countries_json))`。
- `details_json: NOT NULL DEFAULT '{}' CHECK(json_valid(details_json))`。
- `field_origins_json: NOT NULL DEFAULT '{}' CHECK(json_valid(field_origins_json))`。
- `revision: NOT NULL CHECK(revision>=0)`。
- `PRIMARY KEY(media_id,locale)`。

**显式命名索引**

无；主键/UNIQUE 自动索引不重复列出。

## media_metadata_overrides

实体/locale 人工字段覆盖。当前合并冲突保护未完整实现，不能以有表作为保证。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `media_id` | `TEXT` | 1 | 是 | 无 |
| `entity_scope` | `TEXT` | 2 | 是 | 无 |
| `season` | `INTEGER` | 3 | 是 | 无 |
| `episode` | `INTEGER` | 4 | 是 | 无 |
| `locale` | `TEXT` | 5 | 是 | 无 |
| `fields_json` | `TEXT` | — | 是 | 无 |
| `revision` | `INTEGER` | — | 是 | 无 |

**键与约束**

- 主键：`(media_id, entity_scope, season, episode, locale)`。
- `media_id: NOT NULL REFERENCES media(media_id)`。
- `fields_json: NOT NULL CHECK(json_valid(fields_json))`。
- `PRIMARY KEY(media_id,entity_scope,season,episode,locale)`。
- `CHECK((entity_scope='work' AND season=-1 AND episode=-1) OR (entity_scope='season' AND season>=0 AND episode=-1) OR (entity_scope='episode' AND season>=0 AND episode>=0))`。

**显式命名索引**

无；主键/UNIQUE 自动索引不重复列出。

## media_metadata_sources

每实体/locale/来源的当前规范化快照与选择来源；不是响应缓存或历史表。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `media_id` | `TEXT` | 1 | 是 | 无 |
| `entity_scope` | `TEXT` | 2 | 是 | 无 |
| `season` | `INTEGER` | 3 | 是 | `-1` |
| `episode` | `INTEGER` | 4 | 是 | `-1` |
| `source` | `TEXT` | 5 | 是 | 无 |
| `locale` | `TEXT` | 6 | 是 | 无 |
| `provider_entity_id` | `TEXT` | — | 是 | 无 |
| `provider_work_id` | `TEXT` | — | 是 | 无 |
| `normalized_json` | `TEXT` | — | 是 | 无 |
| `present_fields_json` | `TEXT` | — | 是 | 无 |
| `preferences_json` | `TEXT` | — | 是 | 无 |
| `selection_origins_json` | `TEXT` | — | 是 | 无 |
| `fetched_at` | `INTEGER` | — | 是 | 无 |
| `source_revision` | `INTEGER` | — | 是 | 无 |

**键与约束**

- 主键：`(media_id, entity_scope, season, episode, source, locale)`。
- `media_id: NOT NULL REFERENCES media(media_id)`。
- `source: NOT NULL CHECK(source IN ('tmdb','douban','tvdb','bangumi','anilist','fanart'))`。
- `normalized_json: NOT NULL CHECK(json_valid(normalized_json))`。
- `present_fields_json: NOT NULL CHECK(json_valid(present_fields_json))`。
- `preferences_json: NOT NULL CHECK(json_valid(preferences_json))`。
- `selection_origins_json: NOT NULL CHECK(json_valid(selection_origins_json))`。
- `source_revision: NOT NULL CHECK(source_revision>=0)`。
- `PRIMARY KEY(media_id,entity_scope,season,episode,source,locale)`。
- `CHECK((entity_scope='work' AND season=-1 AND episode=-1) OR (entity_scope='season' AND season>=0 AND episode=-1) OR (entity_scope='episode' AND season>=0 AND episode>=0))`。

**显式命名索引**

无；主键/UNIQUE 自动索引不重复列出。

## media_operations

Media 合并/改挂持久计划与阶段。Store 合并实现仍有冲突/冻结/接线缺口。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `operation_id` | `TEXT` | 1 | 是 | 无 |
| `kind` | `TEXT` | — | 是 | 无 |
| `state` | `TEXT` | — | 是 | 无 |
| `survivor_id` | `TEXT` | — | 是 | 无 |
| `loser_id` | `TEXT` | — | 否 | 无 |
| `plan_json` | `TEXT` | — | 是 | 无 |
| `phase_cursor` | `TEXT` | — | 否 | 无 |
| `error` | `TEXT` | — | 否 | 无 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 |
| `updated_at_ms` | `INTEGER` | — | 是 | 无 |

**键与约束**

- 主键：`(operation_id)`。
- `kind: NOT NULL CHECK(kind IN ('merge','reassign'))`。
- `state: NOT NULL CHECK(state IN ('planned','frozen','draining','applying','checking','succeeded','failed'))`。
- `plan_json: NOT NULL CHECK(json_valid(plan_json))`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

无；主键/UNIQUE 自动索引不重复列出。

## media_seasons

已采用季详情；不是新的 Media 身份，不代表拥有该季。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `media_id` | `TEXT` | 1 | 是 | 无 |
| `season` | `INTEGER` | 2 | 是 | 无 |
| `locale` | `TEXT` | 3 | 是 | 无 |
| `title` | `TEXT` | — | 否 | 无 |
| `overview` | `TEXT` | — | 否 | 无 |
| `air_date` | `TEXT` | — | 否 | 无 |
| `poster_url` | `TEXT` | — | 否 | 无 |
| `details_json` | `TEXT` | — | 是 | `'{}'` |
| `field_origins_json` | `TEXT` | — | 是 | `'{}'` |
| `revision` | `INTEGER` | — | 是 | 无 |

**键与约束**

- 主键：`(media_id, season, locale)`。
- `media_id: NOT NULL REFERENCES media(media_id)`。
- `season: NOT NULL CHECK(season>=0)`。
- `details_json: NOT NULL DEFAULT '{}' CHECK(json_valid(details_json))`。
- `field_origins_json: NOT NULL DEFAULT '{}' CHECK(json_valid(field_origins_json))`。
- `PRIMARY KEY(media_id,season,locale)`。

**显式命名索引**

无；主键/UNIQUE 自动索引不重复列出。

## pending_file_deletions

待删除磁盘文件意图、身份检查与重试状态；不是浏览事件流。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `path` | `TEXT` | 1 | 是 | 无 |
| `reason` | `TEXT` | — | 是 | 无 |
| `attempts` | `INTEGER` | — | 是 | `0` |
| `next_retry_at` | `INTEGER` | — | 是 | `0` |
| `last_error` | `TEXT` | — | 否 | 无 |
| `file_id` | `TEXT` | — | 否 | 无 |
| `file_size` | `INTEGER` | — | 否 | 无 |
| `file_inode` | `INTEGER` | — | 否 | 无 |

**键与约束**

- 主键：`(path)`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

无；主键/UNIQUE 自动索引不重复列出。

## playback_logs

终态会话历史；按稳定会话/generation 去重，不依赖文件仍存在。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `id` | `TEXT` | 1 | 是 | 无 |
| `user_id` | `TEXT` | — | 是 | 无 |
| `media_id` | `TEXT` | — | 是 | 无 |
| `season` | `INTEGER` | — | 否 | 无 |
| `episode` | `INTEGER` | — | 否 | 无 |
| `client` | `TEXT` | — | 否 | 无 |
| `device_name` | `TEXT` | — | 否 | 无 |
| `device_id` | `TEXT` | — | 是 | 无 |
| `play_method` | `TEXT` | — | 是 | `'local'` |
| `started_at` | `INTEGER` | — | 是 | 无 |
| `ended_at` | `INTEGER` | — | 是 | 无 |
| `watched_ms` | `INTEGER` | — | 是 | `0` |
| `start_position_ms` | `INTEGER` | — | 是 | `0` |
| `end_position_ms` | `INTEGER` | — | 是 | `0` |
| `duration_ms` | `INTEGER` | — | 否 | 无 |
| `completed` | `INTEGER` | — | 是 | `0` |
| `admin_ended` | `INTEGER` | — | 是 | `0` |
| `play_session_id` | `TEXT` | — | 是 | 无 |
| `generation` | `INTEGER` | — | 是 | 无 |
| `file_id` | `TEXT` | — | 是 | 无 |
| `file_revision` | `INTEGER` | — | 是 | 无 |
| `source_version` | `TEXT` | — | 是 | 无 |
| `timeline_mode` | `TEXT` | — | 是 | 无 |

**键与约束**

- 主键：`(id)`。
- `UNIQUE(user_id,device_id,play_session_id,generation)`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

```sql
CREATE INDEX logs_ended ON playback_logs(ended_at,started_at,id);
CREATE INDEX logs_started ON playback_logs(started_at DESC,id DESC);
CREATE INDEX logs_user_started ON playback_logs(user_id,started_at DESC,id DESC);
```

## playback_metrics

Playback QoE 指标；仅按明确保留策略清理，不作为用户播放次数来源。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `id` | `TEXT` | 1 | 是 | 无 |
| `user_id` | `TEXT` | — | 是 | 无 |
| `media_id` | `TEXT` | — | 是 | 无 |
| `tier` | `INTEGER` | — | 是 | `0` |
| `engine` | `TEXT` | — | 否 | 无 |
| `ttff_ms` | `INTEGER` | — | 否 | 无 |
| `rebuffer_ms` | `INTEGER` | — | 是 | `0` |
| `rebuffer_count` | `INTEGER` | — | 是 | `0` |
| `seek_count` | `INTEGER` | — | 是 | `0` |
| `dropped_frames` | `INTEGER` | — | 否 | 无 |
| `total_frames` | `INTEGER` | — | 否 | 无 |
| `watched_ms` | `INTEGER` | — | 是 | `0` |
| `recorded_at` | `INTEGER` | — | 是 | 无 |

**键与约束**

- 主键：`(id)`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

```sql
CREATE INDEX metrics_recorded ON playback_metrics(recorded_at,id);
```

## playback_session_ids

签发会话的逻辑目标、物理源、生命周期与 generation。生产消费者仍未完整消费绑定字段。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `user_id` | `TEXT` | 1 | 是 | 无 |
| `device_id` | `TEXT` | 2 | 是 | 无 |
| `play_session_id` | `TEXT` | 3 | 是 | 无 |
| `item_id` | `TEXT` | — | 是 | 无 |
| `media_id` | `TEXT` | — | 是 | 无 |
| `season` | `INTEGER` | — | 是 | 无 |
| `episode` | `INTEGER` | — | 是 | 无 |
| `file_id` | `TEXT` | — | 是 | 无 |
| `file_revision` | `INTEGER` | — | 是 | 无 |
| `source_version` | `TEXT` | — | 是 | 无 |
| `library_id` | `TEXT` | — | 是 | 无 |
| `config_revision` | `INTEGER` | — | 是 | 无 |
| `timeline_mode` | `TEXT` | — | 是 | 无 |
| `issued_at` | `INTEGER` | — | 是 | 无 |
| `generation` | `INTEGER` | — | 是 | 无 |
| `state` | `TEXT` | — | 是 | 无 |

**键与约束**

- 主键：`(user_id, device_id, play_session_id)`。
- `file_revision: NOT NULL CHECK(file_revision>0)`。
- `config_revision: NOT NULL CHECK(config_revision>=0)`。
- `timeline_mode: NOT NULL CHECK(timeline_mode='full_file')`。
- `generation: NOT NULL CHECK(generation>=0)`。
- `state: NOT NULL CHECK(state IN ('pending','active','closed'))`。
- `PRIMARY KEY(user_id,device_id,play_session_id)`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

```sql
CREATE INDEX session_generation ON playback_session_ids(user_id,device_id,generation DESC);
CREATE INDEX session_id_expiry ON playback_session_ids(issued_at);
```

## playback_sessions

每用户/设备活动会话及源绑定、事件顺序、传输/观看统计。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `user_id` | `TEXT` | 1 | 是 | 无 |
| `device_id` | `TEXT` | 2 | 是 | 无 |
| `media_id` | `TEXT` | — | 是 | 无 |
| `season` | `INTEGER` | — | 是 | 无 |
| `episode` | `INTEGER` | — | 是 | 无 |
| `client` | `TEXT` | — | 否 | 无 |
| `device_name` | `TEXT` | — | 否 | 无 |
| `client_version` | `TEXT` | — | 否 | 无 |
| `play_method` | `TEXT` | — | 是 | `'local'` |
| `position_ms` | `INTEGER` | — | 是 | `0` |
| `start_position_ms` | `INTEGER` | — | 是 | `0` |
| `duration_ms` | `INTEGER` | — | 否 | 无 |
| `paused` | `INTEGER` | — | 是 | `0` |
| `watched_ms` | `INTEGER` | — | 是 | `0` |
| `rate_bps` | `INTEGER` | — | 是 | `0` |
| `bytes_sent` | `INTEGER` | — | 是 | `0` |
| `connections` | `INTEGER` | — | 是 | `0` |
| `admin_ended` | `INTEGER` | — | 是 | `0` |
| `started_at` | `INTEGER` | — | 是 | 无 |
| `last_report_at` | `INTEGER` | — | 是 | 无 |
| `file_id` | `TEXT` | — | 是 | 无 |
| `file_revision` | `INTEGER` | — | 是 | 无 |
| `source_version` | `TEXT` | — | 是 | 无 |
| `library_id` | `TEXT` | — | 是 | 无 |
| `config_revision` | `INTEGER` | — | 是 | 无 |
| `timeline_mode` | `TEXT` | — | 是 | 无 |
| `tracks_version` | `INTEGER` | — | 否 | 无 |
| `play_session_id` | `TEXT` | — | 是 | 无 |
| `generation` | `INTEGER` | — | 是 | 无 |
| `last_report_at_ms` | `INTEGER` | — | 是 | `0` |
| `last_event_sequence` | `INTEGER` | — | 否 | 无 |

**键与约束**

- 主键：`(user_id, device_id)`。
- `file_revision: NOT NULL CHECK(file_revision>0)`。
- `config_revision: NOT NULL CHECK(config_revision>=0)`。
- `timeline_mode: NOT NULL CHECK(timeline_mode='full_file')`。
- `generation: NOT NULL CHECK(generation>=0)`。
- `PRIMARY KEY(user_id,device_id)`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

```sql
CREATE INDEX sessions_user ON playback_sessions(user_id);
```

## playback_units

用户逻辑观看/收藏事实及一套源绑定恢复包；不 FK 到物理文件，删文件保留原始事实。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `user_id` | `TEXT` | 1 | 是 | 无 |
| `media_id` | `TEXT` | 2 | 是 | 无 |
| `season` | `INTEGER` | 3 | 是 | 无 |
| `episode` | `INTEGER` | 4 | 是 | 无 |
| `played` | `INTEGER` | — | 是 | `0` |
| `has_played_assertion` | `INTEGER` | — | 是 | `0` |
| `played_changed_at_ms` | `INTEGER` | — | 否 | 无 |
| `favorite` | `INTEGER` | — | 是 | `0` |
| `favorite_at` | `INTEGER` | — | 是 | `0` |
| `play_count` | `INTEGER` | — | 是 | `0` |
| `watch_activity_at` | `INTEGER` | — | 是 | `0` |
| `position_ms` | `INTEGER` | — | 是 | `0` |
| `duration_ms` | `INTEGER` | — | 否 | 无 |
| `resume_file_id` | `TEXT` | — | 否 | 无 |
| `resume_file_revision` | `INTEGER` | — | 否 | 无 |
| `resume_source_version` | `TEXT` | — | 否 | 无 |
| `timeline_mode` | `TEXT` | — | 否 | 无 |
| `tracks_version` | `INTEGER` | — | 否 | 无 |
| `audio_track` | `TEXT` | — | 否 | 无 |
| `subtitle_track` | `TEXT` | — | 否 | 无 |

**键与约束**

- 主键：`(user_id, media_id, season, episode)`。
- `media_id: NOT NULL REFERENCES media(media_id)`。
- `played: NOT NULL DEFAULT 0 CHECK(played IN (0,1))`。
- `has_played_assertion: NOT NULL DEFAULT 0 CHECK(has_played_assertion IN (0,1))`。
- `favorite: NOT NULL DEFAULT 0 CHECK(favorite IN (0,1))`。
- `play_count: NOT NULL DEFAULT 0 CHECK(play_count>=0)`。
- `position_ms: NOT NULL DEFAULT 0 CHECK(position_ms>=0)`。
- `duration_ms: CHECK(duration_ms IS NULL OR duration_ms>=0)`。
- `timeline_mode: CHECK(timeline_mode IS NULL OR timeline_mode='full_file')`。
- `PRIMARY KEY(user_id,media_id,season,episode)`。
- `CHECK((season=-1 AND episode=-1) OR (season>=0 AND episode>=-1))`。
- `CHECK((has_played_assertion=0 AND played=0 AND played_changed_at_ms IS NULL) OR (has_played_assertion=1 AND played_changed_at_ms IS NOT NULL))`。
- `CHECK((resume_file_id IS NULL AND resume_file_revision IS NULL AND resume_source_version IS NULL AND timeline_mode IS NULL AND position_ms=0 AND duration_ms IS NULL AND tracks_version IS NULL AND audio_track IS NULL AND subtitle_track IS NULL) OR (resume_file_id IS NOT NULL AND resume_file_revision IS NOT NULL AND resume_file_revision>0 AND resume_source_version IS NOT NULL AND timeline_mode IS NOT NULL))`。
- `CHECK((audio_track IS NULL AND subtitle_track IS NULL) OR tracks_version IS NOT NULL)`。

**显式命名索引**

```sql
CREATE INDEX playback_activity ON playback_units(user_id,media_id,watch_activity_at DESC,season,episode);
CREATE INDEX playback_unit_media ON playback_units(media_id,user_id,season,episode);
```

## probe_job_units

Probe 文件单元、阶段与输出；活动单元/派发索引控制工作窗口。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `job_id` | `TEXT` | 1 | 是 | 无 |
| `file_id` | `TEXT` | 2 | 是 | 无 |
| `kind` | `TEXT` | — | 是 | 无 |
| `force_fingerprint` | `INTEGER` | — | 是 | `0` |
| `reuse_fingerprint_cache` | `INTEGER` | — | 是 | `0` |
| `overwrite_markers` | `INTEGER` | — | 是 | `0` |
| `status` | `TEXT` | — | 是 | 无 |
| `error` | `TEXT` | — | 否 | 无 |
| `reuse_media_info_cache` | `INTEGER` | — | 是 | `0` |
| `stage` | `TEXT` | — | 是 | `'metadata'` |
| `sampling_plan_json` | `TEXT` | — | 否 | 无 |
| `detection_outcome_json` | `TEXT` | — | 否 | 无 |

**键与约束**

- 主键：`(job_id, file_id)`。
- `job_id: NOT NULL REFERENCES probe_jobs(id) ON DELETE CASCADE`。
- `PRIMARY KEY(job_id,file_id)`。

**显式命名索引**

```sql
CREATE UNIQUE INDEX probe_unit_active ON probe_job_units(file_id) WHERE status IN ('queued','running');
CREATE INDEX probe_unit_dispatch ON probe_job_units(stage,status,job_id) WHERE status='queued';
CREATE INDEX probe_unit_status ON probe_job_units(job_id,status);
```

## probe_jobs

专用 Probe 作业与单元计数、阶段策略和历史；不同于 jobs.db 的通用 Job。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `id` | `TEXT` | 1 | 是 | 无 |
| `kind` | `TEXT` | — | 是 | 无 |
| `media_id` | `TEXT` | — | 是 | 无 |
| `season` | `INTEGER` | — | 否 | 无 |
| `scope_key` | `TEXT` | — | 是 | 无 |
| `status` | `TEXT` | — | 是 | 无 |
| `total` | `INTEGER` | — | 是 | 无 |
| `completed` | `INTEGER` | — | 是 | `0` |
| `succeeded` | `INTEGER` | — | 是 | `0` |
| `failed` | `INTEGER` | — | 是 | `0` |
| `partial` | `INTEGER` | — | 是 | `0` |
| `error` | `TEXT` | — | 否 | 无 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 |
| `started_at_ms` | `INTEGER` | — | 否 | 无 |
| `finished_at_ms` | `INTEGER` | — | 否 | 无 |
| `analysis_policy_key` | `TEXT` | — | 否 | 无 |
| `sampling_mode` | `TEXT` | — | 否 | 无 |

**键与约束**

- 主键：`(id)`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

```sql
CREATE UNIQUE INDEX probe_active_scope ON probe_jobs(scope_key) WHERE status IN ('queued','running');
CREATE INDEX probe_media_created ON probe_jobs(media_id,created_at_ms DESC);
CREATE INDEX probe_scope_terminal ON probe_jobs(scope_key,finished_at_ms DESC,id DESC) WHERE status IN ('succeeded','failed','cancelled');
CREATE INDEX probe_terminal ON probe_jobs(finished_at_ms,id) WHERE status IN ('succeeded','failed','cancelled');
```

## probe_season_comparison_history

每 Media/季最近比较摘要与发布时间，不是完整历史集合。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `media_id` | `TEXT` | 1 | 是 | 无 |
| `season` | `INTEGER` | 2 | 是 | 无 |
| `comparison_digest` | `TEXT` | — | 是 | 无 |
| `published_at_ms` | `INTEGER` | — | 是 | 无 |

**键与约束**

- 主键：`(media_id, season)`。
- `PRIMARY KEY(media_id,season)`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

无；主键/UNIQUE 自动索引不重复列出。

## probe_stage_state

每文件/上下文/阶段的退避、错误与活动作业状态。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `file_id` | `TEXT` | 1 | 是 | 无 |
| `context_key` | `TEXT` | 2 | 是 | 无 |
| `stage` | `TEXT` | 3 | 是 | 无 |
| `status` | `TEXT` | — | 是 | 无 |
| `failure_count` | `INTEGER` | — | 是 | `0` |
| `next_retry_at_ms` | `INTEGER` | — | 否 | 无 |
| `error_kind` | `TEXT` | — | 否 | 无 |
| `error` | `TEXT` | — | 否 | 无 |
| `active_job_id` | `TEXT` | — | 否 | 无 |
| `updated_at_ms` | `INTEGER` | — | 是 | 无 |

**键与约束**

- 主键：`(file_id, context_key, stage)`。
- `PRIMARY KEY(file_id,context_key,stage)`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

```sql
CREATE INDEX probe_retry ON probe_stage_state(status,next_retry_at_ms) WHERE status='failed' AND next_retry_at_ms IS NOT NULL;
```

## scan_operations

扫描作用域、generation、全量/定向意图与恢复报告。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `id` | `TEXT` | 1 | 是 | 无 |
| `library_id` | `TEXT` | — | 是 | 无 |
| `scope_signature` | `TEXT` | — | 是 | 无 |
| `status` | `TEXT` | — | 是 | 无 |
| `generation` | `INTEGER` | — | 是 | 无 |
| `created_at_ms` | `INTEGER` | — | 是 | 无 |
| `report_json` | `TEXT` | — | 是 | `'{}'` |
| `targets_json` | `TEXT` | — | 是 | `'[]'` |
| `scope_kind` | `TEXT` | — | 是 | `'full'` |
| `dirty_rev` | `INTEGER` | — | 是 | `0` |
| `consumed_rev` | `INTEGER` | — | 是 | `0` |

**键与约束**

- 主键：`(id)`。
- `scope_kind: NOT NULL DEFAULT 'full' CHECK(scope_kind IN ('full','targeted'))`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

```sql
CREATE INDEX scan_library_status ON scan_operations(library_id,status);
```

## scan_seen

当前扫描 operation 已确认处理的路径版本；与源观察、物理 FileVersion 不是同一语义。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `operation_id` | `TEXT` | 1 | 是 | 无 |
| `path` | `TEXT` | 2 | 是 | 无 |
| `mtime_ms` | `INTEGER` | — | 是 | `0` |
| `file_size` | `INTEGER` | — | 是 | `0` |
| `content_digest` | `TEXT` | — | 否 | 无 |

**键与约束**

- 主键：`(operation_id, path)`。
- `PRIMARY KEY(operation_id,path)`。
- 无显式 FK；其他 ID 由业务写入/清理入口维护。

**显式命名索引**

无；主键/UNIQUE 自动索引不重复列出。

## scrape_sidecars

文件的 NFO/图片 sidecar 路径；登记不等于导出 provenance 合同已完成。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `file_id` | `TEXT` | 1 | 是 | 无 |
| `kind` | `TEXT` | 2 | 是 | 无 |
| `path` | `TEXT` | — | 是 | 无 |

**键与约束**

- 主键：`(file_id, kind)`。
- `file_id: NOT NULL REFERENCES library_files(file_id) ON DELETE CASCADE`。
- `PRIMARY KEY(file_id,kind)`。

**显式命名索引**

无；主键/UNIQUE 自动索引不重复列出。

## unidentified

带 Library/root 的低置信度待认领文件；删除 Library 前需显式清理，不能删磁盘字节。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `library_id` | `TEXT` | 1 | 是 | 无 |
| `root_id` | `TEXT` | — | 是 | 无 |
| `path` | `TEXT` | 2 | 是 | 无 |
| `confidence` | `TEXT` | — | 是 | 无 |

**键与约束**

- 主键：`(library_id, path)`。
- `PRIMARY KEY(library_id,path)`。
- `FOREIGN KEY(root_id,library_id) REFERENCES library_roots(root_id,library_id)`。

**显式命名索引**

无；主键/UNIQUE 自动索引不重复列出。

## user_library_items

用户/Library/Media 稀疏摘要；卡片删除会级联清理，重导入需要从原始用户事实重算。当前此写链存在漏维护。

| 字段 | SQLite 类型 | 主键顺位 | NOT NULL 声明 | DEFAULT |
|---|---|---:|---|---|
| `user_id` | `TEXT` | 1 | 是 | 无 |
| `library_id` | `TEXT` | 2 | 是 | 无 |
| `media_id` | `TEXT` | 3 | 是 | 无 |
| `seen` | `INTEGER` | — | 是 | 无 |
| `played` | `INTEGER` | — | 是 | 无 |
| `favorite` | `INTEGER` | — | 是 | 无 |
| `last_played_at` | `INTEGER` | — | 是 | `0` |
| `favorite_at` | `INTEGER` | — | 是 | `0` |
| `owned_episode_count` | `INTEGER` | — | 是 | `0` |
| `completed_episode_count` | `INTEGER` | — | 是 | `0` |
| `next_season` | `INTEGER` | — | 否 | 无 |
| `next_episode` | `INTEGER` | — | 否 | 无 |
| `resume_season` | `INTEGER` | — | 否 | 无 |
| `resume_episode` | `INTEGER` | — | 否 | 无 |
| `summary_revision` | `INTEGER` | — | 是 | 无 |

**键与约束**

- 主键：`(user_id, library_id, media_id)`。
- `PRIMARY KEY(user_id,library_id,media_id)`。
- `FOREIGN KEY(library_id,media_id) REFERENCES library_items(library_id,media_id) ON DELETE CASCADE`。
- `CHECK(completed_episode_count BETWEEN 0 AND owned_episode_count)`。

**显式命名索引**

```sql
CREATE INDEX user_items_activity ON user_library_items(user_id,library_id,last_played_at DESC,media_id);
CREATE INDEX user_items_media ON user_library_items(library_id,media_id,user_id);
```
