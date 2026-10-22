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
| R12 | 已修（3741bd4，Store/API决议）；Series协议筛选按集归约已回归（53616b4） | `store --test playback_assertions` |
| R13 | 已修（6b1f79d） | `api --test review_remediation_playback` |
| R14 | 已修已验（浏览器Not run） | `node --test web/test/library-file-pagination.test.mjs` |
| R15/R16 | 已修（36fe331、db1328a） | `api --test metadata_priority`、`store --test metadata_adoption` |
| R17 | 已修（89cff86）：已入账视频只经driver导出，begin校验file/source/config/media，磁盘变化或无效XML拒写保字节，先register后终态；**未做**：锁外IO（S01导出侧） | `api --test storage_nfo_export normal_refresh_preserves_invalid_external_sidecar` |
| R18 | 部分（e0c8d1f）：编辑按上次导出baseline三方比较；**未做**：局部(按Library/input)覆盖、删除NFO解除覆盖 | `api --test storage_nfo_export provider_title_changes_when_only_plot_was_edited` |
| R19 | 已修已验（523cbcb、8bcfe2d、16b8689、5134ca8）：roots+locale同提交；unidentified/metadata inputs独立迁移；未覆盖文件预检拒绝；失败批回滚、保freeze、同operation可续 | `store --test library_reconfiguration_roots`、`library_reconfiguration a_failed_batch_rolls_back_keeps_the_freeze_and_stays_recoverable` |
| R20/S02 | 已修已验（29a43ed）：每Media在savepoint内重归约对比items/units/terms/user行（排除*_revision），keyset分页≤batch，repair逐Media复核；100k测量已跑（a6afd79：分页改为各表索引游标LIMIT、preferred子查询避开files_presence全库扫描）：seed 149s，检查1000批共122s，最慢批215ms，前10批1.19s/后10批1.00s | `store --test library_integrity_facts missing_units_and_terms_are_detected` |
| 搜索扇出窗口 | 已修（fe2e4f5）：派生前预留search permit，窗口不再因任务异步取permit被重复扣减而缩小（request_admission/title_search 负载下偶发失败） | `api --test request_admission`、`api --test management title_search_beyond_the_window` |
| R21 | 部分（be68cd5、923e6c4、05200ce）：同实体新快照胜/tie留survivor；人工锁冲突拒绝、互不冲突合并；失败operation保freeze可续不被接管；**未做**：异实体目标季集校验、pending fallback逐字段决议 | `store --test media_merge_snapshots` |
| R22 | 已修（5cbc133） | `store --test media_alias_updates` |
| R23 | 部分（2013e93、7ccb7f7）：Unicode casefold（SQLite函数）、Name原串tie置于全部请求键后、genre id按原名解析；**未做**：episode/locale级谓词、missing选中行水合丢弃 | `api --test jellyfin_sql_page` |
| R24 | 已修已验（9e49994、b09924c）：PersonIds走SQL本地事实，删除Catalog候选路径；替换“Catalog失败500”测试 | `api --test jellyfin_paged_metadata audit::person_filter_is_local_and_survives_catalog_failure` |
| R25/S04 | 已修（2884348、37f4f73、e737e8e）：DELETE同步禁用/吊销后202，后台阶段驱动；`GET /users/removals/{op}`；终态结果保留7天；job loop 60s重试；Web适配器返回DTO、成员页仅succeeded移行 | `api --test users`、`node --test web/test/user-removal.test.mjs` |
| S01 | 部分：观察侧（d1c03de）已锁外；NFO导出仍在Store锁内读盘/写盘 | — |
| S03 | 已修（compose 36fe331；integrity拆分后各函数≤120行） | — |
| Task13历史项 | 已修（e2a1329） | unknown cache source 400、嵌套路由query api_key |

全门（本轮）：`cargo test --workspace` 见提交说明；`api --test reconciliation_budget reconciliation_revalidates_off_lock_scope_freeze_and_source` 在基线2884348即6/6失败（既有问题，本分支间歇通过）；`subtitle_runtime both_waiters_cancelled…` 为偶发超时，单跑通过。Web `npm run typecheck`、`npm test`通过；lint/build未跑。

