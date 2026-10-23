# Storage API Cutover Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 所有已持久文件的请求读取使用显式 Library 归属与有界 SQL，并使自有 API/Web/CLI 只消费一份真实契约。

**Architecture:** 查询先授权、SQL 内过滤/排序/分页，分页后才批量补文件或冷数据；不在 Rust 全库 JOIN。前后端 DTO 同提交切换，不接受旧输入别名、不伪造已删除端点的数据；保留标准 Jellyfin 行为及现有不同业务用例。

**Tech Stack:** Rust/Axum/rusqlite、React/TypeScript/Vite、Node≥22；不引入新框架。

**Spec:** [主规范](<../specs/2026-10-18-library-storage-redesign.md>) §§4.2–4.6/6；[自有 API 约定](<../../api-contracts/self.md>)；[核验与撤回清单](<../../audits/2026-10-23-storage-committed-head-verification.md>)。基线 `572d4fa8870271033068b9ede0cea71ecbdf922b` vs main `86122d73a4a23439e1d83234590fdd98b388fc50`；**仅计划，未实施，不读取未提交修改作为基线。**

## Global Constraints

- “无旧数据导入、兼容视图、双读双写、旧版本回退。”不新增客户端重试旧路由/旧字段的 fallback。
- “禁止请求时全库路径归属判断、文件 stat、NFO 解析和外部网络访问。”这里约束 Library 浏览/详情；明确的搜索、Scrape、claim 命令可按自身用例做锁外 IO。
- “未看查询必须包含缺失行”；授权/精确总数/去重/过滤在 LIMIT 前完成，不做分页后过滤补页。
- movie/video 存储 whole=`(-1,-1)`、TV S00E00合法、未知 units=[]；文件首坐标只作便捷字段，不恢复覆盖。
- 自有成功信封 `{"ok":true,"data":...}`；错误保留 HTTP 4xx/5xx。204等明确无body接口不套虚假数据；协议层不改成自有信封。
- 静态图片保持public，播放流保持鉴权；不得用“移除兼容”删除 Jellyfin 标准参数/无ID但严格匹配的活动会话支持。
- 每触及crate独立 `cargo test -p <crate>`；最终稳定树 `cargo test --workspace`。Web `pnpm --dir web test`/`typecheck`/`build`，Node≥22；无真实网络。
- 超800行文件、超120行函数按实际职责拆分；本轮只新增文档，不执行这些代码任务。

## Review Focus

1. 授权库与隐藏库共用 Media：列表/收藏去重总数正确，隐藏库文件绝不被选源（B1/B2）。
2. missing/unknown/pending：管理可见性不能因统一 scoped helper 被误缩为可播放集合（B1）。
3. 用户没有稀疏摘要时仍未看；分集收藏不能因 whole-item 布尔筛选被丢掉（B2）。
4. preview 后替换文件或更改身份：重识别409且事实零改变，不靠服务器补当前版本（B3）。
5. 已有 Media 根内认领与新识别 Transfer、空/失效下载关联：能力保留、作用域明确，不能以假成功/全局任务冒充 Subscribe 数据（B4）。

---

## 文件责任与任务依赖

| 任务 | 边界 | 依赖与产出 |
|---|---|---|
| B1 | ledger定向读、显式归属、visible scope及所有单Media消费者 | 独立；为B2提供统一 FileReadScope |
| B2 | favorites/up_next/gallery 的 SQL 选择器 | 依B1和运行时A4/A5；输出正确分页逻辑目标 |
| B3 | source别名、played字段、reidentify版本、唯一自有参数/信封 | 依B1，Web/后端同任务发布 |
| B4 | claim两用例统一、sites/subscriptions真实DTO | 独立；保留目标选择/Transfer能力 |
| B5 | 强类型merge持久计划 | 独立；移除缺frozen旧格式回退 |

