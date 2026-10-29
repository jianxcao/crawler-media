# 自有 API 约定（v1）

> **本文不是现行完整路由表。** 资源、方法、字段以代码为准：
>
> - 后端路由：`crates/api/src/http/mod.rs`（`router_with_slow_after`）与路由表 `crates/api/src/http/routes/{mod,member,admin}.rs`（`/api/v1`）
> - 前端调用：`web/lib/api/*`
> - 领域词汇：`CONTEXT.md`
>
> 本文只锁定信封、认证、核心 DTO 形状，以及「明确不做」的产品边界。
> 阶段进度见 `git show 5e44f101:docs/agents/roadmap.md`，那是历史档案，不是接口规范。

本实例的管理面是**自有 REST**。不做上游 MoviePilot / MovieClaw 的信封、DTO 字段名或数字哈希 ID。Jellyfin/Emby 播放协议（Infuse 直连）是另一套表面，见 ADR-0005，不是「上游兼容层」。

当前实现的锁/性能和旧输入缺口见[共享锁复审与未实施方案](../audits/2026-10-28-storage-lock-review.md)。上轮已删除三个旧 Filter kind 及旧根合并路由，结果见修复记录（`git show 5e44f101:docs/audits/2026-10-28-storage-remediation-verification.md`）；仍接受旧 title_ref 是待修缺陷，不是兼容合同。

## 通用约定

- 前缀：自有 REST 挂 `/api/v1`。
- 信封：成功 `{"ok": true, "data": ...}`；失败 `{"ok": false, "error": {"code": "...", "message": "..."}}`，HTTP status 4xx/5xx。
- ID：领域实体用 **UUID 字符串**（`MediaId` / `SiteId` / `SubscribeId` / … 的 Display），不能当数字 hash。Jellyfin协议逻辑ID与物理来源ID是独立表面：当前使用 `w1_` / `s1_` / `u1_` typed item 和 `f1_<file>_<revision>` source格式（`crates/domain/src/library_storage.rs`）。不能用裸UUID替代版本化来源合同；具体会话与取流约束以handler及协议回归为准。
- 认证：`Authorization: Bearer <token>`；`POST /auth/login` 返回 `{token, user}` 并设置 HttpOnly `mc_session` cookie，protected routes 也接受该 cookie（`http/auth.rs`）。静态图片（海报/背板/剧照/章节图/库封面）仍走 public 路由，不得因存在 cookie 而改成必须认证（见 `AGENTS.md`）。
- 错误 code：`resource.action`，如 `subscribe.missing`、`site.invalid`。message 中文可读。
- 枚举：`media.kind` = `movie|tv|video`（`video` 是「其他」库，不识别、不刮削）；`subscribe.fetch_mode` = `search|rss|both`。
- 时间：RFC3339 UTC。空值用 `null`。

## 核心形状

下列是面向现有调用方的核心投影，不是新存储模型的完整 DTO 保证。列表不穷尽字段；handler 的实际 JSON 与前端消费者必须共同核对，历史字段和未切换调用方不能作为新增兼容要求。

### User

`{id, login, role: "admin"|"member", enabled}`

`role` 以 `users.role` 列为准，不是某个固定 UUID。

### Site

`{id, name, url, profile_id, cookie?, api_key?, rss_url?, proxy?, rate_limit_per_minute?, cdp_url?, downloader_id?, enabled, boost?}`

凭证列表/读取时遮盖；刚写入时可回显一次。

### MediaView

`{id, kind, title, year?, original_title?, tmdb_id?, douban_id?, tvdb_id?, bangumi_id?, anilist_id?, poster_url?}`

Media 的权威身份与别名现存 `library.db.media` / `media_aliases`；别名键为 `(source, kind, external_id)`，同一来源可保留多个别名、至多一个 refresh target。上面的五个可空 `*_id` 仍是单值投影，不代表完整别名集合。`Store::update_media`当前保留未被本次编辑替换的别名，并在事务中更新相关卡片；不得把旧“先删后写且不归约”的缺陷描述当现行合同。