## 本轮增量：锁风险修复（基线 999acdb）

- 生产 Probe 派发丢弃路径曾在 `match store.lock().load_probe_dispatch_snapshot(...)` 的分支中取得 `seen`，与强制入队的 `seen -> Store` 构成反向锁序。现在先取 owned 查询结果并释放 Store，再处理 `seen`；`is_queued` 错误回退同样消除嵌套锁。新增竞争回归在修改前失败（持有 Store 等待 seen），修改后随 API 单元测试通过。
- 历史 12 处锁跨 await 候选中，4 处代理配置测试确实跨异步挂起持有进程全局锁，改为同步测试持有隔离锁、在内部 runtime 驱动请求；其余8处显式 drop 改词法作用域，不将这些已提前 drop 的位置误报为已复现死锁。部分阻塞测试屏障/通道增加有界等待，失败不再无限拖住 runtime shutdown。
- 验证：`cargo test -p library` 113 passed；`cargo test -p api --lib` 169 passed。workspace 全 target 锁专项 Clippy 通过：`cargo clippy --workspace --all-targets --locked -- -A clippy::overly_complex_bool_expr -D clippy::await_holding_lock`。未放宽锁 lint，也未在源码加 allow。普通 Clippy 被未修改的 `release/src/boundary.rs:119` 布尔表达式 lint 阻断；因此不声称普通 Clippy 全绿或其余警告归零。
- 此记录只覆盖本轮锁补丁，不替代数据库重构全量审查；其他 cutover 改动另行验证。

## main 基线增量复审与后续设计（本轮）

比较基线为 `main` 的 merge-base `86122d73a4a23439e1d83234590fdd98b388fc50`，起始被审提交 `999acdb`。用户随后要求先提交合理修复，再输出审查与详细设计。本节区别已修补丁与仍待设计的问题，不将历史 R01–R25 再报为新缺陷。

### 设计判断

Media／Library／Playback 放在同一 `library.db`、cache 按生命周期独立、物理文件与覆盖单元分离、局部同事务维护派生摘要，方向合理。DDL 的 root/Library 与 Media/kind 复合外键能防止单纯 ID 错配；但不能保证路径真属于 root，也不能代替所有写入口的生命周期与源版本校验。跨文件操作必须使用持久阶段恢复，不能把进程 Mutex 当作跨数据库原子提交。

### 本轮保留的修复

- 删除扫描旧 scope 编码的识别/升级/CAS/报告链，仅精确匹配当前编码。旧签名测试在修改前实际扫描并新增1文件，修改后取消且零登记。
- Store LEFT JOIN 的空覆盖原本由 `maps::map_file_unit` 补成 whole；现区分 `None` 与显式 `(-1,-1)`，保存未知文件但不伪造单元。同步去掉 Subscribe/NFO/管理/搜索/重复检测与 Web 的空覆盖兼容兜底。TV S00E00 继续保留。
- 管理台账与本地文件搜索一物理文件一行、完整 units；Web Library/Media/User/Filter UUID 不再 Number 转换；无效 numeric member filter 返回400；删除 facets 的 years 兼容键及 movie00 读取分支。`resolve_playback_for` 的 movie00 删除属于一致性清理：旧 `/decide` 与 `/sessions` 公开入口已有校验，不冒称新增复现漏洞。
- 不再认领无标记的已有 Jobs 布局、不再引导明文管理员凭据隐式轮换；当前合法哈希与 Bearer 分离检查保留。凭据检查同时验证 PHC 完整盐/输出与 Argon2 算法/版本/参数。

### 尚未修复的 Spec findings（按风险排序）

#### N01 / P1：重复文件删除只要求任一单元重叠

源码证据：`api/src/http/library_duplicates.rs` 的 `delete_duplicate`，siblings 通过 `.any(|unit| target_units.contains(unit))` 选出，再按物理实体个数判断能否删除。目标覆盖 E01+E02，另一个文件仅 E01，存在独立实体即可放行；剩余 E02 覆盖未证明。空覆盖修复不解决此问题。本轮未执行实际删除反例。

