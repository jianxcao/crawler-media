# 数据库存储重构再次复审：8fbf81b（历史）

本页保留该 SHA 的反例与验证，不是当前待办。2026-10-24 的逐项对账见[5bd696e 修复复核](2026-10-24-storage-fix-review.md)和[历史执行记录](../superpowers/plans/2026-10-23-storage-review-remediation.md)；现行问题及剩余方案见[2026-10-28 复审](2026-10-28-storage-main-rereview.md)。

日期：2026-10-23开始，2026-10-24收口。结论：**Request changes**。架构方向合理，尚有应优先修复的数据一致性和API切换问题。本轮只审查、清理文档并提出方案，不修改Rust/TS/DDL/测试，不操作真实实例。

## 范围与结论

- 比较基线：本地`main=86122d73a4a23439e1d83234590fdd98b388fc50`；被审HEAD：`8fbf81bdf2dd068dcbd07e4e29d21bae5e7accfa`。两者merge-base等于main；没有将远端origin/main当成本地main。本轮源码开始时工作区干净。
- 分支差异1092文件、+117725/-49706；围绕数据库、生产调用链、实际Web adapter和测试生命周期重点抽查，并非逐行证明整个分支无缺陷。历史文档仅供定位，当前结论来自本轮源码与隔离探针。
- 已执行的探针仅使用临时Store、真实Router/adapter、内存或fixture外部依赖；没有真实TMDB、Downloader、Chromium或用户数据库。实测与源码推导分别标注。
- 唯一剩余[修复技术方案](../superpowers/plans/2026-10-23-storage-review-remediation.md)；[当前数据字典](../database/README.md)；[历史性能报告](../benchmarks/2026-10-23-storage-redesign-report.md)。方案没有在本轮实施。

| 用户关切 | 当前判断 |
|---|---|
| 代码与数据库设计 | 五文件按生命周期分工、Media/Library/Playback同库、显式ownership、同事务摘要与冷详情合理。主要缺陷在跨阶段证据、路径键一致性和恢复协议，并不需要再拆服务或增加第二份投影。 |
| 完全去旧 | 数据库旧布局/表和多项旧API已删除；仍有真实Web旧字段、创建入口静默接纳、marks宽松降级、旧信封兼容。不能宣称已全部完成。 |
| 死锁与测试 | 原cancel和Transfer等待锁链已修。本轮未确认新增生产锁环；确认测试fixture join环（本机表现为隐藏线程panic），以及无写deadline和生产持Store磁盘IO。 |
| 数据库优化 | 已有索引/批末用户归约/分面合并/分页后选源/写入期协议索引有收益；优先修复正确性和锁边界，再处理genre扩展性。PRAGMA调参仍需A/B。 |

优先级：P1为持久事实错误、真实功能不可用或源版本隔离失效；P2为测试退出可靠性、错误信封和可用性/性能边界。F01–F05、F09、F10为P1，F06–F08为P2。F07虽有严重慢挂载风险，当前没有新的生产死锁复现。

## F01 / P1：Web仍发后端已经拒绝的Subscribe字段

证据：真实Web adapter请求 + 真实Router定向测试/探针。