### Subscription（CONTEXT：**Subscribe**）

`{id, media: MediaView, user_id, coverage, fetch_mode, filter_id, wash_cut, wash_cut_filter_id?, keep_old_versions, full_season_pack, downloader_id?, library_id?, tracking_state: "active"|"paused", follow_future, search_interval_secs, progress: {total, imported, missing, grabbing, downloaded}, created_at, updated_at}`

`coverage` = `{"kind": "movie"}` 或 `{"kind": "tv", "season", "episode_from", "episode_to"?}`。

暂停 / 续订窗口走 `PATCH /subscriptions/{id}`（`tracking_state` / `follow_future`），没有独立的 `/tracking-state`、`/follow-future` 端点。

### RuleSet（CONTEXT：**Filter** 组）

`{id, name, is_default, atoms: [{kind, value?, priority, exclude?}]}`

`kind` 至少包括：`resolution` / `source` / `free` / `hr` / `title_match` / `hdr` / `size` / `min_seeders` / `subtitle_language` / `audio_language` / `site` / `wash_target` / `upgrade_ladder`。以 `domain::AtomRule` 为准。

HTTP 路径是 `/rule-sets`；表名仍是 `filters`。这是有意的 API 别名。

### Downloader

`{id, name, kind: "qbittorrent"|"transmission", url, username?, password?, category?, path_maps: [{from, to}], is_default, status, last_error?}`

### Library

`{id, kind, name, root_paths, is_default, stats: {item_count, file_count, total_size_bytes, …}, …}`

每种 `kind` 可以有多个库（多根、可见性、刮削开关等）。完整字段见 [Store Library](<../../crates/store/src/libraries.rs>) 与 `GET /libraries`。

### Library 配置变更操作

`GET /settings/metadata-language` 返回 `{language, revision, operations:[{library_id, operation_id, state, error}]}`；`PATCH /settings/metadata-language` 只接受 `{expected_revision, language}`，拒绝未知字段或缺失字段（400 `settings.invalid`）。revision 过期、已有互斥 Library 操作或非 ready 库返回 409 `settings.conflict`。语言实际改变时，在一个 `library.db` 事务内更新默认语言及 revision，并为每个 Library 登记可恢复的 `default_locale` 操作；有操作时返回 202，无变化或无库时返回 200。`PUT /settings/metadata` 的旧 `language` 字段明确返回 400 `settings.language_moved`，不作别名。单库 `display_locale` 优先于实例默认语言；二者切换都重建唯一的查询投影，不读取旧语言投影作兜底。

`PATCH /libraries/{id}`：带 `root_paths`（或 `display_locale`）时是一段可恢复、可观测的重配置，返回 **202** 操作对象（与下面 GET 同字段，`state` 此时为 `planned`）；只改名称/可见性等同步字段仍返回 **200** Library 本体，不带 `operation_id`。重配置期间该库处于 `reindexing`：`POST /libraries/{id}/scan` 等写请求返回 **409** `library.reindexing`，`error.operation_id` 提供可供轮询的操作 ID；操作轮询仍可用，查询投影未就绪的读返回下述 typed 503。`GET /libraries/{id}/operations/{operation_id}` 返回 `{operation_id, library_id, kind, state, cursor, error}`（`kind` 为 `roots|locale|default_locale|integrity|scrape`，`cursor` 是批处理游标）；不存在的 operation、以及属于其他 Library 的 operation 一律 **404**。「冻结」不是独立阶段，而是受理时与 `planned` 行同事务提交的 `libraries.lifecycle_state='reindexing'` + `config_revision` 自增，外加首批之前的排空等待。依据 `crates/api/src/http/library_config.rs`、`http/library_operations.rs`、`crates/api/tests/storage_operations_runtime.rs`。

