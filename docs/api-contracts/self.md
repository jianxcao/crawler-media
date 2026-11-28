# 自有 API 约定（v1）

> **本文不是现行完整路由表。** 资源、方法、字段以代码为准：
>
> - 后端路由：`crates/api/src/http/mod.rs`（`router_with_slow_after`）与路由表 `crates/api/src/http/routes/{mod,member,admin}.rs`（`/api/v1`）
> - 前端调用：`web/lib/api/*`
> - 领域词汇：`CONTEXT.md`
>
> 本文只锁定信封、认证、核心 DTO 形状，以及「明确不做」的产品边界。

本实例的管理面是**自有 REST**。不做上游 MoviePilot / MovieClaw 的信封、DTO 字段名或数字哈希 ID。Jellyfin/Emby 播放协议（Infuse 直连）是另一套表面，见 ADR-0005，不是「上游兼容层」。

当前存储边界、每库 writer 和统一读池见[数据库总览](../database/README.md)。请求字段及拒绝规则以当前 handler 和公开 API 回归为准。

## 通用约定

- 前缀：自有 REST 挂 `/api/v1`。
- 信封：成功 `{"ok": true, "data": ...}`；失败 `{"ok": false, "error": {"code": "...", "message": "..."}}`，HTTP status 4xx/5xx。
- ID：领域实体用 **UUID 字符串**（`MediaId` / `SiteId` / `SubscribeId` / … 的 Display），不能当数字 hash。Jellyfin协议逻辑ID与物理来源ID是独立表面：当前使用 `w1_` / `s1_` / `u1_` typed item 和 `f1_<file>_<revision>` source格式（`crates/domain/src/library_storage.rs`）。不能用裸UUID替代版本化来源合同；具体会话与取流约束以handler及协议回归为准。
- 认证：`Authorization: Bearer <token>`；`POST /auth/login` 返回 `{token, user}` 并设置 HttpOnly `mc_session` cookie，protected routes 也接受该 cookie（`http/auth.rs`）。静态图片（海报/背板/剧照/章节图/库封面）仍走 public 路由，不得因存在 cookie 而改成必须认证（见 `AGENTS.md`）。
- 错误 code：`resource.action`，如 `subscribe.missing`、`site.invalid`。message 中文可读。
- 枚举：`media.kind` = `movie|tv|video`（`video` 是「其他」库，不识别、不刮削）；`subscribe.fetch_mode` = `search|rss|both`。
- 时间：RFC3339 UTC。空值用 `null`。

## 核心形状

下列是面向调用方的核心投影，列表不穷尽字段；handler 的实际 JSON 与前端消费者必须共同核对。

### User

`{id, login, role: "admin"|"member", enabled}`

`role` 以 `users.role` 列为准，不是某个固定 UUID。

### Site

`{id, name, url, profile_id, cookie: null, api_key: null, rss_url?, proxy?, rate_limit_per_minute?, cdp_url?, downloader_id?, enabled, maintenance_via: "http"|"browser", browser_search, timezone?, auth_type: "apikey"|"cookie"|"none"}`

`cookie` 与 `api_key` 在列表、详情及新建/更新回显中始终为 `null`（从不明文回显）；非管理员读取时 `url` 置空串、`rss_url`/`proxy`/`cdp_url` 置 `null`（`rate_limit_per_minute` 与 `downloader_id` 保留原值）。站点上传激励与做种刷流状态由第一方 `builtin.boost` 插件端点独立暴露（见 `docs/plugins/`）。

### MediaView

`{id, kind, title, year?, original_title?, tmdb_id?, douban_id?, tvdb_id?, bangumi_id?, anilist_id?, poster_url?}`

Media 的权威身份与别名位于 `library.db.media` / `media_aliases`；别名键为 `(source, kind, external_id)`，同一来源可保留多个别名、至多一个 refresh target。上面的五个可空 `*_id` 是单值投影，不代表完整别名集合。`Store::update_media` 保留未被本次编辑替换的别名，并在事务中更新相关卡片。

章节与片头片尾的正式范围属于选中的 `(file_id, FileVersion, UnitKey)`。自有章节、播放会话及 Jellyfin 展示只读取当前登记版本匹配的范围；同 Media 同集的另一文件、Whole 与 E01、旧源版本的人工锁均不能共享。启用 TheIntroDB 后，云端结果可作为明确的 `theintrodb` 来源写入当前文件标记并用于播放 overlay；它不表示本文件声纹 `verified`，也不触发声纹或其他音频读取。仅对单文件单集且已知正整数当前文件时长的条目发起 `/v4/media` 请求（`duration_ms`、`details=true`、`merge_unknown=false`；未知时长直接跳过网络请求并保持已缓存章节免写锁只读）；云端未匹配时会回退热门 release，客户端必须检查 `group_duration_ms`，仅在响应坐标一致、已知 release 群组时长与文件时长差不超过 300,000ms 且范围合法位于文件时长内时采纳，未通过校验的结果直接丢弃并记入短期负缓存。已有正式标记（含人工锁和更高可信来源）优先，发布仍复查当前文件版本、时长和覆盖。`no_submissions`、空单段范围、未采纳结果与 `episode_not_found` / `media_not_found` 记入负缓存；429 区分配额耗尽、单条目限制与突发限流，全局冷却按 `(base_url, api_key)` 隔离且不受无关刮削设置保存影响，401/403、5xx 与传输失败进入对应上游冷却，未识别的其他 4xx/旧路由 404 及 JSON 解析错误既不写负缓存也不进全局冷却，均不能记录为已确认没有标记。章节列表的 `image_url` 是 opaque URL：公开 `GET /chapters/{file_id}/{canonical_target_index}` 必须携带 `generation`、`artifact` query，只返回当前登记源及当前目标对应的 DB published 不可变 JPEG；旧身份查询返回 404，缺失 query 拒绝。规范化目标序号不等于显示数组下标；客户端直接使用服务器 URL。缺失/损坏文件不返回图片 URL，显式生成仅修复缺失目标。公开本地侧车图片（`/posters/{ledger_id}`、`/fanart/{ledger_id}`、`/stills/{ledger_id}`）严格限制在所属 Library `root_paths` 内并拒绝符号链接，缺失返回 404 `poster.missing`，权限或 IO 错误返回 500 `poster.permission_denied` / `poster.io_error`（`Cache-Control: no-store`）。

