# 存储修复复审：1845dab

日期：2026-10-22。结论：**Request changes**。只复审/文档，不修业务、不操作真实实例。

## 范围与证据边界

- 修复基线 `aec73cc7709fb5d3769527fc2718981eeaeda686`，被审树 `1845dab9d501e63bbb550d183fc1c3d1c980ae03`；`git diff aec73cc...1845dab`：565文件，+56288/-20824。沿相关生产调用链检查，不把旧报告照搬为当前缺陷。
- 先阅读[执行历史](2026-10-22-storage-cutover-execution-history.md)和[benchmark](../benchmarks/2026-10-18-library-storage.md)的最新修复/更正。历史workspace门带具名skip、10k后来release首次通过、1M只测无关Library文件增长，均保留原边界。
- 本轮只跑隔离临时Rust runner（真实Store、真实Router、真实TmdbCatalog；仅HTTP/Downloader等外部依赖fixture）与实际Web TS adapter烟测。runner的“passed”表示观测程序执行完成，**不是产品合同通过**；以下保留实际反例。部分仅源码证明，明确标记未运行。
- rust-analyzer references启动报 `LSP server exited unexpectedly (code 0)`，已report_issue；之后按源码/SQL/路由核对。第一位Standards代理模型不可用，后续独立只读审查完成，不采信失败代理输出。
- 未跑workspace、各crate全套、前端三门、浏览器UI、真实客户端/NAS、新性能基准；不以历史绿测代替本轮验证。

## Spec findings

### R01 [P1] 生产重扫没有推进 FileVersion

- **证据级别：** 实测 Router。
- **位置：** `crates/api/src/watch_ledger.rs:20-47；http/library_scan/runner/checkpoint.rs:198-220`。
- **根因：** 替换现有A.mkv后真实scan completed，revision/source仍原值；独立observe_source/replace_file_source只有测试调用。冷缓存清除不等于源版本推进。
- **影响：** 当前源变更、旧Probe、已签发恢复包可描述不同字节。
- **复现/回归入口：** A→不同大小B，调用POST scan并等待completed，再读get_file_version；artifact://127。

### R02 [P1] config_revision=0仍是通配符

- **证据级别：** 实测 Store。
- **位置：** `crates/store/src/library_write/publish.rs:383-389；crates/api/src/claim.rs:166-174`。
- **根因：** 配置已从0变1且ready，WriteScope(0)仍成功插入1行。
- **影响：** 第一次重配置后旧观察/写者仍可发布。
- **复现/回归入口：** ZERO_SCOPE current_revision=1 result=Ok(inserted:1)；artifact://133。

### R03 [P1] 多文件增量重复累计新覆盖单元

- **证据级别：** 实测 Store。
- **位置：** `crates/store/src/library_write/publish.rs:122-147；reduce_incremental/units.rs:153-184`。
- **根因：** 所有事实先落库；首delta从最终事实初始化新unit，随后delta重复+1。已有E01卡片、同批两个E02版本得到(3,3)。
- **影响：** 可用版本数漂移；完整归约和增量归约不一致。
- **复现/回归入口：** BATCH_NEW_UNIT expected=(2,2) actual=(3,3)；artifact://133。此为9aaf032优化后的新回归。

### R04 [P1] pending恢复的自写排除集合记错UUID

- **证据级别：** 源码证明，未运行。
- **位置：** `crates/api/src/worker/transfer/recorder.rs:148-153；store/src/library_write/transfer.rs:66-68`。
- **根因：** reserve返回旧持久A ID，但reserved插入新candidate ID；A发布改变freshness，随后B把自己的变化当外部变化。
- **影响：** 重启后同轮Transfer停在A，B不能完成。
- **复现/回归入口：** 新UUID candidate恢复旧pending A，published后prepare B；现有storage_transfer_recovery绕过recorder，不覆盖该链。

### R05 [P1] 实际claim入口丢多集并伪造未知TV whole

- **证据级别：** 源码证明，未运行。
- **位置：** `crates/api/src/claim.rs:47-78；http/library_admin.rs:443`。
- **根因：** from_release无集即WHOLE；existing-Media路由始终unidentified()，不解析E01-E02。
- **影响：** 未知TV成为可播放整片；命名多集丢覆盖。
- **复现/回归入口：** 真实两个claim route分别提交未知TV、E01-E02、TV S00E00；现有同名回归直接调用Store。

### R06 [P1] Jellyfin取流忽略签发绑定并使用裸文件ID