读取唯一投影尚未就绪的 Library 查询返回 typed **503** `library.reindexing`；客户端应轮询操作，不能将空页或旧语言结果视为成功。`POST /libraries/{id}/operations/{operation_id}/retry` 仅接受同库、持有当前 durable owner 且处于 failed 的 `roots|locale|default_locale|integrity` 操作，保留原 operation ID 与游标并返回 **202** `{operation_id,state}`；缺失或其他库的 ID 为 404，错误状态/owner 为 409。此 retry 是冻结写门的唯一精确路径例外。

前端 `web/lib/api/libraries.ts` 把 202 与 200 分成 `updateLibrary` 的 `{kind:"operation"}` / `{kind:"library"}` 两种结果；`updateLibraryAndWait` 负责轮询到终态。`assignLibraryFilesToTitle`（`POST /reidentify`）仍是单次同步 200，不参与这套轮询。

**重新识别契约（唯一形状）**：`GET /libraries/{id}/items/{item_id}/reidentification-preview` 返回 `media_item_id`、`source_revision`，每组 `files: [{file_id, revision, source_version}]`（不再有 `file_ids`）。`POST /reidentify` 只接受 `{media_item_id, source_revision, title_ref, files:[{file_id, revision, source_version}]}`（未知字段 400）；files 非空、无重复、revision>0、source_version 非空、全属 `media_item_id`，否则 400；版本或 source_revision 过期返回 409 且不移动任何文件，服务器从不补当前版本。分集 DTO 只输出 `played`（已删 `watched`）。Media 别名查询的 source 只认 `tmdb|douban|tvdb|bangumi|anilist`，`*_id` 旧词报错。

### Media 合并操作

`POST /media/merges`（`{request_id, survivor, loser, survivor_revision, loser_revision, refresh_targets?}`）返回 **202** `{operation_id}`，且 `operation_id == request_id`：同一份 payload 用同一个 `request_id` 重发返回同一个操作（幂等，不产生第二份冻结），同一 `request_id` 换 payload 返回 **409** `media.merge_conflict`。`GET /media/operations/{id}` 返回 `{operation_id, phase, state, error}`，未知 id **404** `media.operation_missing`。阶段推进由持有数据目录写租约的进程在 job loop 内驱动，GET/POST 都不推进；重启后未完成的操作继续推进。依据 `crates/api/src/http/media_operations.rs`、`crates/api/src/storage_operations.rs`、`crates/api/tests/storage_operations_runtime.rs`。

R6/R7 修复后，metadata/discovery/Playback 及独立 Jellyfin/Emby Router 的变更统一经过实例只读/维护写门禁（409 `instance.read_only` / 503 `runtime.busy` + Retry-After）。Playback 同事务拒绝 merging/retired 身份：self mark 为 409 `playback.protected`，错误信息包含可用 survivor；Jellyfin 可见的冻结项为 409，已退役且不可见项为 404。不重定向旧 ID；已签发的迟到事件仍为 IgnoredStale。门禁只控制新准入，不取消或排空已准入 worker。回归与恢复证据见复审 R6/R7（`git show 5e44f101:docs/audits/2026-10-28-storage-main-rereview.md`）。

### 浏览列表分页：cursor，没有 offset

`GET /libraries/{id}/items`（海报墙的条目）与 `GET /libraries/{id}/gallery`（图廊的图组）都走
**键集游标**：

- 参数：`cursor`（可选，上一页的 `next_cursor` 原样带回）、`limit`（`1..=500`，默认 500，
  越界 400）。`sort` / `order` / 筛选键等其余参数见上面的查询键说明。
- **`offset` 已删除**：出现即 **400** `library.invalid`（合法取值也一样，不做兼容）。
  原因：offset 每页都要 SQL 逐行跳过、且每页都可能带 `COUNT`，同一深度下比键集游标贵一个
  量级（实测 24,338 vs 2,147 VM steps）。要翻页就带 `cursor`。