章节详情接口必须指定有效可播的 `file_id`；TV 还须提供完整 `season`/`episode`，电影省略季集。`GET /libraries/{id}/items/{item_id}/chapters` 保持数组响应；`POST .../chapters/refresh` 返回选中文件的章节，指纹任务仍覆盖整季；条目 `/probe` 仍覆盖全部文件。服务端先验证 Library 可见性、Media、file/Unit 和登记版本，拒绝错误组合，不接受路径或自动改选其他版本。

`POST .../chapters/refresh` 必须提供 JSON `{capture_policy:"reuse_valid"|"recapture"}`；缺失、未知值或额外字段返回 422。`reuse_valid` 按当前源版本、明确音轨、PCM 格式、实际采样窗口和时间映射版本复用有效音频；阈值变化只重新分析。`recapture` 在新任务中重新读取音频，同一 durable job 恢复时只补缺失窗口。Intro/Outro 分别发布已验证结果；未解决 kind 保留同一当前源版本已有值，只有满足 absence coverage 的 NoMatch 才能清除该 kind，不能借用另一来源结果。前端“刷新片头片尾”使用 `reuse_valid`，“重新读取音频”使用 `recapture`。

成员认证的 `GET /libraries/{id}/items/{item_id}/markers?season=N`（电影省略 `season`，映射 `UnitKey::WHOLE`；TV 缺省 `season=1`）返回整季或整片检测进度（优先展示当前 Library 内仍处于 `queued`/`running` 的活动任务，无活动任务时再回退到最新终态任务）、声纹派发暂停状态及诊断列表 `{media_id, season: number|null, active, paused, job: MarkerJobSummary|null, episodes: [{file_id, season: number|null, episode: number|null, duration_ms: number|null, multi_unit, marker: {intro_start_ms, intro_end_ms, outro_start_ms, outro_end_ms, source, locked, updated_at}|null, detection: {job_id, stage, status, error, intro: {status, decision, reason, reason_label}|null, outro: {status, decision, reason, reason_label}|null}|null}]}`，所有 `reason_label` 为中文可读诊断。管理员 `POST .../markers/detect` 接受严格 JSON `{season?: number, episode?: number, file_id?: string, mode: "reuse_valid"|"recapture"|"theintrodb_only"}`：TV 支持整季或单集三种模式（按 `(library_id, media_id, season)` 独立作用域入队与去重，即使后台声纹队列处于暂停状态也允许显式执行；同作用域已有活动任务时返回 `queued: false, already_running: true, job: <该活动任务>`；存在非同作用域冲突任务时返回 `409 markers.conflict`；未传 `file_id` 且同集存在多版本文件时按 `file_id` 升序稳定处理，回执 `job` 优先取首个新入队任务，否则取首个复用的活动任务），电影仅支持 `theintrodb_only`（传 `reuse_valid`/`recapture` 返回 `400 markers.unsupported_kind`，未启用 TheIntroDB 返回 `400 markers.theintrodb_disabled`），`theintrodb_only` 仅同步云端 TheIntroDB 时间戳而不读取本地/远程音频（跳过已 `locked` 标记、并发人工锁导致 UPSERT 影响 0 行或云端确无匹配时返回 `200` 及 `updated_episodes: 0`；上游认证失败返回 `502 markers.theintrodb_auth_failed`，限流返回 `429 markers.theintrodb_rate_limited`，上游不可用或冷却期返回 `503 markers.theintrodb_unavailable` 并附 `Retry-After`；本地发布遇到写忙、存储故障或源变化时返回对应 `503`/`500`/`409` 错误而不伪装成功），返回 `{queued, already_running, skipped_multi_unit, updated_episodes?, job: MarkerJobSummary|null}`。管理员 `PUT .../markers`（严格 JSON `{file_id, season?: number|null, episode?: number|null, intro_start_ms?: number|null, intro_end_ms?: number|null, outro_start_ms?: number|null, outro_end_ms?: number|null, locked: boolean}`，电影省略 `season`/`episode` 映射 `UnitKey::WHOLE`，校验非负递增、已知正时长时不越界 `duration_ms` 且片头片尾不重叠，传 `locked: false` 可解锁已有标记；返回 `{intro_start_ms, intro_end_ms, outro_start_ms, outro_end_ms, source, locked, updated_at}`）与 `DELETE .../markers?file_id=...`（TV 追加 `&season=...&episode=...`，返回 `{deleted: true}`）按当前 `FileVersion` 原子写入或清除人工片头片尾标记并同步更新合成章节与截图任务（源文件版本变化返回 `409`）。

`POST .../chapters/generate` 保留 admin 权限，JSON body 为 `{retry_failed:boolean}`，返回 `{task:ChapterFrameTaskStatus|null,empty:boolean}`。重复提交复用当前任务；只有显式 `retry_failed:true` 才重试 partial/failed，已完成但缺失的输出可由显式生成受控修复。库关闭截图时返回 409；多单元文件无时间映射时返回 422。`empty:true` 仅代表当前没有有效目标；非空目标没有任务是 idle，不能解释为 completed。

成员认证的 `GET .../chapters/frame-status` 通过 reader pool 读取，query 为同一 `file_id`/季集，另外可同时提供 `task_id` 与 `generation`。两者均省略用于页面进入/重进时只读发现当前任务；只提供其中一项返回 400。跟踪时比较这对身份；不同任务即使 generation 相同也返回 `superseded:true` 及新的当前任务。目标变空时返回 `{task:null,empty:true,superseded:true}`（曾提供旧身份）；从不跨 Library 查找任务。

任务形状为 `{task_id,generation,attempt,state,total,ready,failed,targets:[{index,ready,error}]}`，state 为 `queued|running|completed|partial|failed|cancelled|superseded`。状态响应 `{task,empty,superseded}` 是观察时的可用性投影：取拥有完整身份的当前引用快照并释放 reader 后，在有界文件工作中校验每个已发布不可变 JPEG。缺失/损坏输出不计 ready，计 failed 并返回目标错误；原本 completed 的任务投影为 partial/failed，身份和 attempt 不变。GET 不写修复；持久变更由显式 generate/retry 或启动恢复处理。客户端仅在 queued/running 时每秒单请求轮询，终态重载章节，网络错误保留已知状态；公开图片鉴权边界保持不变。


### Subscription（CONTEXT：**Subscribe**）

