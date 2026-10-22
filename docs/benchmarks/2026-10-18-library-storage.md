# 存储重设计：检查、烟测与查询工作量证据

更新：2026-10-21，最新源码基线 `f815b21` 加本轮第二批修复（`crates/api` 5 个文件，见第 7 节），烟测基线为更早的 `70233ed` / `6abf493`。**存储干净切换的实现已落地并在本机跑过 `cargo test --workspace`、前端三门与端到端冒烟（第 7 节）；但真机 NAS、真实 Jellyfin 客户端、真实浏览器播放、10 万–100 万规模性能全部未跑（7.6），因此没有端到端验收与性能达标结论。** 历史与当前记录按节分开，不把表/索引存在或测试通过扩大为端到端验收。

## 1. 版本与检查边界

本节至第3节保留 `0a046e0` / `a3dab1b` 的历史记录；“本次/当前”只指当时取证，不是现在源码。后续测试记载见1.2/1.3，`bd6c3c8`复审见第6节。历史失败不能推导新版本仍失败，历史通过也不能关闭未覆盖的生产合同。

| 检查 | 版本 | 实际记录 | 结论边界 |
|---|---|---|---|
| Store 全套 | `0a046e0` | 57个套件，291通过、1 ignored | 仅该次 Store 检查；不是生产路由/协议全通过 |
| API 定向检查 | `0a046e0` | 4个套件，6通过 | 仅所选目标，不等于 API 全套或完整产品链 |
| web typecheck | `0a046e0` | 通过 | 不代替前端test/build、浏览器或播放验收 |
| workspace | `0a046e0` | 失败：API 单元测试153通过、3项扫描 TOCTOU 失败 | 该次 workspace 未全绿；停止位置后的结果不能推断 |
| checkpoint 失效修复后的必要检查 | `a3dab1b` | **尚未重跑** | 不能沿用旧失败下当前结论，也不能宣称修复已验证 |

原报告中的 Store 7通过/22失败及 `atomic_try_update` 阻断属于更早状态，不能作为当前树结论；无证据支持沿用原来的逐crate“全部通过”清单。

### 1.1 本次文档核对的真实结构烟测（`a3dab1b`）

调用已编译的 `Store::open` 与 `jobs::Queue::open`，在独立临时目录创建五个文件，再读取实际SQLite结构。69张业务表、675字段与[五文件字典](../database/README.md)一致；四个Store文件的application_id分别为1129139248/1129139249/1129139250/1129139251，user_version均为1；Jobs标识/版本均为0。每库 `integrity_check=ok`，`foreign_key_check` 无结果行。Store→Queue顺序的cache有12个命名索引，未运行media后开的变体。

这次只证明真实初始化与字典计数一致，没有重跑checkpoint、播放、扫描、Transfer、Web或性能验收；不改变下节历史缺陷状态。

## 2. 前次真实行为烟测（`0a046e0`）

这些是前次运行观察，不是仅由DDL推导的风险；也不是对 `a3dab1b` 的重新验证。通过片段与失败均保留，不能选择性地把局部成功包装成端到端完成。

| 入口 / 场景 | 实际观察 | 尚需闭合的行为 |
|---|---|---|
| 真实 API：会话→视频流 | 会话请求200；对磁盘上存在的文件请求stream返回404 | 对齐逻辑目标/物理源解析、权限与实际打开路径，最终以协议Range请求验收 |
| 真实 API：观看分面 | 库中有未看项目，watch facets仍为零 | 无稀疏用户行也须计入未看，page/count/facets共用正确谓词 |
| Store：文件续页 | 60个文件得到首50项及cursor | 证明该小规模续页形状可用，不证明所有分页或规模门达标 |
| Store：删除/重导入后的收藏 | 原始favorite仍为true，但favorite过滤count为0 | 重建用户摘要必须依据原始事实，不能丢筛选可见性 |
| Store：多集Transfer | E02–E03文件发布后丢失E03覆盖 | 生产写链完整传递多集coverage，移除单集适配 |
| Store：无效NFO刷新 | 最后有效字段从Value变为Missing | 无效输入保留最后有效值，确认删除/显式清空才解除覆盖 |
| Store：来源采用 | 先TMDB后Douban，后者覆盖前者 | 按固定来源优先级合成，不按写入先后决胜 |
| Store：self-merge | 返回失败前已删除metadata/playback | 预检与事务原子性：拒绝动作不得先破坏事实 |
| Store：多单元文件所在Library删除 | 返回Protected | 删除范围/快照须无损表达全部覆盖，不以有损台账形状阻断合法删除 |

## 3. bundled SQLite 深游标工作量对照

前次在 Rust bundled SQLite 的合成100k行分页数据上，定位第50k行之后并请求51行（50项加续页探测）。两种SQL使用**各自独立的绑定参数布局**，没有把不同SQL的绑定位置混用。

| SQL形状 | 返回行数 | VmStep | FullscanStep | Sort |
|---|---:|---:|---:|---:|
| 展开的OR复合seek | 51 | 300371 | 0 | 0 |
| 元组比较复合seek | 51 | 486 | 0 | 0 |

这组结果说明：`FullscanStep=0`、`Sort=0` 或EXPLAIN显示索引，仍不能证明深游标工作量有界；OR条件可在索引内遍历大量行。它是该SQL形状的引擎工作量对照，不是延迟、NAS吞吐或生产接口性能基准，不能推出 `<1ms`、p50/p95或所有排序已优化。

## 4. `bd6c3c8`布局与生产接线边界

五文件布局及字段/键/索引见[数据库文档](../database/README.md)，历史决策见[ADR-0008](../adr/0008-sqlite-system-of-record.md)。新 `library_files`/coverage/items 与 Store 事务已经存在，但仍有以下生产缺口：

- movie旧whole `(0,0)` 兼容与 `LedgerRow` 有损适配未退出；不是允许保留的兼容合同。（2026-10-21 复审更正：movie 整片持久 Unit 现为 `(-1,-1)`（`domain::UnitKey::WHOLE`，TV 的 S00/E00 是合法值），`(0,0)` 只是部分读／合并路径的行级容忍，不再是前后端请求别名；`LedgerRow` 确实仍未移除，583 处引用按 `FileCoverage`／逐用途 DTO 消费，`crates/store/src/ledger.rs:242,394,518` 只剩文档注释。紧邻下一条里 `protocol_item_page` 仍未成为统一生产入口的表述已被 `485e36f` 取代——浏览入口本轮已统一（`jellyfin_paged_metadata` 25、`jellyfin_sql_page` 9 全绿），但 `commit_playback_event` 的生产写入统一仍未验证，本行以外不动该节。）
- `adopt_metadata`已有部分生产Scrape调用，但经有损DTO重建来源且字段/读取不完整；`protocol_item_page`与`commit_playback_event`仍未成为统一生产入口。不能由方法或测试存在宣称完整协议SQL分页、字段采用、会话/历史原子提交已完成。
- 旧浏览投影链已删除，不代表上述生产调用方和恢复/删除合同已全部切换。