- **证据级别：** 实测 Router。
- **位置：** `crates/api/src/media_server_provider.rs:499-550；crates/media-server/src/routes/media/playback.rs:424`。
- **根因：** PlaybackInfo给裸UUID；resolver忽略PlaySessionId/DeviceId。源revision变更后旧sid+错误设备仍206并返回替换字节XXXX。
- **影响：** 旧会话可取新版本；f1版本化源合同未落生产。不是宣称越过User ACL。
- **复现/回归入口：** 实际ApiServerProvider PlaybackInfo→替换源→带旧sid/错误设备Range，206 XXXX；artifact://127。

### R07 [P1] 同文件不同集的无ID迟到事件仍落错单元

- **证据级别：** 实测 Router。
- **位置：** `crates/api/src/playback_events.rs:153-175；http/playback/heartbeat.rs:351-383；store/src/playback/event.rs:145-175`。
- **根因：** E02活跃、同设备无ID E01 Progress，经物理file-only分类进入unbound writer。
- **影响：** E01 position从2000被改5000；不得以保留sessionless协议为由放行冲突目标。
- **复现/回归入口：** MERGED_NO_ID 200，E01 2000→5000；artifact://127。显式sid多集修复仍承认。

### R08 [P1] 切换设备会话继承旧观看时长和起点

- **证据级别：** 实测 Router。
- **位置：** `crates/store/src/playback/event.rs:608-653`。
- **根因：** upsert按device取previous，未限定session/generation；ON CONFLICT不更新started_at/start_position。
- **影响：** B继承A watched_ms=10000和A起始时间；A终态历史丢失（历史缺失为源码证明，计数继承实测）。
- **复现/回归入口：** A start/progress10s→B start无A Stop；B watched=10000 started_at=1700000000；artifact://127。

### R09 [P1] 源变化后的Web Start又写回旧恢复包

- **证据级别：** 实测 Router。
- **位置：** `crates/api/src/http/playback/heartbeat.rs:138-148,267-279；store/src/playback/event.rs:406-415`。
- **根因：** 会话响应source_changed/start0之后，真实客户端无position Start从旧unit补3600000/duration5400000，并绑定所选新源。
- **影响：** 45分钟版本复用90分钟版本60分钟起点与旧轨道。
- **复现/回归入口：** 在旧源A真实签发/Progress建立绑定后，显式选B响应source_changed/start0；真实无position Start后unit仍position3600000/duration5400000。强化复现artifact://156；不再依赖无binding旧unit或碰巧默认选B。

### R10 [P1] 首次Jellyfin观看无法根据已探测时长完成

- **证据级别：** 实测 Router。
- **位置：** `crates/api/src/media_server_provider/playback.rs:29-40；store/src/playback/event.rs:385-388`。
- **根因：** duration只读旧用户unit，忽略精确源probe duration；fresh user无值。
- **影响：** Playing/Progress/Stopped到120s仍played=false、play_count=0、duration=None。
- **复现/回归入口：** 真实设备header，文件Probe已知120000ms，三个204后读回；artifact://142。

### R11 [P1] 观看活动秒列写入毫秒

- **证据级别：** 实测 Router。
- **位置：** `crates/store/src/playback/event.rs:396-422；api/src/http/playback_views/up_next.rs:383-384`。
- **根因：** watch_activity_at与played_changed_at_ms共用now_ms绑定。
- **影响：** updated_at大1000倍；后续再乘1000，时间窗历史清除误包含旧状态（下游影响源码证明）。
- **复现/回归入口：** EVENT_TIME expected_seconds=1700000010 actual=1700000010000；artifact://127。

### R12 [P1] played断言解析/协议过滤不遵守最新时间

- **证据级别：** 源码证明，未运行。
- **位置：** `crates/store/src/playback/assertions.rs:86-110；library_query/protocol/predicates.rs:239-277；api/src/http/playback.rs:58-67`。
- **根因：** 新resolver按specificity立即返回；SQL无played_changed_at_ms排序；Web exact row、旧mark writer未统一；Series筛选只查whole而返回状态按集完成。
- **影响：** 较新season false不能覆盖旧episode true；详情/SQL/摘要不一致，全看完Series被IsPlayed=true排除。
- **复现/回归入口：** episode true@1000→season false@2000；无episode行season mark；E01/E02全看完但无whole，比较HTTP筛选与UserData。

### R13 [P2] 旧movie零坐标兼容仍在，轨道刷新重置同源进度