- 响应**信封带分页元数据，`data` 仍是数组**：

  ```json
  {"ok": true, "data": [ ... ], "next_cursor": "<不透明字符串或 null>", "total": 250}
  ```

  这条形状是刻意定下来的，不要"整理"成 `data: {items, next_cursor}`：`data` 保持数组，
  一批既有调用方与测试按数组读它。`next_cursor` 为 `null` = 末页（**不允许**"刚好整页"时
  伪造下一页）。`total` **只在第一页**（不带 `cursor` 的那一页）给出，续页为 `null`——
  续页不再跑 `COUNT`；`gallery` 恒为 `null`（图组先剔除无图条目再分页，总数口径不同）。
- 游标是服务端签发的不透明 JSON，客户端不得解析或构造；非 JSON / 空串游标 **400**
  `library.invalid`。当前唯一格式校验版本、Library/User/筛选/排序/方向作用域及排序键类型；不匹配或缺少版本/scope 返回 400，不解码旧无 scope 格式。客户端必须在这些条件改变后从首页重来；item-index 签发的桶游标可用于同作用域列表。修复对账见当前复审（`git show 5e44f101:docs/audits/2026-10-28-storage-main-rereview.md`）。
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

- **线上没有绝对下标**（`offset` 字段已删除）。档位在整份排序里的位置由 `count`
  前缀和还原——`crates/api/tests/library_index_cursor.rs` 与
  `web/lib/library-wall-index.ts` 的 `wallBucketOffsets` 都按这个口径。
- `cursor` 是「从这一档第一格开始」的键集游标，格式与 `/items` 的 `next_cursor`
  完全一致（客户端不得解析或构造）。第一档恒为 `null`：它从第 0 条开始，用没有
  cursor 的首页即可。
- 因此一次 A-Z 跳转 = **一次** `GET /libraries/{id}/items?cursor=<该档游标>`，返回
  的正是这一档的第一页（9.5k 部的库实测 1 次请求；「回到上次位置」同一条路）。
  跳转后窗口起点不为 0，向上滚动不做 prepend（与 cursor-only 分页同一取舍）。
- Library/排序/筛选/方向任一改变，客户端必须重新取 `item-index`。卡片游标已有服务端 scope 校验；item-index 走独立 reader，但准备/最终校验仍依赖共享 Store，边界见[当前复审 L1](../audits/2026-10-28-storage-lock-review.md)。索引生成需遍历排序集合，不承诺一页的常量工作量。

### LibraryItemFile 与文件续页

`GET /libraries/{id}/items/{media_item_id}` 首详情带 `files` 首页与 `next_file_cursor`；
续页 `GET /libraries/{id}/items/{media_item_id}/files?after=&limit=`（`limit` 为 `1..=100`，默认 50）
返回 `{files, next_cursor}`。`cursor` 绑定 Library/Media/`file_id ASC` 顺序，越界或跨 scope 一律 400。

当前 Web adapter 会收齐全部文件页才 resolve 详情，并在 Probe 未结束时定期重拉；这是[复审 L5](../audits/2026-10-28-storage-lock-review.md)的性能缺口，不代表后端已实现的文件分页本身仍读取全部覆盖。首屏与按需加载的新消费者合同尚未实施。

单个文件行（`web/lib/api/libraries.ts` 的 `LibraryItemFile`）：

- `file_id`：文件 UUID 字符串；**不是**逻辑 `ItemId`。
- `units: Array<{season, episode}>`：**完整覆盖**。一个物理文件可同时服务多集（如 E01–E02），
  不能只用一个 `season`/`episode` 代表全部覆盖；`season`/`episode` 仅作为「首个具体坐标」的便捷字段。
- `units: []` 表示覆盖未知，不等于整片；不得从旧 `season`/`episode` 字段重建覆盖。
- `presence: "present" | "missing" | "unknown"`：`missing`/`unknown` **可查看但不可选播**。
- `publication: "published" | "pending"`：browse/详情/播放版本只读 `published`；`pending` 只出现在管理工作状态。
- 同文件的质量/轨道字段保持既有形状，不随 `units` 改变。

