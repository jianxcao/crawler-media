# 文档入口、权威层级与清理记录

更新：2026-10-28。当前代码复审固定于 `feat/storage-redesign@5730b032`，基线为已 fetch 核实的 `main/origin/main@756444f7`。**仍有执行器阻塞、NFO 失败意图无法退出、热点查询放大和游标作用域问题，建议修复后合并。** 本轮仅文档变更，不修改运行代码或用户实例。

## 当前先读

1. 术语：[CONTEXT](../CONTEXT.md)；工程约束：[AGENTS](../AGENTS.md)。当前用户请求是工作单元，不主动创建/关闭 issue。
2. **唯一当前复审与剩余方案：** [2026-10-28 再次复审](audits/2026-10-28-storage-main-rereview.md)。含 R1–R5、前轮 F1–F8 对账、实施顺序及验证边界。旧计划 unchecked 不代表当前待办。
3. **目标合同：** [存储设计](superpowers/specs/2026-10-18-library-storage-redesign.md)、[接口/字段合同](superpowers/plans/2026-10-18-library-storage-contracts.md)、[来源缓存合同](superpowers/specs/2026-10-18-source-separated-cache.md)。目标合同与源码现状分别核对，不添加旧布局/旧自有 API 兼容。
4. **现行结构：** [五文件数据库字典](database/README.md)，library v7。唯一可执行 DDL 在生产源码，docs 不保留另一份建表草案。
5. **历史证据：** [大命中查询](benchmarks/2026-10-25-large-hit-query-read-model.md)、[读取隔离](benchmarks/2026-10-25-library-read-isolation.md)、[结构审计](audits/2026-10-26-library-schema-audit.md)、[性能基线](audits/2026-10-26-perf-baseline.md)。结果只对各自版本/规模负责。
6. **运行/API：** [deploy](deploy.md)、[自有 API 约定](api-contracts/self.md)；完整路由以 [HTTP router](../crates/api/src/http/mod.rs)、[路由表](../crates/api/src/http/routes/mod.rs) 和 `web/lib/api/*` 为准。
7. **验收清单：** [system regression](testing/system-regression-cases.md) 是场景清单，不是已运行证明；设备、NAS、完整容量未测须明确 Not run。

## 权威与保留规则

- 当前用户要求/CONTEXT/接受的ADR定义产品边界；Spec定义目标，计划分配实现任务，源码/公开行为证明现状。三者冲突时修正文档，不用旧计划为兼容代码背书。
- 文档必须区分**目标、已有代码、运行观察、历史记录**。源码存在不是行为通过；当时通过不是今天仍通过，旧失败不是新版本仍坏。
- ADR保留决策和取舍，历史段明确时间/版本；不因改实现删唯一决策证据。运维页不执行历史迁移/删库草案。
- 旧执行步骤被完整替代后移除，未闭环需求必须先迁到保留合同/映射；不按日期、unchecked或文件存在推断关闭。
- 审计finding ID、独有测量、用户批准与未测边界保留，逐ID代码+行为证据对账后才关项。不要为“清理”删除反例或篡改历史数字。
- 删除文档不删除业务代码/实例数据。生产切换必须另行核实停写、真实SQLite/WAL/SHM清单与授权；媒体及sidecar保留。

## 分区与独立剩余合同

| 入口 | 用途/边界 |
|---|---|
| [database](database/README.md) | app/library/subscribe/cache/jobs字段、键、索引及当前接线缺口 |
| [DB8](superpowers/plans/2026-10-12-db8-maintenance-retention.md) | 当前维护合同、历史结果、CAS/硬预算与VACUUM限制；不是授权启用 |
| [10-13 follow-up](superpowers/plans/2026-10-13-database-api-followup.md) | 原T1–T9提交与剩余设备/性能门，T10历史逐ID对账 |
| [10-15整改映射](superpowers/plans/2026-10-15-performance-resource-remediation.md) | 17个DB/API/MEM发现与D/A/R映射、容量/安全/测量门；不恢复旧projection |
| [Subscribe需求映射](subscription-fix-plan.md) | 保留原全部ID/追加要求与回归线索，不重跑旧建表/兼容步骤 |
| [搜索精度](superpowers/plans/2026-10-16-subscribe-search-precision.md) | 搜索规则、历史样本及未关闭的一词别名/逐Site分页gate矛盾 |
| [CDP合同](superpowers/plans/2026-10-16-cdp-obscura-managed-browser.md) | Browser能力/安装/隔离及T0历史实测边界，不属于存储任务必跑前置 |