- **证据级别：** 源码证明，未运行。
- **位置：** `crates/api/src/http/playback/selection.rs:108-114,163-173；http/playback_views/up_next.rs:344-345；store/src/playback/mutate.rs:42-47`。
- **根因：** session创建已拒坐标，但marks/progress/resume仍映射movie00；tracks_version被并入timeline匹配。
- **影响：** 未完成干净去旧；同视频只重探轨道就从0播放。
- **复现/回归入口：** movie00所有公开入口应拒绝且无写，TV00成功；同源轨道1→2保position，仅清轨道。

### R14 [P1] 文件续页截断且状态/覆盖被adapter丢弃

- **证据级别：** 实测 JS实际API模块。
- **位置：** `web/lib/api/libraries.ts:1523-1555；web/lib/library-file-pages.ts:46-47`。
- **根因：** 续页读next_file_cursor而服务端给next_cursor；fileFrom不复制units/presence/publication；缺字段fail-open。
- **影响：** 101文件只返回100；missing文件仍可选播放。
- **复现/回归入口：** 真实TS模块transpile+真实collector+仅HTTP fixture：actual100/requests2/presence=null/units=null/playable=true。

### R15 [P1] 完整标准化生产入口为no-op，人工覆盖不进入详情

- **证据级别：** 实测 Catalog与Router/Store。
- **位置：** `crates/api/src/catalog.rs:118-126,259-475；metadata_service.rs:40-66；store/src/metadata/overrides.rs:39-50；library_query/details/mod.rs:80-86`。
- **根因：** 所有生产Catalog未覆写normalized_metadata，默认None；旧ItemMeta有损采用仍活跃。PUT overrides200但读详情走NFO/短列而非统一compose。
- **影响：** 刷新可报成功却没采用完整响应；人工MANUAL存在但详情仍V1。
- **复现/回归入口：** 真实TmdbCatalog normalized is_some=false；HTTP override200 revision1，detail V1/composed MANUAL；artifact://127。

### R16 [P1] 来源刷新Missing保留过时有效列，合成器与落盘分裂

- **证据级别：** 实测 Store。
- **位置：** `crates/store/src/metadata/adopt.rs:65-108,130-132,212-217,276；metadata/compose.rs:26-52`。
- **根因：** 基于旧effective补丁，不按全部当前snapshot重算；origins整份取最后incoming。
- **影响：** TMDB去掉简介后persisted仍OLD WINNER、compose却BACKUP；字段值/来源和筛选展示不一致。
- **复现/回归入口：** SOURCE_MISSING persisted=OLD WINNER composed=BACKUP；artifact://133。

### R17 [P1] NFO正常导出无CAS并先破坏磁盘，登记终态早于反馈登记

- **证据级别：** 实测 Router/Store。
- **位置：** `crates/api/src/scrape_metadata/export.rs:40-71；http/library_scan.rs:394-409；store/src/metadata/export.rs:121-163；metadata/export_driver.rs:190-199,272-290`。
- **根因：** begin_sidecar_export并不持久意图，caller先旧write_nfo；begin无file/config/media/digest核验，driver当前digest非output即覆盖；complete succeeded先于register。
- **影响：** 过期源/配置/外部编辑可被覆盖；崩溃于complete后tag前不再进入pending恢复（后者源码证明）。
- **复现/回归入口：** STALE_EXPORT Ok/Replaced，编辑变空movie；真实refresh把无效外部XML覆盖。artifact://133、127；重启恢复路径已有digest拒绝case不代表正常入口安全。

### R18 [P1] NFO外部编辑比较了新provider而非上次导出baseline

- **证据级别：** 实测 Store。
- **位置：** `crates/store/src/metadata/export_driver.rs:132-152,404-452；metadata/nfo_input/record.rs:128-140`。
- **根因：** capture从current effective diff，不读export_fields_json；升级全局manual override。
- **影响：** 只改plot会把没改的旧title也锁为manual；删除NFO不能解除该manual，污染另一Library（后两项源码证明）。
- **复现/回归入口：** 导出titleV1/plotV1→只改plot→来源titleV2，title_override=V1；artifact://133。

### R19 [P1] 根+locale组合操作丢locale，迁根范围/失败恢复不完整

- **证据级别：** 实测 Store。
- **位置：** `crates/store/src/library_operations/mod.rs:136-140,224-232,307-318；roots.rs:144-148,160-178`。
- **根因：** 有roots就只运行roots分支；没有匹配根时保旧根却succeeded；unidentified仅随file遍历；失败提交部分变化并ready。
- **影响：** 组合PATCH静默丢语言；请求根不替换完全/未识别孤儿归属不迁；混合代际可读（组合实测，其余源码证明）。
- **复现/回归入口：** ROOT_AND_LOCALE succeeded actual zh-CN expected en-US；artifact://133。