分页语义：实现用 `limit + 1` 探测，只有发现超过 `limit` 行时才给 `next_cursor`，**末页 `next_cursor` 必为 `null`**
（不允许「刚好整页」时伪造下一页）。前端 `web/lib/library-file-pages.ts` 是唯一遍历实现，
遇到重复 `file_id` 必须显式失败，不得静默去重。0/49/50/51/100/101 文件边界由
`crates/api/tests/library_browse_rows.rs` 与 `web/test/library-file-pagination.test.mjs` 覆盖。

管理台账与本地文件搜索同样按物理文件返回一行，携带完整 `units`；搜索结果使用 `file_id` 区分同一 Media 的多个文件。不展开为旧的一单元一行，也不为未知覆盖补 whole。

Playback history/watch stats 的 `member_id` 只接受 User UUID；显式传入数字或其他无效值返回 400，不再按用户顺序映射。Library/Filter/Media ID 在 Web adapter 中始终保留 UUID 字符串，不作 `Number(...)` 转换。

### LibraryFacets

`GET /libraries/{id}/facets`（筛选面板的唯一数据源；前端 `web/lib/api/libraries.ts` 的 `LibraryFacets`）：

`{total, genres, countries, decades, watch, ratings, runtimes, languages, resolutions, hdr, stock}`，每项 `{value, label, count}`。

- **计数**：维度 D 的值 v 的 `count` = 满足「**除 D 以外**全部已选条件」且 D 取 v 的 **distinct Media** 数；`total` = 满足全部条件的 distinct Media 数（不是 ledger 行数）。候选全集来自该库全部可见 Media，含 `count: 0`（前端置灰）。
- **查询键**：items/facets 共用 `browse_filter_from_query`。多值维度用逗号分隔，只接受短键 `g`、`c`、`d`（`2020s|2010s|2000s|1990s|earlier`）、`rt`（`lte60|60to90|90to120|gt120`）、`lang`、`res`、`stock`（`missing|unscraped`），维内 OR、维间 AND。`w` 是单值 `unwatched|watching|played|favorite|seen`，`seen` 为存在真实观看证据的首页语义；`rating_gte` 接受有限 `0..=10` 数值（6/7/8/9 是 UI 档位），`hdr` 为 `true|false`，另有 `identity`。旧长键（genres/countries/decades/runtimes/languages/resolutions/watch）返回 400。旧 watch 值 `watched/unplayed/unseen` 返回 400；排序只使用当前 sort + 独立 order，`release_date_asc` 及其他未知 sort 返回 400，见复审 R9（`git show 5e44f101:docs/audits/2026-10-28-storage-main-rereview.md`）。不能把 UI 预设档位当作 handler 全部接受范围（`crates/api/src/http/library/query.rs`）。
- **`tier`**：`primary` 只填 `genres/countries/decades/watch`，其余空数组；`all` 十维全填；**缺失或空串按 `all`**；其他非空值 → 400 `library.invalid`。
- 未知年份与 `>=2030` 不归入任何 `decades` 档。

### JobDef / Job

`JobDef` = `{id, kind, name, enabled, schedule?: {interval_secs} | null, payload, concurrency_key?, last_status?, last_finished_at?, next_run_after?}`

`Job` status = `queued|running|succeeded|failed|cancelled`。

### 维护任务（DB8）

维护任务是 `kind ∈ {database_maintenance, jobs_retention, playback_history_retention, data_retention}` 的普通 JobDef；
调度与开关仍走既有 JobDef 字段，**保留参数只存在该 def 的版本化 payload 里**（不在 settings 另存一份）。

`payload`（`version: 1`；未知版本/未知字段/越界一律 400，绝不回退成更激进的删除策略）：

