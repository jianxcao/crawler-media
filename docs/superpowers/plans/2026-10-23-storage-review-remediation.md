# 存储重构剩余修复技术方案

状态：**待实施**。依据[8fbf81b再次复审](../../audits/2026-10-23-storage-followup-review.md)，比较`main@86122d7`。本轮只审查和文档，没有修改源码。此页替代已完成任务夹杂其中的10-22计划及10-23运行时/API/查询三个分计划，旧正文可从`git show 8fbf81b:docs/superpowers/plans/<文件名>`取回。

## 约束与实施顺序

保留五文件、显式ownership、事实/摘要同事务、冷详情和持久意图。无旧数据导入、兼容表/视图、双读双写、旧字段别名或旧格式回退；本分支未上线布局直接调整目标DDL与所有消费者，不补旧JSON或旧数据库升级shim。已存在错误数据不在请求时偷偷修复。正式上线后的前向schema版本演进另按其发布约束处理。

磁盘观察/执行在Store锁外，最终事务验证原始expected版本。unknown不等于missing。静态图片保持public，Jellyfin视频按其协议鉴权；合法协议参数、TV S00E00与合理UI默认不属于旧兼容。

建议顺序：T1/T2（NFO键与批迁）→ T3（根操作恢复）→ T4（重启seed）→ T5/T6（API/marks）→ T7（Probe CAS）→ T8（测试退出）→ T9（锁边界）→ T10（信封）→ T11（测量/总验收）。可将独立切片并行，但共享DDL/domain与提交由集成者统一，T8应在大规模测试前完成。T9沿用T3/T7的版本和阶段合同。

每任务先在公开seam跑出可区分的RED，再实现和GREEN。新增/触及Rust文件≤800行、函数≤120行；按职责拆分；关键失败结构化记录operation/file/version/error。只提交相关文件，不操作真实实例或自动部署。

## T1：统一NFO的canonical视频与sidecar键（F10 / P1）

**边界：** `store/src/metadata/nfo_input/{observe,record,reconcile}.rs`、`path_key.rs`、`library_write/reduce/card.rs`及现有NFO输入调用者。

**设计：** 在锁外观察计划准备阶段，以固定RootAliases快照生成canonical视频path_key与sidecar input_key；display路径单独用于IO。持久化/删除/归约使用相同键和配置版本。不能仅在prune增加字符串容错或同时保留两份路径输入。保留导入时locale、人工编辑基线与export digest。

**验收：** real目录+alias根首次publish应有有效输入和正确详情；重扫修改/删除、祖先work/season、real/alias拼写切换结果等价。默认macOS TMPDIR与canonical TMPDIR都通过现有`nfo_scoped_inputs`；不以修改测试TMPDIR掩盖生产缺陷。

## T2：文件批迁与输入保留同事务（F02 / P1）

**边界：** `store/src/roots.rs`、NFO输入迁移和归约；`store/tests/nfo_scoped_inputs.rs`及根迁移回归。

**设计：** 一批先确定≤100文件及影响Media/目录；在同一事务迁文件/覆盖与其关联输入，再归约本批两侧。共享work/season输入在父库仍有覆盖时保留，并为目标复制；独有输入仅在实际对应文件迁移后移动。未处理批次的输入不得被普通孤儿清理提前删除。Unidentified遵循独立有界cursor/阶段，不放入前置无界全量UPDATE。完整最终校验后才清理计划残余。

**验收：** 同Media至少301文件跨多个目录/season，work/season/episode NFO兼有；逐批崩溃重启，输入/locale/provenance/export digest/详情守恒，XML不被改写；其他Library事实不变。先用101文件反例取得行为RED，不能只用250个不同Media无NFO证明正确。

## T3：根迁移纳入持久operation与冻结（F03 / P1）

**边界：** `roots.rs`、`libraries.rs`、`library_operations/*`、`schema/v1/library.sql`与布局清单、API创建/root操作及恢复driver。

**持久合同：** operation保存固定参与Library/root、原/目标config revision、canonical范围、phase/state/error及cursor；cursor与本批事实同事务提交。优先复用现有library_operations，不再建立第二套通用队列。create/insert-root及正式reconfigure统一到该恢复协议；需要异步时前后端同步消费真实202 operation回执，无200/202猜测兼容。

**状态规则：** 创建/受理即阻止相关源/目标新写；目标处于locale/removal等操作时明确冲突或等待，不能从ready_roots另选父库并推进。失败/目标失效保留cursor和阶段；重启按确定计划继续或明确失败。初始化须先恢复/建立相关屏障，再启动可发布worker。读取遵循既有reindexing规则，不返回被当作完成的混合成功页。

