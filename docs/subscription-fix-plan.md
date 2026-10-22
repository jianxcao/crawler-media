# Subscribe／Downloader 修复：需求映射与验收边界

**2026-10-20 整理，源码基线 `bd6c3c8`。** 原计划基于 `d6ae414`（审查 `746cac6`），后续实现与 `crates/api/tests/subscription_e2e/` 已存在；不能再按旧分支串行执行全部条目，也不能凭测试文件存在／历史workspace绿测宣布42条及追加功能全已验收。本文保留所有原ID及安全／行为要求，移除旧schema加列、兼容开关、建harness和提交操作细节。

原稿引用 `subscription-review.md`（33条）与 `subscription-review-tech.md`（复核新增8条等）；当前工作树及这两个根路径的Git日志未找到原文件，不把它们伪装成可用链接或可取回的证据。完整原计划（含根因、严重度、各流程场景）可由 `git show bd6c3c8:docs/subscription-fix-plan.md` 追溯。

## 1. 当前实现线索与执行边界

- 现有公开回归入口：`crates/api/tests/subscription_e2e.rs` 和其同名目录；已有golden_path、fake_catalog、mock_qbit、mock_transmission、harness，不重复建第二套测试框架。
- 源码可见 `pending_reconcile.rs`、`air_gate.rs`、`completion.rs`、`pack_fallback.rs`、`selective_files.rs`、`instance_lease.rs`，Subscribe的pending／absolute／slot_replacement，以及Store的pending_file_deletions。这只是实现线索，不是逐ID关闭证明。
- 原G-M3／G-M4的auto_next_season／pack_fallback_after_days已在Subscribe创建／修改入口接线；原M5已有follow_future门控。仍需从真实HTTP→搜索→Downloader→Transfer→当前Store回读验证，不能保留“字段完全未使用”的现在时。
- 当前文件／Media／播放权威与旧ledger/facts布局不同；存储正确性和事务边界见 [clean-cutover计划](<superpowers/plans/2026-10-22-storage-repair-remediation.md>)，不执行旧新增表／列或兼容迁移。搜索精度独立规则见 [搜索精度](<superpowers/plans/2026-10-16-subscribe-search-precision.md>)；Browser独立见 [CDP](<superpowers/plans/2026-10-16-cdp-obscura-managed-browser.md>)，不是本次存储优化的必跑前置。
- 原计划的P0/P1/P2/P3是当时严重度排期，不是今天全部仍坏；未知状态按公开复现／代码与回归证据逐ID归类。禁止依据日期或旧unchecked全关／全开。

## 2. 逐ID保留合同与现有回归入口

下表文件均相对 `crates/api/tests/subscription_e2e/`，是后续核验入口而非本次运行结果；“独立核验”项不得因未列单独文件被静默关闭。