- `database_maintenance`：`{version: 1, vacuum_enabled: bool}`（默认 `false`）。
- `jobs_retention` / `playback_history_retention`：`{version: 1, retention_days: 1..=36500, dry_run: bool}`。
  `retention_days=0` **不代表永久保留**；永久保留请关闭该任务。`batch_size=500`、`max_batches=20` 是服务端固定安全预算，不暴露给用户。
- `data_retention`（DB-05）：`{version: 1, dry_run: bool, qoe_days: 1..=36500, transient_cache_days: 1..=36500, probe_days: 1..=36500, operations_days: 1..=36500}`。
  首次注册 `enabled=false`、`dry_run=true`、历史窗口默认 90 天、`operations_days` 默认 30 天；生产启用需要用户显式确认。
  `operations_days` 可省略（旧 payload 继续可用，缺省即 30 天）。QoE 只删 `playback_metrics`；
  catalog 只删 URL path 白名单内的 search/discover/trending 临时缓存（详情与未知 key 保护）；Probe 只删终态 job，
  每个 scope 至少保留最新 3 条（时间并列全部保留），被 sample 引用或仍有活动 unit 的 job 保护。samples/models/model_members 本轮不删。
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
- 原 F02 与 F04 的实现现状须区分：queued 刷新与配置同事务路径已经存在，但仅周期冲突检测仍缺少令牌覆盖。历史复核与执行计划不等于本次重新验收；当前存储切换任务见 复审与剩余方案（`git show 5e44f101:docs/audits/2026-10-28-storage-head-review.md`）。

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

2026-10-13 的 总复核（`git show 5e44f101:docs/audits/2026-10-13-database-api-review.md`） 保留为历史缺陷证据；不能将其所有“当前缺陷”直接搬到现行合同，也不能仅据后续代码存在就宣布所有回归通过。DB8 的 SQL 硬预算、VACUUM best-effort 边界及仅周期冲突检测仍须分别核对。

## 当前存储切换边界（2026-10-28核对）

四个Store文件为app/cache/library/subscribe，队列另在jobs；无旧数据库迁移阶梯或兼容视图。旧LedgerRow、重新识别补当前版本、movie `(0,0)` alias等已移除。Jellyfin协议与公开静态图片保留独立合同。

**Transfer/Watch：** `GET /settings/transfer` 返回 `{revision,transfer_mode,scrape,watch_intake,watch_inplace}`；`PATCH` 必填 `expected_revision`，其他字段可选。Watch 缺失=不改、null/空白=清空、值=替换；transfer_mode/scrape 不接受 null；未知字段或坏类型 400，过期 revision 409，均不写。提交仅在 app 单事务修改目标设置，不写 Library 根或 Naming。旧 `/directory` 与 `/directory/roots` 写路由删除，不转发。

**Library 根：** `PATCH /libraries/{id}` 的 root_paths/display_locale 返回 202 operation，按 `/libraries/{id}/operations/{operation_id}` 查询至 state=succeeded；新增嵌套根的归属收敛状态在 Library DTO 的 root_reassignments 可查询（operation_id/state/revision/config_revision/error）。Naming 仅走 `/settings/scrape` 当前 setting 的 naming_entry_dir/naming_movie_file/naming_season_dir/naming_episode_file 四段字段。

**Playback 事件：** 自有 `POST /playback/progress` 必须提交签发的 play_session_id、非负 event_sequence、device_id 和 start/progress/stop event；缺失、空白、伪造或跨用户/设备绑定 400 且不写播放事实。签发绑定固定 target/file/revision/source/config/generation；已签发但过期事件按 IgnoredStale 合同处理，不能补当前源身份。`POST /playback/sessions` 签发失败返回500；成功回执的 play_session_id 必填，流地址仅由签发绑定生成，保留合法STRM直接URL。Web 显式签发后上报，Jellyfin adapter 按协议建立绑定后复用 Store 提交。

