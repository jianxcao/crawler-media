# Storage Redesign Review and Remediation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **执行状态：** 本文最初为仅审查方案；用户随后要求修复所列问题，当前已进入实现与验证。以下审查结论和基线记录保留为历史证据，执行结果见文末。

**Goal:** 对当前数据库重构相对 main 的实现进行审查，给出数据正确性、无旧版兼容、并发安全和数据库效率的修复方案。

**Architecture:** 保留按生命周期划分的五文件结构，以及 Media、Library、Playback 同属 library.db 的事务边界。收敛为完整版本校验的写入口、显式 UnitKey 的分集业务、锁外观察和有界 blocking 工作；跨库操作继续使用持久阶段与幂等恢复，不增加旧布局导入或双读双写。

**Tech Stack:** Rust 2024、rusqlite/SQLite WAL、parking_lot、Tokio、Axum、React/TypeScript。

**Spec:** 本文“审查要求”记录本轮用户要求；领域词汇以 `CONTEXT.md` 为准，开发与测试约束以 `AGENTS.md` 为准，数据库/身份/Job/删除边界见 ADR-0008、0001、0009、0012。本文未把历史 audit 或 benchmark 的结论当作本轮验收。

## Global Constraints

- 初始要求为仅审查；用户随后明确要求修复，授权覆盖该限制。
- “因为我们无需兼容旧版”，后续方案不得增加旧库迁移、兼容视图、旧 DTO 回退或双读双写。
- “Tests first at the behavior's public seam (red → green).”
- “Each crate must be testable **alone**”。测试不得需要真实 TMDB、Downloader 或 Chromium。
- 后续执行时，每个被修改 crate 运行 `cargo test -p <crate>`，最后运行一次 `cargo test --workspace`；前端运行测试和类型检查。
- 只提交本任务内容，不创建 GitHub issue。
- `.rs` 文件硬上限 800 行，生产函数和测试函数硬上限均为 120 行；按职责拆分。
- 静态图片端点保持 public；Jellyfin 流媒体继续遵守其认证合同。

---

## 审查要求与范围

1. 判断代码结构、数据库职责和约束是否合理。
2. 检查后端旧逻辑是否删除、前端是否完全切换到新 API，不保留旧版兼容。
3. 检查生产和测试中的死锁、永久等待、执行器停顿及长锁风险。
4. 给出可以进一步优化的数据库路径，并区分正确性修复和需要测量的优化。

基线：`main = 756444f76bef53f5fabed6d0ef53e9f280e005e9`。
审查对象：`feat/storage-redesign = 710ac08304cfc79a83404958cd9d4c234c930889`。
两者 merge base 即上述 main。范围为一个提交、1,480 个文件，新增 173,713 行、删除 63,253 行；这不是对每一行完成穷尽证明的声明。

审查开始时工作区干净。验证使用临时数据库、现有测试和仓库外的复现程序；唯一计划中的仓库写入是本文档。

## 总体判断

数据库大方向合理：app 保存实例配置与凭证，library 保存 Media/Library/Playback 权威事实，subscribe 保存策略和下载事实，jobs 保存可恢复执行事实，cache 保存可重建外部响应。身份、物理文件、逻辑覆盖、用户观看事实分开建模，比 main 的旧数据组织更清楚；同库事实与派生摘要同事务维护也有价值。

当前实现还不宜视为重构验收完成。关键问题集中在调用方没有完整消费新模型、观察结果缺少版本隔离、业务异步任务仍同步等全局锁，以及独立计数与队列发布顺序不一致。继续加索引或增加读连接不能修复这些问题。

以下 P1 是应先修复的问题；P2 是明确的行为或规模问题；清理项单列，避免把注释和正常 UI 转换误报成运行时兼容。

## 发现

### R1 · P1 · 旧 Presence 观察能误标或删除同 ID 的新版本台账

**位置：** `crates/store/src/ledger_presence.rs:108–126`、`crates/store/src/library_write/publish/file_row.rs:257–276`，生产入口 `crates/api/src/library_presence.rs:244–351`。

Presence 查询只取 Library、Media、path、missing_at 和 publication，比较时不核对 `FileCoverage.file.version`、Library/root。相同 file_id 在原位置替换内容后，旧观察仍匹配。发布 Present 新版本又没有清 missing_at，进一步使旧 Prune 命中。

**已复现：** v1 标 missing_at=1000 → 同 ID 发布 v2/Present → 施加 v1 的 Missing/Prune，返回 `deleted=1, skipped=0`，v2 台账消失。另一例旧 Verify 把 v2 标成 Missing。这里删的是数据库台账及派生事实，不是证明媒体字节被删除。