设计：在 Store 事务内捕获目标完整覆盖、版本及同 Library 的剩余可发布/可用文件；逐目标单元确认至少一个不同物理实体仍覆盖，不能用候选个数代替集合包含。硬链接同实体不当独立副本；空/unknown/pending 均不提供可删除证据。准入时建立删除意图并与该版本绑定，锁外执行文件删除，再事务清理事实/摘要，沿用既有删除恢复机制，不新建兼容路径。

验收：E01+E02 对 E01-only 拒绝且字节/台账不变；剩余两个独立文件分别覆盖 E01/E02 可删除；硬链接、missing、unknown、pending 不满足替代；捕获后版本变化拒绝旧删除；重启幂等恢复。

#### N02 / P1：Transfer 写 API 的 scope 与实际文件未闭合

源码证据：`store/src/library_write/transfer.rs:128-151` 的 `finish_transfer` 校验 scope Library 与 file version，但未核对持久文件 Library 等于 scope、未复核 Media 可写，随后发布实际 file_id 却归约 scope Library。`reserve_transfer` 也未先要求 candidate.library_id 等于 scope；主键冲突更新分支可修改另一条已有文件的 pending 状态。本轮为源码证明，未执行错 scope runner；不宣称外部请求可任意触达。

设计：reserve/finish 共用事务内校验：scope/config/lifecycle、实际 file Library/root/Media/kind、Media 可写、预期版本、reservation pending 状态、源路径与 mode。主键碰撞只允许同一持久 reservation 幂等恢复，禁止将无关 published 行重新置 pending。finish 用 `WHERE file_id + library_id + version + publication_state='pending'` CAS，必须影响1行，再按验证后的实际归属维护 coverage、probe 与摘要。

验收：A scope+B文件拒绝且两库事实/摘要不变；无关 file_id 碰撞拒绝；reserve 后 freeze/merge/retire 或换版本时 finish 拒绝；同一 reservation 重启继续仍使用原 file_id/source version；失败全事务回滚。

#### N03 / P1：重命名写者未维持显式归属和派生事实

源码证据：`store/src/ledger.rs:74-80` 的 `rename_file_path` 只 UPDATE path；无 root/lifecycle/Media 校验、无局部归约。调用者包括 organize 与 path reconciliation。隔离 Store 反例中跨 root rename 返回 `Ok(true)`；尝试补丁因未覆盖规范路径与 sidecar 一致性已撤回，仅保留 organize 对 `Ok(false)` 的错误/回滚处理。

设计：明确分开“同归属改名”和“跨 root/Library 改挂”。观察阶段锁外完成路径规范化、组件边界/符号链接解析、sidecar 清单与文件源版本捕获；同归属请求事务重验 root/config/Media/lifecycle/目标唯一性。跨归属必须走已有持久 operation，不隐式 UPDATE root。文件与 NFO/artwork 的磁盘变更登记持久意图、锁外执行、事务 CAS 发布路径/输入引用并归约同一 Media；磁盘失败可恢复，不能先改字节后吞写入错误。路径对外保留 display 拼写，path_key 采用同一规范策略。

验收：跨 root、嵌套 Library、symlink 目标、冻结、目标占用都拒绝且字节不变；普通改名后卡片 preferred path、NFO 输入、Subscribe 路径引用一致；各磁盘/提交边界崩溃可继续，不留半完成“成功”。

#### N04 / P2：完整性检查把合法稀疏用户摘要判为缺失

源码证据：`library_operations/integrity.rs:279-303` 对 Media 的每个 playback 用户要求每个有文件 Library 都存在用户摘要；而 `playback/user_summary/mod.rs:333-347,485-493` 明确只归约本 Library 拥有季/集并删除空摘要。用户仅收藏 S02，A库只有S01、B库有S02，A库缺行合法，却被反复报 missing；repair 的派生比较可仍判一致。本轮源码证明，未执行 runner。

