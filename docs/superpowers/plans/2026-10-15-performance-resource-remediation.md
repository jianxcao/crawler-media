# 数据库、接口与资源整改：历史映射与未关闭验收

**整理基线：`bd6c3c8`，2026-10-20。不是重新执行 10-15 计划的指令。** 原 D/A/R 实施步骤已被后续实现及存储模型替换；不能继续标为“全部未实施”，也不能因为存在实现或历史绿测而宣布全部验收完成。本页合并原三个子计划与共享设计的有用要求，保留全部 17 个发现 ID。

当前存储正确性与切换任务以 [clean-cutover 计划](<2026-10-23-storage-review-remediation.md>)为准。本文的资源、性能、设备及配置门独立保留；新存储计划不自动授权这些工作或生产清理。

## 1. 证据与替代关系

- 原发现及初始失败：[10-15 审查](<../../audits/2026-10-15-database-api-memory-review.md>)；原命令的成功、失败和未测结果保持不变。
- 历史实现／局部回归：[10-15 acceptance](<../../benchmarks/2026-10-15-performance-resource-acceptance.md>)。该记录的 `fixed-tested` 只适用于记录的版本及 seam，不是当前端到端或生产声明。
- 原共享设计及数据库／接口／内存详细实施稿可从 `git show bd6c3c8:docs/superpowers/` 下对应 `2026-10-15-*-remediation.md` 路径追溯；不再保留重复的建文件、预计 RED、提交命令。
- `6d3d6cc`、`07726b7` 已把媒体／文件／播放权威收敛到 `library.db` 并删除旧 projection 栈。当前连接见 `crates/store/src/open.rs`：app/cache/library/subscribe，另有独立 Jobs。禁止重建旧 catalog/fanart 分库、`browse_changes` journal、旧 projection head 或无匹配默认库回退来“完成”旧任务。
- 不再适用的旧接口签名不是兼容要求；所有生产消费者必须迁移到现行模型，不能加 wrapper、旧 movie `(0,0)` 或双写。

## 2. 全量 ID／任务对账

“存在”只表示下列源码／历史证据可见；未在本次文档整理中运行代码或重验性能。

| 原发现 | 原任务 | 保留证据／模型变化 | 尚需满足的要求 |
|---|---|---|---|
| DB-01 | D1 | 原 catalog 前缀 SQL 属旧模型；历史 VM 记录仍在审查 | 对现行 cache 详情查询重测相近 ID、语言、`%/_`、子路径与 tie，不能照抄旧 SQL 关闭 |
| DB-02 | D1 | 探测义务读取及索引曾实施；旧 ledger 已被文件模型替换 | 当前义务公开查询固定目标下验证索引／工作量，不以 LIMIT 证明扫描有界 |
| DB-03 | A1 | 历史 `8d899f4`；owned／blocking 读取仍是约束 | 慢目录 gate 下 Store 可用、heartbeat／认证推进；副作用重新验证当前来源和 scope |
| DB-04 | D3 | 历史 `2cfe87d`、`4fe9807` journal 后被新存储模型替代 | 当前写事务维护 read model；移动／删除／改 root 使旧新 scope 都正确，不恢复旧跨库 journal |
| DB-05 | D4 | `data_retention` 已注册，Store/API 分类实现存在 | 默认关闭＋dry-run、引用保护和停止边界；按新表／连接核验后才可另行批准启用 |
| DB-06 | D2 | `Store::reconcile_probe_job_counts` 与转移计数实现存在 | 重复／晚到／取消／partial 不重复计数、不复活；当前模型下整轮规模测量仍缺 |
| DB-07 | D5 | 原默认 Library 补集／派生 owner 方案未获批准；新模型使用明确 Library/file 身份 | 旧设计不再执行；保留当前候选查询规模、归属等价与失效证据门，不补旧 root_id／默认库回退 |
| API-01 | A2/A3 | 历史 `d8bb63a` 批读／分页回归；当前 `protocol_item_page` 存在 | Store 方法存在不等于生产调用已接通；按新计划完成真实协议链路，并保留准确 count／过滤／顺序与页内重事实预算 |
| API-02 | A1 | 历史 `8d899f4` reconciliation／锁外 IO | 当前公开入口验证目录仅扫一次、截断明确、错误不伪装完整结果 |
| API-03 | A5 | 历史 `0b05de2`、`0f6bf32`；`83923bc` 改由新 items/files 统计 | same-count 路径变更 freshness（A5-02）、barrier（A5-03）及多库权限／规模门未因改模型自动关闭 |
| API-04 | A4 | 历史 `63857d8`、`4e21459` 页事实回归；旧投影已删除 | 现行热页只解码本页重事实，过滤在选页前；文件 continuation 的现存正确性问题归新计划 |
| API-05 | A6 | 历史 `8d899f4`；本地优先实现／回归记录 | movie/Video 不请求 season；TV 缺标题不等外网，有界后台补齐并保留顺序 |
| MEM-01 | R1 | `media_posters/inflight.rs` 的 worker guard 已存在 | 取消／超时后真实 fetch≤4、同 key 不重复，Err/panic/晚到 follower 最终释放 |
| MEM-02 | R2 | `602f20c`；`negative_cache.rs`、weak inflight 实现存在 | 10k 不同失败 key、到期不再访问、全 live 容量与旧 guard 身份回收验收 |
| MEM-03 | R3 | `chapter_frame_service.rs`、Store 持久任务存在 | 200 文件、2 worker／64 窗口、source/freeze/取消／重启及真实 drain，不能沿用历史“未写实现”也不能直接标全通过 |
| MEM-04 | R4 | `c7703f8`；`probe_manager/dispatch.rs` 容量64/16及shutdown入口存在 | fingerprint channel 仍携 `(id, FingerprintWork)`，shutdown 使用 abort；ID-only 与真实 blocking 工作回收要求不能因入口存在而关闭 |
| MEM-05 | R5 | `web/lib/http.ts` 已接 `linkRequestSignal`，mjs 回归存在 | 保持 finally 释放、初始已 abort／中途取消／throw 和 1k 同 signal 完成后的0监听；历史 web 绿测不替代当前测量 |