B1修改 [ledger.rs](<../../../crates/store/src/ledger.rs>)；B2新 Store 查询模块，不把更多逻辑塞进 [http/playback.rs](<../../../crates/api/src/http/playback.rs>)。B3用真实 [libraries.ts](<../../../web/lib/api/libraries.ts>)，不创建不存在的单数 API 模块。B4不删除整套 UI 默认值政策，只删除明确历史格式/假端点适配。

### Task B1: 有界文件读取与持久 ownership（C14）

**Files:** Modify [ledger.rs](<../../../crates/store/src/ledger.rs>)、[browse_reads.rs](<../../../crates/store/src/browse_reads.rs>)、[visibility.rs](<../../../crates/api/src/http/library/visibility.rs>)、[assets.rs](<../../../crates/api/src/http/library/assets.rs>)、[visible_scope.rs](<../../../crates/api/src/visible_scope.rs>)；Modify consumers [playback.rs](<../../../crates/api/src/http/playback.rs>)、[reidentify.rs](<../../../crates/api/src/http/reidentify.rs>)、[library_artwork.rs](<../../../crates/api/src/http/library_artwork.rs>)、[library_chapters.rs](<../../../crates/api/src/http/library_chapters.rs>)、[library/probe.rs](<../../../crates/api/src/http/library/probe.rs>)、[media.rs](<../../../crates/api/src/http/media.rs>)、[title_ref.rs](<../../../crates/api/src/http/title_ref.rs>)、[subscription_depth.rs](<../../../crates/api/src/http/subscription_depth.rs>)、[arrivals.rs](<../../../crates/api/src/http/subscription_depth/arrivals.rs>)、[subscription snapshot](<../../../crates/api/src/http/subscriptions/snapshot.rs>)、[collections.rs](<../../../crates/api/src/http/collections.rs>)、[search.rs](<../../../crates/api/src/http/search.rs>)、[playback_logs.rs](<../../../crates/api/src/http/playback_logs.rs>)、[library_admin.rs](<../../../crates/api/src/http/library_admin.rs>)；Test [browse_reads.rs](<../../../crates/store/tests/browse_reads.rs>)、[library_browse_rows.rs](<../../../crates/api/tests/library_browse_rows.rs>)、[storage_work_bounds.rs](<../../../crates/store/tests/storage_work_bounds.rs>)。

**Interfaces:** 新 `FileReadScope = Playable | Published | AllRecorded`；`Store::file_coverage_in_library(&self, library:LibraryId, media:Option<MediaId>, scope:FileReadScope)->Result<Vec<FileCoverage>,StoreError>`。Playable=published+present；Published=published含missing/unknown；AllRecorded还含pending。后台全库维护仍可显式使用 list_file_coverage，不删除其独立合法用途。大库管理列表使用现有 keyset page，不能用此Vec方法返回整库后自行分页。

- [ ] **Step 1 — 写失败测试** `scoped_coverage_keeps_presence_and_publication_semantics`：同 Media 两库，各放present/missing/unknown/pending和多集文件；授权A、隐藏B。核对三scope精确行集、多集只一个文件、未知覆盖不变、selected source不来自B。根配置变化但文件仍持久属于A时，读取不能悄悄归默认B。
  ```rust
  assert_eq!(published_ids, vec![present_id, missing_id, unknown_id]); // 排序后比较
  assert_eq!(playable_ids, vec![present_id]);
  assert!(!visible_source_ids.contains(&hidden_file));
  assert_eq!(multi_file.units.len(), 2);
  ```