独立合同不被新存储计划自动关闭，也不自动扩大当前实现范围。需要实现时先核对当前行为和用户授权，不机械重演历史预计RED步骤。

## ADR

| 决策 | 用途 |
|---|---|
| [0001 Media identity](adr/0001-media-identity.md) | 内部身份、kind隔离和多来源别名 |
| [0002 Hooks](adr/0002-hooks-not-plugin-runtime.md) | 编译期第一方扩展，不引入第二运行时 |
| [0003 Site credentials](adr/0003-plaintext-site-credentials.md) | 明文持久凭证风险，不等于可日志输出/进Git |
| [0005 Jellyfin](adr/0005-jellyfin-compatible-playback.md) | 协议表面与真实客户端验收边界 |
| [0006 Indexer](adr/0006-indexer-in-first-slice.md) | 架构取舍，不是外网验收结果 |
| [0007 Browser](adr/0007-cdp-browser-opt-in.md) | CDP开关/隔离/受管Obscura/回退与信任边界 |
| [0008 SQLite](adr/0008-sqlite-system-of-record.md) | 历史拆库与现行五文件，未完成生产切换 |
| [0009 Job](adr/0009-job-queue.md) | Interval/queued-running约束、状态事实与恢复 |
| [0010 Password](adr/0010-password-hashing.md) | Argon2id；旧明文fallback已移除，历史兼容描述不作当前合同 |
| [0011 Probe retry](adr/0011-probe-stage-retry.md) | 持久阶段/退避，不以NFO代替Probe成功 |
| [0012 Library removal](adr/0012-library-removal.md) | freeze/drain、明确归属、保留Media/User事实及字节 |

## 历史审计与测量

历史finding仍保留逐ID对账入口：[09-29](audits/2026-09-29-full-codebase-audit.md)、[10-05](audits/2026-10-05-full-project-bug-audit.md)、[10-10](audits/2026-10-10-full-project-review.md)、[10-10 follow-up](audits/2026-10-10-review-followup-remediation.md)、[10-13](audits/2026-10-13-database-api-review.md)、[10-15三方向](audits/2026-10-15-database-api-memory-review.md)。它们不构成 `bd6c3c8` 全部仍坏或全部已修的结论。

测量分别见 [API release对照](benchmarks/2026-10-12-api-endpoints.md)、[Jobs query plans](benchmarks/2026-10-12-jobs-query-plans.md)、[Media indexes](benchmarks/2026-10-12-media-identity-indexes.md)、[指纹采样协议](benchmarks/2026-10-08-fingerprint-sampling-protocol.md)、[10-15 acceptance](benchmarks/2026-10-15-performance-resource-acceptance.md)、[默认库候选](benchmarks/2026-10-15-default-library-candidates.md)、[三批acceptance](audits/2026-10-13-batched-performance/acceptance.md)。性能数字、失败、Not run和版本一起引用。

三个短复现入口 [endpoint](audits/2026-10-12-remediation-review-repro/endpoint-perf.md)、[Jobs](audits/2026-10-12-remediation-review-repro/jobs-query-plans.md)、[Media lookup](audits/2026-10-12-remediation-review-repro/media-lookup-plans.md)仍被源码注释引用，保留为历史导航，不复制第二份结果。