**方案：** 同一 IMMEDIATE 事务内校验 file_id、Library/root、path、Media、完整 FileVersion 和观察时状态；Present 发布同步清 missing_at。生命周期后续动作必须以实际应用的文件和版本为依据，并在取消/重排探测时保证同一版本绑定。当前 API 在 `library_presence.rs:355` 后把全部 observations 交给生命周期处理，即使 Store skipped 也可能取消新任务，应一并收敛。

**测试缺口：** 现有 replacement 用例主要更换 file_id，未覆盖保留 ID、递增版本。Integrity 冻结控制实验已确认会被下游 guard 拒绝并回滚，本文不报告“Presence 绕过 Integrity”。

### R2 · P1 · 多单元文件仍被压回第一集，合法 E2 标记静默丢失

**位置：** `crates/store/src/library/markers/merge.rs:40–42,112–116`；`crates/api/src/marker_resolver.rs:36–49`；`crates/api/src/probe_manager/projection.rs:14–25`；`crates/api/src/fingerprint_job.rs:34–53`。

新模型允许一个文件覆盖多个 UnitKey，但这些路径仍 `units.first()`，继续使用旧的单季/集形参。章节入口 `library_chapters.rs:153–167` 能用 `units.any()` 选中 E2 对应文件，随后 resolver 却改回 E1。前端 `library-item-detail-view.tsx:335–336` 仍从文件首坐标推导轮询季号。

**已复现：** 一个文件覆盖 E1/E2，公开 `merge_marker_results_checked` 保存 E2 返回 Ok、chapter_updates=0、marker_stored=false；E1 对照返回 Ok、chapter_updates=1、marker_stored=true。

**方案：** 物理轨道探测仍按文件；marker、章节叠加和刷新范围显式带 UnitKey。删除“新覆盖 → 旧单行”机械投影。给定可靠 E2 结果应能独立保存；自动探测若缺乏合并文件的分集时间映射，应明确拒绝相应操作并保留已有 marker，不能把第一集结果复制给全部单元，也不能清空没有完成探测的单元。物理章节缓存与分集 marker 叠加分开，防止 E1/E2 互相覆盖缓存。

### R3 · P1 · 消息先发送、pending 后增加，可能永久阻塞声纹

**位置：** `crates/api/src/probe_manager/dispatch.rs:186–189,251–255`；`queue.rs:6–21`；`worker.rs:153–154`；`progress.rs:200–209`。

当前 `try_send` 成功后才增加 metadata_pending。生产者发送后被暂停，消费者可以正常加载、完成或失败探测，先 drop MetadataStageGuard。此时 pending=0，完成函数仅记录异常；生产者恢复后加到 1，没有对应工作再次扣减。CaptureGate 将永久等 metadata_idle。

这是可达的并发顺序缺陷，尚未用注入调度的真实 worker 测试固定复现。main 的旧顺序是先增加、发送失败再回滚。复核了 seen/Store 锁：正常加载到 guard drop 的路径不取 seen，清 seen 在后续 finish；不能依靠 seen 锁保证计数先于完成。失效快照路径会碰 seen，不作为本发现的触发证据。

**方案：** 两个发送入口统一 `try_reserve → pending +1 → permit.send`，Full/Closed 不动计数；通知和实际消费者结束使用同一生命周期。补确定调度交接测试，不能依赖压力测试偶然碰到。

### R4 · P1 · 新异步路径仍同步等 Store，执行器会停顿

**位置：** `crates/api/src/http/metadata_language.rs:19–20,40–45`；`storage_operations.rs:433–496`；`probe_manager/worker.rs:256–262`。

metadata-language GET/PATCH 直接在 async handler 内 lock。重配置 async drive 在 runtime 上执行整批 `advance_library_operation`；新增 window dispatcher 也直接调用会取 Store 的 refill。长锁或长 SQL 会占住 Tokio worker，等待该 worker 调度的 timeout 本身也不能保证按时运行。

**已复现：** 真实 Router、current_thread runtime、外部线程持 Store 800ms；请求 GET metadata-language 时，无关 10ms timer 实际为 805.820792ms，`<200ms` 断言失败。这证明执行器阻塞；不是 Mutex 环形死锁的证明。

**方案：** 读端复用 store_read；写端用有容量限制的 blocking reservation，等待真实执行结果。驱动每批将租约校验和 SQL 工作移入 blocking worker，保持 Registry→Store 锁序；批间释放 Store、检查取消并让出调度。dispatcher 和 MetadataStageGuard 触发的同步 refill 也应离开执行器。

**测试缺口：** executor_store_bounds 在 runtime 外创建 ApiState，排除了后台驱动，且只覆盖 policy/history 两个端点；通过这些测试不能推出所有 handler/driver 都安全。