- [ ] **Step 2 — RED**：`cargo test -p store --test browse_reads`；`cargo test -p api --test library_browse_rows`，新增作用域/默认库反推用例失败。
- [ ] **Step 3 — 实现**：scope谓词放SQL，复用已存在 media(_batch) helper；授权库传入查询，不先全量读后filter。`library_for_file` 按 file.library_id 点查库，不用路径/default；写入前目标路由仍用现有严格root解析。VisibleScope从授权library_items查DISTINCT media_id。逐消费者登记旧返回语义后替换，不把管理页换成present-only；request失败传播，禁止unwrap_or_default伪空成功。清除 browse_reads 中构造后未使用的 root predicates 及专测死代码，不删合法路径目标选择器。
- [ ] **Step 4 — GREEN**：上述targets、`cargo test -p store --test ownership_snapshot --test storage_work_bounds`、`cargo test -p api --test storage_http_cutover`、store/api独立套件。用现有工作量仪器记录单Media请求不随无关文件数增长；这属于性能测量，不把私有SQL字符串断言塞进行为测试。
- [ ] **Step 5 — Commit**：可按消费者模块拆独立已绿提交，消息 `refactor(storage): scope request reads to persisted library ownership`；同一helper合同只定义一次。

### Task B2: SQL 选择观看视图，不扫全库文件（C14及旧热路径问题）

**Files:** Create `crates/store/src/library_query/playback_views.rs`；Modify [library_query/mod.rs](<../../../crates/store/src/library_query/mod.rs>)、[up_next.rs](<../../../crates/api/src/http/playback_views/up_next.rs>)、[favorites.rs](<../../../crates/api/src/http/playback_views/favorites.rs>)、[gallery.rs](<../../../crates/api/src/http/playback_views/gallery.rs>)；Test [up_next_local_first.rs](<../../../crates/api/tests/up_next_local_first.rs>)；Create `crates/store/tests/playback_view_pages.rs`。

**Interfaces:** 新 `PlaybackViewQuery { user:UserId,libraries:Vec<LibraryId>,limit:usize,offset:usize }`、`PlaybackViewRow { media_id:MediaId,library_id:LibraryId,unit:UnitKey,favorite_level:Option<String>,last_played_at:i64 }`、`PlaybackViewPage { items:Vec<PlaybackViewRow>,total:usize }`；`Store::playback_favorite_page(&self,&PlaybackViewQuery)->Result<PlaybackViewPage,StoreError>`、`Store::playback_up_next_page(&self,&PlaybackViewQuery)->Result<PlaybackViewPage,StoreError>`。返回的unit必须真实覆盖；API在同读快照/当前CAS边界按页批量选源，不用旧位置决定新源位置。

- [ ] **Step 1 — 写失败测试**：有进度/整项收藏/分集收藏/无摘要/全已看/仅隐藏库同Media；同Media跨两个可见库。断言收藏层级/分页total/稳定顺序、missing无可播放源处理、NextUp不进入exclude_from_home库；源变化start=0，原源仍可用则优先它。
  ```rust
  assert_eq!(all_pages.len(), page.total);
  assert_eq!(episode_favorite.favorite_level.as_deref(), Some("episode"));
  assert_eq!(changed_source.start_ms, 0);
  assert!(unseen_ids.contains(&no_summary_media)); // 浏览未看合同不回归
  ```
- [ ] **Step 2 — RED**：`cargo test -p store --test playback_view_pages`；`cargo test -p api --test up_next_local_first`。对性能另记录旧请求工作量，不用仅“现在能查到相同数据”宣称热路径已切换。
- [ ] **Step 3 — 实现**：当前用户/授权库 user_library_items驱动候选，items/units索引连接，必要时对候选Media按playback_units补分集favorite，不把whole favorite布尔当所有收藏唯一事实；跨库逻辑去重在LIMIT前、total同谓词。未知摘要默认未看；不能用INNER JOIN丢掉缺行。NextUp/continue选择使用摘要已保存resume/next坐标，物理恢复仍经 resolve_playback；只对最终页批量读取覆盖与冷字段。拆up_next的候选选择、DTO渲染、外部补全职责，保持≤120行/函数。
- [ ] **Step 4 — GREEN**：新target、`cargo test -p store --test playback_scope --test playback_resume_v1`、api local-first target、store/api独立套件；无catalog可用时仍返回本地NextUp。A4/A5的状态/时长用例在真实API也生效。
- [ ] **Step 5 — Commit**：`git commit -m "perf(playback): select authorized view pages from sparse summaries"`。