`{id, media: MediaView, user_id, coverage, fetch_mode, filter_id, wash_cut, wash_cut_filter_id?, keep_old_versions, full_season_pack, downloader_id?, library_id?, tracking_state: "active"|"paused", follow_future, search_interval_secs, progress: {total, imported, missing, grabbing, downloaded}, created_at, updated_at}`

`coverage` = `{"kind": "movie"}` 或 `{"kind": "tv", "season", "episode_from", "episode_to"?}`。

暂停 / 续订窗口走 `PATCH /subscriptions/{id}`（`tracking_state` / `follow_future`），没有独立的 `/tracking-state`、`/follow-future` 端点。

`POST /subscriptions` 不接受 `tracking_state`；传入该字段返回 400，创建后的初始状态由服务端决定。该限制不影响上述 PATCH 合同或响应中的状态字段。

### RuleSet（CONTEXT：**Filter** 组）

`{id, name, is_default, keep_old_versions, atoms: [{kind, value?, priority, exclude, unknown?}]}`

`kind` 至少包括：`resolution` / `source` / `video_codec` / `free` / `hr` / `title_match` / `platform` / `team` / `hdr` / `size` / `min_seeders` / `subtitle_language` / `audio_language` / `site` / `wash_target` / `upgrade_ladder`。以 `domain::AtomRule` 为准。

`hr` 原子必须显式提供 `unknown: "exclude"|"allow"`。排除 H&R 时使用 `exclude: true`；`unknown: "exclude"` 同时排除状态未知的 Torrent，确认无 H&R 的 Torrent 不受此限制。搜索结果的 `hr` 为 `true|false|null`，`null` 表示未能确认。`platform` 与 `team` 是独立原子，不转换为标题子串条件；平台匹配发布标题中的完整标记，制作组匹配解析出的完整组名。前端直接保存 atoms；编辑过的偏好组保存新优先级，未编辑的组及隐藏条件保留原值。

HTTP 路径是 `/rule-sets`；表名仍是 `filters`。这是有意的 API 别名。

### Downloader

响应 DTO：`{id, name, kind: "qbittorrent"|"transmission", url, username?, auth_type: "password"|"apikey", has_api_key, has_password, category?, path_maps: [{from, to}], is_default, enabled, status: "disabled"|"active"|"failed"|"pending", last_error?}`。

`password` 与 `api_key` 仅为 `POST /downloaders`（成功返回 201）与 `PATCH /downloaders/{id}` 的只写输入字段，响应 JSON 永不输出这两个键；非管理员读取 `GET /downloaders` 与 `GET /downloaders/{id}` 时，`url` 置空串，`username` 与 `last_error` 置 `null`，`has_api_key` 与 `has_password` 置 `false`。响应中的 `auth_type` 由服务端根据 qBittorrent 实例当前是否存有非空 `api_key` 派生。`transmission` 仅支持 `auth_type: "password"`（传 `apikey` 返回 400 `downloader.invalid`，切换为非 qBittorrent 时清除已存 `api_key`）。`POST` 选择 `auth_type: "apikey"` 时忽略 `username` 与 `password`；`PATCH` 在单个 `app.db` `IMMEDIATE` 事务（`Store::mutate_downloader`）中原子执行读改写：省略凭据保留原值，传空串清空对应凭据，切为 `auth_type: "password"` 时同步清除 `api_key`，切为 `"apikey"` 时清除 `password`（及未显式传入的 `username`），连接、鉴权、分类或路径映射变化时同事务重置验证状态与 `last_error`。

### Library

`{id, kind, name, root_paths, is_default, stats: {item_count, file_count, total_size_bytes, …}, …}`

每种 `kind` 可以有多个库（多根、可见性、刮削开关等）。完整字段见 [Store Library](<../../crates/store/src/libraries.rs>) 与 `GET /libraries`。

管理员 `POST /libraries/{id}/media-info/refresh` 接受唯一 JSON 形状 `{mode:"missing"|"force",request_id:UUID,after?:file_id|null}`，缺失、未知模式或额外字段返回 422。`missing` 仅为媒体信息缺少或失效的在位文件入队，显式提交可重试之前失败的抓取；`force` 为本库全部已发布且在位文件重新读取媒体信息。两者跳过已有活动任务，不跨库扩展同一 Media 的文件；每请求最多检查 50 个文件并持久化入队，返回 `{mode,total,queued,skipped,next_cursor}`；客户端以同一 `request_id` 顺序请求到 `next_cursor=null`，重试同一操作不会重复抓取已经完成的文件（`total` 为本次遍历到的已发布文件数，包含跳过的失联文件；`queued` 为已持久化文件数，非完成数；各数字为本批计数，客户端累计）。未扫描的磁盘源变化返回 409 `library.probe_source_changed`，提示先扫描；不能以旧文件版本创建注定失效的任务。旧媒体信息保持可见直到成功替换；片头片尾和声纹仍服从库开关，不转换成显式标记刷新。此操作独立于 `/metadata/refresh` 的 TMDB 简介、海报、背景与 NFO 元数据刷新。

### Library 配置变更操作

`GET /settings/metadata-language` 返回 `{language, revision, operations:[{library_id, operation_id, state, error}]}`；`PATCH /settings/metadata-language` 只接受 `{expected_revision, language}`，拒绝未知字段或缺失字段（400 `settings.invalid`）。revision 过期、已有互斥 Library 操作或非 ready 库返回 409 `settings.conflict`。语言实际改变时，在一个 `library.db` 事务内更新默认语言及 revision，并为每个 Library 登记可恢复的 `default_locale` 操作；有操作时返回 202，无变化或无库时返回 200。同值保存仅返回当前状态，保留现有工作及 pending/failed 操作，不派发 driver；failed 操作通过显式 retry 恢复。`PUT /settings/metadata` 不接受 `language`，返回 400 `settings.language_moved`。单库 `display_locale` 优先于实例默认语言；二者切换都重建唯一的查询投影。

