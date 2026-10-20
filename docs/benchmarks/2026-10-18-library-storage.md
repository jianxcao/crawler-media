# 存储重设计：检查、烟测与查询工作量证据

更新：2026-10-19。**部分实现，生产干净切换未完成；没有完整性能达标结论。** 本页替代早期无测量支撑的“架构目标达成”及 `<1ms` 声明，不把表/索引存在当作运行验收。

## 1. 版本与检查边界

前次审查/运行记录固定在 `0a046e0`。当前提交为 `a3dab1b`，用户已修复扫描 checkpoint 失效，但相关检查尚未重跑。下列历史失败不是当前已证实失败；该修复也不是 workspace 全绿证据。本次未重新运行Cargo测试、构建或产品行为烟测；仅补充第1.1节的结构初始化烟测。

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

## 4. 当前布局不等于生产切换

五文件布局及字段/键/索引见[数据库文档](../database/README.md)，历史决策见[ADR-0008](../adr/0008-sqlite-system-of-record.md)。新 `library_files`/coverage/items 与 Store 事务已经存在，但仍有以下生产缺口：

- movie旧whole `(0,0)` 兼容与 `LedgerRow` 有损适配未退出；不是允许保留的兼容合同。
- Store的 `protocol_item_page`、`adopt_metadata`、`commit_playback_event` 尚未接入 API 生产消费者。不能由方法或测试存在宣称完整协议SQL分页、字段采用政策、会话/历史原子提交已完成。
- 旧浏览投影链已删除，不代表上述生产调用方和恢复/删除合同已全部切换。

后续按[切换修复计划](../superpowers/plans/2026-10-19-storage-cutover-remediation.md)修复并补验收；[原始实施计划](../superpowers/plans/2026-10-18-library-storage-redesign.md)保留T1–T12完整交付门，不因历史部分成功而追认勾选。

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

本轮新增/改写的回归覆盖：

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