| 原优先级／ID | 不能丢失的行为要求 | 回归入口／后续范围 |
|---|---|---|
| P0 D-S1 | HTTP enclosure按owned tags／持久hash辨认；16分钟后正常任务不误删；识别异常熔断，不动他人任务 | `d_s1_pending_cleanup.rs` |
| P0 S1 | probe失败不批准Wash-cut删在位文件，低置信新文件保留／重试有明确归属；成功probe才可替换 | `s1_wash_probe_fail.rs`、`n1_probe_before_publish.rs` |
| P0 D-H3／M8 | no-replace发布；canonical／备用名多次碰撞递增，竞态不覆盖，重复src幂等，三种Transfer mode一致 | `d_h3_m8_no_overwrite.rs` |
| P1 H1 | 洗版已有槽位也计in-flight，连续轮次不重复同质量升级；卡死占位由对账释放 | `h1_wash_in_flight.rs` |
| P1 H2 | probe否决持久rejected、自动搜索不复活／重复提交；显式手动投递有可验证重试语义 | `h2_probe_rejected.rs` |
| P1 H3 | 不打开UI也后台对账；grace／长期无进度释放占位并换源，正常任务不误清 | `h3_background_reconcile.rs`、`h3_stalled_timeout.rs`、`h3_claimed_task.rs` |
| P1 H4 | 纯季包占位与选择同口径，手动／定时、pack开关两路径不重复下载 | `h4_season_pack_pending.rs` |
| P1 D-H1 | qB SID过期最多一次受控重登，持续403明确失败、不无限重试／并发登录风暴 | `d_h1_qbit_relogin.rs` |
| P1 D-H2 | 相同infohash任务不永久Fails；认领需确切身份、持久hash／任务状态，破坏性操作不凭标题推断 | `d_h2_existing_infohash.rs` |
| P2 G-H1 | 未来播出日门控，未播不请求Site；播出后搜索，目录缺失／错误有明确策略与最长延后 | `g_h1_air_date_gate.rs`；原48h加频／最长7天是待核验策略，不自动新增开关 |
| P2 G-H2 | 推断季不能无目录校验进入错误季／触发Wash-cut删除；明确季正常 | `g_h2_season_inferred.rs` |
| P2 M1 | pack／单集覆盖重叠只按明确策略提交，不按集独立最优导致重复下载 | `m1_overlap_policy.rs`、`m1_m2_absent_evidence.rs` |
| P2 M2 | 非Wash-cut范围包只填缺槽，在位文件／facts不改变、不重复导入已有集 | `m2_owned_slot_skip.rs` |
| P2 M3 | 缺槽用主Filter、升级用Wash-cut Filter，分数尺度不混；高标准洗版不妨碍普通缺集补齐 | `m3_split_admission.rs` |
| P2 M6 | 文件／事实／pending／wanted提交一致；物理删除失败有持久重试，重启无半写；新布局不用旧跨库假事务 | `m6_deletion_retry.rs`；当前原子发布按新存储计划 |
| P2 G-M2 | 同owned任务error／missing不当成功，paused按策略恢复；不写虚假active pending | `g_m2_existing_task_state.rs` |
| P2 G-M5 | 同数据目录只允许安全单writer；第二活实例拒绝／只读，心跳超时接管无双提交 | `g_m5_instance_lock.rs`；不以allow_multi_instance绕过安全合同 |
| P3 M4a／M4b | 完成／达标停止无益搜索；删除文件／范围扩大／规则改变可恢复；开放追踪／Wash-cut完成语义与UI一致 | `m4a_completed_state.rs`、`m4_probe_veto_state.rs`；M4b原“永不complete”与“达标completed”矛盾，需按现行明确状态合同核验 |
| P3 M5 | follow_future false只追已播范围，true扩展且受播出门控，搜索与wanted一致 | `m5_follow_future.rs` |
| P3 M7 | collect后放弃提交：只回滚本轮新Copy/Hardlink目标；Move不丢数据且有可恢复孤儿记录 | `m7_abandoned_transfer.rs` |
| P3 M9 | 无阶梯／旧分数／规则变化不以高分低质量降级；默认质量阶梯与按分数模式是明确合同 | 独立核验choose／slot_replacement及Wash-cut现行规则，不新增兼容分支 |
| P3 D-M1 | 同设备Move不无益全复制，PT做种风险／任务清理明确；跨设备安全 | 独立核验 `crates/library/src/file_transfer.rs`、Downloader ownership |
| P3 D-M2 | Hardlink失败回退策略和结果告警可见，不静默大量复制；跨设备copy／fail语义明确 | 同file_transfer及设置消费者独立核验 |
| P3 D-M3 | HTTP种子hash持久，Transmission重建／无labels仍能确切收集，不靠内存映射 | `d_m3_persisted_infohash.rs` |
| P3 D-M4 | HTML／403不当torrent上传；bencode身份校验、代理／超时合同、临时文件所有出口清理 | `d_m4_torrent_validation.rs` |
| P3 D-M5 | 手动投递持Subscribe互斥、槽位冲突／force明确、规则分数正确、非法Site400，pending失败撤销不误删他人 | `d_m5_manual_submit.rs` |
| P3 G-M1 | 重叠订阅范围冲突明确，Wash-cut不能删除其他有效引用的文件 | `g_m1_overlapping_subscriptions.rs` |
| P3 G-M3 | auto_next_season默认关闭，完成后复制配置只创建下一季一次并通知 | `g_m3_auto_next_season.rs` |
| P3 G-M4 | pack fallback期限前不选单集、期后按策略回退，未配置仍等pack | `g_m4_pack_fallback.rs` |
| P3 L1 | 小体积／SP不误判全季占满；实际文件不足释放未覆盖占位 | `l1_small_season_pack.rs` |
| P3 L2 | 允许超集pack时只导入订阅范围，不额外入库 | `l2_wider_pack.rs` |
| P3 L3 | 非法ladder维度保存400，运行未知维度可观测，不静默改变比较 | 独立核验Filter保存与选择入口 |
| P3 L4 | active→completed通知一次，重新补齐可再通知，删除订阅不残留完成marker | `l4_completion_marker.rs` |
| P3 L5 | 被拒E999不撑大开放wanted，已播目录／合理增长上限一致 | `l5_rejected_episode_bound.rs` |
| P3 L6 | 删除FS在Store锁外，清理持Subscribe guard；慢盘不阻塞无关API | `l6_deletion_lock_audit.rs`；原p95<基线+50ms是未重测目标，不是实测 |
| P3 D-L1 | 单文件完成可导入，部分pack pending仍active；所需全部完成后imported，Move部分收集防丢 | `d_l1_partial_collect.rs` |
| P3 D-L2 | 定向hash／tag过滤，一轮快照复用，N个pending不反复全量torrents/info | `d_l2_filtered_info.rs`；保留2000任务／10pending规模门 |
| P3 D-L3 | 宽松标题匹配仅展示／标推断，删除／认领／清理不调用 | `d_l3_loose_match_audit.rs` |
| P3 D-L4 | 已删／禁用Downloader旧pending有明确orphan／迁移状态，提交持久具体downloader_id，不无限报错或跟默认漂移 | 独立核验delivery／Downloader删除／任务列表 |
| P3 G-L1 | 完成判断按目标Library事实，不被别库拥有文件误触发 | `g_l1_library_scoped_completion.rs` |
| 追加 F-1 | selective_files默认关闭；缺集文件选择，未知文件保留；metadata未就绪不阻塞，按所需文件完成判定 | `f1_selective_files.rs` |
| 追加 G-H2b | 绝对集号仅目录可证时映射后续季；跨季／越界拒绝，搜索／手动／Transfer一致 | `g_h2b_absolute_episode.rs`；原最多50季惰性构建门保留 |