Agent导航：[domain](agents/domain.md)；[issue tracker](agents/issue-tracker.md)只在用户明确要求时用；[roadmap](agents/roadmap.md)是阶段档案，不是当前API规范。

## 2026-10-20全目录清理记录

本轮对原 `docs/` 全部文件分区阅读全文：父任务负责数据库/存储Spec与合同/旧SQL/ADR0008/存储证据，三个并行切片负责运维API/其余ADR与agents、全部历史审计/测量、旧执行计划。保留的历史证据不重判，当前错误状态按源码与本轮review纠正。仅文档及CONTEXT相关一句修改，没有执行新方案或操作实例。

| 删除文件（相对docs/） | 删除原因/保留去向 |
|---|---|
| `superpowers/plans/2026-10-18-library-storage-redesign.md` | 原T1–T12步骤被10-20计划全部重新映射，目标留Spec/合同；历史文档验证留Git |
| `superpowers/plans/2026-10-19-storage-cutover-remediation.md` | R1–R9完成状态被复审反例否定，剩余正确性/O1–O3移到新计划，不继续维护冲突状态 |
| `superpowers/plans/2026-10-18-library-storage-schema.sql` | 生产library.sql已成为唯一DDL，草案term枚举等已漂移 |
| `superpowers/plans/2026-10-18-storage-partitions.sql` | app/subscribe生产DDL及共享cache常量取代，旧索引名已漂移 |
| `superpowers/plans/2026-10-15-database-remediation.md` | D1–D5要求合并到10-15整改映射，不保留旧模型实施步骤 |
| `superpowers/plans/2026-10-15-api-performance-remediation.md` | A1–A6要求合并到10-15整改映射 |
| `superpowers/plans/2026-10-15-memory-resource-remediation.md` | R1–R5要求合并到10-15整改映射 |
| `superpowers/specs/2026-10-15-performance-resource-remediation.md` | 安全/容量/测量要求合并到10-15整改映射 |

删除前正文可按表内路径用 `git show bd6c3c8:docs/<路径>` 取回。另压缩Subscribe、CDP、搜索、DB8、10-13/10-15旧计划，保留所有独有要求与未闭环门；修复已删源码/文档链接及错误当前时态。更早10-15删除的51份历史产物原清单亦可从 `git show bd6c3c8:docs/README.md` 查询，不在本页重复维护。

## 2026-10-22修复复审与旧计划撤下

- 删除`superpowers/plans/2026-10-20-storage-clean-cutover.md`：已执行步骤、新建文件建议、历史“已完成”表与后续更正混在同一执行入口，已被当时13任务技术方案取代；现行剩余任务见本页当前入口。
- 原完整文件可用`git show 1845dab:docs/superpowers/plans/2026-10-20-storage-clean-cutover.md`读取；独有修复记录及R15/R19决策原样迁入[历史归档](audits/2026-10-22-storage-cutover-execution-history.md)，benchmark测量未删除/重写。
- 当时的问题报告保留为[1845dab历史审查](audits/2026-10-22-storage-repair-review.md)；当时以[5bd696e修复复核](audits/2026-10-24-storage-fix-review.md)和[剩余技术方案](superpowers/plans/2026-10-23-storage-review-remediation.md)为准。其他历史文档有独有要求/证据，不按年龄删除。
- 本轮仅docs变更；隔离Store/Router/实际Web adapter烟测，不修改Rust/TS，不操作真实实例。

## 2026-10-23 再次复审与方案合并

- 删除已被替代的四份步骤文档：`2026-10-22-storage-repair-remediation.md`、`2026-10-23-storage-runtime-correctness.md`、`2026-10-23-storage-api-cutover.md`、`2026-10-23-storage-query-efficiency.md`（均原在`docs/superpowers/plans/`）。它们的已完成步骤不再作为待办；未闭环A6/A7/A8/A10/A11/A12、B3和性能/现场验收门已迁入唯一当前方案。
- 原版本可用`git show 8fbf81b:docs/superpowers/plans/<文件名>`取回。两份旧复审与核验保留固定版本反例、撤回理由、历史测量，不再维护互相冲突的当前状态表。
- 更新数据库字典遗漏的rename/root队列、三个字段、索引和实际统计；纠正Spec/ADR/API及测试文档把旧缺陷当现状的表述。历史benchmark数值保留，仅注明阶段与当前剩余限制。
- 当轮发现保留于[8fbf81b历史复审](audits/2026-10-23-storage-followup-review.md)，不作为新版本仍坏的依据。