## 3. 保留的行为与容量合同

### 读取、分页与发布

- `CONTEXT.md` 词汇；domain 无 IO；store/jobs/media-server 不依赖 api。静态图片 public，播放流／字幕按 Jellyfin 协议鉴权、即时撤销。
- Store/Queue 不嵌套，Registry→Store 锁序；同步 DB/FS/CPU 进入有界 blocking。permit 持有至真实 worker 结束；timeout 只结束等待，不能丢 JoinHandle 假称已停。
- owned 快照不授权后续删除／Transfer／heartbeat。发布前重校验 source version、Library scope、freeze／删除与用户权限；未知／错误 fail-closed。当前新模型不得沿用旧默认归属回退。
- 文件核对保持深度≤5、每 root 一次遍历、100,000 项及30秒协作预算；symlink 不逃逸，权限／IO错误标不完整。单个 FS 调用不能硬中断；preview 不是 apply 授权。
- 普通 HTTP 工作沿既有8容量／30秒预算，Busy→503、等待超时→504；不得错误返回空集合或转无限慢路径。
- 协议与 REST 在选页前完成所有过滤、排序与准确 count；name/time ties 稳定，unknown parent 空页，显式 IsPlayed 与默认 played-last、同 Media 多库可见性不退化。NextUp 一次候选聚合，不按 Media 重扫全库。
- 本页之外不构建 tracks/chapters/NFO 等重事实；高级过滤如仍 O(M+U)，明确披露。OFFSET 深页不宣称常数成本；新的分页契约以现行接口为准，不能把旧“省略 limit 全量”要求复制成新 API 兼容层。
- 统计不能用假0表示未就绪；scope/config/文件变更后计数、size、stale／refreshed_at 必须真实。用户 unit 变更不得错误重建或污染公共卡片。

### 资源与恢复