### R20 [P1] 完整性检查漏units/terms/错误值，repair最终复核漏用户

- **证据级别：** 实测 Store +源码。
- **位置：** `crates/store/src/library_operations/integrity.rs:138-174,282-382`。
- **根因：** 只查item/user存在；最终remaining只有item有无，非完整事实对照。每批先物化全Library。
- **影响：** 删unit仍clean；错误计数/terms不发现，repair可能虚假清clean；max_media非工作量上界。
- **复现/回归入口：** MISSING_UNITS before1→0，succeeded clean空报告；artifact://133。

### R21 [P1] Media合并丢新快照/锁冲突与失败恢复失守

- **证据级别：** 实测 Store +源码。
- **位置：** `crates/store/src/media_operations/references.rs:164-194；apply.rs:66-70,215-225；preflight.rs:184-207`。
- **根因：** 冲突snapshot无条件删loser，不比provider/fetched_at；overrides同键静默删；失败state=failed被恢复/holder排除。
- **影响：** 较新合法来源丢失；人工锁冲突未拒；阶段失败后冻结无自动续跑且新操作可能接管。
- **复现/回归入口：** 同provider loser fetched2000，merge succeeded选择survivor1200；artifact://133。选定目标无snapshot pending provenance仍未实现为源码结论。

### R22 [P2] 非刷新目标别名不能被扫描重新识别

- **证据级别：** 实测 Store。
- **位置：** `crates/store/src/media/mod.rs:350-375；media/alias.rs:9-13`。
- **根因：** 先由全alias找到Media，随后拿refresh-only平面ID与incoming比较冲突。
- **影响：** 合法保留alias100、target999时NFO100被拒，无法重复导入。
- **复现/回归入口：** NON_TARGET_ALIAS Protected Ambiguous/conflicting；artifact://133。

### R23 [P1] 协议谓词与实际显示/协议ID语义不一致

- **证据级别：** 实测 SQL +源码。
- **位置：** `crates/store/src/library_query/protocol/predicates.rs:188-198；order.rs:38-43；facts.rs:94,212；sql.rs:23-39；api/src/media_server_provider/selection/mod.rs:136-145`。
- **根因：** Unicode needle与ASCII lower不对称；Name插原串tie抢后续键；genre UUID用lower后的名字；episode/locale筛选用work/首语言；missing选中后水合丢弃。
- **影响：** ÉCOLE搜école=0；/Genres Drama UUID过滤=0；A/a忽略DateCreated；分集简介搜不到、total/页不符。
- **复现/回归入口：** Unicode、genre、多键顺序实测artifact://116；episode/locale/availability为源码证明，未运行。

### R24 [P1] PersonIds保留现场Catalog/NFO整候选路径

- **证据级别：** 源码证明，未运行。
- **位置：** `crates/api/src/media_server_provider/selection/mod.rs:86-89,195,292-300；selection/page.rs:144`。
- **根因：** PersonIds显式绕开已实现SQL，测试甚至固定期待Catalog故障500。
- **影响：** 本地采用人物查询依赖网络，清cache/断网改变可用性，违背本地读取合同。
- **复现/回归入口：** 使用现有SelectiveCatalog先采纳人物，断网后真实/Items?PersonIds应同ID/total200；现有audit测试期待500须替换。

### R25 [P1] 用户删除恢复吞错且不消费持久phase

- **证据级别：** 源码证明，未运行。
- **位置：** `crates/api/src/user_removal.rs:44-66,94-120；crates/api/src/job_loop.rs:150-159`。
- **根因：** 恢复忽略record内容而重新start、新op覆盖；用户已删记录仍在时get_user早退；任何恢复错误let_吞并Ok，启动后无轮询重试。
- **影响：** 中断删除可永久残留；跨库失败没有可见失败或恢复保证。
- **复现/回归入口：** 在subscribe写锁/最后删User后delete_setting前两个边界恢复；断言稳定operation、禁用、失败可见、最终清理。

## Standards（独立轴）