### R5 · P2 · 秒写入毫秒字段，刚完成的操作会被立即清理

**位置：** `crates/api/src/storage_operations.rs:104,367,475`、`crates/api/src/http/metadata_language.rs:44`；清理比较见 `crates/store/src/operations_retention.rs:193–201`。

这些调用向 now_ms 传入 unix_now() 秒。created_at_ms/updated_at_ms 因而小 1,000 倍，30 天保留的毫秒 cutoff 把刚完成的操作当成很久以前的数据。

**已复现：** public begin_library_integrity 保存 updated_at_ms=1793077770，实际 now_ms=1793077770000；沿当前 driver 的秒参数完成操作后，30 天保留立即删除 1 条，原 operation 查不到。新操作历史、轮询和诊断报告过早消失；不等价于删除了整个 Library。

**方案：** 所有持久 operation 统一明确的毫秒时钟；现有需要秒的 Job/cache 等调用保持其合同。引入类型或命名约束以防再次混用。按无兼容要求，不补“秒/毫秒猜测后换算”的读取回退。

### R6 · P2 · 完整 SDR Probe 保留 Release 猜测的 HDR

**位置：** `crates/store/src/library_write/probe.rs:128–145`。

`hdr=COALESCE(?4,hdr)` 无法表达“已确认没有 HDR”。SDR 的 hdr=None 被当成“不更新”，原 Release HDR10 留在热列，但 quality_source 已标 Probe，影响卡片筛选和 Subscribe/Wash-cut 质量判断。

**已复现：** Release HDR10 → 完整 bt709/1080p/h264 Probe，结果仍为 hdr=Some(HDR10)、quality_source=Probe。

**方案：** 显式区分 NotObserved 与 Observed(None)。完整视频观察允许写 NULL；局部更新只对未观察字段保留原值。换 source_version 时同步检查热列与冷详情的失效规则。

### R7 · P2 · 省略开关的新建 Library 请求默认开启昂贵探测

**位置：** `crates/api/src/http/library_config.rs:130–134`。

detect_intros/enable_fingerprint 的省略默认由 main 的 false 改成 true，与当前 DDL DEFAULT 0 和 ADR-0012 不一致。最小 POST name/kind/root_paths 就会开启指纹及片头片尾探测，创建后还自动启动扫描。前端显式发 false，因此 UI 测试容易漏掉。

**方案：** 恢复缺省 false，用创建 DTO 表达缺省和拒绝未知字段；保留显式 true。现有 library_create_detection 仅覆盖显式 true/false，应补省略和错误类型。

### R8 · P2 · Presence 仍全实例取数，且每个文件重复全量重建同一 Media

**位置：** `crates/api/src/library_presence.rs:261,281–283`；`crates/store/src/ledger_presence.rs:84–92`；`library_write/reduce/card.rs:22–50,114–141`。

核对一个 Library/Media 时先读全实例 coverage 和 missing map，再内存过滤。写入用 Vec 收集每个变化文件的 Affected，同一 Media 反复执行 full reducer，重复读取全部文件、重建 units 和用户摘要。N 个同 Media 文件变化时，仅重复读文件部分就有 N×N 工作量，且在同一个事务和 Store guard 内。

**已测：** 一次 Verify 修改同 Media 的 4 个文件，missing=4，item_revision 增加 4，发生 4 次重建；这不是延迟 benchmark。

**方案：** 读取按 Library/可选 Media 的 SQL keyset 窗口；锁外观察；写入去重 `(LibraryId,MediaId)`，每窗 settle 一次；窗口间释放 Store。大 Media 的批次继续用增量摘要，避免每窗扫描整部 Media。

### R9 · P2 · 声称 pure 的 Subscribe facts 仍在 Store 内做文件系统解析

**位置：** `crates/store/src/subscribe_library.rs:59–110`；调用方 `crates/api/src/completion.rs:35–39`、`air_gate.rs:139–144`、`http/downloaders/submission.rs:224`。

evaluate_subscribe_facts_pure 和 load_library_subscribe_facts 最终仍调用 LibraryPathScope::resolve，解析全部 Library 根，saved facts 又 resolve_path。生产调用方持 Store guard。慢 NAS 的 canonicalize 会阻塞全局数据库访问；“is_file 使用恒 true”没有使整个函数变纯。

**方案：** 复用 subscribe_library_read 的 owned snapshot 模式；记录归属直接消费 library_id，只有未登记 saved path 才需锁外解析。最终入选或提交前重验作用域 fingerprint。删除隐藏 IO 的 Store 便利入口，pure evaluator 的输入包含已经解析好的 saved-fact 归属。

### R10 · 清理 · 无兼容要求尚未完全收口

