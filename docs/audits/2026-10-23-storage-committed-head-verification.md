# Storage Review：已提交 HEAD 核验与修复入口

日期：2026-10-23。状态：**仅核验与技术方案，未实施代码修复。**

## 1. 固定范围、方法与证据等级

- 审查 HEAD：`572d4fa8870271033068b9ede0cea71ecbdf922b`；main：`86122d73a4a23439e1d83234590fdd98b388fc50`。
- 从 `git archive HEAD` 独立快照核验，忽略当前工作区未提交代码；本报告链接的行号均对应上述提交。
- 本轮只新增本报告和三份计划，不修改业务/测试/DDL/锁文件、不重建用户数据库、不部署服务。push 指当前 Git 分支的文档提交。
- **确认**＝源码可定位的缺口；**局部复现**＝隔离 SQL 探针；**撤回**＝已修复、误报或违背规范；**优化候选**＝必须先测量。
- 未运行本轮 Rust/Web 完整测试、跨盘实验或生产基准；计划中的 RED/GREEN 是未来验收，不是已通过声明。SQL 探针使用 Python SQLite 3.54.0，不冒充项目 bundled SQLite 的运行结果。

## 2. 总结与执行入口

五文件职责、显式 ownership、同库事实/摘要事务及冷详情拆分方向合理。仍有锁反序、阻塞执行器、播放标记时钟/入口分裂、根迁移遗漏 NFO、rename 磁盘/数据库阶段耦合、全库请求读取及重复归约成本。

**前述聊天报告包含明显误报，不能原样执行。** 优先修正确性，再做新读模型切换，最后按证据优化；不承诺未经测试的倍数收益。

| 子计划 | 内容 | 依赖 |
|---|---|---|
| [运行时正确性](<../superpowers/plans/2026-10-23-storage-runtime-correctness.md>) | 锁、执行器、事务、Playback、root、rename、locale、测试防挂起 | A1 优先；持久写任务依 A3 |
| [API 与新读模型切换](<../superpowers/plans/2026-10-23-storage-api-cutover.md>) | 定向查询、显式归属、前后端唯一契约、真实 DTO | 观看视图依 A4/A5 |
| [查询与写成本优化](<../superpowers/plans/2026-10-23-storage-query-efficiency.md>) | 索引、批内归约、facet、冷字段、测量门禁 | 正确性先固定；共享 DDL 串行集成 |

共同依据：[存储主规范](<../superpowers/specs/2026-10-18-library-storage-redesign.md>)、[自有 API 约定](<../api-contracts/self.md>)、[仓库规则](<../../AGENTS.md>)。本轮补充[旧计划](<../superpowers/plans/2026-10-23-storage-review-remediation.md>)，不重新执行旧计划已落地的任务。

## 3. 确认存在的正确性缺口