`PATCH /libraries/{id}`：带 `root_paths`（或 `display_locale`）时是一段可恢复、可观测的重配置，返回 **202** 操作对象（与下面 GET 同字段，`state` 此时为 `frozen`）；只改名称/可见性等同步字段仍返回 **200** Library 本体，不带 `operation_id`。同一请求的名称、权限、开关与重配置登记在一个 Library 事务内提交；路径校验、生命周期或 revision 冲突、操作登记失败时全部回滚，不取消已有任务。仅在提交成功后发布内存屏障、取消任务并启动 driver。缺少 `display_locale` 表示保留原配置，显式 `""` 表示清除单库覆盖并重建；新建和默认 Library 均继承实例语言，实例未配置时有效默认值为 `zh-CN`。重配置期间该库处于 `reindexing`：`POST /libraries/{id}/scan` 等写请求返回 **409** `library.reindexing`，`error.operation_id` 提供可供轮询的操作 ID；操作轮询仍可用，查询投影未就绪的读返回下述 typed 503。`GET /libraries/{id}/operations/{operation_id}` 返回 `{operation_id, library_id, kind, state, cursor, error}`（`kind` 为 `roots|locale|default_locale|integrity|scrape`，`cursor` 是批处理游标）；不存在的 operation、以及属于其他 Library 的 operation 一律 **404**。「冻结」是与操作 `frozen` 行同事务提交的 `libraries.lifecycle_state='reindexing'` + `config_revision` 自增，首批之前等待排空。依据 `crates/api/src/http/library_config.rs`、`http/library_operations.rs`、`crates/api/tests/storage_operations_runtime.rs`（合并于 `api_suite_3`）。

读取唯一投影尚未就绪的 Library 查询返回 typed **503** `library.reindexing`；客户端应轮询操作，不能将空页或旧语言结果视为成功。`POST /libraries/{id}/operations/{operation_id}/retry` 仅接受同库、持有当前 durable owner 且处于 failed 的 `roots|locale|default_locale|integrity` 操作，保留原 operation ID 与游标并返回 **202** `{operation_id,state}`；缺失或其他库的 ID 为 404，错误状态/owner 或已有 driver 为 409（`library.operation_running`）。已受理的操作会等待 filesystem blocking budget 容量，满载不会丢弃冻结操作；driver 错误会以 owner/revision CAS 持久化为 failed，保留已提交游标供 retry 或启动恢复。此 retry 是冻结写门的唯一精确路径例外。

根目录占用在 Library 写事务内校验：其他 Library 已登记的相同 canonical root key，以及其他 Library 尚未完成（含可重试 failed）的 roots 计划，都会使创建、PATCH 或 root 登记返回 **409** `library.root_conflict`，请求的配置、revision 和工作状态保持原值。相同请求内的重复 root 为 **400**。同一 Library 保留自己已登记的 root 合法；独立嵌套 roots 继续使用最长路径匹配归属。计划占用随操作持久化，重启后仍有效；操作成功释放的旧 root 可重新登记。

retry 的空 body 表示继续原计划。failed roots 操作在首个批次提交前（持久化 `cursor_json IS NULL`）可携带严格 JSON body `{"expected_revision":1,"root_paths":["/media/movies"]}` 修正 roots：`expected_revision` 必须等于当前冻结的 Library revision 及操作原 revision + 1。请求在取得独占 driver claim 后，同事务复查 owner、revision、root 占用和已记录文件覆盖，只替换 roots，保留 operation ID、其他计划字段及冻结 revision，再推进原操作。错误 revision/owner 返回 **409** `library.operation_not_owner`，已有批次返回 **409** `library.operation_not_replannable`，新 roots 冲突返回 **409** `library.root_conflict`，无效 body/重复 roots/覆盖不足返回 **400** `library.invalid_operation`；拒绝均保留原计划和游标。已有批次仍可用空 body 从原游标 retry。

前端 `web/lib/api/libraries.ts` 把 202 与 200 分成 `updateLibrary` 的 `{kind:"operation"}` / `{kind:"library"}` 两种结果；`updateLibraryAndWait` 负责轮询到终态。`assignLibraryFilesToTitle`（`POST /reidentify`）仍是单次同步 200，不参与这套轮询。

**重新识别契约（唯一形状）**：`GET /libraries/{id}/items/{item_id}/reidentification-preview` 返回 `media_item_id`、`source_revision`，每组 `files: [{file_id, revision, source_version}]`。`POST /reidentify` 只接受 `{media_item_id, source_revision, title_ref, files:[{file_id, revision, source_version}]}`（未知字段 400）；files 非空、无重复、revision>0、source_version 非空、全属 `media_item_id`，否则 400；版本或 source_revision 过期返回 409 且不移动任何文件，服务器从不补当前版本。分集 DTO 输出 `played`。Media 别名查询的 source 只认 `tmdb|douban|tvdb|bangumi|anilist`。

预览对尚未绑定任何外部 ID 的 Movie/TV 才搜索候选，搜索在 Store 读快照释放后执行；候选同类型、与已知年份一致，按 TMDB ID 去重并最多返回 20 个。多个候选 `code=ambiguous`，零候选 `no_match`，上游搜索失败 `tmdb_unreachable`；单候选 `code=null` 仍须确认，不能冒充已绑定身份。已绑定身份 `source=current` 直接回显且不重复搜索。预览不修改 Media、文件版本或 NFO；通过现有 `POST /reidentify` 选定候选后持久化。界面候选显示 TMDB ID 以区分同名同年份项，详见[身份复核](../design/metadata-identity-review.md)。

### Media 合并操作

`POST /media/merges`（`{request_id, survivor, loser, survivor_revision, loser_revision, refresh_targets?}`）返回 **202** `{operation_id}`，且 `operation_id == request_id`：同一份 payload 用同一个 `request_id` 重发返回同一个操作（幂等，不产生第二份冻结），同一 `request_id` 换 payload 返回 **409** `media.merge_conflict`。`GET /media/operations/{id}` 返回 `{operation_id, phase, state, error}`，未知 id **404** `media.operation_missing`。阶段推进由持有数据目录写租约的进程在 job loop 内驱动，GET/POST 都不推进；重启后未完成的操作继续推进。依据 `crates/api/src/http/media_operations.rs`、`crates/api/src/storage_operations.rs`、`crates/api/tests/storage_operations_runtime.rs`（合并于 `api_suite_3`）。

metadata/discovery/Playback 及独立 Jellyfin/Emby Router 的变更统一经过实例只读/维护写门禁（409 `instance.read_only` / 503 `runtime.busy` + Retry-After）。Playback 同事务拒绝 merging/retired 身份：self mark 为 409 `playback.protected`，错误信息包含可用 survivor；Jellyfin 可见的冻结项为 409，已退役且不可见项为 404。不重定向已退役 ID；已签发的迟到事件为 IgnoredStale。门禁只控制新准入，不取消或排空已准入 worker。