- `web/lib/api/discover.ts:529–540` 仍接受裸数字引用，非 douban 的来源都会退到 TMDB。应仅接收明确支持的稳定引用；其他来源明确拒绝，不能误发 TMDB 请求。未证明当前正常 UI 会传这些旧输入，因此按合同清理处理。
- `crates/store/src/schema/probe_backpressure.rs` 是未被 mod 引用的已跟踪文件，仍含旧 ledger 表的修补 SQL；应直接删除。这不是当前启动时仍执行迁移的证据。
- `apply_presence` 与生产 `apply_ledger_presence` 重复，前者只有测试消费者，Prune 还绕过统一 cleanup。保留一个生产 seam 并迁移测试，避免测试证明的是另一套逻辑。
- RuleSetSpec/specToAtoms 当前用于表单与 atoms 转换，实际 HTTP 只发新合同；titleRef 当前是稳定引用编码。这些不是旧服务器兼容，不应整段删除。可把表单草稿移出 API 层、改名并清理过时注释。

已核查的正向结果：旧 /directory 前后端入口已移除；Transfer settings 使用 revision CAS；主前端 API 使用新信封和 UUID；新 file units 已进入 DTO；静态图片仍在 public router。不能据此宣称所有新模型消费者均已迁完，R2 是明确反例。

## 进一步的数据库优化

先修 R1–R9，再评估下列索引。只列当前 SQL 的实际缺口，不主张为每个字段都加索引。

| 路径 | 当前证据 | 候选改动 | 验收 |
|---|---|---|---|
| fingerprint_attempts 按 file_id 清理 | ledger_cleanup.rs:41；只有 (job_id,file_id)，EXPLAIN 为 SCAN | file_id 前缀索引 | 大历史量下删除 VM/持锁时间下降，保留 job 查询效率 |
| playback_session_ids 按 file_id 关闭 | reassign.rs:201；EXPLAIN 为 SCAN | file_id 部分索引，谓词与 state<>'closed' 相同 | 重挂文件成本不随无关历史线性增长 |
| media_operations 终态保留 | EXPLAIN 为 SCAN + TEMP B-TREE | 终态谓词下的 (updated_at_ms,operation_id) 部分索引 | 500 行窗口的扫描和排序有界 |
| library_operations 终态保留 | EXPLAIN 为 MULTI-INDEX OR + TEMP B-TREE | 与 owning terminal predicate 对齐的部分索引 | 同时检查新增索引的写放大与现有 dispatch/index 重叠 |

EXPLAIN 使用当前 library.sql，在临时内存数据库执行，SQLite 3.54.0；这是查询计划证据，不是生产规模性能或 bundled SQLite 下的耗时验收。

不建议此时再次拆 Media/Library/Playback 数据库，也不建议去掉正确性所需的摘要。优先压缩全实例取数、重复 reducer、长事务和锁内文件 IO，再按实际读写量决定索引、PRAGMA 或额外连接容量。

另一个当前容量问题是 metadata receiver 使用 Option::take 等消息，其余空闲 worker 看到 None 即退出，可能退化为单 worker；该实现 main 已有，单列为既存问题，不能算本次新增回归。后续用两个受控 TracksProbe 同时进入的行为测试验收并发度，不能仅检查 JoinHandle 数量。

## 并发判断与测试设计边界

未确认 Store/Jobs/reader lease/Library registry 主路径存在新的 Mutex 环形死锁。已确认执行器停顿，已识别可导致永久等待的计数竞态。二者都必须修，但不要统一称为“数据库死锁”。

现有测试运行中出现过超过 60 秒的分页/万集扫描用例；在继续输出并完成前不能仅凭该提示判断死锁。新并发测试必须用外部线程/进程 watchdog：若 runtime worker 被同步锁占用，runtime 内的 tokio timeout 自身也可能无法推进。

## 后续文件与责任划分

| 任务 | 修改范围 | 职责 |
|---|---|---|
| T1 | store ledger_presence / publish file_row；api library_presence | 完整版本 CAS、实际应用结果、同版本探测生命周期 |
| T2 | store marker merge；api marker_resolver/projection/fingerprint/chapter；web detail | 显式 UnitKey，删除单行模型适配 |
| T3 | api probe dispatch/queue/worker/progress | 消息发布与 pending 生命周期一致 |
| T4 | api metadata_language / storage_operations / probe worker | 有界 blocking 工作和可测试的执行器响应 |
| T5 | api operation 时间入口 | 统一毫秒时钟，无旧值读取回退 |
| T6 | store probe publication；api scrape_store 等调用 | 完整视频事实与局部更新的区别 |
| T7 | api library_config；web discover；store dead module | 严格的新 API 合同和默认值 |
| T8 | store/api Presence 查询；Subscribe snapshot evaluator | 有界读取、增量摘要、锁外文件 IO |
| T9 | store DDL/object lists；相应规模测试 | 逐个测量并补足缺失索引 |