## 3. 共用验收要求与未关闭项

- 从创建Subscribe→搜索→准入／提交→pending→单文件完成→Transfer→当前Library回读验证；不仅断言私有helper或HTTP200。用fake Indexer／Catalog／Probe、mock qB／Transmission、TempDir、注入时钟，不能访问真实外网／Downloader／ffprobe来跑普通crate测试。
- 故障／竞态：probe失败、403、无owned标签、下载器不可达／任务缺失、双实例、时钟推进、collect中删除／freeze、发布碰撞、事务故障、EACCES、重启回读。失败时先释放gate，不能把测试卡死当RED；不靠sleep等15分钟宽限期。
- Hash和owned证明是破坏性操作的门，不把认领成功等同有权Move／删除用户原任务；删除只涉及本轮创建或当前确证拥有的文件。Move回滚不能删源且丢唯一副本。
- 逐ID状态需补代码＋当前模型公开回归／真实结果，未核验按“证据不足”保留；现有suite名字不能证明M9／D-M1／D-M2／L3／D-L4及UI、整链路、性能目标已关闭。所有新增状态／配置都要同步真实前端与调用方，不为回滚保留旧schema／API兼容shim。
- 原每条PR、固定分支、串行P0→P3、“先实现后补测试”、自动兼容默认和git revert即数据库回滚的做法不再是现行开发要求。后续按用户授权、当前AGENTS公开seam测试先行、最小可独立验收slice实现；稳定树由主执行者统一验证，不在此次文档整理运行测试／构建或生产清理。