后续按[当前干净切换计划](../superpowers/plans/2026-10-22-storage-repair-remediation.md)补齐；有效需求保留于Spec/合同，10-18/10-19旧步骤由Git保留，不因旧步骤状态“完成”追认验收。

## 5. 未取得的完整验收

- **未完成**100k Media / 1M文件 / 1M Playback单元全规模验收；100k合成SQL行不等于完整应用数据规模。
- **未取得**真实NAS、冷热缓存、并发扫描+心跳的p50/p95、事务/锁等待、busy失败、WAL/索引体积、内存与吞吐报告。
- **未完成**空库真实API/CLI/Jellyfin/web全链路、重启/清cache/删除重导入，以及多集/换源/NFO/合并恢复的完整验收。
- 完整目标仍按[设计第8节](../superpowers/specs/2026-10-18-library-storage-redesign.md#8-规模预算增长与验收)执行。先修正确性与干净切换，再基于实际查询证据优化，不删减筛选、权限、精确count或Playback合同来制造达标。

## 1.2 本轮修复后的验证（`feat/storage-redesign`，2026-10-20）

提交范围：`2f59103`…`6d35343`（R1–R9）。命令在本机 debug profile 执行。

| 检查 | 结果 |
|---|---|
| `cargo test -p store` | 全部套件通过（含 `metadata_adoption`、`library_removal`、`browse_reads`、`playback_resume_v1`、`file_publication`、`probe_publication`） |
| `cargo test --workspace` | **通过**，含全部 crate 单元/集成测试与 doc-tests（单次约 523s） |
| `cargo test -p api --test jellyfin --test jellyfin_paged_metadata --test jellyfin_read_scale` | 63 / 24 / 3 通过 |
| `cargo test -p store --test adopted_filter_facts --test library_query_filters` | 3 / 8 通过 |
| `web: npm run typecheck` | 通过 |

当时记录的新增/改写回归与修复意图如下（不表示在所有生产入口成立，后续复审已发现反例）：

- self-merge 在任何事实变动前被拒绝（`media_merge_v1`）。
- Web 会话取流返回真实字节并支持 `Range`（`playback_read_regression`）。
- 多集覆盖经 Transfer 收尾后完整保留；pending 重试复用身份；TV 有季无集不写非法覆盖。
- 文件增删同事务重建受影响用户摘要；电影 whole 不再计入分集数。
- 自动续播优先原可用源、换源归零；电影 `(0,0)` 兼容读写与测试已删除。
- 观看分面计数正确；详情返回 `next_file_cursor`；`content_rating` 贯通到前端适配。
- Jellyfin 物理多版本折叠为同一逻辑条目，genre/studio/search/person 过滤读取采用元数据。
- 来源优先级（tmdb > douban > …）生效；无效/未知 NFO 保留最后有效输入。
- 多覆盖文件与 Unidentified 不再阻断 Library 删除；新增嵌套根会重归属既有文件并重建两侧摘要。

仍未取得：真实 NAS 冷热缓存、并发扫描+心跳的 p50/p95 与锁等待、100k Media / 1M 文件规模基准、真实 Jellyfin 客户端与浏览器播放验收。这些不因本轮测试通过而自动满足。

### 1.3 `bd6c3c8`提交所记编译与回归修复（历史）

`bd6c3c8`包含 `5ef1957` 之后当时工作树的修复；以下是原执行记录，不是本轮重跑结果。该工作树一度无法编译（api库19个错误），修复后记录：

| 检查 | 结果 |
|---|---|
| `cargo test -p store` | 通过（全部套件） |
| `cargo test -p api` | 通过（全部套件；`management` 573 passed / 0 failed） |
| `cargo test --workspace` | 通过（单次约 398s） |
| `web: npm run typecheck` | 通过 |
| `web: npm test` | 795 passed / 0 failed |
| `web: npm run build` | 通过 |

修复的确定性缺陷（均先由现有回归或新行为失败暴露）：

- `create_library` 丢失 `replace_roots`：新建 Library 不落根，路径归属全部失败。
- 扫描发布丢失封面发现；Movie 扫描未写 whole 覆盖，导致播放会话 404。
- 扫描/`insert_ledger` 不再读取 NFO sidecar：详情 overview/评分/语言为空。现由调用方在发布前构造 `NfoObservation`。
- TV 刷新重构删掉了每集 `write_nfo`：rename/refresh 后无自产 NFO。已恢复为“采用元数据 + 写盘”同时进行。
- `library_metadata_inputs.source_version` 写入文件版本而非 `nfo_v1`，Library 删除时 `library_roots` 外键失败。
- `reduce` 未拆 `details_json` 的 `{version, fields}` 包装；`MetadataFields` 缺 `serde(default)`；`library_item_facts` 从未初始化结果 map。
- movie `(0,0)` 续播读取未规范化到 `(-1,-1)`；`upsert_unit` 无物理源时逻辑标记被拒绝、进度声明未 clamp。
- 缓存 DDL 两份并存（`store/schema/v1/cache.sql` 已删、`media/response_cache/schema.sql` 残留且索引命名不同）；统一为 `media::response_cache::CACHE_SCHEMA_SQL` 后删除死文件。

以上来自当时本机debug profile，改动已包含在 `bd6c3c8`，不再称“尚未提交”。仍不覆盖设备/规模/真实客户端验收；下面审查使用独立公开行为烟测发现套件未钉住的缺陷。

## 6. `bd6c3c8` 干净切换复审

2026-10-20，对 `main@86122d7...HEAD@bd6c3c8` 只读review，未改业务代码。临时独立Cargo runner直接依赖仓库Store/API，Vite SSR加载实际前端API函数；外部网络用fixture/禁用Fetcher。以下不是完整workspace或浏览器/设备验证，临时脚手架已删除，没有修改用户实例。

| 用例 | 本轮实际结果 | 边界 |
|---|---|---|
| 多集文件E02 Web start | HTTP200，但ended_by_admin=true、position=0、play_count=0 | get_ledger代表覆盖与逻辑目标混用 |
| 已签发A会话→同file发布B→旧session进度 | HTTP200；恢复包记录revision2/source B/position8000 | 旧事件被写成新源进度，非推测 |
| 新增E03的增量发布 | 公共episode_count3，用户owned仍2/completed仍1 | 漏维护用户摘要 |
| 实际前端详情枚举101文件响应 | 只请求两页，返回100文件 | 续页next_cursor被错读 |
| 实际前端枚举恰好50文件 | 第二页空终页触发“文件分页 cursor 未推进” | 非浏览器截图证明 |
| TMDB runtime100/rating8.5标准化 | 两字段均Missing | normalizer缺映射 |
| adopted details含Tagline/R | Store详情两字段仍Missing | 冷payload未完整水合 |
| 有文件根/review/a改/review | FOREIGN KEY constraint failed | 旧根删除早于从属迁移 |
| Media改标题/多alias回写 | media=After、card=Before；alias数2→1 | 扁平旧shape有损写入 |
| scan清观察版本并登记新stamp | FileVersion前后仍revision2/source B | 只验证公开Store步骤；完整重扫调用链另经源码核对 |
| Jellyfin PlaybackInfo | MediaSources[0].Id为u1逻辑ID | 未输出f1物理版本 |
| integrity-checks/media merges | 真实Router均404 | 必需管理操作未生产接线 |

源码还确认movie(0,0)前端生产者+边界alias、LedgerRow认领丢episode_to、NFO已有路径提前continue、Jellyfin现场NFO/catalog、持久操作无完整恢复链。不能把这些源码发现写成已运行对应全部场景。

上一轮（2026-10-20）未重跑workspace、前端三门、真实浏览器播放、真实Jellyfin客户端、NAS并发或100k/1M规模；历史1.2/1.3通过保留，但不能抵消上述新反例。**本轮（2026-10-21）已补跑 `cargo test --workspace`、前端三门与本机端到端冒烟，实测见第 7 节；真机／规模项仍未跑，见 7.6。** 后续关闭条件/逐项回归见[当前计划](../superpowers/plans/2026-10-22-storage-repair-remediation.md)。

## 7. Task 17 整体门：干净切换的冻结、冒烟与测试证据（2026-10-21）

本节只记录实测。未运行项集中在 7.6，不得由本节通过推导为它们已验收。

### 7.1 冻结与版本

| 项 | 实测值 |
|---|---|
| 分支 | `feat/storage-redesign` |
| 切换代码合入点（会话前） | `6abf493 refactor(storage): reject legacy filter DSL and plaintext password rows` |
| 本轮第一批提交（已入库） | `485e36f`（jellyfin 分页前先选规范本地条目）、`b0250c7`（nfo 多实体观测与崩溃恢复导出）、`d384f5c`（collect/library 删除拆分到 800 行内）、`32ae21d`（可测量查询/争用/保留工作负载）、`70233ed`（遗留明文管理员行必须显式轮换）、`f815b21`（修正浏览排序对 `release_date_sort` 的列类型声明） |
| 本轮第二批修复（就是 7.3/7.9 验证的代码） | 5 个 `.rs`：`crates/api/src/http/reidentify.rs`、`crates/api/tests/management/{library_configuration,library_gaps,library_sidecar_cancellation,playback_movie_units}.rs`（244 插入 / 35 删除） |
| 代码指纹 | 所有 `.rs` 合并 diff 的 sha256 `ffe2b6706002629fe0b25f403b22b64897a1010dd05c3edc368b27c0f9684b29`，基线 `f815b219905d8965caf47de406289a1c80913697`；第二批提交后该 diff 为空 |
| 冒烟树（`70233ed`，干净树） | `git status --porcelain -uall` 为空；70 条 blob 清单与冻结树逐路径 `git rev-parse` 比对 mismatch 0；清单 sha256 `01a0367ea44ca6074ac27dc4533f43798fa34b0f922d1e519231970733a28fbc` |
| 冒烟二进制 | debug `crawler-media serve`，sha256 前缀 `9f43c1778964dfc0` |
| 冒烟根 / 端口 | `/tmp/crawler-smoke6.OlIbPT` / `127.0.0.1:18772`（`CRAWLER_MEDIA_BROWSER=false`，UI=`web/dist`） |
| 冒烟日志 | `/tmp/cmsmoke/phase6c.log`，`PHASE6C_EXIT=0` |

### 7.2 端到端冒烟 marker（全部为实际响应）

| 证据 | 实测 |
|---|---|
| 五文件布局 | `app.db` / `cache.db` / `library.db` / `subscribe.db` / `jobs.db` 建立 |
| 播放 Range | `range_0_1023=206 bytes=1024` |
| 显式 seek | `seek_1000_2000=206 bytes=1001` |
| 越界 Range | `range_past_eof=416` |
| TV 集流 | `tv_episode_stream=206 bytes=2048` |
| 令牌边界 | `stream_no_token=401`、`items_no_token=401`、`subtitle_no_token=401` |
| Jellyfin 分页 | Recursive 页返回规范 `w1_…` Series+Movie |
| Jellyfin 剧集 | `/Shows/w1_…/Episodes` → `u1_…_1_1`，count 1 |
| 未播计数 | Series `UnplayedItemCount 1` |
| 字幕索引规则 | 广告索引 `…/subtitles/51707560-6c36-40c1-abf6-0dac9723ed2b/2`（`srt`，`external:Smoke Show - S01E01 - Pilot.srt`）可服务；未广告索引 0 → 404（预期） |
| 字幕交付 | `subtitle_delivery=200 bytes=47`；VTT `200 bytes=55 head=WEBVTT` |
| 重启持久性 | `total 2 / played true / favorite true` |
| 清 cache 后身份 | `identity_preserved_cache_clear=True` |
| 重导入后身份 | `identity_preserved_reimport=True`、`favorite_preserved=True`、`played_after_reimport=True`、`range_after_reimport=206 bytes=1024` |
| CLI over HTTP | `CRAWLER_MEDIA_URL` + `CRAWLER_MEDIA_TOKEN` 输出 `Smoke Movie\tmovie` / `Smoke Show\ttv` |
| Web 静态 | `web_index=200 ctype=text/html` |
| 日志噪声 | `error_lines=16 warn_lines=41`，逐条核对均为环境降级：`media::cache` 无可用 metadata cache source（tmdb/bangumi）、`api::runtime_downloader` 生产环境拒绝内存假下载 |

### 7.3 测试门（冻结树实测，`cargo test`）

| 目标 | 实测 | 目标 | 实测 |
|---|---|---|---|
| `cargo check -p api --lib --tests` | 干净 | `store --lib` | 34 passed |
| `-p subscribe` | 89 passed / 18 targets | `store --test library_query_v1` | 5 passed |
| `-p store` | 337 passed / 67 targets / 0 failed | `store --test subscribe_library_scope` | 12 passed |
| `-p jobs` | 13 passed | `store --test data_retention` | 11 passed |
| `-p library` | 20 suites / 0 FAILED | `store --test playback_compatibility` | 5 passed |
| `api --test management`（整目标） | 574 passed / 0 failed / 1 ignored（32.31s） | `store --test password_migration` | 10 passed（`70233ed` 后） |
| `api --test jellyfin` | 64 passed | `store --test login_snapshot` | 6 passed |
| `api --test jellyfin_paged_metadata` | 25 passed | `store --test filter_codec_roundtrip` | 1 passed |
| `api --test jellyfin_sql_page` | 9 passed | `media-server --test jellyfin_scope` | 10 passed |
| `api --test jellyfin_read_scale` | 3 passed | `media-server --test next_up_paging` | 4 passed |
| `api --test storage_operations_runtime` | 9 passed | `api --test storage_nfo_export` | 3 passed |
| `api --test storage_end_to_end` | 4 passed | `api --test password_login_runtime` | 8 passed |
| `api --test design_improvements` | 10 passed | `api --test users` | 5 passed |
| `api --test bootstrap_credentials` | 13 passed（`70233ed` 后） | `api --test data_retention_control` | 9 passed |
| `api --test reidentify_scoped` | 5 passed | `api --test management -- watch_scrape` | 5 passed |

`api --test management` 整目标在第二批修复前从未运行过（564 passed / 9 failed，见 7.9）；修复后为 574 passed / 0 failed / 1 ignored（32.31s）。

**待复核（2026-10-21，task-71/R12）**：本表 `-p store | 337 passed / 67 targets`（原 :190）是 2026-10-18 的历史值，**本轮未复核**，本次也不给出新数字。已知自 2026-10-21 起 store 测试目标/用例数已变化（`storage_work_bounds` 由 2 个用例变为 8 个，见 8.3），且同期有多个 `crates/store/tests/**` 与 `crates/store/src/**` 正在改动；全量 `cargo test -p store` 属 Task 17 门后的统一重测项，引用该行前必须以当时树的实测日志为准。

### 7.4 前端门（最终冻结树）

| 门 | 实测 |
|---|---|
| `npx tsc --noEmit` | exit 0 |
| `npm run test`（`node --test`） | `# tests 832 / # suites 6 / # pass 832 / # fail 0`，`NODETEST_EXIT=0` |
| 其中 `test/library-operations.test.mjs` + `test/library-removal.test.mjs` | 22/22 |
| `npm run build` | `BUILD_EXIT=0`，`✓ built in 2.67s` |
| `npm run lint` | **exit 1**：`131 problems (15 errors, 116 warnings)`，全部在会话未触碰的文件里（`git status --porcelain web` 为空），属既有 lint 债；不作为本轮通过项 |

日志：`/tmp/cmsmoke/web_final.log`、`/tmp/cmsmoke/web_lint2.log`。

### 7.5 本轮修复：遗留明文管理员凭据不再被静默忽略（`70233ed`）

- 现象与原因：干净切换把非 Argon2 PHC 行改为 fail closed（`verify_password` 只接受可解析 PHC，`finish_verified_login` 拒绝非 PHC 观测值），同时删掉了旧自检赖以判定不安全凭据的明文比较，`legacy_admin_password_matches_known_token` 因此恒为 false，`prepare_admin_credentials` 落到 `Keep`：运维显式提供的 `CRAWLER_MEDIA_ADMIN_PASSWORD` 被静默忽略，随后登录 401。
- RED：`cargo test -p api --test bootstrap_credentials` → 11 passed / 2 failed；`cargo test -p store --test password_migration` 编译失败 `E0599`（新方法不存在）。
- GREEN：`store` 新增 `password::is_verifiable_hash` 与 `Store::admin_credential_is_legacy_plaintext`（只按形态识别遗留行，不参与登录校验）；`bootstrap_credentials` 在该形态存在时进入 `prepare_legacy_admin`，生产缺独立密码即 `InsecureLegacyNeedsPassword` 拒绝启动，提供独立密码即 `Rotate` 写入 Argon2 哈希。登录路径仍 fail closed，`password_migration` 的明文行断言不变。
- 修复后：`cargo test -p store` 67 targets / 0 failed；`cargo test -p api --test bootstrap_credentials` 13 passed；`--test password_login_runtime` 8、`--test users` 5、`--test storage_end_to_end` 4 全通过。该修复不是本次 workspace 全绿的最后一个障碍，见 7.9。

### 7.6 明确 Not run（不得视为通过）

真机 NAS（含 SMB/NFS 抖动、断电、磁盘满）、真 Jellyfin 客户端（Infuse/官方 App）、真实浏览器浏览与 seek、10 万–100 万条目 release 规模性能与延迟分位、真实多用户并发压力、全部 63 条索引的逐条消费者审计、`commit_playback_event` 的统一性。本节及 7.3/7.9 的结论不能外推到这些场景。

### 7.7 有证据的偏差（相对计划原文）

1. Task 13 Step 3 冻结屏障实现为单事务 `libraries.lifecycle_state='reindexing'` + `config_revision+1` 加 `wait_idle`，没有持久 `state='frozen'` 行；相应断言落在 `409 reindexing` 与闲置等待上。
2. `LedgerRow` 未按“审查10”移除：583 处引用按 `FileCoverage` 与各用途 DTO 消费，`crates/store/src/ledger.rs:242,394,518` 的 “legacy” 只出现在文档注释；这是有证据的偏差。
3. 行级 `(0,0)` 容忍仍在读/合并路径（整片 UnitKey 已统一为 `(-1,-1)`）。
4. 仅浏览入口实测统一（`select_page_blocking` / `protocol_item_page`）；`commit_playback_event` 统一未验证，`/Items/Resume|Latest`、`/Genres`、`/Studios`、`/Shows/{id}/Seasons|Episodes`、`/Users/{id}/Views` 仍走 `list_visible_items` 加应用层过滤（13 处已知差异）。
5. nfo 侧无 `library_operations/scrape.rs`（`crates/store/src/library_operations/mod.rs:226-228` 返回 `Invalid("library operation kind scrape is not a reconfiguration")`）；Watch Scrape / `scrape_tv_sidecars` 未接线（`crates/api/src/watch_scrape.rs:122-125`）；被显式删除的 metadata 字段未升级为 overrides。
6. `web/lib/api/playback.ts` 调用的 `/playback/files/{id}/fonts`、`/playback/files/{id}/trickplay` 路由不存在（会话前既有死消费者，仅上报）。
7. 计划正文的 `- [ ]` 复选框未勾选；逐任务状态见计划文件 `## 实施状态与证据`。
8. 历史提交 `21cdc9a`、`fd955ae` 引用当时未跟踪的 `library_operations/integrity.rs`，不可独立编译；只有 `be67df1` 起（含 `6abf493`、第一批与本批提交）的树经过上述验证。
9. `485e36f` 曾把 `release_date_sort` 的排序键误声明为 `KeyKind::Int`，`f815b21` 改回 `KeyKind::Text`；这是本轮唯一被 workspace 门捕获的回归，证据见 7.9。
10. 第二批修复的次要语义变化（有意）：冻结库中缺 `title_ref` 的请求由 400 变 409，已就位文件的重复请求由 200 `claimed:0` 变 409；`crates/api/src/http/reidentify.rs` 的库冻结门读取 store 失败时按 `ok().flatten()` 放行（fail-open），多库 409 响应体里的 `library_id` 因 `HashSet` 迭代顺序不确定。三者均上报，未在提交信息里隐藏。
11. `task-30` 在抽取 `reassign_parsed_files` 时一度把旁车刷新目标由 `to_media` 换成 `target`（未披露的语义变化），由 Lead 复查发现并在提交前改回原语义。
12. 测试函数规模：`library_gaps::path_reconciliation_and_root_consolidation` 原 137 行（已超 120 硬限），第二批加到 170 行后拆为 `path_reconciliation_reports_and_applies_ledger_moves`（89 行）与 `root_consolidation_accepts_duplicate_root_as_async_operation`（52 行）；同文件 `collections_crud_and_items`（128 行）是本轮未触碰的既有超限，仅记录。全仓仍有 13 个 `.rs` 文件超 800 行（`6abf493` 与本轮 HEAD 同样存在），属既有债。
13. 电影坐标被拒时没有新增 `info!`/`warn!` 日志，与相邻的 `start_ms` 校验保持一致；作为观察记录，不计为缺陷。
14. 播放会话快照的“电影非零季集坐标”→ `400 playback.invalid` 判定位于 `crates/api/src/http/playback/session.rs:32-58`（`b675940` 引入），它遮蔽了 `crates/api/src/http/playback/selection.rs:31-33` 的旧 404 分支，该分支现无测试固定；保留为纵深防御。

### 7.8 覆盖映射证据对照（14 行 → 本轮实测）

| 要求/复审项 | 本轮证据 |
|---|---|
| 五文件/拒旧/缓存隔离 | 7.2 五文件建立；`store --test storage_work_bounds clean_start_end_to_end_storage_lifecycle`；清 cache 后离线链与重导入 |
| S1 锁内 IO、源版本/旧 worker | `api --test storage_operations_runtime` 9、`--test storage_end_to_end` 4；冒烟 seek/Range |
| 用户摘要与原始事实 | `api --test jellyfin` 64（UnplayedItemCount / Played）；冒烟重启后 `played true` |
| 旧 writer、多集/pending | `store --test playback_compatibility` 5、`api --test design_improvements` 10；偏差 7.7-2 |
| 会话与多集、迟到事件 | `store --test login_snapshot` 6、`api --test password_login_runtime` 8 |
| 前端分页/选版本 | 7.4 前端门；`api --test jellyfin_paged_metadata` 25 |
| 元数据、provenance、多 NFO | `api --test storage_nfo_export` 3、`api --test jellyfin` 64；偏差 7.7-5 |
| 别名/身份、合并 | `api --test storage_operations_runtime` 9；冒烟 `identity_preserved_*` |
| 协议本地读取与原始进度 | `api --test jellyfin_sql_page` 9、`api --test jellyfin_read_scale` 3；偏差 7.7-4 |
| 根/locale/integrity | `api --test storage_operations_runtime` 9（202/409）；`api --test management` 的 `library_configuration`/`library_gaps`；偏差 7.7-1 |
| 电影坐标拒绝不写库 | `api --test management -- playback_movie_units::movie_nonzero_episode_is_not_an_alias_for_whole_movie`（600000ms 读回未被覆盖） |
| 冻结库拒绝写 | `api --test management -- library_sidecar_cancellation`（409，原文件未动、无 `new/`、无 `.nfo`） |
| User/Subscribe/Job/维护保留 | `store --test data_retention` 11、`api --test data_retention_control` 9、`api --test management -- watch_scrape` 5 |
| 无兼容、文件规模 | 7.3 全门；硬限核对（既有 13 个历史超限文件另记，本轮拆分 3 个目标文件已达标，偏差 7.7-12） |
| 精确过滤/权限/count、查询成本 | `store --test filter_codec_roundtrip` 1、`--test library_query_v1` 5、`--test storage_work_bounds` |
| 100k/1M 规模/锁/WAL/空间 | **Not run**（见 7.6）；仅 release 小规模对照见第 8 节 |

### 7.9 `cargo test --workspace` 红 → 绿（含第二批修复）

1. 第一次在 `70233ed`（干净树）上运行：日志 `/tmp/cmsmoke/workspace.log` 末尾 `WORKSPACE_EXIT=101`；唯一失败目标是 `crates/api/tests/management/main.rs`，`564 passed; 9 failed; 1 ignored`。归因：
   - `gallery_contracts::library_browse_accepts_every_wall_sort_key`：`500 {"error":{"code":"store.error","message":"Invalid column type Text at index: 23, name: release_date_sort"}}` —— 本轮 `485e36f` 把该排序键误声明为 `KeyKind::Int`，属本轮回归；`f815b21` 改回 `KeyKind::Text` 后 `api --test management -- gallery_contracts::` 7 passed、`store --test library_query_v1` 5 passed。
   - 其余 8 条（`library_configuration::libraries_patch_replaces_roots_and_renames`、`library_gaps::path_reconciliation_and_root_consolidation`、`library_sidecar_cancellation::frozen_library_rejects_organize_reidentify_and_sidecar_refresh`、`playback_movie_units` 5 条）是合并点前既有合同（`fd955ae` 的 202/409 契约、`b675940` 的电影坐标拒绝）与陈旧测试之间的差异：`git diff --stat 6abf493..HEAD -- crates/api/src/http/library_config.rs crates/api/src/http/playback/session.rs` 为空，证明生产侧本轮未改这两处。
2. 第二次（第二批修复后，含 `to_media` 修正）：`/tmp/cmsmoke/workspace_final2.log` 末尾 `WORKSPACE_EXIT=0`，317 个测试目标全部 `0 failed`，`management` 573 passed / 0 failed / 1 ignored，汇总 2375 passed。
3. 第三次（最终冻结树，含 `f815b21`、第二批修复与根归并测试拆分）：`/tmp/cmsmoke/workspace_final3.log` 末尾 `WORKSPACE_EXIT=0`（333 个测试目标全部 0 failed，汇总 2376 passed；`management` 574 passed / 0 failed / 1 ignored，32.31s）。
4. 判定口径：只采信日志文件内的 `WORKSPACE_EXIT=` 行，包装进程的状态码不作为证据。

## 8. 释放模式性能门与结论（Task 18–20，2026-10-21）

`cargo test --release` 实测（日志 `/tmp/cmsmoke/release_perf.log`，退出码 0）：

| 命令 | 实测 |
|---|---|
| `cargo test -p store --release --test storage_work_bounds` | 2 passed / 0 failed（0.70s），含 `unrelated_library_growth_does_not_expand_seek_work`：`fullscan_steps=0`、`vm_steps ≤ 2× baseline` |
| ↳ **复审更正 2026-10-21（task-71/R12）：上一行的 `2 passed`（0.70s）已过时** | 同一命令现为 `8 passed / 0 failed`（debug 1.42s、release 1.08s），raw 退出码 0；逐字 `test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.08s`（release，2026-10-21T10:48:49Z）；该用例已拆为父文件 + 2 个子模块，明细与第 8 节其余行的复核状态见 8.3 |
| `cargo test -p api --release --test storage_query_scale` | 1 passed / 0 failed（0.06s） |
| `cargo test -p api --release --test storage_contention` | 1 passed / 0 failed（0.06s），`unrelated_user_growth_does_not_expand_heartbeat_write_scope` |
| `cargo test -p store --release --test data_retention` | 11 passed / 0 failed |
| `cargo test -p api --release --test data_retention_control` | 9 passed / 0 failed |
| `cargo test -p api --release --test maintenance_runtime` | 14 passed / 0 failed（0.40s） |

### 8.1 结论：Task 18/19/20 本轮无代码改动（有证据的“无变化”）

- Task 18：计划 Step 2 期望的“深游标工作量随无关增长扩张”在本轮工作负载上没有出现，`fullscan_steps=0` 且 `vm_steps ≤ 2× baseline`，故按计划 Step 3 记录**无变化**，不改查询/索引形状；该断言保留为回归门。
- Task 19：`unrelated_user_growth_does_not_expand_heartbeat_write_scope` 在 release 下通过，写事务范围未随无关用户增长扩张，故**无变化**。
- Task 20：未删除任何索引。抽查候选中每个都有活消费者（如 `sites.enabled`、`playback_session_ids.issued_at`、`playback_logs.ended_at`、`fingerprint_samples.captured_job_id`、`fingerprint_model_members.sample_id` 均在 `.rs` 查询路径被引用）；计划 Step 3 要求“无消费者且有通过的读写对照”才可删，两个条件都不成立，故**无变化**。全部 63 条索引的逐条消费者审计未做（Not run）。

### 8.2 边界

上述 release 运行的数据规模为 50 与 500 行量级、单机单进程，**不是**性能基准：没有延迟分位、没有 10 万/100 万条目、没有真机 NAS 并发与 WAL/空间对照。因此本节不构成“性能达标”结论。

### 8.3 复审更正与待复核明细（2026-10-21，task-71/R12）

本节只做**事实更正与挂账**，不修改第 7/8 节的历史数字，也不删除历史行。执行者 task-71（meta-split-worker），依据 Lead 裁定 R12；本节的更正依据是本轮 task-64 的实测证据，**本节自身未跑任何 cargo**。

**复审更正 1 —— 第 8 节表格第 1 行（原 :282）`storage_work_bounds` 的用例数**

- 更正对象：原记录 `2 passed / 0 failed（0.70s）`。
- 现状：`crates/store/tests/storage_work_bounds.rs` 已拆为父文件 + `crates/store/tests/storage_work_bounds/{scan_windows.rs, scan_windows_measurement.rs}`，用例数 **2 → 8**；本轮另新增 store 侧 Task 19 case：扫描提交窗口上界（`MAX_SCAN_BATCH = 100`，101 行窗口被拒且库内计数不变）、同一 Media 下 N 文件单窗口只产生一次归约（读 `library_items` 的摘要行/revision 而非私有 helper）、窗口工作量不随既有文件总数增长、窗口失败后重启事实与摘要一致、以及写锁争用下的诊断（plain 写者等锁 vs `BEGIN DEFERRED` 读→写升级 vs store 窗口）。
- 命令原文：`cargo test -p store --test storage_work_bounds -- --nocapture`（debug）与 `cargo test -p store --release --test storage_work_bounds -- --nocapture`（release）。
- raw 退出码：debug `0`（2026-10-21T10:47:59Z–10:48:04Z）、release `0`（2026-10-21T10:48:49Z–10:48:52Z）。
- 逐字 `test result:` 行：
  - debug：`test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.42s`
  - release：`test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.08s`
- 并发说明：两次运行前/后 `pgrep -fl -x cargo` 均为空（无其他 cargo 在跑，target 锁未争用），故上述数字不是并发产物。
- 原记录中同行的 `unrelated_library_growth_does_not_expand_seek_work`（`fullscan_steps=0`、`vm_steps ≤ 2× baseline`）用例仍存在、未改名、仍通过，该结论部分**未**被更正。
- 测试名与归属核对（原 :250 `store --test storage_work_bounds clean_start_end_to_end_storage_lifecycle`、原 :264 `--test storage_work_bounds`）：target 名与用例名均未变（该用例字节未变），这两处**无需更正**。

**待复核 —— 第 7.3 节表格 `-p store | 337 passed / 67 targets / 0 failed`（原 :190）**

- 该行是 2026-10-18 的历史值，本轮**未复核**；本节不给出替代数字（本轮没有在统一树上重跑全量 `cargo test -p store`）。
- 已知变化：`storage_work_bounds` 用例数 2 → 8（见上），故 store 的 target/用例总数与 2026-10-18 相比已不同；同期 `crates/store/tests/**` 与 `crates/store/src/**` 仍有多个文件在改动中。
- 处置：全量 `cargo test -p store` 属 Task 17 门后的统一重测项，引用该行前必须以当时树的实测日志为准。

**本节未覆盖**：第 8 节其余行（`data_retention`、`data_retention_control`、`maintenance_runtime`、`storage_query_scale`、`storage_contention` 等）本轮均未复核，其数字保持 2026-10-18 的历史含义；本节不为它们给出新数字。

## 9. Task 18 查询工作量基线（2026-10-22，release，本轮实测）

本节只记录本轮真跑出来的数字，命令与 `RAW_EXIT` 逐字保留；没有测到的项在末尾逐条列为 `未跑`，不外推。

### 9.1 命令与退出码

| 命令 | 时间窗（UTC） | 原始结果 | 日志 |
|---|---|---|---|
| `cargo test -p store --release --test storage_work_bounds -- --nocapture` | 2026-10-22T01:57:32Z → 01:58:16Z | `test result: ok. 12 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 4.81s`；`RAW_EXIT[store_release]=0` | `/private/tmp/task18-step2-baseline.log` |
| `cargo test -p api --release --test storage_query_scale -- --nocapture` | 2026-10-22T01:58:16Z → 01:59:15Z | `test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s`；`RAW_EXIT[api_release]=0` | 同上 |
| `CRAWLER_MEDIA_SEEK_GROWTH_ITEMS=1000000 cargo test -p store --release --test storage_work_bounds seek_work_stays_flat_while_an_unrelated_library_grows -- --nocapture` | 2026-10-22T02:00:35Z → 02:02:05Z | `test result: ok. 1 passed; 0 failed; ...; finished in 89.62s`；`RAW_EXIT[1m_growth]=0` | `/private/tmp/task18-step4-1m.log` |

环境：macOS arm64（Apple M4 Pro，12 核）、SQLite `3.50.2`（bundled）、`target/release`、运行前后 `pgrep -x cargo` 为空（无并发 cargo，排除锁争用）。冷热缓存未做分层对照（见 9.4）。

### 9.2 关键度量（用例自打印原文）

```text
# 默认规模（无关库 1,000 → 10,000 文件，目标库 300 条目）
growth: other 1000->10000, target 300; seed 1.56s; db 25972736 B; sqlite 3.50.2;
        before QueryWorkStats { fullscan_steps: 0, vm_steps: 1963, rows_returned: 50 };
        after  QueryWorkStats { fullscan_steps: 0, vm_steps: 1963, rows_returned: 50 }

# 深游标（50,000 条目，100 页游标扫描）
deep: 50000 items seeded in 4.45s; 100 cursor pages in 0.17s

# 计划 Step 4 要求的大规模（无关库 1,000 → 1,000,000 文件，目标库 300 条目）
growth: other 1000->1000000, target 300; seed 89.26s; db 2464907264 B; sqlite 3.50.2;
        before QueryWorkStats { fullscan_steps: 0, vm_steps: 1963, rows_returned: 50 };
        after  QueryWorkStats { fullscan_steps: 0, vm_steps: 1963, rows_returned: 50 }
```

**结论（只就这三条数据）**：无关库文件数 1,000 → 1,000,000（1,000 倍）时，目标库 seek 的 `fullscan_steps` 恒为 `0`、`vm_steps` 恒为 `1963`、返回行数恒为 `50`，即计划 Step 1 的 `fullscan_steps == 0` 与 `vm_steps ≤ 2×基线` 在 1M 文件规模下成立（比值为 1.000）。因此本轮**没有**改动查询生产代码，也就没有 `perf(query): bound canonical reads with measured compound seeks` 这条提交——计划 `:414` 建议的提交信息与实际改动不符，如实记偏差。

### 9.3 覆盖到的用例（本轮全部通过）

- `seek_work_stays_flat_while_an_unrelated_library_grows`（含 `fullscan_steps == 0`、`vm_steps <= before*2` 断言，`crates/store/tests/storage_work_bounds/seek_scale.rs:393-400`）
- `deep_cursor_sweep_has_no_duplicate_or_gap`（50k 深游标无重无漏）
- `ordering_and_cursor_agree_across_null_ties_and_unicode`（NULL/tie/ASC/DESC/Unicode）
- `watch_filters_counts_and_cross_library_media_share_one_snapshot`（未看/last_played/跨库去重/count 同快照）
- api：`api_query_scale_page_and_cursor`、`api_cross_library_media_keeps_one_card_per_library`、`api_ordering_windows_are_stable_across_blank_ties_and_unicode`、`api_watch_filters_and_unwatched_first_over_http`

### 9.4 本节 `未跑`（不外推）

- 100k Media / 1M Playback 规模对照（只做了「无关库 1M 文件」这一维）
- p50/p95 分位与峰值 RSS（用例未埋点，本轮未另做采样）
- 冷热缓存分层对照、SQL 次数计数、返回/解码行字节统计
- exact facets / 包含搜索 / offset 这三类**允许增长**路径的实测（计划 Step 2 要求区分，本轮未测）
- 实际 Web/Jellyfin 深页烟测（浏览器侧仍为 Not run）

## 10. Task 19 写放大优化实测（2026-10-22，release，本轮实测）

### 10.1 先测后改：原始曲线（未改动代码，`release`）

把 `crates/api/tests/storage_contention.rs` 的规模改成可用环境变量调整（`CRAWLER_MEDIA_CONTENTION_EPISODES`，默认仍 10,000，断言语义不变），量出原始曲线：

| 集数 | 实测总时长 | 吞吐 |
|---|---|---|
| 500 | 336 ms | 1488.1 文件/秒 |
| 1000 | 2,210 ms | 452.5 文件/秒 |
| 2000 | 17,043 ms | 117.4 文件/秒 |

每翻倍约 6.6×–7.7×（指数 ≈ 2.95）。按此外推 10k ≈ 35 分钟，与「`--release` 下 1800 s 看门狗被杀」的历史记录一致。

### 10.2 根因（可指认到行）

1. `crates/store/src/library_write/reduce/card.rs::rebuild_units`：对每个 unit 发两条 SQL（`reduce.rs::unit_counts` 查询 + `INSERT ... ON CONFLICT`），一个 10k 集 media 的单次归约就是 20,000 条语句。
2. `crates/store/src/library_write/publish.rs`：同一 media 在一个 100 文件窗口里出现多次时走 `MediaChange::Multiple` → 整媒体全量重算，**每个窗口重读该 media 的全部已发布文件**，总代价 O(文件数 × 窗口数)。

### 10.3 改动

- `rebuild_units` 改为集合式：一条聚合算 `file_count`/`available_file_count`，一条相关子查询算 `preferred_file_id`（`ORDER BY f.file_revision DESC, f.file_id ASC LIMIT 1`，与原逐条逻辑同序），一条 `INSERT ... SELECT ... ON CONFLICT DO UPDATE`。SQLite 的 upsert+SELECT 解析歧义用 `WHERE true` 消除；因此变成死代码的 `coverage_units` 一并删除。
- `publish.rs` 窗口归约改为**按文件逐条增量**（`PendingChange` 向量），窗口代价与「本窗口真正改动的文件数」成正比。
- 回归修复：首文件时条目卡片尚不存在，增量路径内部会回退一次全量重算，若随后继续跑剩余增量会把同一批文件重复计入（`file_publication::twelve_episodes_two_versions_reduces_to_one_item_twenty_four_files` 实测 `left: 47, right: 24`）。修法：窗口归约前判断卡片是否存在，不存在则只做一次全量重算并跳过其余增量。

### 10.4 改后（`release`，同机）

| 集数 | 改前 | 改后 | 倍数 |
|---|---|---|---|
| 500 | 336 ms | 163 ms | 2.1× |
| 1000 | 2,210 ms | 1,096 ms | 2.0× |
| 2000 | 17,043 ms | 1,757–1,763 ms | 9.7× |
| 10000 | 从未跑完（>1800 s 被看门狗 kill；debug 8m25s 不返回） | **60,889 ms，`RAW_EXIT=0`** | 首次跑完 |

10k 用例自打印的 Step 2 指标（原文）：

```text
扫描: 100 批 × 100 文件 = 10000 集，实测总时长 60889 ms，吞吐 164.2 文件/秒
单批提交（含等待持锁）: p50=530 ms, p95=1286 ms, max=1394 ms
并发心跳 51 次，端到端延迟: p50=1049 ms, p95=2430 ms, max=3965 ms
心跳状态码分布: {200: 51}；busy(503/429)=0；非 2xx=[]
WAL: 1128912 -> 6212992 字节（增长 5084080）
进程 RSS: 40336 -> 25984 KiB（增长 -14352）
锁等待: 未测得（无公开计数器）
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 3 filtered out; finished in 61.13s
```

用例断言全过，包括 Task 19 Step 1 点名的三项：同一 Media 长到 10k 集后心跳**写作用域不增长**、逐表写集合相同、不拥有该单元的 Library 未被写；并发扫描期间 51 次心跳全部 2xx、无 503/429。

### 10.5 本节 `未跑`（不外推）

- 锁等待纯值（无公开计数器，只有「事务时长 + 排队」上界）
- 100k Media / 1M Playback 规模
- Task 19 Step 4 的「临时服务并发真实 HTTP 观察」（本轮只有 in-process `oneshot` 路由，未起真实监听端口）
- S1 读作用域计数（`READ_SCOPE_UNAVAILABLE`，需新增 store 侧公开计数器）
- 1000 集以下之外的其他 profile（debug 下 10k 未重跑，历史记录为 8m25s 不返回）

### 10.6 改动后的回归门（本轮实测）

| 门 | 命令 | 时间窗（UTC） | 原始结果 | 日志 |
|---|---|---|---|---|
| store 全 crate | `cargo test -p store` | 2026-10-22T02:16:45Z → 02:18:37Z | 71 targets / 373 passed / **0 failed**；`RAW_EXIT[store_tests]=0` | `/private/tmp/task19-verify2.log` |
| 争用 target（全 4 用例） | `cargo test -p api --release --test storage_contention` | 02:20:52Z → 02:21:51Z | `test result: ok. 4 passed; 0 failed; ...; finished in 58.95s`；`RAW_EXIT=0` | `/private/tmp/task19-api-verify.log` |
| 工作区（与上轮同口径，带具名 10k skip） | `cargo test --workspace -- --skip concurrent_scan_of_ten_thousand_episodes_keeps_heartbeat_write_scope_bounded` | 02:27:07Z → 02:35:31Z（504 s） | **338 targets / 2467 passed / 0 failed / 10 ignored**；`RAW_EXIT[workspace]=0`、`WORKSPACE_EXIT=0` | `/private/tmp/task19-workspace-gate.log`（342,851 B，md5 `22e0b760566595ba5290064266257bcb`） |

**一次抖动如实披露**：`cargo test -p api`（debug 全 crate，02:21:51Z→02:26:42Z）出现过 1 个失败——`request_admission::site_over_capacity_degrades_to_per_site_failure`（断言「占位请求应先占满 4 个搜索 permit」）。该用例与存储/发布路径无关，单独重跑 **3/3 通过**（每次 0.29 s），随后的工作区门同用例通过，判定为满载时序抖动，不记为代码缺陷，也不据此宣称「api 全绿」——api 的绿以上表工作区门为准。

## 11. Task 20 索引/保留实测与索引审计（2026-10-22，本轮实测）

### 11.1 Step 2 测量（release，命令与 `RAW_EXIT` 逐字）

| 命令 | 时间窗（UTC） | 原始结果 | 日志 |
|---|---|---|---|
| `cargo test -p api --release --test maintenance_runtime --test data_retention_control -- --nocapture` | 2026-10-22T02:37:28Z → 02:37:39Z | `maintenance_runtime` 15 passed / 0 failed；`data_retention_control` 9 passed / 0 failed；`RAW_EXIT[api_retention]=0` | `/private/tmp/task20-step2.log` |
| `cargo test -p store --release --test data_retention --test storage_work_bounds -- --nocapture` | 02:37:39Z → 02:37:58Z | `data_retention` 11 passed / 0 failed；`storage_work_bounds` 12 passed / 0 failed；`RAW_EXIT[store_retention]=0` | 同上 |

**未测得**：fixture 的 DB/索引/WAL 字节、page 利用率、写吞吐、清理与 VACUUM 锁时长——计划 `:440` 点名要记录这些，本轮四条命令没有打印这些量，`--nocapture` 也没有输出，故记「未测得」，不填估算值。

### 11.2 Step 3 索引审计（56 条全量枚举）

方法与逐条结果见 `/private/tmp/task20-index-audit.md`（84 行，md5 `668bc3a367cd9951222744d8e29d84e4`）。要点：

- 索引总数 **56**（`library.sql` 51 / `app.sql` 2 / `subscribe.sql` 3），从 DDL 正则枚举。
- 源码 SQL 字面量 **703** 条参与匹配（`crates/**/*.rs`，长度 ≥20、含 `SELECT|INSERT|UPDATE|DELETE`）。
- 判定 `direct+near+filelevel == 0` 的**零证据候选：0 条**——56 条索引全部至少有一条同文件消费痕迹。
- 结论：依计划 `:446`「没有收益则记录不改」，本轮**不删除任何索引**、不改持久形状、不引入格式迁移；`crates/store/tests/schema_redundant_indexes.rs`（工作区门内 1/0/0）保持不变。
- **方法学局限（与结论同读）**：SQL 大量由字符串拼接动态生成（如 `crates/store/src/playback/log_scope.rs:146` 的 `FROM playback_logs l WHERE 1=1` 后追加谓词），静态匹配只能证明「存在消费痕迹」，**不能**证明执行计划真的用了该索引；SQLite 无按索引使用计数器。因此本审计只用于排除「完全无证据」的索引，不用于声称「索引被实际使用」。
- 证据最弱的 12 条（`terms_media`、`chapter_dispatch`、`sites_enabled_idx`、`season_model_lookup`、`sample_job`、`sample_lookup`、`session_id_expiry`、`model_file`、`probe_retry`、`session_generation`、`attempt_job_file`、`probe_obligation_dispatch`）列为未来若真要删索引时的优先复核对象（需 EXPLAIN + 读写对照）。

### 11.3 Step 1 保护 case（本轮工作区门内的原始计数）

| target | 结果 | 位置 |
|---|---|---|
| `backup_consistency` | 2 passed / 0 failed | `crates/store/tests/backup_consistency.rs`（受控停写快照 vs 直接复制活跃 WAL 的反例） |
| `data_retention_control` | 9 passed / 0 failed | `crates/api/tests/data_retention_control.rs` |
| `data_retention` | 11 passed / 0 failed | `crates/store/tests/data_retention.rs` |
| `maintenance_runtime` | 15 passed / 0 failed | `crates/api/tests/maintenance_runtime.rs` |

### 11.4 本节 `未跑`

真实 NAS 写入、真实 Jellyfin/Infuse/VidHub 设备、真实浏览器人工浏览/seek、100k–1M 规模、fixture 的 DB/索引/WAL 字节与 page 利用率、VACUUM 锁时长、独立全分支 review、用户真实实例切换（停写/路径/DB-WAL-SHM 清单/授权核对）。

### 11.5 优化后的端到端复跑（同一棵树）

Task 19 的 `publish.rs`/`reduce/card.rs` 改动落在扫描与发布路径上，因此冒烟必须在新树上复跑，不能沿用优化前的证据。

| 项 | 值 |
|---|---|
| 命令 | `bash /private/tmp/t17-step4-smoke.sh`（env 与计划 `:395` 逐字一致，仅去 `cargo run` 包装） |
| 被测二进制 | `target/debug/crawler-media`，`mtime(UTC)=2026-10-22T02:22:24Z`（晚于两个被改源文件的 02:16:40Z / 02:07:20Z，确认含本轮改动） |
| 时间窗 | 2026-10-22T02:39:15Z → 02:39:53Z |
| 结果 | **通过 121 / 未通过 0 / 未跑 3**，22 步全达；`RAW_EXIT[script]=0` |
| 日志 | `/private/tmp/t17-step4-smoke-driver10.log`（md5 `6c6346ab087055a6e309839dc6ad715f`） |

3 条 `未跑` 与前次相同（真实浏览器人工操作、前端 UI 三门由本节 §9/§10 之外的门单独覆盖、真实 NAS/客户端写入）。本轮批次 2–4 未改 `web/`，前端三门沿用 2026-10-21 的实测（typecheck 0 错、830 pass / 0 fail、lint 130 problems = 既有基线）。