### Task B3: 唯一自有契约与客户端版本固定（C15/C16）

**Files:** Modify [reidentify.rs](<../../../crates/api/src/http/reidentify.rs>)、[library.rs](<../../../crates/api/src/http/library.rs>)、[playback/episodes.rs](<../../../crates/api/src/http/playback/episodes.rs>)、[library/query.rs](<../../../crates/api/src/http/library/query.rs>)、[system_logs.rs](<../../../crates/api/src/http/system_logs.rs>)、[search/history.rs](<../../../crates/api/src/http/search/history.rs>)、[media/mod.rs](<../../../crates/store/src/media/mod.rs>)；Modify [libraries.ts](<../../../web/lib/api/libraries.ts>)、[library-filter.ts](<../../../web/lib/library-filter.ts>)、[logs.ts](<../../../web/lib/api/logs.ts>)、[catalog-cache.ts](<../../../web/lib/api/catalog-cache.ts>)、[health.ts](<../../../web/lib/api/health.ts>)、[ui.ts](<../../../web/lib/api/ui.ts>)、[subscriptions.ts](<../../../web/lib/api/subscriptions.ts>)、[sites.ts](<../../../web/lib/api/sites.ts>) 与真实导入这些变更函数的组件；Test [reidentify_scoped.rs](<../../../crates/api/tests/reidentify_scoped.rs>)、[media_lookup_indexes.rs](<../../../crates/store/tests/media_lookup_indexes.rs>)、[library-dto-contract.test.mjs](<../../../web/test/library-dto-contract.test.mjs>)；Create `web/test/reidentify-version-contract.test.mjs`。

**Interfaces:** 新唯一请求 `ReidentifyRequest { media_item_id:MediaId,source_revision:i64,title_ref:String,files:Vec<ReidentifyFile> }`，`ReidentifyFile { file_id:LedgerId,revision:i64,source_version:String }`；files非空、无重复、全属同source Media且版本合法。preview增加相同结构的files与source_revision；Web `assignLibraryFilesToTitle(request:ReidentifyRequest):Promise<{claimed:number}>`。删除单独file_ids输入，不同时接受两套列表。source查询仅认 tmdb/douban/tvdb/bangumi/anilist；保留 DTO 的 tmdb_id 等合法展示字段。

- [ ] **Step 1 — 写失败测试**：preview→替换文件→POST返回409且所有文件归属不变；缺files/source_revision、重复/无效ID、空source_version/非正revision返回400；混source或版本不符拒绝。前端请求exact shape；回包无watched；旧裸数组信封/旧success信封被拒绝而非空成功。
  ```rust
  assert_eq!(stale.status(), StatusCode::CONFLICT);
  assert_eq!(after_files, before_files);
  assert!(episode_json.get("watched").is_none());
  ```
  ```js
  assert.deepEqual(JSON.parse(captured.body), expectedPinnedRequest);
  await assert.rejects(readCurrentResponse({ success: true, data: [] }), /contract/i);
  ```
  readCurrentResponse 是本测试的传输fixture调用，不要求生产导出同名函数。
