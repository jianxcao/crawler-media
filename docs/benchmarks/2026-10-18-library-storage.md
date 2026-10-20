# 存储重设计：检查、烟测与查询工作量证据

更新：2026-10-20，最新源码/烟测基线 `bd6c3c8`。**部分实现，生产干净切换未完成；没有完整性能达标结论。** 历史与当前记录按节分开，不把表/索引存在或测试通过扩大为端到端验收。

## 1. 版本与检查边界

本节至第3节保留 `0a046e0` / `a3dab1b` 的历史记录；“本次/当前”只指当时取证，不是现在源码。后续测试记载见1.2/1.3，`bd6c3c8`复审见第6节。历史失败不能推导新版本仍失败，历史通过也不能关闭未覆盖的生产合同。

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

## 4. `bd6c3c8`布局与生产接线边界

五文件布局及字段/键/索引见[数据库文档](../database/README.md)，历史决策见[ADR-0008](../adr/0008-sqlite-system-of-record.md)。新 `library_files`/coverage/items 与 Store 事务已经存在，但仍有以下生产缺口：

- movie旧whole `(0,0)` 兼容与 `LedgerRow` 有损适配未退出；不是允许保留的兼容合同。
- `adopt_metadata`已有部分生产Scrape调用，但经有损DTO重建来源且字段/读取不完整；`protocol_item_page`与`commit_playback_event`仍未成为统一生产入口。不能由方法或测试存在宣称完整协议SQL分页、字段采用、会话/历史原子提交已完成。
- 旧浏览投影链已删除，不代表上述生产调用方和恢复/删除合同已全部切换。

后续按[当前干净切换计划](../superpowers/plans/2026-10-20-storage-clean-cutover.md)补齐；有效需求保留于Spec/合同，10-18/10-19旧步骤由Git保留，不因旧步骤状态“完成”追认验收。

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

当时记录的新增/改写回归与修复意图如下（不表示在所有生产入口成立，后续复审已发现反例）：

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

### 1.3 `bd6c3c8`提交所记编译与回归修复（历史）

`bd6c3c8`包含 `5ef1957` 之后当时工作树的修复；以下是原执行记录，不是本轮重跑结果。该工作树一度无法编译（api库19个错误），修复后记录：

| 检查 | 结果 |
|---|---|
| `cargo test -p store` | 通过（全部套件） |
| `cargo test -p api` | 通过（全部套件；`management` 573 passed / 0 failed） |
| `cargo test --workspace` | 通过（单次约 398s） |
| `web: npm run typecheck` | 通过 |
| `web: npm test` | 795 passed / 0 failed |
| `web: npm run build` | 通过 |

修复的确定性缺陷（均先由现有回归或新行为失败暴露）：

- `create_library` 丢失 `replace_roots`：新建 Library 不落根，路径归属全部失败。
- 扫描发布丢失封面发现；Movie 扫描未写 whole 覆盖，导致播放会话 404。
- 扫描/`insert_ledger` 不再读取 NFO sidecar：详情 overview/评分/语言为空。现由调用方在发布前构造 `NfoObservation`。
- TV 刷新重构删掉了每集 `write_nfo`：rename/refresh 后无自产 NFO。已恢复为“采用元数据 + 写盘”同时进行。
- `library_metadata_inputs.source_version` 写入文件版本而非 `nfo_v1`，Library 删除时 `library_roots` 外键失败。
- `reduce` 未拆 `details_json` 的 `{version, fields}` 包装；`MetadataFields` 缺 `serde(default)`；`library_item_facts` 从未初始化结果 map。
- movie `(0,0)` 续播读取未规范化到 `(-1,-1)`；`upsert_unit` 无物理源时逻辑标记被拒绝、进度声明未 clamp。
- 缓存 DDL 两份并存（`store/schema/v1/cache.sql` 已删、`media/response_cache/schema.sql` 残留且索引命名不同）；统一为 `media::response_cache::CACHE_SCHEMA_SQL` 后删除死文件。

以上来自当时本机debug profile，改动已包含在 `bd6c3c8`，不再称“尚未提交”。仍不覆盖设备/规模/真实客户端验收；下面审查使用独立公开行为烟测发现套件未钉住的缺陷。

## 6. `bd6c3c8` 干净切换复审

2026-10-20，对 `main@86122d7...HEAD@bd6c3c8` 只读review，未改业务代码。临时独立Cargo runner直接依赖仓库Store/API，Vite SSR加载实际前端API函数；外部网络用fixture/禁用Fetcher。以下不是完整workspace或浏览器/设备验证，临时脚手架已删除，没有修改用户实例。

| 用例 | 本轮实际结果 | 边界 |
|---|---|---|
| 多集文件E02 Web start | HTTP200，但ended_by_admin=true、position=0、play_count=0 | get_ledger代表覆盖与逻辑目标混用 |
| 已签发A会话→同file发布B→旧session进度 | HTTP200；恢复包记录revision2/source B/position8000 | 旧事件被写成新源进度，非推测 |
| 新增E03的增量发布 | 公共episode_count3，用户owned仍2/completed仍1 | 漏维护用户摘要 |
| 实际前端详情枚举101文件响应 | 只请求两页，返回100文件 | 续页next_cursor被错读 |
| 实际前端枚举恰好50文件 | 第二页空终页触发“文件分页 cursor 未推进” | 非浏览器截图证明 |
| TMDB runtime100/rating8.5标准化 | 两字段均Missing | normalizer缺映射 |
| adopted details含Tagline/R | Store详情两字段仍Missing | 冷payload未完整水合 |
| 有文件根/review/a改/review | FOREIGN KEY constraint failed | 旧根删除早于从属迁移 |
| Media改标题/多alias回写 | media=After、card=Before；alias数2→1 | 扁平旧shape有损写入 |
| scan清观察版本并登记新stamp | FileVersion前后仍revision2/source B | 只验证公开Store步骤；完整重扫调用链另经源码核对 |
| Jellyfin PlaybackInfo | MediaSources[0].Id为u1逻辑ID | 未输出f1物理版本 |
| integrity-checks/media merges | 真实Router均404 | 必需管理操作未生产接线 |

源码还确认movie(0,0)前端生产者+边界alias、LedgerRow认领丢episode_to、NFO已有路径提前continue、Jellyfin现场NFO/catalog、持久操作无完整恢复链。不能把这些源码发现写成已运行对应全部场景。

本轮未重跑workspace、前端三门、真实浏览器播放、真实Jellyfin客户端、NAS并发或100k/1M规模；历史1.2/1.3通过保留，但不能抵消上述新反例。后续关闭条件/逐项回归见[当前计划](../superpowers/plans/2026-10-20-storage-clean-cutover.md)。