- [subscriptions.ts:599](../../web/lib/api/subscriptions.ts#L599)仍发`selected_seasons`，实际调用者是[TV调整弹窗:157](../../web/components/subscription-adjust-dialog.tsx#L157)；后端[PATCH DTO:82](../../crates/api/src/http/subscriptions/types.rs#L82)已拒未知字段，调整失败400。
- [subscriptions.ts:657](../../web/lib/api/subscriptions.ts#L657)仍发`rule_set_id`，[Wash-cut弹窗:81](../../web/components/upgrade-run-dialog.tsx#L81)换Filter触发该路径；后端[UpgradeInput:28](../../crates/api/src/http/subscription_depth.rs#L28)Filter字段只收`filter_id`（另有合法`wash_cut`）。空body使用原Filter的分支不受此缺陷影响。前端`:639`报告类型也仍是旧字段。
- [api-contracts测试:36](../../web/test/api-contracts.test.mjs#L36)反而断言`rule_set_id`，所以现有Web全绿掩盖了断链。
- [CreateSubscriptionInput:3](../../crates/api/src/http/subscriptions/types.rs#L3)没有`deny_unknown_fields`；[StrictJson:20](../../crates/api/src/http/strict_json.rs#L20)只改变拒绝的HTTP状态，不会自动拒未知字段。真实创建携合法coverage和旧`selected_seasons/rule_set_id`仍201，旧值被忽略并采用默认Filter。

实测adapter输出：`upgrade-runs → {rule_set_id:"filter-id"}`，`PATCH → {selected_seasons:[2]}`；Router现有回归明确两者400。

修复：前后端统一`filter_id/coverage`和真实回执；创建/PATCH/upgrade都严格拒旧字段。替换错误测试，增加实际adapter→Router行为验证，不恢复别名。换Filter、换TV季应成功；旧输入400且零写。

## F02 / P1：根分批迁移删除后续批次的NFO输入

证据：当前Store实测，canonical临时路径对照也复现。

[roots.rs:146](../../crates/store/src/roots.rs#L146)先将全部scoped输入迁到目标Library；文件每批只迁100个；[批末:335](../../crates/store/src/roots.rs#L335)调用完整归约，[prune_nfo_inputs:262](../../crates/store/src/library_write/reduce/card.rs#L262)把后续文件目录的输入视为孤儿并删除。

同一TV Media，目录A 100文件、目录B 1文件及episode NFO：

```text
before: episodeB=1
create nested + full drain succeeded: nested_files=101, episodeB=0
```

实际XML仍在，丢失的是已采用输入及其本地事实；不能以以后重扫可能恢复关闭问题。

修复：文件/覆盖与对应输入在同一批事务迁移；共享work/season输入根据两边剩余覆盖复制/保留。归约不得清除尚未处理的计划输入。跨至少3批、同Media多目录、locale/provenance/export digest和每批重启必须全部守恒。

## F03 / P1：冻结目标使cursor确认未迁完工作

证据：当前Store、正式locale operation与崩溃测试seam实测。

队列[roots.rs:153](../../crates/store/src/roots.rs#L153)只存kind/root/cursor；每批重新从[ready_roots:223](../../crates/store/src/roots.rs#L223)选目标。目标正在reindexing时被排除，[候选处理:279](../../crates/store/src/roots.rs#L279)可能保留父归属，随后[游标确认:293](../../crates/store/src/roots.rs#L293)仍推进甚至删除。

```text
after first 100/150 files: parent=50, nested=100, ready, pending=1
freeze target via locale → drain → complete locale:
parent=50, nested=100, ready, pending=0
```

结果是永久漏迁且无pending工作。创建先提交ready再drain，也没有覆盖整个迁移的正式冻结/状态合同。

修复：纳入持久根operation，固定目标、参与Library/config revision、作用域与状态；相关源/目标受写入屏障保护。目标暂不可写/版本变化时等待或失败并保留cursor，禁止换成另一目标后确认。验收root/locale/removal交错、每批重启、新写入落在cursor之前、失败后可见重试。

## F04 / P1：Probe发布把当前版本重新当作旧计算的expected

证据：生产调用链源码推导，未在本轮跑完整门控Probe端到端复现。

1. [markers.rs:223](../../crates/api/src/probe_manager/markers.rs#L223)读取r1/vA样本并锁外计算。
2. 同路径替换使[source.rs:131](../../crates/store/src/library_write/source.rs#L131)推进到r2/vB并清旧派生数据。
3. 发布前[markers.rs:69](../../crates/api/src/probe_manager/markers.rs#L69)调用[source_observations:162](../../crates/api/src/probe_manager/markers.rs#L162)，重新取得r2/vB作为expected。
4. [fingerprint_job/source.rs:15](../../crates/api/src/fingerprint_job/source.rs#L15)只比路径/Media/覆盖，未比计算输入FileVersion；[Store守卫:134](../../crates/store/src/library/markers/source_guard.rs#L134)比较r2/vB与r2/vB，于是不能拒绝A的结果。

普通指纹[probe.rs:661](../../crates/api/src/probe_manager/probe.rs#L661)也在位置校验后无版本CAS写cache，旧采集可重建/覆盖换源已清缓存；读取key校验仍可阻止部分旧值被复用，不声称所有旧指纹都会被Playback采用。普通tracks换源已有source string拒绝，不能把这条Marker缺陷直接套成tracks必现；其磁盘探测前后token验证仍需补验收。

修复：计算/采集前固定原始版本、路径、Library/config revision和完整输入成员；最终事务消费这些原expected，另核对锁外实际磁盘观察。旧结果拒绝并重新派发，不能发布前刷新expected。现有Store手动旧snapshot测试不能替代真实生产链。

## F05 / P1：marks错误坐标被写成整项已看

证据：真实Router、三个独立TV Media，读取持久原始行。

[watch_state.rs:194](../../crates/api/src/http/playback/watch_state.rs#L194)用`as_i64().unwrap_or(-1) as i32`解析，旧字段/坏类型默认whole，溢出截断；[watch_unit:127](../../crates/api/src/http/playback/selection.rs#L127)对TV缺少范围/坐标组合验证。GET marks[同文件:89](../../crates/api/src/http/playback/watch_state.rs#L89)也静默降级。

| 输入（均另带合法Media和played=true） | HTTP | 实际持久结果 |
|---|---:|---|
| `season:2, episode:3`旧键 | 200 | `season=-1, episode=-1, played=true` |
| `season_number:"bad", episode_number:"bad"` | 200 | 同上 |
| 两个坐标均`4294967295` | 200 | 同上 |

修复：GET/POST共用严格marks目标解析，区分省略、whole、season、episode；拒旧键/类型/范围/非法负数/单独episode，不允许整数截断。保留合法TV S00E00、整季和明确whole；不能直接复用要求完整分集坐标的session校验。坏请求400且原始assertion/恢复包和其他分集零写。

## F06 / P2：测试HTTP fixture有join环和无界写等待

证据：rustc直接引用当前真实[lifecycle.rs](../../crates/api/tests/support/lifecycle.rs)的独立探针，每模式5秒外部进程watchdog。

[MockQbit:63](../../crates/api/tests/subscription_e2e/mock_qbit.rs#L63)及[MockTransmission:37](../../crates/api/tests/subscription_e2e/mock_transmission.rs#L37) handler upgrade若成为最后Arc owner，会在connection线程析构owned FakeServer。[Drop:168](../../crates/api/tests/support/lifecycle.rs#L168)等accept，accept在[:142](../../crates/api/tests/support/lifecycle.rs#L142)等该connection：`connection → accept → connection`。

本机macOS表现为accept线程panic `Resource deadlock avoided (os error 11)`，join错误被吞，进程exit0；**没有实测本机永久挂死**。另[respond:105](../../crates/api/tests/support/lifecycle.rs#L105)无write timeout；64MiB响应的peer不读时，Drop在750ms内不返回，关闭peer后返回。read timeout不覆盖write或整请求deadline。

修复：分离handler共享状态与server owner，避免worker析构并join自身所属server；关闭时shutdown活跃socket，设置读/写/总请求deadline，检查join错误。RAII gate已修不等于server退出已闭环。外部watchdog覆盖最后owner、慢/半请求、peer不读与panic退出。

## F07 / P2：事务分段仍未释放外层Store，磁盘IO仍持锁

证据：当前生产调用链源码；未测真实NAS延迟，未确认新增生产锁环。

- [housekeeping:194](../../crates/api/src/http/library_housekeeping.rs#L194)持guard调用[apply_reconciliation:351](../../crates/api/src/http/library_housekeeping.rs#L351)，包含begin/disk/sidecar/complete。
- organize receipt中段已拆锁外，但[begin调用:244](../../crates/api/src/http/library_organize_apply.rs#L244)与[complete:151](../../crates/api/src/http/library_organize_apply.rs#L151)仍持guard；[rename_durable:111](../../crates/store/src/library_write/rename_durable.rs#L111)、[:274](../../crates/store/src/library_write/rename_durable.rs#L274)有stat/read_dir/STRM读取；[finish:142](../../crates/store/src/library_write/rename_phases.rs#L142)仍删staging。
- [season.rs:169](../../crates/api/src/probe_manager/season.rs#L169)、[markers.rs:279](../../crates/api/src/probe_manager/markers.rs#L279)仍锁内观察source。所谓ledger-only[source检查:31](../../crates/api/src/fingerprint_job/source.rs#L31)调用路径解析，间接stat/canonicalize。
- [startup:343](../../crates/api/src/management/state.rs#L343)持一个guard耗尽所有root batches；[:351](../../crates/api/src/management/state.rs#L351)耗尽到期rename并执行IO。create/root入口也同步drain全队列。

影响：慢挂载可阻塞所有需要Store的请求、心跳和恢复；每事务≤100候选不等于总锁时长或工作量有界。前置NFO/Unidentified迁移仍全量；完整归约可重读同Media全部文件/units。

修复：Store方法只消费owned evidence并短事务CAS；IO完整移到预算池。持久文件按library_id/root_id验证，不重新解析磁盘归属。driver一次有限窗口、批间释放Store，保留阶段/cursor；用门控IO观测并行查询/心跳，不能只增加busy_timeout或HTTP超时。

## F08 / P2：Web仍接受旧信封/裸payload并伪空

证据：源码与真实adapter探针。

[catalog-cache.ts:30](../../web/lib/api/catalog-cache.ts#L30)接受裸数组或data数组，坏合同返回`[]`；[health.ts:13](../../web/lib/api/health.ts#L13)兼容裸对象；[http.ts:149](../../web/lib/http.ts#L149)及多个API模块仍接受/声明success旧信封。当前后端返回ok/data。

```text
catalog_accepts_old_array=[{id:"old-array"}]
catalog_hides_bad_contract=[]
```

修复：唯一ApiEnvelope类型与成功检查；移除裸对象/数组、success旧合同，损坏payload明确报错，保留明确204和合法业务空值/UI默认。全部fixture同步当前信封，避免兼容代码与旧测试互相证明“绿”。

## F09 / P1：重启复活用户已经移除的内建根

证据：当前Store正式重配置→关闭→重开实测，与main源码对比。

[schema.rs:65](../../crates/store/src/schema.rs#L65)每次open选首个同kind Library；[:86](../../crates/store/src/schema.rs#L86)检查内建路径是否存在，不存在便[:96](../../crates/store/src/schema.rs#L96)重新加入。main仅在该kind无任何root时seed。

```text
formal roots operation succeeded; before reopen=[custom], config_revision=1
after Store::open=[custom, data/library/movies], config_revision=1
```

这绕过正式重配置/代际，改变扫描与配置范围；是否改变Transfer主根取决于排序，不能写成必现。

修复：只在全新kind初始化默认Library及其根，事务提交；已有用户配置不补回默认路径。验收替换默认根/删除默认Library/多Library后重启，根集合、顺序、default标记与revision完全不变。

## F10 / P1：NFO路径未统一canonical导致有效输入误删

证据：实际测试失败、真实Store显式symlink探针及canonical路径对照。

[record.rs:32](../../crates/store/src/metadata/nfo_input/record.rs#L32)只normalize display视频路径；[observe.rs:28](../../crates/store/src/metadata/nfo_input/observe.rs#L28)sidecar input_key保留display拼写，[:208](../../crates/store/src/metadata/nfo_input/record.rs#L208)原样入库；[prune:275](../../crates/store/src/library_write/reduce/card.rs#L275)与canonical文件path_key比较，首个full reduce立即删掉有效输入。

canonical临时目录内另建real/alias软链，root注册alias：`observations=1, persisted_inputs=0, detail_plot=None`。macOS `/var → /private/var`只是现成触发条件；显式alias证明不依赖该平台特例。F02在全部canonical路径下仍复现，两项独立。

修复：观察计划从同一RootAliases快照准备视频/sidecar canonical key，display路径只用于锁外IO。录入/重扫/删除观察/根迁移/归约统一键。验收首次发布、改NFO/删除、祖先共享、locale保留及real/alias切换。

## 前几轮状态对账

| 原任务 | 本轮处理 |
|---|---|
| S01/S02、A1/A2 | 原Probe删除guard及cancel锁环、Transfer持Subscribe等idle链已有修复；抽查确认A1锁外cancel、A2锁外等待，不重复开原项。 |
| A3/A4/A5/A9 | IMMEDIATE、marks时钟统一、同源duration和元数据locale/卡片归约已有代码/回归；本轮没有把旧C03–C06/C12–C13重复报为当前问题。F05是目标解析的新缺口。 |
| A6 | 共享祖先NFO复制、100候选/事务和cursor已实现，但“DONE”撤回：F02/F03/F10及F07仍开。 |
| A7/A10/A11 | 部分实现；锁外receipt、RAII gate、手动旧snapshot CAS已存在，仍被F04/F06/F07反例限制。 |
| A8/A12 | 不直接判当前行为坏；竞争写者旧代码行为RED、NFO export失败区分性RED仍缺。旧代码A12测试也通过，不能以此证明回归测试有区分性。 |
| B1/B2/B4/B5 | 定向读取、观看视图、真实回执/claim action、强类型merge计划已落地；旧假adapter/路由/补当前版reidentify不再开项。 |
| B3 | source别名、played字段、reidentify版本固定等已修；“前端全切换/信封全扫完成”撤回，F01/F05/F08尚开。 |
| Q1/Q2/Q3a/Q3b/Q4/Q5 | 反向索引、批末用户摘要、studio投影、facet合并、后置选源/协议索引、Store连接FULL与删除分批均已有；不重演全部旧步骤。genre线性、物理参数A/B和整体规模门仍开。 |

本表不是对所有历史finding做新的全量行为验收。旧版本独有反例和测量保留在历史审查/benchmark；已被替代步骤文档删除，Git可恢复。

## 下一步数据库优化判断

1. 先修F02/F03/F09/F10及F04，避免把错误结果做得更快；同时完成真实API切换和测试退出边界。
2. 根/rename driver按批释放全局Mutex；记录最大Store持锁时间、事务p95、busy错误与心跳p95。针对同Media大覆盖单独测归约，100候选并不限制派生工作量。
3. genre仍有精确候选验证/排序/count线性成本。历史5k/20k/50k为21.3/85.4/221.0ms；约70k超过300ms是外推，非实测。可评估按genre/有效语言事实的有序索引或物化成员读模型；必须保留ACL、locale、跨库去重和精确total，keyset不能消除全量精确count。
4. person历史50k为6.0ms；substring search为58.7ms。暂不为它们引入FTS5；FTS词项匹配与任意子串不等价，只有真实预算不达标才另做等价方案。
5. cache NORMAL、mmap、cache_size、temp_store、WITHOUT ROWID保持可选A/B，报告读写/WAL/内存/耐久取舍，不设未经测量的全实例参数。Store连接显式FULL不作为性能优化被降级。Jobs Queue目前仍未显式设置synchronous（`jobs/src/queue.rs::apply_connection_policy`），需补连接级耐久性门；不能把bundled默认值写成已显式统一。

## 本轮验证与未测边界

| 验证 | 当前结果 |
|---|---|
| `npm test`（web） | 842通过/0失败；含旧合同错误断言，不能据此关闭F01。 |
| `npm run typecheck`（web） | 通过。 |
| API management `subscription_depth::upgrade_runs_report_matches_facts --exact` | 1通过/0失败，591 filtered；明确旧upgrade/PATCH字段400。 |
| `cargo test --offline -p store --test root_reassign_batched` | 1通过/0失败；现有不同Media无NFO场景漏掉F02/F03。 |
| `nfo_scoped_inputs`默认macOS TMPDIR | 1通过/6失败；nested case单跑也失败。 |
| 同一NFO binary，仅TMPDIR改realpath | 7通过/0失败；不是源码修复，印证F10。 |
| Store独立探针，canonical TMPDIR | F02/F03/F09/F10均复现；F02另有canonical独立对照。 |
| Router/实际Web adapter独立探针 | F01/F05/F08复现；F05读取持久whole行，创建旧字段仍201。 |
| std-only fixture探针 | join环触发隐藏panic；peer不读阻止Drop，关闭peer后释放；5s watchdog无进程超时。 |

临时证据：`/tmp/api-cutover-review-probe/{src/main.rs,frontend.mjs,output.log,frontend.log}`；`/tmp/crawler-media-storage-review-20261023-locking/{fixture_join_probe.rs,results.log}`；schema探针位于本机TMPDIR的`schema-review-probes-0sbqc4q3/{main.rs,results.txt}`。关键输入/输出已写入本报告，临时路径不作为持久文档依赖。

本轮未跑Rust crate全套/workspace、Web build、真实浏览器、Jellyfin设备、NAS和完整100k/1M规模；F04/F07为源码证据。历史完整门禁只对应05a7363，不代表8fbf81b重新全绿。文档变更不操作实例数据；后续代码实施必须按仓库公开seam RED→GREEN、每触及crate独立测试、最终稳定树workspace与Web门禁验收。

文档验收：修改/新增文件仅在docs；353条相关本地链接及本报告源码行号检查通过；library字典47表/553字段/57索引与生产DDL逐项一致；`git diff --check`通过。Rust/TS/DDL源码树保持8fbf81b，已发现源码缺陷按本报告记录，未在文档任务中修复。