- [ ] **Step 2 — RED**：`cargo test -p api --test reidentify_scoped`；`cargo test -p store --test media_lookup_indexes`；`pnpm --dir web exec node --test test/reidentify-version-contract.test.mjs test/library-dto-contract.test.mjs`。更新已有“只传file_ids成功”的错误合同测试，不保留兼容测试。
- [ ] **Step 3 — 实现**：Typed serde请求强制字段，preview在B1授权scope内提供版本；POST只用请求版CAS，不回填当前版。全部调用者同步改；`get_media_by_alias_kind` 参数改名source并拒绝*_id旧词，批量迁caller/test。分集仅played。短filter键g/c/d/w/rt/lang/res保留为当前唯一合同，删长名同义分支并同步文档；日志唯一target/q、Subscribe唯一filter_id/coverage，删除selected_seasons/rule_set_id兼容。搜索快照只读当前items/keyword键，旧schema本次不导入。Web各模块统一ok/data信封，去phase/state/job id多形态fallback；合法可空业务字段与UI默认值保留，不把所有 `??` 都视为兼容。请求不认识的旧字段应明确400，不静默忽略旧输入继续成功。
- [ ] **Step 4 — GREEN**：上述测试、`pnpm --dir web test`、`pnpm --dir web typecheck`、`pnpm --dir web build`，store/api独立测试。核对Web、CLI、自有API调用，不变更Jellyfin参数或无ID匹配政策；静态图片不被误移受保护路由。
- [ ] **Step 5 — Commit**：前后端一起提交 `git commit -m "refactor(api)!: pin reidentification and remove obsolete self-api aliases"`；更新[API文档](<../../api-contracts/self.md>)实际DTO，无双版本过渡部署方案。

### Task B4: claim 保全用例，删除假桩/虚构回执（C17及需设计项）

**Files:** Modify [claim.rs](<../../../crates/api/src/claim.rs>)、[library_admin.rs](<../../../crates/api/src/http/library_admin.rs>)、[routes/admin.rs](<../../../crates/api/src/http/routes/admin.rs>)、[api/cli/actions.rs](<../../../crates/api/src/cli/actions.rs>)；Modify [libraries.ts](<../../../web/lib/api/libraries.ts>)、[sites.ts](<../../../web/lib/api/sites.ts>)、[subscriptions.ts](<../../../web/lib/api/subscriptions.ts>)、[site-config-section.tsx](<../../../web/components/site-config-section.tsx>)、[subscription-inspector-view.tsx](<../../../web/components/subscription-inspector-view.tsx>)；Test [management/unidentified.rs](<../../../crates/api/tests/management/unidentified.rs>) 认领用例、[library-dto-contract.test.mjs](<../../../web/test/library-dto-contract.test.mjs>)；Create `web/test/subscription-receipts.test.mjs`。

**Interfaces:** 目标契约选择 `POST /unidentified/claim`，唯一body为 `ClaimBody { path:String,action:ClaimAction }`，serde action tag=`bind_existing | identify_and_transfer`：前者携 `media_id:MediaId`、只根内绑定；后者携现有 `title,kind,year?,tmdb_id?`、走现有identify/Transfer流程。两个用例不强制共享磁盘执行算法，只共享入口判别；Web/CLI同改后删除 `/unidentified/{id}/claim`，未知action/旧shape明确400。

- [ ] **Step 1 — 写失败测试**：已识别Media根内文件bind不移动字节，根外bind拒绝；新识别Transfer按Naming template目标发布并生成Media；旧body/旧route不接受。删除/cleanup Web直接消费服务端queued/deleted回执，不期待cleanup_job_id。下载任务以wanted info_hash关联，空hash不展示其他任务。
  ```rust
  assert_eq!(bind_after_path, bind_before_path);
  assert_eq!(outside_bind.status(), StatusCode::BAD_REQUEST);
  assert_eq!(obsolete_route.status(), StatusCode::NOT_FOUND);
  ```
  ```js
  assert.equal("cleanup_job_id" in receipt, false);
  assert.deepEqual(tasksForEmptyWanted, []);
  assert.deepEqual(shownHashes, expectedWantedHashes);
  ```