### 浏览列表分页：cursor，没有 offset

`GET /libraries/{id}/items`（海报墙的条目）与 `GET /libraries/{id}/gallery`（图廊的图组）都走
**键集游标**：

- 参数：`cursor`（可选，上一页的 `next_cursor` 原样带回）、`limit`（`1..=500`，默认 500，
  越界 400）。`sort` / `order` / 筛选键等其余参数见上面的查询键说明。
- **`offset` 不接受**：出现即 **400** `library.invalid`。翻页使用 `cursor`，使续页从排序键定位，不按深度逐行跳过。
- 响应**信封带分页元数据，`data` 仍是数组**：

  ```json
  {"ok": true, "data": [ ... ], "next_cursor": "<不透明字符串或 null>", "total": 250}
  ```

  `data` 保持数组，不嵌套为 `data: {items, next_cursor}`。`next_cursor` 为 `null` = 末页（**不允许**"刚好整页"时
  伪造下一页）。`total` **只在第一页**（不带 `cursor` 的那一页）给出，续页为 `null`——
  续页不再跑 `COUNT`；`gallery` 恒为 `null`（图组先剔除无图条目再分页，总数口径不同）。
- 游标是服务端签发的不透明 JSON，客户端不得解析或构造；非 JSON / 空串游标 **400**
  `library.invalid`。格式校验版本、Library/User/筛选/排序/方向作用域及排序键类型；不匹配或缺少版本/scope 返回 400。客户端必须在这些条件改变后从首页重来；item-index 签发的桶游标可用于同作用域列表。
- 前端只有一个实现：`web/lib/api/libraries.ts` 的 `buildBrowseQuery`，
  它**绝不**写 `offset`（`web/test/library-paging.test.mjs` 锁死）。海报墙是**前缀模型**：
  从第 0 条起连续加载，追加用 `next_cursor`；A-Z 跳转/「回到上次位置」使用下节
  item-index 签发的桶游标，不再按 offset 或多页游标走到目标。
- 依据 `crates/api/src/http/library.rs` 的 `BrowseWindow`/`browse_response`、
  `crates/api/src/http/library_gallery.rs`、`crates/api/tests/library_cursor_paging.rs`。

### 首字母索引与 A-Z 跳转：每档一个不透明游标

`GET /libraries/{id}/item-index?sort=&order=&<筛选键>` 返回按当前排序/筛选分好的首字母档：

```json
{"ok": true, "data": [{"initial": "A", "count": 12, "cursor": null},
                      {"initial": "B", "count": 28, "cursor": "<不透明游标>"}]}
```

- **响应不提供绝对下标**。档位在整份排序里的位置由 `count`
  前缀和还原——`crates/api/tests/library_index_cursor.rs` 与
  `web/lib/library-wall-index.ts` 的 `wallBucketOffsets` 都按这个口径。
- `cursor` 是「从这一档第一格开始」的键集游标，格式与 `/items` 的 `next_cursor`
  完全一致（客户端不得解析或构造）。第一档恒为 `null`：它从第 0 条开始，用没有
  cursor 的首页即可。
- 因此一次 A-Z 跳转 = **一次** `GET /libraries/{id}/items?cursor=<该档游标>`，返回
  的正是这一档的第一页；「回到上次位置」使用相同路径。
  跳转后窗口起点不为 0，向上滚动不做 prepend（与 cursor-only 分页同一取舍）。
- Library/排序/筛选/方向任一改变，客户端必须重新取 `item-index`。卡片游标已有服务端 scope 校验；item-index 通过 Library 读取流程使用统一只读池，准备与最终校验不获取 writer 锁；多库快照与发布校验边界见 [ADR-0008](../adr/0008-sqlite-system-of-record.md)。索引生成需遍历排序集合，不承诺一页的常量工作量。

### LibraryItemFile 与文件续页

`GET /libraries/{id}/items/{media_item_id}` 首详情带 `files` 首页与 `next_file_cursor`；
续页 `GET /libraries/{id}/items/{media_item_id}/files?after=&limit=`（`limit` 为 `1..=100`，默认 50）
返回 `{files, next_cursor}`。`cursor` 绑定 Library/Media/`file_id ASC` 顺序，越界或跨 scope 一律 400。

当前 Web adapter 会收齐全部文件页才 resolve 详情，并在 Probe 未结束时定期重拉。后端分页有界不代表客户端首屏只取一页；页面性能测量必须包含实际续页工作。

单个文件行（`web/lib/api/libraries.ts` 的 `LibraryItemFile`）：

- `file_id`：文件 UUID 字符串；**不是**逻辑 `ItemId`。
- `units: Array<{season, episode}>`：**完整覆盖**。一个物理文件可同时服务多集（如 E01–E02），
  不能只用一个 `season`/`episode` 代表全部覆盖；`season`/`episode` 仅作为「首个具体坐标」的便捷字段。
- `units: []` 表示覆盖未知，不等于整片；不得从单个 `season`/`episode` 字段重建覆盖。
- `presence: "present" | "missing" | "unknown"`：`missing`/`unknown` **可查看但不可选播**。
- `publication: "published" | "pending"`：browse/详情/播放版本只读 `published`；`pending` 只出现在管理工作状态。
- 同文件的质量/轨道字段保持既有形状，不随 `units` 改变。

分页语义：实现用 `limit + 1` 探测，只有发现超过 `limit` 行时才给 `next_cursor`，**末页 `next_cursor` 必为 `null`**
（不允许「刚好整页」时伪造下一页）。前端 `web/lib/library-file-pages.ts` 是唯一遍历实现，
遇到重复 `file_id` 必须显式失败，不得静默去重。0/49/50/51/100/101 文件边界由
`crates/api/tests/library_browse_rows.rs` 与 `web/test/library-file-pagination.test.mjs` 覆盖。

管理台账与本地文件搜索同样按物理文件返回一行，携带完整 `units`；搜索结果使用 `file_id` 区分同一 Media 的多个文件，不为未知覆盖补 whole。

Playback history/watch stats 的 `member_id` 只接受 User UUID；数字或其他无效值返回 400。Library/Filter/Media ID 在 Web adapter 中保留 UUID 字符串，不作 `Number(...)` 转换。

### Similar 推荐就绪状态