**验收：** 100/150后冻结目标，恢复后必须nested=150,parent=0且无cursor；并发locale应按确定合同拒绝或等待，不能覆盖根操作的冻结；与locale/removal/另一root操作交错、所有批边界重启、相同操作幂等、新文件插在cursor前、失败后显式重试，归属/输入/摘要均精确。布局改动不读旧队列格式填默认。

## T4：seed只初始化全新Library配置（F09 / P1）

**边界：** `store/src/schema.rs::seed_libraries_and_roots`、fresh-start及Library重配置测试。

**设计：** 仅在全新kind初始化默认Library和默认root，单事务完成。已有kind配置不检查并补回内建路径；正式删除/改根结果不被open覆盖。既有配置缺根按领域合法性明确处理，不能自动猜测恢复旧默认。

**验收：** 替换内建默认根→重启，根集合/顺序/default/revision完全不变；删除默认Library后已有同kind库不被插根；反复open与首次空目录初始化均正确。

## T5：真实Web与Subscribe严格DTO一次切换（F01 / P1）

**边界：** `web/lib/api/subscriptions.ts`、`web/lib/subscription-form.ts`、`web/components/subscribe-dialog.tsx`、`subscription-inspector-view.tsx`、TV调整/Wash-cut弹窗、`http/subscriptions/types.rs`、`subscription_depth.rs`及现有合同测试。

**唯一合同：** create/PATCH使用`coverage/filter_id`；upgrade body使用`filter_id?/wash_cut?`，report的Filter字段使用`filter_id`。删除`selected_seasons/rule_set_id`请求映射及旧report字段；创建也设置严格未知字段拒绝并通过StrictJson返回400。表单内部选择需正确构造合法coverage，不能恢复后端旧字段。

**验收：** 实际adapter→真实Router换Filter与TV season成功；回读持久Filter/coverage；旧字段create/PATCH/upgrade全部400且事实不变。替换`api-contracts.test.mjs`断言旧词的错误测试，不以mock也接受旧词建立假闭环。

## T6：marks严格目标解析与零写失败（F05 / P1）

**边界：** `http/playback/watch_state.rs`与共享目标校验，Web当前marks调用者和真实Router回归。

**合同：** 使用强类型marks输入，明确whole/season/episode三种合法组合。省略与错误类型分开；坐标转换用checked conversion，拒旧键、溢出、非法负数、单独episode/重复非法查询参数。GET/POST共用规则。保留TV S00E00、整季和whole，不机械套用session必须完整分集坐标的限制。

**验收：** 旧键、字符串、4294967295、非法坐标均400；比较原始played assertion、favorite、完整恢复包及其他分集，全部零写。正确三层标记/撤销仍按既有时钟/归约合同工作；不用清空事实避免错误输出。

## T7：Probe从计算开始携带原版本至发布（F04 / P1）

**边界：** `probe_manager/{markers,marker_jobs,probe,season}.rs`、`fingerprint_job/source.rs`、Store Marker/cache发布方法。

**接口设计：** 计算输入携`FileVersion`、file_id、path、持久Library/root/config revision及完整样本成员集合；锁外观察携实际source token/presence。Marker/章节、指纹cache、tracks发布在短IMMEDIATE事务验证原始expected与当前事实。禁止计算后重取current作为expected；多文件season不能只校验产生chapter_updates的子集。旧结果拒绝/终止旧工作并重派当前版本，状态和错误可观测。

**验收：** 门控capture/comparison后同路径换r2/vB，再放行r1/vA；旧Marker/章节/cache零发布，新版本任务正常完成；成员变化、Library冻结、Unknown、文件删除/改挂同测。tracks保留已有source string校验，并补真实磁盘探测前后token变更回归。必须经过真实Probe生产发布链，Store手动传旧snapshot不足以关项。

## T8：测试fixture退出有界且不隐藏panic（F06 / P2）

**边界：** `api/tests/support/lifecycle.rs`、`subscription_e2e/mock_{qbit,transmission}.rs`及fixture生命周期测试。

**设计：** handler只持共享业务状态，server owner归测试scope；connection释放最后state不得析构并joinserver自身。显式stop/Drop关闭活跃socket、唤醒accept，设read/write timeout及总请求deadline；检查join错误并让隐藏线程panic可见。完成线程要join/回收，不能丢handle吞panic。保留已实现RAII gate。