- **S01 / P1：锁内冷IO。** `api/src/scrape_metadata/export.rs:48-57,77,97` 持全局Store mutex，调用`metadata/export_driver.rs`的read/digest/sync_all/rename；scan `runner/walk.rs:190-194`→`watch_ledger::record_paths`→NFO/stat/artwork；`store/src/scan_probe_obligations.rs:77-83`还在事务中discover artwork。源码证明，未制造慢NAS。违反锁外IO合同；不把“事务外”误作“全局锁外”。
- **S02 / P1：checker是假有界。** `library_operations/integrity.rs:138-174`全库三查询BTreeSet后take，最后全库remaining；每批`max_media=1`也全量读。源码证明，未跑100k checker。违反每批≤100 Media且单Media内部有界要求。
- **S03 / P2：生产函数超硬限。** `metadata/compose.rs:10-142` 133物理行；`library_operations/integrity.rs:124-248` 125物理行。按函数完整起止计，超过AGENTS的120；这不替代功能缺陷，按加载/合成及分页/复核职责拆。变化树Rust文件均≤800，不重复旧13文件超限结论。
- **S04 / P1：恢复静默失败。** `api/src/user_removal.rs:117`吞掉失败、`:120`返回Ok，外层error日志不会触发；与R25同根因但此处按不得静默unexpected failure规则独立登记。

## 已修项，不重新开旧问题

- domain::LedgerRow与旧insert_ledger写入口已移除；FileCoverage替换完成不是零缺陷证明。R01/R04/R05是生产接入断层，不声称旧类型仍在。
- 增量路径已扇出用户摘要；R03是同批共享新unit计数回归，不沿用“从未更新用户摘要”。
- 显式session事件已进commit_playback_event；显式E02 membership、旧源拒写、重复Stop事务守卫已有实现。R06/R07/R09分别指协议流、无ID逻辑冲突、Start恢复包。
- 后端文件分页limit+1/EOF已修；R14是实际Web adapter仍错，不再报告exactly50服务器问题。
- identity edit保alias、release_date文本排序已修；R22是扫描读refresh-only形状产生的新冲突。
- 普通Jellyfin Items等已有生产SQL调用；不再泛称query仅供测试。PersonIds特定旁路见R24。
- 共享cache DDL、严格拒旧、cache-only停机重建runbook及受控备份case保留，不重新实现。

## 本轮运行记录

临时crate `storage-rereview`通过path依赖当前源码，临时fixture全部在系统temp；运行命令：

```sh
cargo run --quiet --manifest-path "$REVIEW_ROOT/Cargo.toml" --target-dir target --offline
cargo test --quiet --manifest-path "$REVIEW_ROOT/Cargo.toml" --target-dir target --offline --test query_review -- --nocapture
cargo test --quiet --manifest-path "$REVIEW_ROOT/Cargo.toml" --target-dir target --offline --test playback_review --test merged_review --test api_review -- --nocapture
node "$REVIEW_ROOT/web-review.mjs"
```

运行器组合中曾有缺临时依赖/import、scope编码与重复CAS的夹具错误；修正后取得上列反例，编译错误不计产品finding。`artifact://133`为Store9场景，`artifact://127`为真实Router/Catalog及多集/会话/扫描，`artifact://156`为旧源真实绑定后的source_changed→Start及正确Device header首次Jellyfin完成，`artifact://116`为SQL，JS输出为100/101与missing可播放。本页实际数值为持久证据；内部artifact不保证跨会话可用，永久回归按[详细技术方案](../superpowers/plans/2026-10-22-storage-repair-remediation.md)在现有fixture新增，不把临时runner提交为产品测试。

## 文档处置与历史问题

撤下旧`2026-10-20-storage-clean-cutover.md`执行文档：其原步骤/接口由Git保留，独有修复记录、决策及历史门禁迁入本目录历史归档，不删除反例或伪造闭环。规范、合同、benchmark、ADR、独立Subscribe/CDP/DB8要求保留。其他旧文档无充分证据可安全整篇删除，故不按年龄批量删除。

旧执行记录已披露但未在本轮重放的`DELETE /catalog/cache?source=unknown`返回500、仅api_key的Web session stream返回401，仍保留历史未闭环，纳入新计划最后验收；不能标为本轮新发现。任何P1未通过公开生产seam前，不宣布clean cutover完成。

## 修复进度（2026-10-22，feat/storage-redesign）

只记录已提交且有回归的部分。“已修已验”=对应回归在本分支通过；“部分”列出剩余缺口。浏览器/真机/NAS/100k基准均Not run。