`GET /libraries/{id}/items/{media_item_id}/similar` 返回带状态的 `data`：

- HTTP 202：`{state: "pending", retry_after_ms: 100}`，不含 `items`。客户端按提示重取同一 URL；较短间隔可减少就绪结果的观察滞后，也会增加轮询请求。
- HTTP 200：`{state: "ready", items: [...]}`，包含完整推荐字段。来源明确返回 NotFound 或未配置来源时可以为空数组。
- 来源或缓存读取失败返回 HTTP 502 错误 envelope；队列满返回 HTTP 503。失败不转换为空推荐。

Web 详情展示等待状态，只在 `pending` 时轮询；离开当前条目时取消轮询，失败展示错误。
每次请求均检查当前 Library 可见性、Media 所属和发布状态。来源查询按 Media kind/TMDB ID 合并，
每个服务实例最多保留 64 个待处理或近期完成状态、最多 4 个来源查询并行；成功结果只保留在持久 catalog 缓存。

perfgate 分别记录首次 `pending`/`ready` 和获取完整 `ready.items` 的耗时、终态；首次 202 耗时不能当作完整推荐性能。
完整结果超时或来源失败仍使验证失败，不通过预热或空成功掩盖来源等待。

### LibraryFacets

`GET /libraries/{id}/facets`（筛选面板的唯一数据源；前端 `web/lib/api/libraries.ts` 的 `LibraryFacets`）：

`{total, genres, countries, decades, watch, ratings, runtimes, languages, resolutions, hdr, stock}`，每项 `{value, label, count}`。

- **计数**：维度 D 的值 v 的 `count` = 满足「**除 D 以外**全部已选条件」且 D 取 v 的 **distinct Media** 数；`total` = 满足全部条件的 distinct Media 数（不是 ledger 行数）。候选全集来自该库全部可见 Media，含 `count: 0`（前端置灰）。
- **查询键**：items/facets 共用 `browse_filter_from_query`。多值维度用逗号分隔，只接受短键 `g`、`c`、`d`（`2020s|2010s|2000s|1990s|earlier`）、`rt`（`lte60|60to90|90to120|gt120`）、`lang`、`res`、`stock`（`missing|unscraped`），维内 OR、维间 AND。`w` 是单值 `unwatched|watching|played|favorite|seen`，`seen` 为存在真实观看证据的首页语义；`rating_gte` 接受有限 `0..=10` 数值（6/7/8/9 是 UI 档位），`hdr` 为 `true|false`，另有 `identity`。排序使用 `sort` 和独立 `order`；未知键、枚举或排序返回 400。不能把 UI 预设档位当作 handler 全部接受范围（`crates/api/src/http/library/query.rs`）。
- **`tier`**：`primary` 只填 `genres/countries/decades/watch`，其余空数组；`all` 十维全填；**缺失或空串按 `all`**；其他非空值 → 400 `library.invalid`。
- 未知年份与 `>=2030` 不归入任何 `decades` 档。

### JobDef / Job

`JobDef` = `{id, kind, name, enabled, schedule?: {interval_secs} | null, payload, concurrency_key?, last_status?, last_finished_at?, next_run_after?}`

`Job` status = `queued|running|succeeded|failed|cancelled`。

### 维护任务

维护任务是 `kind ∈ {database_maintenance, jobs_retention, playback_history_retention, data_retention}` 的普通 JobDef；
调度与开关仍走既有 JobDef 字段，**保留参数只存在该 def 的版本化 payload 里**（不在 settings 另存一份）。

`payload`（`version: 1`；未知版本/未知字段/越界一律 400，绝不回退成更激进的删除策略）：

- `database_maintenance`：`{version: 1, vacuum_enabled: bool}`（默认 `false`）。
- `jobs_retention` / `playback_history_retention`：`{version: 1, retention_days: 1..=36500, dry_run: bool}`。
  `retention_days=0` **不代表永久保留**；永久保留请关闭该任务。`batch_size=500`、`max_batches=20` 是服务端固定安全预算，不暴露给用户。
- `data_retention`：`{version: 1, dry_run: bool, qoe_days: 1..=36500, transient_cache_days: 1..=36500, probe_days: 1..=36500, operations_days: 1..=36500}`。
  首次注册 `enabled=false`、`dry_run=true`、历史窗口默认 90 天、`operations_days` 默认 30 天；生产启用需要用户显式确认。
  `operations_days` 可省略，缺省为 30 天。QoE 只删 `playback_metrics`；
  catalog 只删 URL path 白名单内的 search/discover/trending 临时缓存（详情与未知 key 保护）；Probe 只删终态 job，
  每个 scope 至少保留最新 3 条（时间并列全部保留），被 sample 引用或仍有活动 unit 的 job 保护。samples/models/model_members 不在此清理范围。
  operations 只删**已到终态且最后更新时间早于窗口**的 operation 行：`library_operations`（`succeeded`，以及不可重试 kind 的 `failed`）、
  `scan_operations`（`completed|partial|cancelled|failed`）、`media_operations`（`succeeded`），
  连同 `scan_seen`、`library_query_count_rebuild` 子行一起删除；
  `library_removals` 与 `library_removal_roots` 永久保留（作为已删除媒体库防线，防止旧路径被误重新识别/绑定）。
  `running/applying/planned` 等活行、可重试的失败行与仍在窗口内的最新行一律保留。

维护 def 在 `GET /jobs` / `GET /jobs/{id}` 上**额外**返回（非维护 def 不返回这些键，既有字段不变）：

- `maintenance`：typed 策略摘要，如 `{kind, version, retention_days, dry_run}`、`{kind, version, vacuum_enabled}` 或 `{kind, version, dry_run, qoe_days, transient_cache_days, probe_days, operations_days}`；payload 损坏时为 `{kind, invalid: true, error}`。
- `payload_token`：**修改令牌**，等于服务端当前 `payload` 的原始 JSON 字符串。
- `last_report`：最近一次维护运行报告（`job.progress` 的原始 JSON 字符串，可能为 `null`）。

`PUT /api/v1/jobs/{id}/maintenance-config`（**仅管理员**，会员 403）：