- 海报真实上游并发4，等待10秒；负缓存 TTL30秒、最多4096条，全局到期回收＋容量淘汰；gate 同4096上限，全部 live 满时503，同 key 仍 join，旧 guard 不误清新代；图片8MiB限制保留。
- 章节实际 worker2、内存窗口64；按 file/source/targets 唯一持久义务，full 延后不丢。无runtime只 persist，不另开无限线程；不可变暂存＋版本／lease 发布；取消 kill+wait+reader join，崩溃 running 恢复。
- Probe metadata pending64、fingerprint pending16，active metadata沿配置1..8、fingerprint1；持久 stage为来源，metadata→fingerprint先持久再派发，旧 job 消息不完成新 retry。seen 只限窗口＋active；重启定向读取窗口。保持 metadata 优先，饥饿应测量而非扩大队列掩盖。
- shutdown 必须等真实 worker／blocking／child／reader 收尾；不能把 abort async wrapper 当 drain。当前 R4 与理想 ID-only 合同的差异仍需独立处理。
- request-signal 仅抽取消生命周期：无 external 时15,000ms，external 不增加本地 timeout，保留原取消错误／401行为；release 幂等且所有出口 finally 执行。

### 分类 retention

- 复用现有 Job/config/报告，不另建 scheduler；data_retention 默认 `enabled=false,dry_run=true`、周期86400秒，默认90天、范围1..36500。每批500、最多20批或2秒；这是批边界预算，不是单 SQL／FS 的硬停止承诺。
- QoE 不影响进度／收藏／session；cache 只删已分类 transient search/discover/trending，未知／详情 stale-if-error 保护，TTL不是可删期限；Probe只删超龄终态，同scope保留最新3及ties，active／sample引用保护，attempt随安全可删job处理，不删samples/models/members。
- preview/dry-run零写，不推进游标；实跑恢复游标、examined/deleted/protected/incomplete 分开，lower bound不假报全量。关闭后不启动下一批，未知版本／错误不能回落更激进默认。物理文件／用户数据删除需另行授权。

## 4. 未关闭的测量与发布门（原 X1）

1. 当前公开 seam 组合验证：慢 IO＋维护并发时认证／heartbeat推进；取消／重启不突破资源限额、不丢持久工作；后台修改下的 scope、名单、count、顺序等价。gate 在断言失败时也必须释放。
2. release 同fixture/seed、每格≥5轮：100/2k/10k Media，1/5/20 Library，50k files，Probe100k pending，章节200文件，负缓存10k失败ID，同signal1k请求。分别测 cold/hot/mixed、有无NFO、同Media多库／hidden/nested/ambiguous/frozen、缺失／同路径替换。
3. 旧 D1 固定目标1k→50k VM≤2倍、D2整轮100→1k VM≤15倍是原目标，不是当前结果；模型改变后先建立对应查询基线，不拿旧 benchmark 自动关闭。热页50k→100k无关files时重facts解码只限本页，NextUp不随series数量重复全读。
4. 记录 median/p95、response bytes、CPU、RSS峰值及静止后、线程／子进程、锁等待／持有、VM/decoded rows、DB/WAL大小。无数据写 Not run，不用 checksum 循环代替数据库／接口规模证据。
5. crash/restart/source替换/freeze/取消／清理disabled 的恢复与发布须独立实证。NAS、真实ffmpeg、VidHub／Infuse按授权环境记录版本、Range206/416、断开取消与图片展示；router mock不是设备验收。
6. 后续实现按公开行为先 RED→GREEN，触及crate独立验证、稳定树统一workspace及web checks，记录真实exit与初始失败。本次文档整理未运行这些命令；历史结果不覆盖新存储正确性缺陷。

## 5. 旧稿清理范围

已合并并移除：`2026-10-15-database-remediation.md`（D1–D5→§2–4）、`2026-10-15-api-performance-remediation.md`（A1–A6→§2–4）、`2026-10-15-memory-resource-remediation.md`（R1–R5→§2–4），以及 specs 下同日期共享设计（安全／容量→§3、矩阵→§4）。源文件版本保留在 `bd6c3c8` Git 历史。删除原因是重复步骤与已经被 cutover 替代的模型指令，不是按日期推断要求已关闭。

旧 [10-13 follow-up](<2026-10-13-database-api-followup.md>) 的配置、历史逐ID核验和设备／性能门仍独立保留；DB8实际维护合同见 [DB8](<2026-10-12-db8-maintenance-retention.md>)。本页不批准pool、旧owner索引、数据库兼容迁移、自动清理、VACUUM或生产切换。