## 后续执行任务

以下保留原审查时的任务和检查项；实际执行结果与最终验证记录见文末。任务边界以可独立审查和测试的行为为准。

### T1: Presence 的版本隔离与统一生命周期

**Files:** 修改 `crates/store/src/ledger_presence.rs`、`library_write/publish/file_row.rs`、`library_write/delete.rs`、`crates/api/src/library_presence.rs`；测试 `crates/store/tests/ledger_presence.rs`、`crates/api/tests/probe_missing_source.rs`、`crates/api/tests/library_presence_safety.rs`。

**Interfaces:** 生产入口保留 apply_ledger_presence；结果增加已应用条目的 file_id、FileVersion 和明确 outcome。删除仅测试使用的 apply_presence；取消/恢复消费实际应用的同版本结果。

- [ ] 在 ledger_presence 现有 fixture 中加入保留 file_id 的 v1→v2 测试；Verify 和 Prune 都必须 skipped=1，v2/Present 保留。以下是 Prune 核心行为，使用该测试文件现有 helper：

```rust
let old = store.file_coverage_by_id(file.file_id).unwrap().unwrap();
let mut replacement = old.file.clone();
replacement.version.source_version = "v2".into();
replacement.presence = domain::FilePresence::Present;
store.publish_files(
    &WriteScope { library_id: replacement.library_id, config_revision: 0 },
    &[StorePublication {
        file: &replacement, units: &[UnitKey::WHOLE],
        expected_version: Some(&old.file.version),
        nfo_observation: None, probe_required: false,
    }], 2000,
).unwrap();
let result = store.apply_ledger_presence(
    &[observation(&store, old, Some(1000), ObservedPresence::Missing)],
    LedgerPresenceMode::Prune, 3000,
).unwrap();
assert_eq!(result.skipped, 1);
assert!(store.file_coverage_by_id(file.file_id).unwrap().is_some());
```

- [ ] `cargo test -p store --test ledger_presence`，确认新增断言在现实现失败；补 API 用例确认 skipped 观察不取消 v2 探测。
- [ ] 查询并比较完整身份；写 Present 时同步清 missing_at；让删除走统一 ledger_cleanup，生命周期版本校验与数据库事实一致。
- [ ] 运行 `cargo test -p store`、`cargo test -p api`；保留 Integrity 冻结回滚、换 Library/root、事务中途失败的控制测试。
- [ ] 只提交 T1 文件：`fix(storage): reject stale presence generations`。

### T2: 多单元 marker 与章节切换

**Files:** 修改 `crates/store/src/library/markers/merge.rs`、`crates/api/src/marker_resolver.rs`、`probe_manager/projection.rs`、`fingerprint_job.rs`、`http/library_chapters.rs`、`web/components/library-item-detail-view.tsx`；新建 `crates/store/tests/file_unit_markers.rs`，扩展 API 章节测试。

**Interfaces:** marker/章节查询显式消费 `(file_id, FileVersion, UnitKey)`；物理探测只消费文件；可靠的分集结果以 `(MediaId,UnitKey)` 保存。自动探测缺少分集时间映射时返回明确的不支持结果并保留现有事实。

- [ ] 用 `store::test_support::publish_fixture_file_units` 发布 E1/E2 同文件；公开 merge 与读取的核心断言：

```rust
let members = store::observe_sources(
    &store.source_snapshots(&[file.file_id]).unwrap(),
);
let result = StoredMediaMarker {
    media_id, season: 1, episode: 2,
    intro_start_ms: Some(10_000), intro_end_ms: Some(70_000),
    outro_start_ms: None, outro_end_ms: None,
    source: "review".into(), locked: false, updated_at: 0,
};
store.merge_marker_results_checked(
    &[coverage], &members, &[result], false,
).unwrap();
assert!(store.get_media_marker(media_id, Some(1), Some(2)).unwrap().is_some());
```

- [ ] `cargo test -p store --test file_unit_markers`，确认 E2 断言失败；增加公开章节 E2 不使用 E1 marker、跨季覆盖和刷新保留未处理单元测试。
- [ ] 去掉所有首单元投影；按请求 UnitKey 解析 marker，在物理章节基础上按单元叠加，前端使用完整 seasons/units。
- [ ] 运行 `cargo test -p store`、`cargo test -p api`、`cd web && npm test` 和 `npm run typecheck`。
- [ ] 提交 `fix(playback): preserve explicit unit identity in marker workflows`。

### T3: 探测队列的发布顺序