- body = `{payload, expected_payload, interval_secs?}`；`payload` 与 `expected_payload` 必填。
- `expected_payload` 是并发令牌：**保存前的原 payload**，不是 payload 内的 schema `version`。与当前值不符 → 409 `job.conflict`；缺失 → 400 `job.missing_token`。
- 未知 def → 404 `job.missing`；def 存在但非维护 kind → 400 `job.not_maintenance`；非法配置 → 400 `job.invalid_config`（且不落库）。
- `interval_secs` 只对维护任务加范围 `3600..=31536000`，越界 → 400 `job.invalid_interval`；其他任务契约不变。
- 返回 `{id, kind, payload, payload_token, schedule?}`。当前 `jobs::update_def_config_cas` 在**同一事务**比较 payload、更新 payload/可选周期并刷新 queued 子任务的 payload；running 保留执行快照。省略周期时 handler 回读实际 schedule。并发令牌仍仅覆盖 payload，**不能检测仅改周期的冲突**，不能称为完整配置 revision。

`POST /api/v1/jobs/{id}/maintenance-preview`（**仅管理员**，会员 403）：

- body = `{payload?}`（缺省用该 def 当前已保存的 payload）。
- **只读窗口预览**：不写 def、不创建删除 Job、不写游标、不返回播放明细。当前已通过 `http_work` blocking budget 执行，等待上限 10 秒，超时返回 504 `job.preview_timeout`；该等待上限不证明底层 SQLite 单条语句已硬停止。
- 返回 `{kind, policy_version, retention_days?, cutoff?, scanned, candidates, protected, truncated, note?}`；Jobs 另返回 `protected_truncated`，`data_retention` 另含 `examined, incomplete, classes[]`（每类 `examined/candidates/protected/incomplete/resume_after`）；`classes[]` 的类别为 `qoe/transient_catalog/probe_history/operations`（数组形状不变，类别集合新增 `operations`）。
- Jobs 当前走 `prune_terminal_window`，一个原始 keyset 窗口包含保护行；`truncated` / `protected_truncated` 来自窗口外是否还有未检视行，计数只描述该窗口，不是全库精确总量。数据库维护没有删除候选，计数字段恒为 0。
- `scanned` 不等于 SQLite VM 实际扫描量；LIMIT 与批间预算不构成 SQL 执行时间/持锁硬上限。依据 `http/job_maintenance.rs`、`jobs/src/retention_cursor.rs`。

手动执行（`POST /jobs/{id}/run`，仅管理员）：

- `enabled=false` 的 def → 409 `job.disabled`（不再"返回 queued:true 却没入队"）。
- 当前返回 `status`：新建为 `created`，既有 future queued 被提前为 `expedited`，已排队为 `already_queued`，维护任务正在执行为 `already_running`；同时返回相应 `queued` / `live_status`，不把重复点击误报成新增工作。依据 `http/jobs.rs`、`jobs/src/schedule.rs`。
- 修改配置会刷新尚未运行的 queued payload，running 继续快照；这不等于运行中策略已被改变，也不撤销已提交的删除。

清理语义提醒：启用播放历史清理后，**历史页与依赖播放历史的统计窗口都只剩保留期内的数据**（不是仅隐藏列表）；删除不可撤销，只能从一致备份恢复。

当前 runner 在批/库边界检查 def 禁用、取消、停机与读取失败，停止后不启动下一批；已开始的一批不因此回滚，已删除数据也不恢复。SSE `/jobs/stream` 的 `count` 可因清理**下降**，不能用作单调递增序列；变更通知另有 revision（`jobs/src/change_state.rs`）。

## 存储与写入边界

四个 Store 文件为 app/cache/library/subscribe，队列在 jobs。Jellyfin 协议与公开静态图片保留独立合同。完整架构与测试方法见 [存储性能设计](../design/storage-performance.md) 和 [存储性能测试](../testing/storage-performance.md)。

**Transfer/Watch：** `GET /settings/transfer` 返回 `{revision,transfer_mode,scrape,watch_intake,watch_inplace}`；`PATCH` 必填 `expected_revision`，其他字段可选。Watch 缺失=不改、null/空白=清空、值=替换；transfer_mode/scrape 不接受 null；未知字段或坏类型 400，过期 revision 409，均不写。设置在 `library.db` 单事务内更新，不写 Library 根或 Naming。

**Library 根：** `PATCH /libraries/{id}` 的 root_paths/display_locale 返回 202 operation，按 `/libraries/{id}/operations/{operation_id}` 查询至 state=succeeded；新增嵌套根的归属收敛状态在 Library DTO 的 root_reassignments 可查询（operation_id/state/revision/config_revision/error）。Naming 仅走 `/settings/scrape` 当前 setting 的 naming_entry_dir/naming_movie_file/naming_season_dir/naming_episode_file 四段字段。

**Playback 事件：** 自有 `POST /playback/progress` 必须提交签发的 play_session_id、非负 event_sequence、device_id 和 start/progress/stop event；缺失、空白、伪造或跨用户/设备绑定 400 且不写播放事实。签发绑定固定 target/file/revision/source/config/generation；已签发但过期事件按 IgnoredStale 合同处理，不能补当前源身份。`POST /playback/sessions` 签发失败返回500；成功回执的 play_session_id 必填，流地址仅由签发绑定生成，保留合法STRM直接URL。Web 显式签发后上报，Jellyfin adapter 按协议建立绑定后复用 Store 提交。

**BFCache Playback 恢复：** 成员 `POST /playback/sessions` 可带 `resume_from_play_session_id`（有效 UUID 字符串）与保存的 `start_ms`。服务端在同一 Library 写事务中校验前驱的 User、device、Media/unit、管理员结束事实和设备最新 generation，再按当前文件版本、coverage、Library 可见性与配置签发新 SID。pending、active 或空闲回收的前驱均可恢复；已归档的管理员结束按原 SID 持久拒绝，不受设备级 10 秒窗口影响。缺失/跨 User/跨 device 前驱返回 404 `playback.session_missing`；目标改变返回 409 `playback.resume_target_changed`；已有更新代次返回 409 `playback.resume_superseded`；管理员结束返回 409 `playback.ended_by_admin`。字段为空、null、坏类型或非法 UUID 返回 400 `playback.invalid`；当前来源不可读沿用 404 `playback.item_missing`，内部存储失败记录错误并返回 500。失败不登记新代次。