| ID | 状态 | 提交 / 回归入口 |
|---|---|---|
| R01 | 已修（d1c03de，生产scan/Probe接CAS） | `api --test storage_write_runtime` |
| R02 | 已修已验 | `store --test library_reconfiguration initial_zero_scope_is_rejected_after_reconfiguration` |
| R03 | 已修已验 | `store --test file_publication existing_card_batch_two_versions_of_new_episode` |
| R04 | 已修已验（Downloader端到端Not run） | `api --lib restarted_pending_a_with_fresh_candidate_does_not_block_b` |
| R05 | 已修（03de319补真实claim route与S00E00） | `api --lib claim_coverage_follows_the_file_name_and_media_kind` |
| R06/R07 | 已修（95495b4） | `api --test review_remediation_playback`、`playback_event_identity` |
| R08–R10 | 已修（dbce599） | `api --test playback_watch_time`、`review_remediation_playback` |
| R11 | 已修已验 | `store --test playback_assertions event_activity_clock_is_seconds_not_milliseconds` |
| R12 | 已修（3741bd4，Store/API决议）；Series协议筛选按集归约未单独回归 | `store --test playback_assertions` |
| R13 | 已修（6b1f79d） | `api --test review_remediation_playback` |
| R14 | 已修已验（浏览器Not run） | `node --test web/test/library-file-pagination.test.mjs` |
| R15/R16 | 已修（36fe331、db1328a） | `api --test metadata_priority`、`store --test metadata_adoption` |
| R17 | 已修（89cff86）：已入账视频只经driver导出，begin校验file/source/config/media，磁盘变化或无效XML拒写保字节，先register后终态；**未做**：锁外IO（S01导出侧） | `api --test storage_nfo_export normal_refresh_preserves_invalid_external_sidecar` |
| R18 | 部分（e0c8d1f）：编辑按上次导出baseline三方比较；**未做**：局部(按Library/input)覆盖、删除NFO解除覆盖 | `api --test storage_nfo_export provider_title_changes_when_only_plot_was_edited` |
| R19 | 已修已验（523cbcb、8bcfe2d、16b8689、5134ca8）：roots+locale同提交；unidentified/metadata inputs独立迁移；未覆盖文件预检拒绝；失败批回滚、保freeze、同operation可续 | `store --test library_reconfiguration_roots`、`library_reconfiguration a_failed_batch_rolls_back_keeps_the_freeze_and_stays_recoverable` |
| R20/S02 | 已修已验（29a43ed）：每Media在savepoint内重归约对比items/units/terms/user行（排除*_revision），keyset分页≤batch，repair逐Media复核；100k测量Not run | `store --test library_integrity_facts missing_units_and_terms_are_detected` |
| R21 | 部分（be68cd5、923e6c4、05200ce）：同实体新快照胜/tie留survivor；人工锁冲突拒绝、互不冲突合并；失败operation保freeze可续不被接管；**未做**：异实体目标季集校验、pending fallback逐字段决议 | `store --test media_merge_snapshots` |
| R22 | 已修（5cbc133） | `store --test media_alias_updates` |
| R23 | 部分（2013e93、7ccb7f7）：Unicode casefold（SQLite函数）、Name原串tie置于全部请求键后、genre id按原名解析；**未做**：episode/locale级谓词、missing选中行水合丢弃 | `api --test jellyfin_sql_page` |
| R24 | 已修已验（9e49994、b09924c）：PersonIds走SQL本地事实，删除Catalog候选路径；替换“Catalog失败500”测试 | `api --test jellyfin_paged_metadata audit::person_filter_is_local_and_survives_catalog_failure` |
| R25/S04 | 已修（2884348、37f4f73、e737e8e）：DELETE同步禁用/吊销后202，后台阶段驱动；`GET /users/removals/{op}`；终态结果保留7天；job loop 60s重试；Web适配器返回DTO、成员页仅succeeded移行 | `api --test users`、`node --test web/test/user-removal.test.mjs` |
| S01 | 部分：观察侧（d1c03de）已锁外；NFO导出仍在Store锁内读盘/写盘 | — |
| S03 | 已修（compose 36fe331；integrity拆分后各函数≤120行） | — |
| Task13历史项 | 已修（e2a1329） | unknown cache source 400、嵌套路由query api_key |

全门（本轮）：`cargo test --workspace` 见提交说明；`api --test reconciliation_budget reconciliation_revalidates_off_lock_scope_freeze_and_source` 在基线2884348即6/6失败（既有问题，本分支间歇通过）；`subtitle_runtime both_waiters_cancelled…` 为偶发超时，单跑通过。Web `npm run typecheck`、`npm test`通过；lint/build未跑。