**Files:** 修改 `crates/api/src/probe_manager/dispatch.rs`、`queue.rs`、`worker.rs`；测试放 `probe_manager/queue_tests.rs` 或职责独立的相邻测试模块。

**Interfaces:** 一个成功交给消费者的 metadata 消息始终已有一个 pending 计数；Full/Closed 无计数变化；一次真实结束恰好归还一个计数。

- [ ] 增加仅测试的交接观察点，放在消息已可见、发送函数尚未返回处；受控消费者在生产者返回前完成。公开 CaptureGate 的等待必须有界结束；同时覆盖 Full/Closed、取消和错误结果。
- [ ] 运行 `cargo test -p api metadata_dispatch`，用外部 watchdog 固定测试超时；确认旧顺序会留下 pending=1，不能以随机 sleep 代替交接控制。
- [ ] 两个入口改用下面相同的发布顺序；reservation 失败返回原错误，不增加计数：

```rust
let permit = tx.try_reserve()?;
self.metadata_pending.fetch_add(1, Ordering::AcqRel);
permit.send(id);
```

- [ ] 运行 `cargo test -p api`，验证单元完成、队列耗尽和优先级 gate；所有通知都以实际归零为准。
- [ ] 提交 `fix(probe): account for metadata before channel publication`。

### T4: 异步路径离开执行器等锁

**Files:** 修改 `crates/api/src/http/metadata_language.rs`、`storage_operations.rs`、`probe_manager/worker.rs`、`queue.rs`；扩展 `crates/api/tests/executor_store_bounds.rs` 和 `driver_executor_bounds.rs`。

**Interfaces:** 只读 handler 复用 `http::store_wait::store_read`；驱动每批 owned 输入进入有界 blocking 工作，返回持久状态后再决定下一批；timeout 不提前释放真实 worker 所持 permit/lease。

- [ ] 在 current_thread runtime 同时执行 metadata-language 请求和 timer；外部线程持 Store 800ms，timer 必须 `<200ms`。扩展到实际 reconfiguration 和 dispatcher 启动，不只测试无后台任务的 ApiState。
- [ ] 先运行上述测试确认现实现失败。外部线程的 `recv_timeout` 或子进程 watchdog 是最终保险，不能只依赖 Tokio timeout。
- [ ] GET 使用现有 store_read；PATCH 和驱动每批在 blocking reservation 中取得有界 Store guard、执行 SQL、返回状态。驱动不能持 Store 等 registry、reader pool 或网络；批间释放 guard。
- [ ] 运行 `cargo test -p api --test executor_store_bounds`、`cargo test -p api --test driver_executor_bounds`、`cargo test -p api`。
- [ ] 提交 `fix(runtime): bound storage work outside tokio workers`。

### T5: Operation 时间单位

**Files:** 修改 `crates/api/src/storage_operations.rs`、`http/metadata_language.rs`；测试 `crates/api/tests/metadata_language_operation.rs`、`storage_operations_runtime.rs`，Store 保留规则测试继续保留。

**Interfaces:** operation 的 `now_ms` 一律从明确毫秒时钟取值；Job 调度的秒时钟保持独立。可复用已有 `state.playback_clock.now_ms()`，或引入同语义实例时钟，不增加猜测旧值的读取逻辑。

- [ ] 通过公开入口创建/完成 operation，然后执行 30 天保留；断言当天操作保留，超过 30 天且已终态的操作删除，active/可重试失败不删。
- [ ] 固定测试时钟，例如 `now_ms=1_800_000_000_000`、cutoff=`now_ms-30*86_400_000`，先确认现实现错误删除。
- [ ] 修正 begin/advance/default-locale 各调用；不混用 `unix_now()` 和 `_ms` 参数：

```rust
let now_ms = state.playback_clock.now_ms();
```

- [ ] 运行 `cargo test -p api` 和 `cargo test -p store --test operations_retention`。
- [ ] 提交 `fix(storage): stamp durable operations in milliseconds`。

### T6: 完整 Probe 的空值语义

**Files:** 修改 `crates/store/src/library_write/probe.rs` 和所有 ProbePublication 构造点；测试 `crates/store/tests/probe_publication.rs`，扩展 API 探测到质量事实的用例。

**Interfaces:** 使用明确的字段观察状态，区分没有采集和确认无值：

```rust
pub enum Observed<T> {
    NotObserved,
    Value(T),
}
// HDR 的 T 为 Option<&str>：Value(None) 明确写 NULL。
```