Web 的 persisted pagehide 保存位置和播放意图，提交递增 sequence 的 paused progress，暂停媒体及心跳；匹配的 persisted pageshow 只恢复一次，并使用新 SID、stream URL 和从 1 开始的事件序列。主动暂停保持暂停，真正播放后才 start；旧请求与旧 SID 的迟到响应不影响新会话。普通 pagehide、SPA 切集和卸载消费当前上报身份并终止一次；错误/同意/结束页面不自动恢复。恢复请求在途时再次缓存保留原前驱；若先前请求已签发但回包过期，后续恢复可返回 Superseded 并进入错误页。错误页手动重试重新执行正常签发。分享的 local progress 作用域复用分享入口，不发送成员恢复字段。

**Unidentified/Downloader：** Unidentified 列表从登记的 library_id/root_id 返回并在 SQL 按 library_id 过滤，不按路径猜 Movie/TV。Claim 的独立 unidentified_id/source revision 在文件发布事务内 CAS 消费（删除再停放不会复用身份）；明确 IdentifyAndTransfer 允许改变最终 kind/Library。默认 Downloader 从 downloaders 行及明确支持的环境覆盖选择。

**Subscribe 请求字段：** 创建/PATCH 使用 `coverage` 和 `filter_id`；`POST /subscriptions/{id}/upgrade-runs` body 为 `{filter_id?, wash_cut?}`，回执也返回 `filter_id`。

**Playback 历史清除与继续观看隐藏：** `DELETE /playback/history` 支持 `scope=item|library|all`（缺省 `all`；`item`/`library` 配合 `media_item_id` / `library_id`，没有独立的 `/up-next/.../dismiss` 端点，单条目从继续观看移除走 `scope=item`）及可选非负秒级时间戳 `since`，在同一 `library.db` 事务中清除观看恢复/断言、播放日志与 QoE 指标，返回 `{deleted_states, deleted_metrics}`（`deleted_states` 为重置的单元状态数、删除的播放日志数与本次新增隐藏订阅数之和）。对范围内的当前用户在订 TV 订阅（不论 `active`/`paused`；提供 `since` 时仅限窗口内存在观看活动或播放日志、且窗口前已无更早观看状态的 Media），先经 `Store::mutate_setting` 原子写入 `app.db` 的 `user.<id>.up_next.dismissed`（遇忙返回 503 `runtime.busy`，存储失败返回 500 `store.error` 且不触碰 `library.db`）；若随后 `library.db` 写入被拒绝或失败（如 409 `playback.protected` / 503 `runtime.busy`），在写准入预算外执行逐 Media 补偿回滚以防静默隐藏。`GET /playback/up-next` 读取隐藏表时如实传播底层 SQLite 错误（读池忙返回 503 `runtime.busy`，SQLite 读取故障返回 500 `up_next.store`，绝不把存储读取失败当成空隐藏表而泄漏已移除条目；仅当设置值本身为非法 JSON 文本时告警并按空表兜底）。重新创建同 Media 订阅时清除对应隐藏记录，创建失败则恢复原时间戳。

**Playback marks 目标：** 字段为 `season_number` / `episode_number`，支持整项、整季与合法 TV S00E00。GET/POST 拒未知键、重复坐标与坏类型；坐标按 0..2147483647 checked conversion，越界 400，不饱和或截断。坏请求不写 playback_units。

成功信封使用 `{ok:true,data}`，明确 204 无 body 单独处理，保留合法业务空值。

普通协议分页使用写入期 genre/person 精确成员、分类计数与页内选源。精确 total、筛选交集和深 offset 成本单独测量，不推广为常量查询。

## 现行表面（去哪看）

| 面 | 挂载 | 说明 |
|---|---|---|
| 自有 REST | `/api/v1` | 唯一自有业务与管理契约。member / admin 在 `http::router` 分层。 |
| Jellyfin / Emby | `/` 与 `/emby` | 专供 Infuse / VidHub 等播放器直连，严格遵守 Jellyfin 官方开放协议。 |

完整 method/path 以 `crates/api/src/http/mod.rs` 的 `router_with_slow_after()` 与 `crates/api/src/http/routes/{mod,member,admin}.rs` 的 `.route(...)` 注册为准。

前端 `web/lib/api/*` 的路径相对同源 `/api/v1`。改密走 `PATCH /users/{id}` 的 `password`，没有 `POST /users/{id}/reset-password`。

## 明确不做

这些不是「暂时 stub」，是产品边界。不要为它们补端点或前端入口。

- AI：`/agent*` `/llm/*` `/mcp/*` `/skills`、对话 session
- 插件市场 / 浏览器扩展：`/extension/*`
- 转码 / 字幕生成 / PGS OCR
- IM 通道：微信 / Telegram / Discord / 上游 webhook。通知只走 `GET|PUT /notify/config` → `jianxcao/notify`
- OTA：`/app/update/*`
- Photos 模块

### 退订与用户删除

退订预览 `GET /subscriptions/{id}/removal-preview` 返回且仅返回 `library_file_count`、`library_bytes`、`torrent_count`、`torrent_count_inferred`、`torrent_titles`；文件数量为整个 Media，Torrent 数可能标记为推断，不支持按季预览。`season-cleanup` 在请求期间同步执行，回执 `queued` 不表示后台 Job；跨季共享物理文件可能整文件删除。

持久 User removal 记录的 `version` 必填且仅支持 1；恢复在驱动 pending 操作或修剪结果前验证全部 pending 记录。无效终态结果保留并记录。Subscribe 时间输出为固定24字符 UTC 毫秒字符串；非法时间返回错误。

Library roots 由 Library 配置管理。Playback 的诊断表面以实际 session/browser 路由为准，不提供 diagnostics/upload 字段。

### Downloader task snapshots

`GET /api/v1/downloaders/tasks` returns `{items}`. Each task contains `id`, `info_hash`, `name`, `downloader_id`, `downloader_name`, `progress`, `size_bytes`, `dlspeed_bytes`, `upspeed_bytes`, `uploaded_bytes`, `completed_bytes`, `state`, `site_id`, `media_item_id`, `media_title`, `media_kind`, `score`, and `subscriptions`. A subscription reference contains only `id`, `media_item_id`, `media_title`, and `media_kind`. No placeholder error, Site name, resolution, source-health list, episode progress or wash-cut purpose is returned. The Web uses actual task state and a readable generic error explanation.

`GET/PUT /api/v1/downloaders/limits` supports `download_limit_bytes` and `upload_limit_bytes` only; responses add `status` and `error`. No queue or alternate-speed settings are exposed.