| ID / 优先级 | 当前证据、触发与影响 | 方案 |
|---|---|---|
| C01 / P1 | [library_lifecycle.rs:437–444](<../../crates/api/src/library_lifecycle.rs#L437-L444>) cancel 持 entries 调 control.cancel；publication 闭包获取/析构 lease 可反取 entries，构成 entries→gate / gate→entries。freeze/block 已释放 entries，cancel 漏改。旧报告 Arc/active 伪代码不是当前 controls 类型。 | A1：锁内置 cancelled、克隆 controls，锁外 cancel。 |
| C02 / P1 | [Transfer:418–438、471–506](<../../crates/api/src/worker/transfer/run.rs#L418-L506>) 持 Subscribe 同步锁等待库 idle；[patch:25–26](<../../crates/api/src/http/subscriptions/patch.rs#L25-L26>) 等 async handler 同步等锁。受限运行时可饥饿，不能说所有部署必死锁。 | A2：锁外等待、锁内重新读策略/获取 lease；阻塞工作进预算池。 |
| C03 / P1 | [data_retention.rs:282](<../../crates/store/src/data_retention.rs#L282>)、[response_cache/mod.rs:196](<../../crates/media/src/response_cache/mod.rs#L196>) 等先读后写 DEFERRED 在其他连接提交后可升级失败，busy_timeout 不修复过期 WAL snapshot。 | A3：写事务 IMMEDIATE；只读事务不改。 |
| C04 / P1 | [watch_state.rs:207–210](<../../crates/api/src/http/playback/watch_state.rs#L207-L210>) 秒值传给 [units.rs:657–674](<../../crates/store/src/playback/units.rs#L657-L674>)，同时写 played_changed_at_ms 和 watch_activity_at；纯标记还伪造观看活动。 | A4：断言毫秒、activity/favorite 秒，mark 不推进 activity。 |
| C05 / P1 | [units.rs:651–695](<../../crates/store/src/playback/units.rs#L651-L695>) 级联覆写子行，与 [mutate.rs:292–347](<../../crates/store/src/playback/mutate.rs#L292-L347>) 目标 scope 断言并存；[playback.rs:59–70](<../../crates/api/src/http/playback.rs#L59-L70>) 单行读取；[assertions.rs:57–59](<../../crates/store/src/playback/assertions.rs#L57-L59>) 无断言时丢掉 favorite/progress 行；mutate 取消收藏不清 favorite_at。 | A4：唯一写事务、有效 played 读模型，保留恢复包/收藏既有语义。 |
| C06 / P1 | [event.rs:385–432](<../../crates/store/src/playback/event.rs#L385-L432>) duration=None 覆写 NULL，随后完播计数已无时长可 fallback。 | A5：只继承同源恢复包时长，禁止裸 COALESCE 跨源拼接。 |
| C07 / P1 | [roots.rs:197–214](<../../crates/store/src/roots.rs#L197-L214>) 移文件/coverage 后归约旧库，未先迁 NFO/Unidentified；[reduce.rs:117–131](<../../crates/store/src/library_write/reduce.rs#L117-L131>) 删除旧库最后文件的 nfo_v1 输入。实际 XML 不一定丢，已采用本地输入会丢。 | A6：关联事实先迁后归约；共享 work/season 输入按仍有覆盖的库保留，不机械全表 UPDATE。 |
| C08 / P1 | [library_operations/roots.rs:229–250、307–338](<../../crates/store/src/library_operations/roots.rs#L229-L338>) 仍比较 canonical path_key 与 display roots。**另一路** [roots.rs:137–181](<../../crates/store/src/roots.rs#L137-L181>) 已 canonical，不能重复报错。 | A6：所有重配置前缀预先 canonical，按组件边界比。 |
| C09 / P1 | [path_key.rs:38–45、127–138](<../../crates/store/src/path_key.rs#L38-L138>) 内存连接以空路径共享全局 RootAliases，reload 可发布未提交 roots；[roots.rs:112–126](<../../crates/store/src/roots.rs#L112-L126>) 删根未刷新。 | A6：连接/事务局部别名快照，测回滚与跨 Store 隔离。 |
| C10 / P1 | [library_organize_apply.rs:127–139](<../../crates/api/src/http/library_organize_apply.rs#L127-L139>) move 后 [rename_durable.rs:204–240](<../../crates/store/src/library_write/rename_durable.rs#L204-L240>) 再 move 并持锁做 IO；[rename_disk.rs:63–68、149–175](<../../crates/store/src/library_write/rename_disk.rs#L63-L175>) 跨设备恢复用 size+精确 mtime，低精度目标可失败。不是所有跨盘必现，set_modified 当前传播错误。 | A7：持久磁盘 receipt、单一执行入口、锁外 IO；不放宽为 mtime 容差。 |
| C11 / P1 | [rename_sidecars.rs:102–127](<../../crates/store/src/library_write/rename_sidecars.rs#L102-L127>) 事务内读完整文件；检查到执行之间目标可变化，apply_sidecar 对已存在目标直接成功，原Move决议可能仍驱动源删除。complete_file_rename 已重载持久sidecars，不能声称它静态使用begin旧决议。 | A8：锁外观察、有证据的执行结果、no-clobber目标发布和删除前校验。 |
| C12 / P1 | [reduce.rs:361–469](<../../crates/store/src/library_write/reduce.rs#L361-L469>) locale 精确命中/任意 NFO fallback 与 [details/mod.rs:137–179](<../../crates/store/src/library_query/details/mod.rs#L137-L179>) 的语言链过滤不同。display_locale 为 NOT NULL 默认 zh-CN；旧报告 NULL 常态说法不成立，区域变体/外语 NFO 仍有缺口。 | A9：共享选择规则；同语言链一致，显式请求另一语言可有意不同。 |
| C13 / P1 | [media/mod.rs:47–103、319–345](<../../crates/store/src/media/mod.rs#L47-L103>) 更新身份字段/别名后仅改卡片 identity_state，未定向重算标题/年份/别名。 | A9：同事务 reduce_media_everywhere，保留 adopted/NFO 优先级。 |
| C14 / P1 | [visibility.rs:12–21](<../../crates/api/src/http/library/visibility.rs#L12-L21>) 路径反推已持久文件归属，unmatched 回默认库；[visible_scope.rs:23–41](<../../crates/api/src/visible_scope.rs#L23-L41>) 拉全库再算。 | B1/B2：授权 IDs + 定向 SQL；Transfer 目标默认规则保留。 |
| C15 / P1 | [reidentify.rs:177–227](<../../crates/api/src/http/reidentify.rs#L177-L227>) 缺 files 版本时取服务器当前版；[Web:1655–1665](<../../web/lib/api/libraries.ts#L1655-L1665>) 仅发 file_ids/title_ref，不能拒绝预览后替换。 | B3：强制 files/version/source_revision，前后端同批切换。 |
| C16 / P2 | [library.rs:569–570](<../../crates/api/src/http/library.rs#L569-L570>)、[episodes.rs:43–47](<../../crates/api/src/http/playback/episodes.rs#L43-L47>) 双输出 played/watched；[media/mod.rs:277–297](<../../crates/store/src/media/mod.rs#L277-L297>) source 认 *_id 同义词。 | B3：删旧输入词/重复字段，不删除合法 tmdb_id 展示字段。 |
| C17 / P2 | [sites.ts:205–229](<../../web/lib/api/sites.ts#L205-L229>) no-op/空统计；[subscriptions.ts:846–928](<../../web/lib/api/subscriptions.ts#L846-L928>) 伪 cleanup_job_id、忽略 Subscribe ID 的任务 adapter。UI 后续会按 hash 关联，不声称必然泄漏其他订阅。 | B4：真实回执/任务 DTO，删除假桩与无功能 UI。 |
| C18 / P2 | [poster_cache_capacity.rs:251–279](<../../crates/api/tests/poster_cache_capacity.rs#L251-L279>)、[up_next_local_first.rs:231–283](<../../crates/api/tests/up_next_local_first.rs#L231-L283>) 提前失败可能不释放 gate；部分 TCP fixtures 缺 read timeout。 | A10：RAII release、可停止 server、外部 watchdog。 |
| C19 / P1 | [marker source_guard:51、99](<../../crates/store/src/library/markers/source_guard.rs#L51-L99>) 在写事务中try_exists；[probe season:138–183](<../../crates/api/src/probe_manager/season.rs#L138-L183>) 持Store锁观察源；[library.rs:363–387](<../../crates/store/src/library.rs#L363-L387>) 逐文件exists；管理删除预览仍有持锁stat。慢挂载会阻塞全局Store，与必然死锁需区别。 | A11：锁外有界观察，短事务版本/成员校验，Unknown不当Missing。 |
| C20 / P2 | [preflight.rs:19–26、50–67](<../../crates/store/src/media_operations/preflight.rs#L19-L67>) FrozenPair 的默认空字段和缺frozen回退明确接纳旧持久JSON，[apply.rs:331–337](<../../crates/store/src/media_operations/apply.rs#L331-L337>) 空状态跳过恢复；这确是旧格式兼容。 | B5：强类型required frozen，坏计划明确拒绝并保留冻结，不导入旧格式。 |
| C21 / P1 | [export.rs:36–69、89–96](<../../crates/api/src/scrape_metadata/export.rs#L36-L96>) 把未登记文件、建立意图失败、读取失败压成None；[library_scan.rs:416–430](<../../crates/api/src/http/library_scan.rs#L416-L430>) None继续原地写NFO。真实风险是失败降级绕过保护，不是所有无台账Scrape均属旧功能。 | A12：区分NotLedgered/Prepared/Error，失败不写盘、不覆盖编辑。 |

## 4. 撤回或收窄旧结论：禁止据此改坏代码

| 旧结论 | 核验及决策 |
|---|---|
| 所谓 scanned_files死表/scanned_ledger全无调用、metadata_inputs无人消费 | **错误。** 当前DDL无scanned_files表；[scanned_ledger.rs](<../../crates/store/src/scanned_ledger.rs>) 写的是library_files，[auto_resolve.rs:489](<../../crates/api/src/auto_resolve.rs#L489>) 仍调用；metadata_inputs由card/details/protocol消费。不能按名字删生产模块。 |
| auto-v2一定是旧兼容 | **错误。** [library_cover_service.rs:23](<../../crates/api/src/library_cover_service.rs#L23>) 仍定义并使用MARKER_RETRY，不能只看clear分支删当前状态。 |
| list_file_coverage 是 N+1 且仅 present | **错误。** [ledger.rs:14–35、126–135](<../../crates/store/src/ledger.rs#L14-L35>) 单条 LEFT JOIN 后 group，无 presence/publication 过滤。真正问题是全库范围；present-only 替换会破坏管理/缺失页面。 |
| terms/user items 无复合 FK，最后文件删除留孤儿 | **错误。** [library.sql:170–175、203–209](<../../crates/store/src/schema/v1/library.sql#L170-L209>) 有 ON DELETE CASCADE。原始观看事实不应级联删。 |
| reassign/rename 不为目的库生成用户摘要 | **已覆盖。** [reduce/card.rs:56–94](<../../crates/store/src/library_write/reduce/card.rs#L56-L94>) 已 UNION raw users；[reassign.rs:58–61](<../../crates/store/src/library_write/reassign.rs#L58-L61>) 调两侧归约。不重复添加刷新循环。 |
| roots.rs 仍全同 kind 扫描且比较 display/canonical | **部分已修复。** 当前 [roots.rs:137–181](<../../crates/store/src/roots.rs#L137-L181>) 已 canonical 范围读；范围有界不等于事务≤100。另一路 operations/roots 尚有 C08。 |
| merge 失败解冻、丢 loser 会话历史引用 | **撤回概括。** [apply.rs:214–226](<../../crates/store/src/media_operations/apply.rs#L214-L226>) 只记录失败；[references.rs:640–715](<../../crates/store/src/media_operations/references.rs#L640-L715>) 已 merge watch facts。历史原 ID 可按规范保留，不能机械加禁止原 ID 的 FK。 |
| merge true@10 + false@30 应得 false | **违背主规范 §4.6。** 明确归并 true@30；不同于普通 mark 最新断言规则。 |
| full_file 完成 E01 不完成 E02 是 bug | **违背主规范 §4.5。** 必须只作用所选逻辑分集。 |
| 成功 merge 后重启 jobs 永不收敛 | **启动窗口已覆盖。** [job_loop.rs:146–149](<../../crates/api/src/job_loop.rs#L146-L149>) 获取实例租约后无条件 converge；不能只看 if advanced。 |
| 无 PlaySessionId 的事件都必须删 | **不成立。** 主规范 §4.5 允许严格匹配活动逻辑目标/物理源的无 ID 协议事件；不删除标准 Jellyfin 能力。 |
| 多库 lease 顺序相反必死锁 | **未证实。** [acquire:266–292](<../../crates/api/src/library_lifecycle.rs#L266-L292>) 共享注册 lease 不独占整个库。排序只作确定性改进，不能冒称 ABBA 修复；C01 才有等待环。 |
| side.to 存在一定删除 source | **过度概括。** [rename_sidecars.rs:109–118](<../../crates/store/src/library_write/rename_sidecars.rs#L109-L118>) 已把不同字节改为 Copy；修持久决议传播/竞态，不取消目标胜出、源保留政策。 |
| size+近似 mtime 足以证明 rename 目标 | **不安全。** 外来同尺寸文件可碰撞；A7 采用 receipt/身份，不放宽时间容差。 |
| file 首坐标与 units 共存就是兼容 | **不成立。** [API 约定:88–96](<../api-contracts/self.md#L88-L96>) 允许便捷字段；[fileUnits:56–69](<../../web/lib/library-file-pages.ts#L56-L69>) 仅认 units。 |
| 海报排序 season.max(0) 必须删 | **不成立。** 主规范 §4.1/4.3 明确 season或0/episode或0 排序；排序键不是存储 sentinel。 |
| HTTP (0,0) 一律非法 | **逐 DTO 判断。** 存储 whole=-1；TV S00E00 合法，download/wanted 展示坐标不是播放存储键，禁止全局0→-1。 |
| 两条 unidentified claim 可直接删一条 | **用例不同。** [已有 Media 根内认领](<../../crates/api/src/http/library_admin.rs#L406-L447>) 与 [识别并 Transfer](<../../crates/api/src/claim.rs#L356-L402>) 不同；B4 保全两个用例后统一显式入口。 |
| 短过滤键全是旧格式 | **不成立。** [API 约定:114](<../api-contracts/self.md#L114>) 正式记录短键。B3 选择现有短键为唯一自有参数，同步删除后端长名别名，不单方破坏 UI。 |
| library.ts / ReidentifyDialog 等历史路径都需改 | **路径过时。** 当前实际 API 是 [libraries.ts](<../../web/lib/api/libraries.ts>)；3533e63 已删部分 adapter，[DTO 测试:74–96](<../../web/test/library-dto-contract.test.mjs#L74-L96>) 覆盖严格字段/章节对象/trash/duplicates 删除。 |
| 旧报告 idx_* / fields/value / people_json 是当前模型 | **错误基线。** terms 实为 term_kind/term_value，PK 已是 library/term/value/media；files 已有 files_library_media，不能照抄旧 DDL。 |
| NORMAL 无损且吞吐必升2–5倍 | **错误。** WAL NORMAL 保一致性但 OS crash/断电可丢最近已确认提交；权威库保 FULL，cache 单独测。 |
| 两个 cache 索引完全重复 | **错误。** DESC/ASC 反扫为 ASC/DESC，不是 ASC/ASC；删前须验证相同时间戳 tie-break/cursor 合同。 |
| WITHOUT ROWID 必省30–50%、journal_size_limit 是 WAL 硬上限 | **错误。** 宽复合 PK 会进入二级索引；活跃 reader 可阻止 WAL checkpoint/reuse。必须测，不能作保证。 |
| IMMEDIATE 消除所有 BUSY | **错误。** 避免 snapshot 升级失败，但 writer 竞争/超时仍存在，需预算与错误传播。 |

## 5. 优化候选：确认代码结构，不虚构执行计划

| ID | 确认范围与约束 | 方案 |
|---|---|---|
| O01 | [incremental:194–214](<../../crates/store/src/library_write/reduce_incremental/mod.rs#L194-L214>) 每文件可能重复同 Media 用户摘要；BatchSettled 已避免新 unit 重复 delta。before.units 相同也不能跳公共摘要：presence/publication/首选源/图片等仍会变。 | Q2 |
| O02 | [hydration:69–150](<../../crates/store/src/library_query/items/hydration.rs#L69-L150>) 页内冷 JSON 取 studio；favorite 仅轻量摘要查询，分页后批量补读本身被规范允许。 | Q3 |
| O03 | [facets:95–143、231–335](<../../crates/store/src/library_query/facets.rs#L95-L335>) 结构上 All26条、Primary12条 SQL，桶可用条件索引，不说26次全表扫。保留 exclude-own-dimension、零桶、精确 total。 | Q3 |
| O04 | [protocol/sql:150–201](<../../crates/store/src/library_query/protocol/sql.rs#L150-L201>) 有 source 子查询/窗口，[facts:105–200](<../../crates/store/src/library_query/protocol/facts.rs#L105-L200>) 有冷字段过滤；CTE 可 inline/延迟求值，真实 VM/EQP 才能判执行次数。 | Q4 |
| O05 | [library.sql](<../../crates/store/src/schema/v1/library.sql>) 中 fingerprint_attempts/file_id、probe_stage_state/active_job_id、metadata inputs/media_id 等缺前缀索引。用 [objects.rs](<../../crates/store/src/schema/v1/objects.rs>) 真实清单，不照抄旧 mod.rs 常量。 | Q1 |
| O06 | [roots:161–215](<../../crates/store/src/roots.rs#L161-L215>) collect 影响范围后一个事务；[library_deletion](<../../crates/store/src/library_deletion/mod.rs>) 大删除事务仍可长持锁。分批必须有持久 cursor/freeze。 | A6/Q5 |
| O07 | [open:251–265](<../../crates/store/src/open.rs#L251-L265>) PRAGMA、固定热点语句缓存、WITHOUT ROWID 可评估但非 P0；多连接内存预算不能每连接任意32MB。 | Q5 |

## 6. 已执行的隔离 SQL 探针

在快照完整 library.sql 上打开 foreign_keys，构造两个 Library、一个 Media/file/card、用户摘要、nfo_v1 输入；另用双连接 WAL 验证 snapshot 升级：

```text
schema_SQLite_version: 3.54.0
old_user_summary_after_item_delete: 0
NFO_inputs_after_root_move_and_clear: 0
mixed_unit_latest_assertion: (1, 1700000000000)
omitted_duration_after_current_update: None
deferred_snapshot_write_error: SQLITE_BUSY_SNAPSHOT
```

第一项计数证明 FK 已级联；第二项证明先移文件再清旧卡片 NFO 会丢输入；混合时钟使较晚秒级 false 输给先前毫秒 true；当前 duration=NULL 更新抹时长；双连接 A先读/B提交/A写确实 BUSY_SNAPSHOT。仅为 SQL 层，不冒称 HTTP/跨盘/线程用例已复现。

## 7. 规模治理与文档验收

快照 wc 确认8个 `.rs` 超800行：[references](<../../crates/store/src/media_operations/references.rs>)879、[http/playback](<../../crates/api/src/http/playback.rs>)837、[units](<../../crates/store/src/playback/units.rs>)810、[event binding tests](<../../crates/store/tests/playback_event_binding.rs>)1105、[reconfiguration tests](<../../crates/store/tests/library_reconfiguration.rs>)917、[seek scale](<../../crates/store/tests/storage_work_bounds/seek_scale.rs>)860、[identity tests](<../../crates/api/tests/playback_event_identity.rs>)857、[statistics tests](<../../crates/api/tests/library_statistics_scale.rs>)826。[store/lib.rs](<../../crates/store/src/lib.rs>)261行超150软限制但未超800硬限制，其 Store/构造器定义不符合仅模块/导出形状。触及即按职责拆分，测试逻辑也受800/120硬限制；不做无意义转发层。

本次验收仅为 Markdown 链接、基线/范围、任务覆盖、签名一致性、风险措辞与文档专属暂存/提交 diff。审查期间工作区代码仍有外部并行变更，整体代码差异指纹不稳定；这些变更不读取为修复证据、不覆盖、不回滚、不暂存。以本次提交相对审查 HEAD 的代码树完全相同证明没有夹带代码，而非声称外部工作区在时间上静止。未来实现需逐任务 RED→GREEN、触及 crate 独立测试、最终稳定树 workspace 测试，不把文档完成等同生产验收。

## 8. 实施状态

| ID | 状态 |
|---|---|
| C01 (A1) | 已修复：cancel 锁内置 cancelled 并克隆 controls，锁外 cancel。回归 `library_work_lifecycle::cancel_during_publication_allows_nested_lease_drop`（旧实现子进程 40s 锁环）。 |
| C02 (A2) | 已修复：Transfer 锁外等待库 idle，重新加锁后重读订阅/目标并取 lease，目标变化或忙碌则重新等待；patch/search/missing handler 的 guard 工作进 http_work。回归 `management::subscribe_wait_liveness`（单 worker 饥饿、等待中删除）。 |
| A3 | done: store write_tx (IMMEDIATE) for all write txs, jobs IMMEDIATE, cache init under IMMEDIATE + WAL switch retry; tests write_transaction_contention, cache_writer_contention |
| A3 | verified: media+jobs+api 1674 pass/0 fail; fanart shared cache 96579b9 |
| A4 | Fixed: scoped marks via mutate_playback, favorite_at seconds clock, marks never move activity; tests playback_assertions + api playback_clock |
| A5 | Fixed: duration reused only on same file/revision/source_version/full_file; one duration drives completion+play_count; tests playback_event_duration |
| A6 | Fixed: PreparedRoot canonical keys persisted in plan (C08); per-connection in-memory alias tables, publish after commit incl. root deletes (C09); scoped inputs/unidentified reparented before reduce (C07). Risk: shared work NFO not duplicated into nested library; root changes via create/insert not batched ≤100 |
| A7 | Done: dest dev/ino receipt persisted; execute outside store lock, record idempotent; staging kept until recorded. Risk: real cross-FS not run; sidecar IO still in record (A8). |
| A8 | Done: durable SidecarAction (Move/Copy/KeepSource/AlreadyIdentical) decided from lock-free streaming compare + short tx recheck, persisted; no-clobber hard-link publish (competitor→KeepSource); refs/deletion read persisted decision; unknown IO fails retryable; LEFT JOIN vanished file→failed; due-only resume (next_retry_at seconds, *_ms columns ms). RED for competitor test written alongside impl (not run on old code). store 454+, api 1549/0. |
| A9 | Done: metadata/locale.rs select_locale (exact→prefix, sorted ties; AdoptedFirst/UnboundInputOnly) shared by reduce card + detail; update_media/update_media_if_matches reduce cards in-tx (+writable barrier). store 460/0 (includes A7/A8 fixes), api metadata_priority 3/0. |
| A10 | DONE — shared tests/support/lifecycle.rs (FakeServer, ReleaseOnDrop); gated tests release on panic; management/subscription_e2e/downloader fake servers per-connection, read timeouts, stop+join on drop. fixture_lifecycle 3/3, management 589/0, subscription_e2e 75/0, downloader all green. |
| A11 | Fixed: sources observed outside Store lock; marker publication CAS on SourceObservation (source_observation_liveness). store 460/0, api green. |
| A12 | Fixed: begin_sidecar_export -> Result<ExportPreparation>; Err skips write_nfo (no unguarded fallback). Regression tests nfo_export_failure_isolation 2/2; RED not reproducible via IO (library::write_nfo already refuses unreadable sidecar). |
| B1 | Partial: FileReadScope + file_coverage_in_library (SQL scope, persisted library_id); library_for_file by file.library_id (strict = owner AND strictly inside its roots); VisibleScope via DISTINCT library_items. Consumers not individually migrated to scoped helper. |
| B5 | Fixed: typed MergePlan requires frozen with provisional/confirmed states; malformed plan -> Invalid, stays merging (malformed_frozen_plan_is_refused_and_stays_frozen). store 462/0. |
| B3 | Partial: pinned ReidentifyRequest (typed, deny_unknown, preview exposes files+source_revision, stale -> 409, no server fill); episode DTO drops watched; alias source rejects *_id; web assignLibraryFilesToTitle(request). Not done: filter long-name aliases, logs/Subscribe legacy params, web envelope fallback sweep. |
| B4 | Partial: unidentified claim is typed {path, action: bind_existing|identify_and_transfer}; bind never moves bytes; old /unidentified/{id}/claim removed; CLI+web migrated. Not done: sites/subscriptions receipts, downloader-task adapter, fake annotation adapter. |
| B2 | Partial: favorites page selected in SQL (playback_favorite_page: authorized libs, cross-library dedupe before LIMIT, same-predicate total, unwatched_first tier from user summaries); handler no longer scans list_file_coverage. Deferred: up_next and gallery still scan list_file_coverage. |
| Q1 | Done: EQP baseline showed full scans for probe active_job_id prune, unidentified root_id removal, media survivor_id FK check; added probe_stage_active_job, unidentified_root, media_survivor. Other candidates had no scanning real statement; not added. |