- [ ] seed Release HDR10，发布完整 SDR 视频事实，断言 `file.hdr == None`、quality_source=Probe，卡片和 Subscribe 质量事实一致；补局部未观察保留 HDR、换代拒绝旧 Probe。
- [ ] `cargo test -p store --test probe_publication`，确认完整 SDR 断言先失败。
- [ ] 完整观察直接写相应值，包括 NULL；只有 NotObserved 保留旧值。不要用统一 COALESCE 处理两者。
- [ ] 运行 `cargo test -p store`、`cargo test -p api`；任何额外被修改 crate 也单独验证。
- [ ] 提交 `fix(probe): clear absent hdr after complete video observation`。

### T7: 默认值与旧输入严格收口

**Files:** 修改 `crates/api/src/http/library_config.rs`、`web/lib/api/discover.ts`；删除 `crates/store/src/schema/probe_backpressure.rs`；测试 `crates/api/tests/library_create_detection.rs` 和 `web/test` 对应 discover 契约测试。

**Interfaces:** 新建 Library 省略 detect_intros/enable_fingerprint 即 false；稳定引用只支持当前已实现来源和形状，未知输入在发送请求前拒绝。

- [ ] 在现有创建 fixture 发送仅 name/kind/root_paths 的 POST，响应及持久 Library 都断言两个 false；另测显式 true 和非法类型，先确认缺省用例失败。
- [ ] 前端依次传 `603`、`tvdb:tv:603`、`tmdb:bad:603`，必须拒绝且不发请求；`tmdb:movie:603`、`tmdb:tv:1399`、`douban:1291843` 保持当前端点。
- [ ] 创建字段以显式默认和类型校验接收；删除裸数字和未知来源 fallback。删除未引用的旧数据库修补文件，保留正常 atoms 表单转换和 titleRef 编码。
- [ ] 运行 `cargo test -p api`、`cargo test -p store`、`cd web && npm test`、`npm run typecheck`；搜索旧路由/字段确保不再由真实调用方发出。
- [ ] 提交 `fix(api): enforce current storage contracts and defaults`。

### T8: 有界 Presence 和真正锁外的 Subscribe 评估

这两部分可分别提交；没有数据依赖，均在正确性修复后实施。

**Files:** 修改 `crates/api/src/library_presence.rs`、`crates/store/src/ledger_presence.rs`、`file_reads.rs`、`subscribe_library.rs`、`crates/api/src/subscribe_library_read.rs` 及 completion/air_gate/submission 调用方；测试相应 Presence 与 Subscribe 公共 seam。

**Interfaces:** coverage 页面输入明确包含 LibraryId、可选 MediaId、file_id keyset 和固定 limit；pure facts evaluator 只消费 owned/已解析输入，不做 canonicalize。提交前用现有 scope fingerprint 重验。

- [ ] Presence 行为测试保留所有统计和事实，再加工作量验收：同 Media 四个变化文件只 settle 一次；无关 Library 从 100 增至 10,000 文件，目标窗口的 coverage 读取不增长。
- [ ] 使用有界按归属读取替代 list_file_coverage/missing_at 全实例读取；每窗去重受影响键，事务外观察、窗口间释放 Store。
- [ ] Subscribe 用 gated path resolver，在 canonicalize 停住时验证其他线程仍能读取 Store；目录变更后旧评估不得被提交。先让现入口触发失败，再切到 snapshot→锁外评估→重验。
- [ ] 删除 load_library_subscribe_facts 中隐藏的文件 IO；纯计算先构建 path membership 集合，避免覆盖内循环重复扫描全部 snapshot.rows。
- [ ] 分别运行 `cargo test -p store`、`cargo test -p api`；记录 100/1,000/10,000 文件的 SQL VM、全扫描步数和锁等待，不以 wall time 单一阈值代替行为与工作量断言。
- [ ] 分别提交 `perf(presence): bound scope reads and settle each media once`、`refactor(subscribe): evaluate owned snapshots outside store locks`。

### T9: 按实际清理路径补索引

**Files:** 修改 `crates/store/src/schema/v1/library.sql`、`schema/v1/objects.rs`、`schema/layout.rs`；扩展 file cleanup/reassign/operations retention 的行为和规模测试；同步 ADR-0008 与数据库字典的当前版本描述。

**Interfaces:** 不改变清理/重挂/保留的业务规则；schema 变更一起调整声明与布局版本，旧布局明确拒绝，无 ALTER 导入和静默修补。

- [ ] 用当前 bundled SQLite 在临时库准备 100/1,000/10,000 历史行；记录四条候选 SQL 的计划、VM 和写入成本，确认 file_id 查询的无关历史扫描。
- [ ] 逐个评估这些候选 DDL；retention 谓词必须复用 owning module 的精确定义，不另外手抄不一致的终态列表：

```sql
CREATE INDEX attempts_file ON fingerprint_attempts(file_id);
CREATE INDEX session_ids_live_file ON playback_session_ids(file_id)
 WHERE state <> 'closed';
CREATE INDEX media_operations_terminal_age
 ON media_operations(updated_at_ms,operation_id) WHERE state='succeeded';
```