**Unidentified/Downloader：** Unidentified 列表从登记的 library_id/root_id 返回并在 SQL 按 library_id 过滤，不按路径猜 Movie/TV。Claim 的独立 unidentified_id/source revision 在文件发布事务内 CAS 消费（删除再停放不会复用身份）；明确 IdentifyAndTransfer 允许改变最终 kind/Library。默认 Downloader 仅从当前 downloaders 行及明确支持的环境覆盖选择，旧 settings JSON 不再读取。修复前反例保留于固定版本复审（`git show 5e44f101:docs/audits/2026-10-24-storage-rereview.md`），当前验证见再次复审（`git show 5e44f101:docs/audits/2026-10-28-storage-main-rereview.md`）。

**Subscribe唯一请求字段：** 创建/PATCH使用 `coverage` 和 `filter_id`；`POST /subscriptions/{id}/upgrade-runs` body为 `{filter_id?, wash_cut?}`，回执也返回 `filter_id`。本轮已核对实际Web调用者和create严格拒旧，原F01生产问题关闭；Rust共享fixture已使用当前字段；不恢复旧inline filter兼容。

**Playback marks目标：** 字段为 `season_number` / `episode_number`，支持整项、整季与合法TV S00E00。GET/POST严格拒未知/旧键、重复坐标与坏类型；坐标按0..2147483647 checked conversion，越界400，不饱和或截断。坏请求不写playback_units；Web已移除无用途的device_id。合法whole/整季/S00E00继续支持。

成功信封只使用 `{ok:true,data}`。实际Web HTTP、catalog/health及消费data回执的组件已切换；旧success、裸数组/对象与缺data被拒。明确204无body单独处理，保留合法业务空值。

普通协议分页使用写入期 genre/person 精确成员、分类计数与页内选源。精确 total 与 offset 的新模型预算及实测状态见2026-10-25 验收记录（`git show 5e44f101:docs/benchmarks/2026-10-25-large-hit-query-read-model.md`）；旧实现的增长曲线保留在历史性能报告（`git show 5e44f101:docs/benchmarks/2026-10-23-storage-redesign-report.md`）和2026-10-24 测量（`git show 5e44f101:docs/benchmarks/2026-10-24-storage-query-rereview.md`）。

## 现行表面（去哪看）

| 面 | 挂载 | 说明 |
|---|---|---|
| 自有 REST | `/api/v1` | 唯一自有业务与管理契约。member / admin 在 `http::router` 分层。 |
| Jellyfin / Emby | `/` 与 `/emby` | 专供 Infuse / VidHub 等播放器直连，严格遵守 Jellyfin 官方开放协议。 |

根路径上的历史遗留管理路径（`/subscribes` `/filters` `/search` 等）已彻底移除并返回 404；自有管理面完全统一在 `/api/v1`。

完整 method/path 以 `crates/api/src/http/mod.rs` 的 `router_with_slow_after()` 与 `crates/api/src/http/routes/{mod,member,admin}.rs` 的 `.route(...)` 注册为准（2026-10-21 由 818 行的 `http/mod.rs` 拆分而来，路由字符串与 merge 顺序未变）。

前端 `web/lib/api/*` 的路径相对同源 `/api/v1`。改密走 `PATCH /users/{id}` 的 `password`，没有 `POST /users/{id}/reset-password`。

## 明确不做

这些不是「暂时 stub」，是产品边界。不要为它们补端点或前端入口。

- AI：`/agent*` `/llm/*` `/mcp/*` `/skills`、对话 session
- 插件市场 / 浏览器扩展：`/extension/*`（issue #129 挂起）
- 转码 / 字幕生成 / PGS OCR
- IM 通道：微信 / Telegram / Discord / 上游 webhook。通知只走 `GET|PUT /notify/config` → `jianxcao/notify`
- OTA：`/app/update/*`
- Photos 模块

历史上的上游兼容层（`webapi/` 信封、`numeric_id`）已删除，不要加回来。