设计：检测与修复共用同一 scoped summary 归约结果，expected row 应是 Option，None 表示合法缺省；禁止从“任一 playback 行存在”推导每库必有摘要。最终复核必须使用与检测相同的期望集合与值，不能修复后仅复核另一套口径。

验收：跨 Library 不相交季/集的收藏/观看不误报，whole 与 season 断言按拥有范围生效；真正缺失/错误用户行可发现并修复；check→repair→check 稳定 clean，未伪造其他用户状态。

### 尚未修复的 Standards finding

#### N05 / P2：完整性报告累积数组不受 REPORT_CAP 限制

源码证据：`integrity.rs:40-41` 承诺最多100个样本，但 `:272,275,300,322` 的 missing/orphan 四组数组无界 push；仅 mismatched_media 截断。分页工作有界不代表 cursor/report 空间有界。

设计：五类样本统一采用上限100的追加策略；精确总数独立持久化，不用样本长度推计数。保持 last_media_id keyset 游标、失败恢复与累计计数幂等，截断状态显式可观察。不要把整库坏行列表当持久 operation 游标。

验收：远超100个坏 Media/用户、batch=1和100及中断续跑，样本均≤100、精确计数相同、序列化大小有固定上界；最后一页修复失败不虚报 clean。

### 无兼容切换仍未完全收敛

剩余源码证据：`api/src/worker/transfer/run.rs:108-115` 在持久与携带 units 都空时仍补 WHOLE；`media_server_provider/items.rs:48-64` 的 coverage_units 同样补 whole，被 `selection/light.rs:252` 和人物图片读取消费。`web/lib/api/libraries.ts:1798-1815,1895-1903` 仍接受多组旧 DTO 别名（key/path/file_path、id/file_id、season_number/season）。这意味着本轮提交后仍不能宣布用户要求的“所有旧逻辑清零”。前两者是明确剩余空覆盖推断，Web 别名属于合同残留，不仅凭命名就宣称运行时已出错。

设计：Transfer 的空覆盖原样发布；若调用业务必须有明确覆盖，准入时拒绝而不补 whole。协议查询与水合保留物理文件但仅从明确 unit 生成逻辑可播身份，人物图片不能借未知单元冒充 work。Web 以实际后端分组/缺失清单 DTO 定义唯一字段，迁移全部渲染与请求调用方后一次删除别名适配；不静默返回空成功掩盖 schema 错误。验收覆盖 movie 显式 whole、TV S00E00、TV unknown、合并集文件及 unknown 的协议总数/页内结果一致；实际 API 模块对新响应保持字段且不读取旧别名。

### 验证边界

已实际执行：空覆盖/旧签名/未知Jobs/凭据公开 Store/Router 回归；Web 类型检查、841测试、生产构建；隔离生产进程 HTTP smoke（facets 无years、numeric member400、UUID member200、auth响应）；浏览器登录并点击UUID电影库成功。独立补丁审查发现并处理旧 Jobs 和 facets 测试契约残留。普通 Web lint 有15 errors/115 warnings（包括未改的 library-detail-view 空块和 request-signal 全局声明），普通 Clippy 有 release 布尔 lint 阻断；两者均不称全绿。全量 crate/workspace 结果另据本次最终执行输出登记，不把中途失败改写为通过。

最终单 crate 门：`cargo test -p store` 395 passed、2 ignored；`cargo test -p api` 1507 passed、5 ignored；`cargo test -p library` 113 passed。新增严格边界定向门（schema/password/NFO及HTTP/cache/Subscribe/scan）19+23 passed、1 ignored。`cargo fmt --all --check`通过；最终 `cargo test --workspace`：2519 passed、11 ignored（344 suites），中途失败均已保留在执行证据，不冒充首次全绿。

本轮不修改用户真实数据目录，不运行真实 Downloader/第三方 Catalog/NAS 压力验收。上列未修项保持 review/design 状态，不因已提交局部补丁宣称“所有旧逻辑已清除”或“系统不存在死锁”。