- [ ] 只有对应工作量改善且写成本可接受时才保留索引；Library retention 索引另验证 OR 谓词是否被规划器使用，不能仅凭 DDL 已创建宣布完成。
- [ ] 运行 `cargo test -p store`；验证空库新建、当前布局重开、旧布局拒绝；最后运行 `cargo test --workspace`、前端测试和类型检查。
- [ ] 提交 `perf(storage): index measured file cleanup and retention paths`。

## 本轮验证记录

已执行：

- 前端 `npm test`：847/847 通过。
- 前端 `npm run typecheck`：exit 0。
- `cargo test --workspace`：exit 0；462 个测试组，共 3,039 通过、0 失败、24 ignored。耗时超过 60 秒的分页/万集扫描用例最终完成，未把其判成死锁。
- 仓库外 schema harness：5 项，4 个正确行为断言失败，1 个 Integrity 冻结控制项通过；失败覆盖 stale Verify、stale Prune、完整 SDR 和重复 reducer。
- 仓库外多单元 harness：E2 标记不保存，E1 对照保存。
- 仓库外真实 HTTP 并发 harness：10ms timer 实测 805.820792ms，响应性断言失败。
- 仓库外 operation 时钟 harness：新完成 operation 被 30 天保留立即删除。
- 当前生产 DDL 的临时 EXPLAIN：上述清理和保留路径存在全表扫描/临时排序。

完整 Rust 测试日志：`/tmp/crawler-media-storage-review-workspace.log`。复现源码分别位于 `/tmp/crawler-media-schema-review/review.rs`、`/tmp/crawler-multiunit-review.rs`、`/tmp/crawler-media-review-20261027/operation_time.rs`；HTTP 并发复现位于系统临时目录 `crawler-review-concurrency-x43p8a7k/metadata_language.rs`。这些临时文件用于本轮复核，不是项目新增测试。

上述复现均针对当前实现，不是已完成修复的通过证明。未执行浏览器回归、生产实例切换、生产数据重建或 NAS 实机性能测试。

## 方案自检

- 四项用户要求均有对应发现和任务；每个发现区分已运行反例、静态并发推导、优化候选。
- 不把旧注释、正常表单转换或 Jellyfin 协议支持误报成旧数据库兼容。
- 未把下游已回滚的 Presence 冻结路径误报为可写，也未把既存 receiver 问题归入本次回归。
- 审查阶段制定的修复任务以测试先行和公开行为验收为原则；其后的执行结果另行记录。

## 修复实施记录

- Presence 使用有界范围分页、版本复核和批量归约；文件缺失时取消探测，恢复文件不会复活已取消任务。
- 物理章节缓存与各单元标记分开保存，播放和章节查询使用明确的单元；没有单元时间映射的自动多单元刷新明确拒绝。采样写入与缓存复用会检查任务取消状态。
- 探测派发避免在 Tokio worker 上等待 Store；繁忙时按单元重试，媒体信息到声纹阶段的交接使用事务。关闭超时会返回错误并保留数据库探测租约，直到旧工作退出；若 runtime 在排空时销毁，租约保留至进程退出。
- Subscribe 在锁外评估快照，并在入队或 Transfer 预留前重新验证；操作时间统一使用毫秒，完整 SDR 探测可以清除旧 HDR 事实。
- Library 创建严格校验当前输入，前端使用当前接口和字段；删除旧数据库修补代码。Library schema 为 v6，旧布局明确拒绝，不迁移或双读双写。
- 按实测清理和保留路径增加五个索引。10,000 行的 SQLite VM 粗测显示相关写入指令数增加约 7% 至 10%，不是生产耗时基准；未在生产数据或 NAS 上实测。
- 测试复核删除 `adaptive_review11_regression` 中与 `adaptive_review10_regression` 完全重复的 14 项，以及被完整同流程用例覆盖的 1 项 Library 删除测试；修正目录搜索测试的名称和无关准备步骤。强化取消后无样本和多单元物理章节内容断言。队列饱和测试改为真实文件和受控采集门控，WAL 与管理器重启夹具消除后台任务时序竞争。

最终验证：`cargo test -p store` 702 项通过、11 项忽略；`cargo test -p api --no-fail-fast` 1767 项通过、9 项忽略；`cargo test --workspace --no-fail-fast` 3096 项通过、24 项忽略。工作区测试启动后删除的重复 Library 测试及调整后的 catalog 测试，已分别验证保留的对应行为。前端 `npm test` 849 项通过，`npm run typecheck` 通过。未执行生产数据重建或 NAS 实机验证。