- [ ] **Step 2 — RED**：`cargo test -p api --test management unidentified`；`pnpm --dir web exec node --test test/subscription-receipts.test.mjs`。先固定两个业务用例，不以删除路由404测试替代产品能力测试。
- [ ] **Step 3 — 实现**：新action分发到两个已有用例，移除raw path/id混用推断和旧路由，CLI/组件只发新契约。删除无调用no-op站点函数/import；空sync stats移除对应伪UI，不新建不存在的产品端点。下载adapter改为明确的全局DownloaderTask DTO，在inspector只按其Subscribe的真实wanted hashes定向关联；不再以void id伪装作用域查询。mutation不伪造wanted=[]详情，直接用返回的Subscribe，确需wanted则显式刷新现有detail接口。移除永远reject的手工片源标注adapter与无功能dialog调用，保留已在用的正常物理源选择。
- [ ] **Step 4 — GREEN**：api管理测试（CLI同属api crate）、Web test/typecheck/build、api独立测试；新增失败断言确保错误不被.then伪造成成功对象。核查旧导出没有真实组件依赖后删除，不能凭名字为similar功能误删UI。
- [ ] **Step 5 — Commit**：按claim唯一入口与假桩清理分别独立提交，`refactor(api)!: make unidentified claim actions explicit` / `refactor(web): consume real receipts and downloader task scope`。

### Task B5: 去除旧 merge plan JSON 回退（C20）

**Files:** Modify [preflight.rs:19–76](<../../../crates/store/src/media_operations/preflight.rs#L19-L76>)、[apply.rs:331–337](<../../../crates/store/src/media_operations/apply.rs#L331-L337>)；Test [media_merge_recovery.rs](<../../../crates/store/tests/media_merge_recovery.rs>)、[media_merge_conflicts.rs](<../../../crates/store/tests/media_merge_conflicts.rs>)。

**Interfaces:** 私有 `MergePlan { #[serde(flatten)] request:MediaMergeRequest, frozen:FrozenPair }`，frozen字段required，两个before状态required且只允许可恢复的provisional/confirmed值；保留当前flat request键避免改变合法json_extract查询，不保留缺frozen解码。`frozen_of_plan`可改为解码整个MergePlan后的访问，不返回default空状态。

- [ ] **Step 1 — 写失败测试**：正常begin生成强类型plan，重启继续相同操作成功；删除frozen或survivor_before、填非法状态时advance返回Invalid且保持冻结、不移动文件、不伪报succeeded。同request幂等比较仍只比较request，不把服务端frozen纳入用户幂等payload。
  ```rust
  assert!(malformed_plan_result.is_err());
  assert_eq!(identity_after_error, "merging");
  assert_eq!(files_after_error, files_before_error);
  assert_eq!(repeated_operation_id, first_operation_id);
  ```
- [ ] **Step 2 — RED**：`cargo test -p store --test media_merge_recovery --test media_merge_conflicts`，新增缺frozen负向用例旧实现接纳default而失败；原正常恢复用例保持非回归。
- [ ] **Step 3 — 实现**：删除serde defaults及None→FrozenPair::default，空状态不再作为控制条件；编码/解码/重放共用MergePlan。坏新布局数据报存储错误、保留操作供修复，不添加“读取旧计划后补字段”的兼容路径；不改规范要求的merge played OR三元组。
- [ ] **Step 4 — GREEN**：上述targets与 `cargo test -p store`；正常每阶段重启/幂等与失败冻结合同全部保留。
- [ ] **Step 5 — Commit**：`git commit -m "refactor(storage)!: require frozen identity state in merge plans"`。

## 自查与完成定义

- C14–C17/C20均有任务；B4明确解决两个claim用例，而不是将needs-design误报当重复端点删除。B3保留正式短键、便捷首坐标和Jellyfin协议；清理的是多契约fallback。
- B1读取三scope显式区分；B2逻辑分页和源解析职责分开，Favorite层级不被单一whole布尔吞并；B3一个版本字段名revision贯穿preview/Web/POST。
- Review Focus分别由B1/B2/B3/B4行为测试覆盖。新文件/类型是目标设计，基线中不存在不能声称已调用。
- 最终实施记录：crate独立测试、Web门禁、最终workspace结果、扫描剩余全库调用的合法用途清单；本轮未授权执行，上述复选框保持未勾选。