**验收：** 子进程外部watchdog覆盖handler最后owner释放、peer不读大响应、半请求、慢速分段和断言panic。退出无join环、无隐藏panic；释放peer不应成为Drop结束的必要条件。本机原实现表现为OS拒join panic，其他平台是否永久挂起不作为已复现事实。

## T9：完整拆开锁外IO与批间调度（F07 / P2）

**边界：** organize/housekeeping/startup、rename begin/complete/finish、Probe source观察及root driver。

**设计：** 统一准备→锁外观察/执行→短事务CAS→锁外staging清理。begin不能隐含read_dir/stat，complete不能读STRM/stat，finish不能锁内删文件；清理失败保留可恢复记录。持久归属校验用library_id/root_id，不通过library_for_path_strict间接访问磁盘。恢复driver一次有限窗口，批间释放Store并检查取消/停机；IO使用已有预算池，避免在async worker同步耗尽队列。等待与发布屏障保持T3/T7语义。

**验收：** 门控每个磁盘阶段，同时Store查询和心跳能在限时内完成；大量root/rename pending时批间可读、重启续跑。单Media 10k分集/多用户单独测归约与最大持锁时间；≤100候选不是耗时或派生行写入上限。没有锁环证据时按阻塞/饥饿报告，不用重入锁或加busy_timeout掩盖。

## T10：移除Web旧信封与伪空（F08 / P2）

**边界：** `web/lib/http.ts`、`web/lib/api/*`的ApiEnvelope、catalog-cache/health等adapter及全部调用者fixture。

**合同：** 当前成功只认`{ok:true,data}`，当前错误保留HTTP/错误正文语义。明确204无body单独处理；删除success信封、裸对象/数组fallback。各endpoint对payload必要类型作验证，错误抛出而不是`[]/{}`成功。保留合法业务可空值、首次配置默认和UI默认。

**验收：** 实际adapter当前信封成功，旧success/裸数组/裸对象/损坏payload均报合同错误；UI错误可见。测试fixture一并迁移，不允许一套测试继续模拟旧后端。

## T11：优化选择与最终证据门

1. **genre**：固定ACL/locale/跨库去重/稳定多键排序/精确total后，按5k/20k/50k/100k命中分布测EQP、VM、p50/p95及锁时长。评估按genre和有效语言事实的有序索引或物化成员，禁止把所有组合预建成笛卡尔索引。keyset只处理页面定位，不伪称能免掉精确count；不改成近似total或分页后过滤。
2. **写放大/长操作**：分别测同Media巨量文件与大量不同Media，记录每批读取/写入集合、最大事务/Store持锁时间、心跳p95、busy错误及WAL峰值；禁止只看每批100输入就关性能项。
3. **物理参数**：cache NORMAL/mmap/cache_size/temp_store/WITHOUT ROWID按单变量A/B决定；考虑连接总内存、长reader/checkpoint和rowid依赖。无净收益维持默认；Store显式FULL保留。Jobs Queue需补显式FULL及连接级测试（先设OFF再调用policy断言恢复FULL），不能借bundled默认通过；这是耐久性门，不依赖调参A/B。person/search当前历史指标满足既有目标，不为追求形式引入改变substring语义的FTS。
4. **补证**：A8竞争写者旧代码行为RED；A12构造旧代码能写盘但未登记意图的区分性失败（现有EACCES测试旧代码也过，不算RED）。若旧代码无法复现，明确保留为证据缺口，不伪造失败也不推断当前必有bug。
5. **完整验收**：每触及crate运行`cargo test -p <crate>`，所有改动稳定后`cargo test --workspace`一次；Web test/typecheck/build。默认与canonical路径测试均要过；无真实网络/Chromium。
6. **现场/规模**：本地空目录端到端、换源/根操作/删除重导入/重启/清cache，真实Web/Jellyfin设备、NAS及100k Media/1M文件/1M Playback另按可用环境执行。没有环境即Not run，不以历史子集/代理SQL数字冒充通过；不自动操作用户实例。

## 完成定义与文档去重

- F01–F10各有可区分公开行为回归和当前提交结果；坏输入零写、旧任务不污染新源、批迁输入/归属守恒、fixture可靠退出。
- 当前字典由生产DDL核对，统计/字段/索引同步；API与实际Web合同同步。此页只维护剩余任务，完成后以事实和提交号关闭，不追加互相覆盖的Done/Partial表。
- 历史审计保留固定SHA反例/撤回与测量；已替代的四份计划移除，不恢复其旧步骤。旧A/B/Q到本轮F映射见复审，规范与未测边界保留。
- 代码修复、实例重建或部署均未在本轮执行；此技术方案供后续实施review使用。