## 2026-10-24 首轮修复复核（5bd696e 历史）

- 新增[5bd696e复核](audits/2026-10-24-storage-fix-review.md)，逐项关闭已修反例，记录当时9项问题和真实生产seam探针。
- 唯一方案已删除完成步骤并收敛为7项剩余任务；上一轮全文留Git，不新增互相覆盖的执行计划。
- 同步API约定、数据库生命周期、ADR/Spec状态及测试入口；旧审计与benchmark独有数字保留固定版本。该版本workspace/store失败，Jobs和Web通过；结果固定在5bd696e。

## 2026-10-24 再次复核与补修

- 新增[cf7fb70 补修报告](audits/2026-10-24-storage-fix-followup.md)，将5bd696e审查标为历史，保留逐R编号证据。
- 唯一计划剔除已完成的实施步骤，剩余为根操作调度、Unidentified候选工作量、查询/连接策略测量及现场验收；旧步骤从Git读取。
- 同步数据库字典、scan_seen主键、NFO身份校验、API marks合同与回归入口。历史性能数字不重写，最终门禁以本次报告为准。

## 2026-10-24 cf7fb70 只读复审与文档收敛

- 当前发现只维护在[本轮复审](audits/2026-10-24-storage-rereview.md)，技术任务更新原唯一方案，不创建第二套执行步骤。
- 删除数据库字典的来源合成/override 冲突/played 时钟等过时缺陷描述，修正“已全部无兼容/仅剩性能”的结论；ADR-0008 去除旧四文件、导入器及重复旧 SQL 正文，保留历史 Git 定位和决策理由。
- 上一轮补修、历史审计与 benchmark 保留独有反例/数字并固定版本；当前测量见[查询与写入复测](benchmarks/2026-10-24-storage-query-rereview.md)。不因绿色测试保留旧协议，不以旧失败覆盖当前证据。

## 2026-10-28 再次复审与过时执行稿清理

本次删除 10-26 修复执行稿、旧 Codex 审查 brief，以及两份 10-27 修复计划。已实现项、残留问题和历史测量集中在[当前复审的对账与清理记录](audits/2026-10-28-storage-main-rereview.md#文档收敛记录)；原文可用 `git show 5730b032:docs/superpowers/plans/<文件名>` 读取。独有 schema/benchmark/审计证据保留，旧独立报告标为历史；不再维护多份互相冲突的“唯一当前方案”。

[10-23 执行记录](superpowers/plans/2026-10-23-storage-review-remediation.md) 和 10-25 A/B 计划只记录各自阶段。同步修正数据库总览 v5/旧统计、ADR 的 cache 耐久设置、主 Spec/API 的当前指针，以及数据库字典里已修复入口仍被描述为未接线的段落。本轮没有执行新的修复步骤。

## 2026-10-28 文档核对

对照当前 DDL、API、备份脚本及历史证据，修正 Library v7、索引清单、Library DTO、操作错误字段、文件续页和 Jobs 初始化说明；备份回滚要求保留实际存在的数据库、WAL 和 SHM 文件。更新过时的当前复审导航及回归场景，并将失效的历史源码链接改为固定版本路径线索。历史审计和 benchmark 保留其反例、数据及版本边界；未删除新的文档，也未修改代码或重跑代码测试。当前仍按[复审](audits/2026-10-28-storage-main-rereview.md)处理 R1–R5，不能因文档整理将问题标为关闭。
