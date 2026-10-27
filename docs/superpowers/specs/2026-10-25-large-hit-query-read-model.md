# 大命中查询读模型与读取隔离技术方案

状态：**子项目 A/B 已实现并通过最终本地验证；§9.1 本机 release 目标已实测通过，独立容量/现场项未跑**。设计日期：2026-10-25；最终验证：2026-10-26。原设计代码基线：`feat/storage-redesign@1085a9a`。A 的范围与证据见[读模型记录](../../benchmarks/2026-10-25-large-hit-query-read-model.md)，B 的并发/WAL/指标和最终门见[隔离记录](../../benchmarks/2026-10-25-library-read-isolation.md)。未修改现有实例数据。

实施拆为两个可分别验收的子项目，推荐 A → B：

- [A：事务读模型、精确统计与分页](../plans/2026-10-25-large-hit-query-read-model.md)。降低查询本身的工作量，交付后现有 Store 调用者可以直接受益。
- [B：有界只读连接与查询隔离](../plans/2026-10-25-library-read-isolation.md)。复用 A 的查询核心，解决昂贵只读 SQL 占用 writer Store Mutex 的问题。

现行 A 结构见 [ADR-0008](../../adr/0008-sqlite-system-of-record.md) 和[数据库字典](../../database/README.md)；A 的新实测见[10-25 验收记录](../../benchmarks/2026-10-25-large-hit-query-read-model.md)。[10-24 测量](../../benchmarks/2026-10-24-storage-query-rereview.md)保留为历史数据。本文件同时保留已实施 B 的设计约束；验收结果与未跑范围见[隔离记录](../../benchmarks/2026-10-25-library-read-isolation.md)。

## 1. 目标、约束与非目标

### 1.1 必须满足的需求

| ID | 需求与验收含义 |
|---|---|
| R01 | 单库无筛选、单分类精确 total 读取事务维护的数量；首页不再依赖完整命中集的窗口计数。 |
| R02 | genre/studio 精确匹配直接使用当前有效协议成员，不逐命中条目解析 Metadata JSON；人物保留 Scope 内 ID→角色/名字/TMDB 别名展开语义。 |
| R03 | 常用协议排序有匹配完整方向与收敛键的索引；只对本页选源和水合。 |
| R04 | facets 保留候选全集、零桶、排除自身维度、favorite 候选规则；通过精确集合与边际计数计算。 |
| R05 | 同维多值 OR、跨维 AND；任意组合、Folder、用户条件和跨库去重仍返回精确结果。 |
| R06 | 原始事实、卡片、成员和计数同事务；一个发布批次每个 dirty card 只结算一次；删除与合并不漏扣旧成员。 |
| R07 | Library locale 与实例默认语言变更均有明确持久重建边界；中断后重试，不能读到半更新投影。 |
| R08 | Query count、page、facets 内的相关读取使用同一 library.db 读快照；不把两个 SQLite 文件的事务当成全局原子快照。 |
| R09 | 完整 HTTP 路径先选页，再读取页内源、Media、Playback 与元数据；不能先枚举整库台账再 retain 本页。 |
| R10 | B 的读 SQL 不持 writer Store Mutex；并发、等待、SQL 执行、取消和连接回收均有界。 |
| R11 | ACL、用户身份或 Library 配置在查询期间发生变化时重验；拒绝越权和静默返回部分 total。 |
| R12 | library.db 明确升为 user_version=2；旧布局只读拒绝，不导入、不修补、不自动重建事实库。 |
| R13 | 保留 Web/Jellyfin 的现有查询语义与鉴权；新增语言配置接口时 Web/CLI 消费者同批切换，旧字段明确拒绝。 |
| R14 | 可复跑同形状基线、功能 oracle、VM/排序/冷读取、写放大与并发门；目标与测量结果分别记录。 |
| R15 | 可从原始事实修复当前版派生投影；完整性报告能够发现成员、计数、revision 不一致。 |

### 1.2 Global Constraints

以下各行原样应用于两个实施计划：

- 保留 SQLite 五文件边界；查询投影、成员和数量放在 library.db，domain 只放类型，不引入 IO。
- 不兼容旧数据库和旧自有 API：不做导入、迁移阶梯、字段别名、转发 shim 或双读双写。
- 不修改 synchronous/cache_size/mmap/temp_store/WITHOUT ROWID，不引入 Redis、外部搜索服务或位图依赖。
- 精确 total、跨库逻辑去重、ACL、locale、稳定排序、零桶与排除自身维度语义必须保留。
- 静态图片端点保持 public；Jellyfin 播放鉴权和签发绑定保持现有合同。
- Rust 文件硬上限 800 行，生产函数与测试函数硬上限 120 行；按职责拆分。
- 每项行为先 RED→GREEN；测试使用临时数据库与 fake，不依赖 TMDB、Downloader 或 Chromium。
- 每个触及 crate 运行 cargo test -p <crate>，最终运行 cargo test --workspace；Web 改动运行 pnpm test、pnpm typecheck、pnpm build。
- 仅在当前分支提交本任务文件；不推送、不部署，不清空或替换现有实例目录。

### 1.3 非目标

- 不承诺任意条件组合、深 offset、任意子串搜索的常量成本。
- 不改变协议的 offset/limit、必返精确 total，也不通过延迟或近似 total 掩盖成本。
- 不预计算所有 User × Library × 分类组合，不为任意多键排序创建组合爆炸的索引。
- 不把 FTS token 查询替换现有任意子串匹配，不改变 Unicode 规则。
- B 首轮隔离昂贵的选页/统计/批量事实读取；剩余页内冷详情短读单独计时，不宣称整个 HTTP 已完全无 Store 锁。
- NextUp/Resume 的业务排序与选择模型不是本轮重构对象；共享的权限、安全和预算规则继续适用。

## 2. 当前证据与设计缺口

代码位置以基线为准，实施时先复核行号：

| 当前路径 | 已确认问题 | 设计处理 |
|---|---|---|
| `library_query/protocol/sql.rs:195–207` | 单库也做 ROW_NUMBER 去重；COUNT OVER 在 LIMIT 前遍历完整命中集。 | count、page 分离；单库利用主键唯一性；多库过滤后去重。 |
| `protocol/facts.rs`、`predicates.rs` | 全 locale 索引只是预筛，精确匹配仍读选定 NFO/adopted JSON；预筛成员子查询未完整前推 Library 范围。 | 精确协议成员投影；读取倒排时即施加合法 Library 集合。 |
| `protocol/order.rs`、`library_write/reduce.rs` | 协议排序是 casefold(title)，但 title_sort 当前保存原串。 | 新增独立 protocol_title_key，不篡改 Web title_sort。 |
| `facets/matrix.rs` | cards 全量物化，固定桶反复 CASE，term 分支重复连接与聚合。 | 无筛选读边际计数；有筛选只对窄成员集合聚合。 |
| `items/query.rs` | 无用户条件的 count 也 LEFT JOIN user_library_items。 | 计划明确 needs_user；无需要时不连接用户摘要。 |
| API `selection/scope.rs:117–158`、`selection/mod.rs:92–122` | 选页前仍加载范围内 coverage/Media/用户事实，选页后才 retain 本页。 | 父级解析、SQL 选页、页内事实读取三个阶段分开。 |
| `http/library_scan.rs:623–653` | facets 在 blocking worker 内仍长时间持 Store Mutex。 | 子项目 B 独立只读 lease 与同快照执行。 |
| `http/settings.rs:99` | 默认语言直接写 app.settings；投影与配置不能同库原子失效。 | 将语言单一权威归到 library.db，并登记持久重建。 |

10-24 release 合成形状：每 Media 一个文件、无 adopted metadata，7 次调用含首轮。100k facets p50 约 297–342ms；protocol All p50 426ms。它们是 Store 耗时，不能当作 HTTP 或 NAS SLA，也不能外推大 JSON、多语言、百万文件结果。

## 3. 语义冻结表

### 3.1 逻辑成员与去重

- Work 键为 `media_id`；Unit 键为 `(media_id, season, episode)`。Work 的坐标为 `UnitKey::WHOLE=(-1,-1)`，不与 Unit 相交。
- 单 Library 已由主键保证唯一；多 Library 必须**每库先按自己的语言和条件过滤，再按逻辑键去重**，在匹配成员中保持现有选库规则。
- Root 总数不能相加各库 total；同一分类多个值也不能相加各值边际数。
- `limit=0`、无匹配、offset 越界仍返回精确 total。保留全部现有混合类型、Folder、Series/Season 范围行为；本 SQL 不凭空增加 Season 行。

### 3.2 协议成员与 Web 成员不能混用

| 读取项 | 当前合同，实施必须保留 |
|---|---|
| 偏好语言 | 非空 Library display_locale，否则实例默认语言。 |
| work NFO | 独立选偏好语言下 input_key 最小的有效输入；无命中时按 locale,input_key 首项。 |
| adopted | 独立选偏好语言，无命中时按 locale 首项。NFO 和 adopted 不强制同语言。 |
| 协议 genre | 当前 Web genre terms ∪ 选定 adopted genres ∪ 选定 work NFO genres；name 和 UUID5 wire ID 规则原样保留。 |
| 协议 studio | 当前 studio terms ∪ 选定 work NFO studios ∪ adopted 两种现有合法 studios 形状。 |
| 协议 person | 先在当前授权 Scope 解析请求 ID 对应的角色、名字/TMDB 集合，再按角色匹配 name OR TMDB；不能改成单一 wire ID 相等。 |
| Unit genre/studio/person | 继承上述 work 事实；不擅自换成 episode metadata。 |
| Work search | card title、选定 NFO original_title/overview、选定 adopted original_title/overview 的逐字段子串匹配。 |
| Unit search | 本集 NFO/adopted title/overview；不使用 work 搜索字段代替。 |
| Name 排序 | Unit 仍用 work title。casefold 实际为 Rust `str::to_lowercase()`；不引入 NFC/NFKC 或完整 Unicode case folding。 |
| tie-break | 所有显式排序键比较完成后，显式 Name 的原串 title ASC 才参与；然后 Library/Media/season/episode ASC。 |

任意子串搜索保留其专用精确路径，只在 SearchTerm 非空时连接必需冷字段；本轮先消除无搜索的大命中 genre/facets 成本。不能把字段串接后产生的跨字段匹配当成正确结果。

### 3.3 facets

设授权单库卡片全集为 U，共同条件（identity、require_image 等）为 C；第 d 维多选的并集为 S[d]。

```text
total = | U ∩ C ∩ S[0] ∩ ... ∩ S[n] |
facet[d,v] = | U ∩ C ∩ (所有 k≠d 的 S[k]) ∩ Members[d,v] |
```

- 无选择的维度是 U；未知但合法的 genre 值是空集合，不是取消条件。
- 候选全集从合法 U 的数据取值，固定桶始终返回，计数允许 0；不能从已经过滤后的集合缩小候选列表。
- favorite 仅在当前 User 在库内存在收藏时出现，即使其当前过滤计数为 0。
- 同一 Media 多种 resolution/genre 对同一桶只计一次；rating 的 6/7/8/9 是累积阈值桶。
- `WATCH_TIER_SQL` 的现有顺序不变：seen=0 为未观看；否则 played=1 为已看，剩余为在看。缺省用户记录视为未观看、未收藏。
- `min_rating=7.5` 等任意有限阈值保留标量谓词；不能近似成 7 或 8 的桶。

## 4. 数据结构与唯一权威

### 4.1 布局版本

新增 `LIBRARY_USER_VERSION: i64 = 2`，只用于 library.db。`application_id=1129139249` 保持；app/subscribe/Jobs/cache 的现行身份不变。`schema/v1/` 是现有代码组织目录，不代表需要保留旧 library DDL。

更新唯一生产 DDL、objects 清单、Store 预检和初始化调用；新布局原子初始化，重开校验新增对象。旧 library version=1 在创建或修改任何其他库前只读拒绝，错误指明文件、预期版本、保留原目录和新目录启动方式。没有 ALTER/import/自动删除路径。

### 4.2 卡片与 Web 成员

在 `library_items` 增加：

| 列 | 类型与意义 |
|---|---|
| protocol_title_key | TEXT NOT NULL；严格等于现有 casefold(title)，原串 title/title_sort 保留。 |
| query_projection_revision | INTEGER NOT NULL；结算后等于该卡片 item_revision。 |
| query_config_revision | INTEGER NOT NULL；该投影采用的 library_query_config.revision。 |

复用 `library_item_terms`，扩展 term_kind 的 CHECK，增加 `decade/runtime/language/rating/hdr/stock` 的标准桶；保留现有非协议 term 的用途和取值规则。`genre/country/resolution` 不复制第二份。固定桶成员由卡片事实派生，不存在的值不造事实行；固定零桶由响应候选字典补齐。

增加常用协议 work 排序索引：

```text
items_protocol_name_asc(library_id, protocol_title_key ASC, title ASC, media_id ASC)
items_protocol_name_desc(library_id, protocol_title_key DESC, title ASC, media_id ASC)
items_protocol_default(library_id, added_at_sort DESC, protocol_title_key DESC, media_id ASC)
units_protocol_order(library_id, season ASC, episode ASC, media_id ASC)
```

Name 降序的 title tie 仍为 ASC，因此不能简单倒扫升序索引。任意多键/混合方向、Mixed 默认排序保留精确通用计划；只给上述明确常用顺序承诺索引路径。

### 4.3 精确协议成员

新表 `library_protocol_terms`：

```text
library_id TEXT NOT NULL
media_id   TEXT NOT NULL
term_kind  TEXT NOT NULL CHECK IN ('genre','studio')
term_value TEXT NOT NULL
PRIMARY KEY(library_id,term_kind,term_value,media_id)
FOREIGN KEY(library_id,media_id) → library_items ON DELETE CASCADE
INDEX protocol_terms_value(term_kind,term_value,library_id,media_id)
INDEX protocol_terms_media(library_id,media_id,term_kind,term_value)
```

genre 值复用既有 `name:<Unicode lower>` / `id:<UUID simple>` 编码；studio 存规范化名字。输入 ID 的简单、带横线、genre_ 前缀与大小写规则由同一个请求归一化函数处理。字符串 `All` 没有特殊通配语义。

新表 `library_protocol_people`：

```text
library_id, media_id, role, name_key, tmdb_id, wire_id: TEXT NOT NULL
role CHECK IN ('Actor','Director','Writer'); tmdb_id 无值用 ''
PRIMARY KEY(library_id,media_id,role,name_key,tmdb_id)
FOREIGN KEY(library_id,media_id) → library_items ON DELETE CASCADE
INDEX protocol_people_wire(wire_id,library_id,media_id,role,name_key,tmdb_id)
INDEX protocol_people_name(role,name_key,library_id,media_id)
INDEX protocol_people_tmdb(role,tmdb_id,library_id,media_id)
```

保存当前选中输入的关联记录。请求 ID 在授权 Scope 内先命中 wire/TMDB，再展开角色与 name/TMDB 别名集合；最终查询也在相同 Scope 内。该过程只读结构化成员，不重新解析 Metadata。删除只为预筛存在的全语言 protocol term 写入与查询分支；人物/genre wire ID 的生成函数可保留并集中复用。

### 4.4 精确边际计数

新表 `library_query_counts`：

```text
library_id TEXT NOT NULL REFERENCES libraries(library_id) ON DELETE CASCADE
surface    TEXT NOT NULL CHECK IN ('browse','protocol_work')
dimension  TEXT NOT NULL
value      TEXT NOT NULL
member_count INTEGER NOT NULL CHECK(member_count >= 0)
PRIMARY KEY(library_id,surface,dimension,value)
```

- browse 的 `all/*` 是卡片总数；静态十维中的九维（除 Watch）按 distinct card 维护边际数。
- protocol_work 的 `all/*`、`kind/<kind>`、`genre/<normalized-key>`、`studio/<name>` 按单库 distinct Work 维护。
- 值归零时删除边际行，固定零桶由合同字典补齐。没有计数行表示 0；不一致由 Integrity 检出，不能在读时补写。
- 仅当计划证明“单 Library、Work、无残余条件、无用户/Folder/season 限制”时使用对应边际数。
- Unit、跨库、组合条件仍精确求集合 cardinality；本轮不增加所有 genre × unit 数量的额外写入维护。
- User 继续使用 `user_library_items` 稀疏状态，不新建 User × 分类计数表。

### 4.5 默认语言的归属

新表 `library_query_config` 单例：`id INTEGER PRIMARY KEY CHECK(id=1), default_locale TEXT NULL, revision INTEGER NOT NULL DEFAULT 0`。默认 `default_locale=NULL` 保留“未设置”的意义；Catalog 未设置时继续使用当前 `zh-CN` 启动默认值。

新表 `library_query_count_rebuild` 是同版 Integrity 检查/修复的持久暂存：`operation_id TEXT REFERENCES library_operations(operation_id) ON DELETE CASCADE, surface TEXT, dimension TEXT, value TEXT, member_count INTEGER CHECK(member_count>=0)`，各列 NOT NULL，主键 `(operation_id,surface,dimension,value)`。每批最多100个原始 Media 的预期 distinct 成员累加进此表，最终在冻结库内同事务校验该库边际数（仅repair替换）并清理暂存；不把十万条卡片的汇总塞进 cursor_json，也不每修一张卡片就全库 GROUP BY。

这是实例默认语言的**唯一**权威；移除 app.settings 中 `metadata.language` 的读写消费者。Store 的通用 `put_setting/delete_setting` 对此 key 明确拒绝，防止形成第二个配置源；不导入旧值。其他 Scrape 语言优先级和 source credentials 不随此搬迁。

新 admin 接口：

```text
GET   /api/v1/settings/metadata-language
PATCH /api/v1/settings/metadata-language
body: { expected_revision: i64, language: string }
response: { language: string, revision: i64, operations: [{library_id,operation_id,state,error}] }
```

language 必填，trim 后空串代表清空，null/缺失/未知字段 400；CAS 冲突 409。相同值 200 不新增操作；变化无 Library 时 200，有重建时 202。GET 可在重启后查看未完成子操作。source credentials 的 `/settings/metadata` 不再接受或输出 language；旧 PUT 含 language 在写任何 key 前 400，不转发。

Web 新 API 模块和设置行显式显示处理中、失败与重试操作；使用现有 Library operation 查询/重试能力，不在前端伪装立即生效。Catalog 运行时对象的语言依旧按现有启动合同生效，界面区分“查询投影重建”和“Catalog 重启要求”。

## 5. 事务维护、重建与失败恢复

### 5.1 正常写入结算与独立预期投影

在 `crates/store/src/query_projection/` 定义以下 crate-private 接口；投影不会成为第二事实源：

```rust
#[derive(Clone, Copy, Eq, PartialEq, Hash)]
struct ProjectionKey { library_id: LibraryId, media_id: MediaId }
struct ProjectionBefore { /* 存在性与去重后的旧成员集合；不含 IO 句柄 */ }
struct ProjectionDelta { cards: u64, inserted: u64, deleted: u64, counters: u64 }

fn capture(tx: &Connection, key: ProjectionKey) -> Result<ProjectionBefore, StoreError>;
fn settle(tx: &Connection, key: ProjectionKey, before: ProjectionBefore)
    -> Result<ProjectionDelta, StoreError>;
fn remove(tx: &Connection, key: ProjectionKey, before: ProjectionBefore)
    -> Result<ProjectionDelta, StoreError>;
```

`ProjectionBefore` 的字段在 `types.rs` 固定为 `card_exists: bool` 与 `members: BTreeSet<CounterMember>`；`CounterMember { surface: String, dimension: String, value: String }`。旧精确 term/person 行由 settle 在本事务中按新期望集合差量替换，旧计数来源必须在原卡片/terms 被删除或覆盖前 capture。

结算顺序：capture → 修改事实/卡片 → 构建新成员 → 差量成员和数量 → 标记卡片 query revision → COMMIT。任一步失败整个事实事务回滚；不排后台异步“补齐”影响可见性的计数。

Integrity 不通过正常 `settle/remove` 修复损坏计数。增加两个 crate-private 接口：

```rust
fn expected_projection(tx: &Connection, key: ProjectionKey)
    -> Result<ExpectedProjection, StoreError>;
fn repair_projection(tx: &Connection, owner: &IntegrityOwner, key: ProjectionKey,
    expected: &ExpectedProjection) -> Result<(), StoreError>;
```

`ExpectedProjection` 是私有 owned 结果，含从原始事实计算的预期卡片、单元、用户摘要、Web/协议成员、人物、revision 和 distinct `CounterMember` 集合；不读取现存成员/计数作为期望，不写表。正常结算与 Integrity 共用事实派生规则，check 直接比较现存行与该结果，不再通过会触发计数差量的 reducer savepoint 探测。`IntegrityOwner` 仅由持久操作续跑时在当前写事务内校验产生，包含 operation ID、Library 和冻结 revision，不能由调用者构造绕过权限的 bool。

repair 只在该 owner 持有冻结屏障时替换派生行，不扣减或增加损坏的 live counter；该批预期计数累加到暂存表，最后统一替换。普通写入继续严格差量与非负约束，不能忽略错误或将负数钳为 0。即使 counter 缺失/少计且存在多余成员，check 仍能报告问题，repair 仍能完成。

一个最多100文件的发布窗口用 `HashMap<ProjectionKey, ProjectionBefore>` 在首次修改前捕获；批末按LibraryId/MediaId的规范UUID字符串排序后每个key只settle一次。纯源版本更新、分类未变时不 DELETE/INSERT 全套成员，也不写相同计数。

### 5.2 写入覆盖清单

| 入口 | 必须结算的对象 |
|---|---|
| publish / incremental / pending↔published | 每个受影响 Library+Media 一次；包括最后可用文件消失。 |
| delete_files / Library removal | 删除前 capture；最后卡片消失扣减成员与数量。整 Library 行删除由 FK 清理其边际行。 |
| reassign / roots apply_moves / rename complete | 原归属和新归属双方；版本变化不重复增加卡片。 |
| adopt / override / scraped display / NFO reconcile | 受影响 Media 的所有实际拥有库；NFO record 的发布调用者负责最终 settle。 |
| Media merge | loser 在 `media_operations/apply.rs::move_files` 直接 DELETE 前 capture/remove；survivor 再 settle，不能只靠 FK。 |
| Collection 与展示字段变化 | 只要影响卡片或索引，沿同一读模型结算入口；无关字段不写静态成员。 |
| Library/default locale | 冻结后每批最多 100 Media 重建；完成之前读请求不能消费半更新结果。 |
| Playback / favorite / played | 沿现有 user_library_items 维护；不重建静态 genre/国家/边际数。 |
| Integrity repair | 从原事实生成预期投影，修复成员/计数/revision；不能从已经损坏的计数自证正确。 |

### 5.3 语言变化的发布边界

选择“冻结期间显式暂不可读”，不引入两套长期投影或暗中返回部分 Root 总数：

1. Library 显式 locale 继续使用现有 operation/config_revision 冻结屏障，扩大其结算范围。
2. 默认语言修改在一个 library.db IMMEDIATE 事务内 CAS config revision、更新单例，并为**全部现存 Library**登记 `default_locale` 子操作、递增 config_revision、置 reindexing。全部库都纳入，覆盖详情链中实例 fallback 对显式 Library locale 的影响。
3. 任一库已经有未完成重配置/删除/Integrity 操作时，整次默认语言 PATCH 409，零写；避免两个重建互相覆盖。不阻止正常短事实事务按 SQLite 次序先完成。
4. 子操作逐 100 Media keyset 重建，批间释放 Store 并 yield。`default_locale` 不改 Library.display_locale，不能把继承关系改成显式覆盖。
5. 当前版本 Integrity/locale 完成门核对该库全部有效卡片 projection revision；最后一批与 ready 状态同事务发布。失败保留 frozen/reindexing、错误和游标，启动后继续或显式重试。
6. 新 Library 在重建期间可创建：使用当前 config revision，空投影直接一致；其后写入按新配置结算。

读侧先进行 ACL/kind/具体父级范围判定，再检查范围内可见 Library 的 lifecycle。删除仍按现行合同隐藏；重配置中的显式 Library 返回 503 `library.reindexing`。Root 的目标范围若包含重配置库也返回同类 503，不能把这些库静默剔除后声称 exact total。未授权库的状态不通过错误泄露。前端按现有 operation 进度展示与重试。

### 5.4 Integrity 独占屏障与暂存发布

check 和 repair 都在开始时以单个 library.db IMMEDIATE 事务登记持久 owner、递增 config_revision 并置 reindexing；只有 ready 且没有未完成操作的库可以开始。locale/default_locale、roots、删除、其他 Integrity 与涉及该库的 Media merge 双向互斥，失败但仍持有屏障的操作也算未完成。续跑只接受同一 operation ID 与冻结 revision，不能仅凭 lifecycle=reindexing 判断可写。

- 所有会改变被检查事实/派生行的入口都在自己的写事务内检查屏障：文件发布/删除/移动、adopt/override/scraped display/NFO、Media 身份/合并、Collection 展示字段，以及会更新用户摘要的 Playback/favorite。检查全部受影响 Library；共享 Media 只要涉及一个 Integrity 冻结库，整个事务拒绝，不能部分更新其他库。只读 Playback/流服务不因此禁止；写入返回既有 Protected/409 合同。
- 沿同一事务检查后再修改，旧 WriteScope 也因 revision 变化而失效；不能只在 HTTP 层检查。操作自己的 repair 通过已验证的 IntegrityOwner 写派生行，其他调用者没有跳过屏障的入口。
- 每批至多100 Media，从原始事实及派生行反向孤儿的并集按 keyset 枚举；预期成员去重后累计。派生修复、暂存累加与游标推进在一个事务提交，中断重跑不重复累计。check 同样暂存预期数量，但不替换实时计数或派生行。
- 末批在 owner/revision 仍有效时核对预期计数与投影 revision；repair 同事务替换全库计数、清暂存、完成操作并恢复 ready。check 完成后清暂存、记录 integrity_state=clean/failed 并释放屏障；不能把发现不一致记为已修复。执行失败保留屏障/暂存/游标，启动恢复和显式重试继续同一操作，不能提前解冻。
- 冻结期间目标库的浏览/分类/统计返回503，Root包含目标库也不能返回部分 total；该读门在A2随暂存修复引入，A3复用并扩展语言操作。只凭正常写入的差量维护无法保证暂存正确，必须通过“第一批后修改已扫描 Media 被拒绝”的回归证明屏障完整。

## 6. 查询执行设计

### 6.1 请求归一化与计划分类

新增内部 `CountStrategy::{Marginal, ExactMembers}` 和 `PageStrategy::{IndexedSingleLibrary, GeneralExact}`。分类只决定物理执行，不改变请求意义：

- scope 先按权限、kind、父级收窄，索引成员子查询使用同一 Library 集合。
- 多值 OR 先去重；不同条件 AND；选择最小候选成员集进行求交。
- 多库先库内过滤，再按逻辑键去重；不能先全局选定某库再过滤。
- 无用户筛选且排序无需用户状态时，不连接 user_library_items；有用户条件只读该 User 的稀疏成员。
- 无选择/单分类可证明命中边际数时读计数；identity/require_image/任意 rating/Folder/用户/组合等残余条件一律走 exact members。
- 不将每个计数 SQL 单独开事务；count 与 page 在同一个 library 读事务内执行。

### 6.2 协议分页

将 `sql.rs` 职责拆为 membership/count/page/source：

1. 单库直接利用 Work/Unit 主键唯一性，不加 ROW_NUMBER。
2. 多库集合按逻辑键去重并保留匹配库选取规则。
3. count 读边际数或窄成员集合；不读排序键、文件详情或 JSON。
4. page 使用匹配索引；实际 Jellyfin 无 `SortBy` 的 Work 请求显式传入 `[DateCreated DESC, Name DESC]`，其完整顺序还包含原始 title ASC 与固定 ID 收敛项，使用单库完整顺序索引，不能将它错归为单个 Name 或靠空排序的索引改变平局语义。Unit 默认显式 `[SeasonNumber ASC, EpisodeNumber ASC]` 使用 Unit 顺序索引；其他通用排序只携带必要热键，精确排序后取页。
5. 页内保留当前 `source_expr` 的修订优先和文件 ID 平局规则，不用可能落后的 preferred_file_id 取代文件事实验证。
6. 空页直接使用已经算好的 total，不重新执行整条分页链。

首轮保留协议 offset。Web Cursor 按既有完整排序键 seek；深 offset 的额外遍历成本单列报告。

### 6.3 facets

- 仅当无共同条件且全部维度（包括 Watch/favorite）均无筛选时：静态候选/计数直接读 browse 边际行，Watch 使用该 User 的稀疏摘要计算；无 User 直接得到默认 Watch 桶。仅有 Watch 筛选也必须进入集合路径，各静态桶与该 User 命中集合求交；只有 Watch 自身的桶排除 Watch 条件。
- 对单个桶可在同一读事务内使用集合恒等式，而不改变下述排除自身公式：候选桶成员 `B⊆U` 的权威边际数为 0 时，`|E∩B|=0`；边际数等于权威 `|U|` 时，`B=U` 且 `|E∩B|=|E|`；只有证明该桶的完整 eligible 集合 `E=U` 时，`|E∩B|` 才直接读取该桶边际数。`|E|` 必须取该集合的精确计数，不能从不同维度边际相乘或从返回页推算；其余桶仍走精确成员交集。静态单值所选成员的边际数等于 `|U|` 时，也可证明该条件本身为全集并从物理集合计划中省去；多个 OR 值的边际数不可相加作此证明。不得对任意 rating、identity、图片或 Watch 条件套用该静态单值简化。无 User 的 Watch 候选可从其准确的 `E` 给出 unwatched=`|E|`、watching/played=0，favorite 候选仍缺席；Watch 自身排除规则保持不变。
- 有筛选：构建每个已选维的窄 ID 集合，复用前缀/后缀交集求“排除自身”的集合；不是重复扫描完整 cards。
- 相同的排除集合只生成一次；同一候选值 GROUP BY 时按唯一 `(library,media)` 计数。
- 任意 rating、identity、图片等残余条件只在必要热卡片上求值。返回候选仍来自未过滤 U，不能因候选集窄而丢零桶。
- 空共同条件结果 `U ∩ C` 可使全部计数为0，但仍返回来自U的完整候选（含动态候选）。某个已选维命中集合为空只使 total 及保留该条件的其他维桶为0；该维自身排除条件后仍须计算，不能整体提前清零。例如未知 genre 选值下，已有 Drama/Comedy 桶仍可非零。无选择不等于空命中集合，不访问冷 Metadata。
- 跨多个 SQL 使用同一显式读事务即可一致，不把“恰好一条 SQL”当正确性要求；替换现有 `work.statements == 1` 的实现绑定断言。

## 7. 完整 API 与读取隔离

### 7.1 先选页，再读取页内事实（A）

`selection::read_scope` 不能继续作为正常协议页的入口。新增轻量请求范围解析，只读取用户角色、合法 Library/根、显式 Media 身份；不枚举库内 coverage。

选出 `ProtocolItemPage` 后，按引用中的源 File ID 和逻辑键批量读取源、Media、coverage、所需用户状态及卡片汇总。库级季/集计数从已有 summary 聚合读取；不能为了一个 Series 卡片把全部分集事实拉到 Rust。

未能水合选中引用时按事实不一致/源版本变化返回明确错误或一次受预算限制的重试，不能 `continue` 后仍返回原 total 的短页。成功响应必须满足 `items.len() == min(limit, total.saturating_sub(offset))`（转换为同一数值类型比较）；重试后按最终成功快照的 total 计算，错误响应单独断言。只为正常分页替换旧整库候选入口；NextUp/Resume 的专有算法保持其明确用途。

### 7.2 有界只读 lease（B）

- 默认池大小 **2**，不增加第三方 pool 依赖；租用独占 Connection，空池立即 Busy，不建立无界等候队列。
- reader 仅打开已经验证的 library.db，`READ_ONLY`、`query_only=ON`、foreign_keys=ON；不调用 Store::open，不 seed，不设置持久 journal_mode，不触碰 Jobs/cache/subscribe。
- 实际路径从 Store 读取，不能按 cwd 猜测；复用纯 casefold UDF。热查询不调用全局可变 alias 的 path UDF。
- 本轮覆盖的查询路由在认证之前创建一次绝对 deadline（进入请求时刻 + **30 秒**），通过请求 Extension 和协议 principal 传递。认证、设备凭据绑定、准备、准入、busy、执行、页内冷详情、结果重验及一次重试共享剩余预算，不能在 handler/重试重新获得30秒；不改变静态图片和视频流的路由合同。
- 认证、prepare/validate及剩余页内 Store 读取全部在有界 blocking worker 中执行；在派生 worker 前立即申请 permit，拿不到返回503。获取 Store 使用 `try_lock_until(deadline)`，锁前/锁后及SQL前/结果发布前检查剩余预算；async线程禁止直接调用阻塞 `lock()`，仅包 Tokio timeout 不构成锁等待有界。
- reader progress handler 每 **1000 VM** 检查截止/取消，busy 等待不超过剩余预算与现行 5000ms 的较小值。短 Store 阶段的 app/library SQL 同样临时应用剩余预算的 busy/progress 控制，并由RAII在成功、SQL错误、取消或panic时恢复连接原策略；不能把迟到取消留给后续writer。凭据绑定的事务按现有幂等规则重试，超时不宣称其必然零写。
- 外层超时不提前释放 Connection 或 permit；worker 真正停止并 rollback/清理回调后才归池。物理 IO 已阻塞时不承诺瞬间中止。
- 关闭或确认数据库文件身份/布局已变化时拒绝新租用、取消在途查询、排空后关连接；不自行删除WAL/SHM。Reader不获取第二个writer租约；现有只读实例仍可提供授权读，写者租约失效继续按现行规则禁止写入，不能因此无故取消只读服务。

### 7.3 跨库授权的一致性

User/role/token 在 app.db，ACL/lifecycle/locale 在 library.db，不能用 ATTACH 宣称跨文件原子读。

采用明确三阶段：

1. 短Store锁产生owned `LibraryReadContext`，包含原请求凭据、角色/启用状态、不可替换的请求副本、授权范围及Library ACL/config/lifecycle指纹；释放锁。凭据与Context不实现Serialize/默认Debug。
2. 获取 reader，在单一 library 读事务内再次检查库级范围和版本，执行查询及页内批量事实读取；关闭事务并归还 lease。
3. 短Store锁重验身份、凭据、角色和库级指纹，并重新解析请求的合法Library roster，检测查询期间新建/删除/变更可见性的库。身份撤销则拒绝；配置/ACL/范围变化丢弃结果，最多重试一次，仍变化则503。不因普通Playback数据变化重试。

**锁顺序：任何时候不同时持 Store Mutex 和 reader lease；writer 路径不等待读池。** 不返回重验前构建的敏感结果。最后一次成功重验是授权线性化点；之后提交的权限变化由后续请求生效，不声称跨文件严格串行化。

## 8. 模块与接口边界

以下是实施目标，现有公开 Store 查询签名保持；完整文件分配和任务签名见两个计划。

| 模块 | 职责 |
|---|---|
| `query_projection/{mod,types,browse,protocol,people,counts,expected,repair}.rs` | 事务内捕获、有效输入、成员差量、边际数、只读预期投影与冻结修复。 |
| `library_query/members/{mod,plan,facets,user}.rs` | 授权窄成员集合与排除自身维度集合；无长期请求缓存。 |
| `library_query/protocol/{request,membership,count,page,source}.rs` | 协议规范化、执行分类、精确去重、分页与页内选源。 |
| `library_operations/{default_locale,integrity_guard,integrity_query}.rs`、`metadata_language.rs` | 单库配置权威、独占操作屏障与持久分批重建。 |
| `library_query/page_inputs.rs`、API `selection/request.rs` | 页内 owned 数据批量读取与无台账枚举的请求解析。 |
| `library_read/{mod,context,pool,lease,query}.rs` | 只读打开、受限租用、快照、取消、清理。 |
| API `library_read_work.rs` | 准备/worker/重验、HTTP 错误映射与完整耗时。 |

## 9. 性能预算与验收方法

### 9.1 设计目标，不是已测结果

以下为本机 release、热缓存、单请求、100k Media/每 Media 一个文件、limit=100 的**工程验收目标**。同一机器同一 SQLite 构建重新测 1085a9a 与实施版，不能用历史 debug 或不同数据形状宣称倍数。

| 场景 | 目标 p50 / p95 | 确定性工作量门 |
|---|---|---|
| 无筛选 facets | ≤20 / ≤50ms | 总量/静态桶走边际行；固定桶数量不变时 VM 不随卡片数线性增长。 |
| Drama 50% / All 100% / 三维 facets | ≤80 / ≤150ms | 不读冷 Metadata，不全量物化完整卡片；精确成员交集/计数。 |
| 单库 Work 默认或 Name 首页，无筛选 | ≤20 / ≤50ms | 无窗口总数、无全命中排序；count 读摘要，选源/水合仅页内。 |
| 单库 Work genre 大命中，精确 total | ≤30 / ≤75ms | 不解析逐 Media JSON；数量读合法边际，分页走热索引。 |
| 跨库/Unit/任意组合/特殊排序 | 报告曲线，首轮不设统一毫秒上限 | 精确结果相同，工作量与合法成员规模相关，不随无关库/类型膨胀。 |

若达不到目标，任务保持未通过，记录瓶颈再决定是否需要分块 bitmap；不能更改语义、减少 total、或改测量口径来过门。bitmap 不是本方案的隐藏必需实现。

### 9.2 固定矩阵

- 5k/20k/50k/100k Media；另外 100k Media/1M 文件/1M Playback 作为独立容量形状，不能混算基线。
- 无 Metadata；每个 Media 8KiB adopted JSON；zh-CN/en-US/ja 三语言 NFO/adopted 冲突；Genre 1%/50%/100%；actor 0/20/100 人。
- 单库与同 Media 跨 2/5 个库重叠；固定 1000 Movie，增加 5k→100k TV；Folder/Series/Season/Mixed。
- User 稀疏 1% 与密集 100% 状态；收藏、seen/played、空用户；无筛选与多维残余条件。
- 首页、limit=0、空页、offset=50000、Web cursor 深页；Name 大小写/Unicode、多排序键、同值平局。
- 每形状 **5 次预热 + 50 次计时**，记录 p50/p95/max；另报告首次调用。功能测试不依赖 wall-clock 阈值。
- 同时报告完整 SQL VM、fullscan、sort、statement、精确逻辑命中数、实际页内水合行数、SQL 时间与 HTTP 端到端耗时。无搜索分类路径另用 SQLite authorizer 拒绝冷表读取的故障回归证明其独立性；不能从少量返回行或0计数推断内部没有读取冷表。
- B 并发形状为 2 个读取者 + 1 个每批 100 文件的写者；报告写入 p50/p95、读池拒绝/超时、Store 准备/重验持锁时间、WAL/checkpoint 与 RSS。

### 9.3 写入和恢复验收

- 100 文件/一个 Media 的窗口只结算一次；100 actor 的未变集合不重复写入，保留上一轮写放大回归。
- 更新纯源版本或 Playback 时，静态分类成员与边际数写入均为 0；分类确实改变时仅写差集。
- 回滚注入后事实、成员、计数、revision 全部恢复；库冻结/重建第 1/100/101/末批中断后重开守恒。
- 原始事实 oracle 对照增量结果；修复同版损坏的 counter/member，重开后结果一致。覆盖 counter=0/缺失/少计配合多余成员，check不修改派生数据，repair不中途触发负计数；末批替换故障回滚后可续跑。
- Integrity第一批100项后分别尝试更新已扫描Media的adopt/override/NFO、Playback及合并/删除：全部零写拒绝，操作重开后计数与原事实一致；解冻后重试成功且精确计数更新。覆盖不同操作开始顺序的互斥和失败owner保留。旧布局始终拒绝，不借 Integrity 做导入。
- 所有操作用临时目录，不操作 `data/live`。现场 NAS/设备、掉电与操作系统冷缓存测量单列 Not run。

## 10. 风险、取舍与发布

新增静态桶、精确 protocol 成员、三个常用排序索引增加磁盘与写入成本，需报告每卡片成员数、DB/WAL 大小和发布耗时；先做差量结算，不再复制冷 JSON。计数正确性比单条 SELECT 的便利更重要，因此发布、删除、合并、重建都进入强制回归矩阵。

默认语言的单库权威调整带来一次明确接口切换，Frontend/CLI/启动 Catalog/详情选择链一起修改；本方案不包含保留旧 key 的“临时”读取。重配置期间显式 503 的产品行为需在 review 时重点确认，它用可观测短暂不可用换取单套投影的一致发布。

实施仅在隔离新目录验证。既有 `data/live` 需保留；将来若要切换现有实例，另行明确数据目录与授权，并提供停写备份/回退原二进制+原目录的操作步骤。禁止在启动失败后建议只 DROP 某一张表或只移动 cache.db。

## 11. 需求覆盖

| 需求 | A 计划任务 | B 计划任务 |
|---|---|---|
| R01/R03/R05 | A5/A6 | B2 |
| R02/R06/R15 | A2/A3/A4/A9 | — |
| R04 | A5/A7 | B2 |
| R07/R12/R13 | A1/A2/A3 | B1/B3 |
| R08/R10/R11 | A5/A6/A7 | B1/B2/B3/B4 |
| R09 | A8 | B4 |
| R14 | A9 | B5 |

此段以下的初稿自审和 review 日志记录当时的设计阶段状态，不代表当前 A/B 验收。A 的实际代码和性能结果见[10-25 验收记录](../../benchmarks/2026-10-25-large-hit-query-read-model.md)；B1–B5 已完成独立审查与最终完整门，请求指标、WAL/取消回归、本机并发测量及 2026-10-26 整体验证记录见[隔离验收记录](../../benchmarks/2026-10-25-library-read-isolation.md)。独立容量和现场项明确 Not run。

### 初稿自审与验证（de1dd81）

已由主 agent 按 writing-plans 的五项自审检查需求覆盖、步骤粒度、类型/签名一致性、Review Focus 测试归属与篇幅。修正了语言权威必须先于协议读切换、domain ID 未实现Ord、凭据不能只剩UserId、Root roster新增库重验、只读实例合同和typed 503传播等问题。

文档链接、任务结构、代码围栏及diff空白检查通过。`cargo test --workspace`退出0：**2749通过、0失败、14忽略，405个结果汇总**，日志 `/tmp/large-hit-query-plan-workspace.log`。这是未改业务代码的现行基线验证，不是本方案新回归或性能目标已经通过；本轮没有运行新容量/现场测量，也没有操作现有实例目录。

### 2026-10-25 review 修订

修订 `de1dd81` 的五项发现：Integrity 全写入口屏障（§5.4/A2）、不依赖损坏计数的修复（§5.1/A2）、Watch与空集合 facets 条件（§6.3/A7）、认证前共享 deadline 与有界锁等待（§7.2/B3–B4）、成功页完整性等式（§7.1/A8）。本段为当时 review 的历史记录；A 项已有实现和新回归，B3–B4 已完成，后续实际进度见[隔离计划](../plans/2026-10-25-library-read-isolation.md)与隔离验收记录；此处保留设计阶段历史。下面的历史基线测试不能代替新增行为或性能实测。

修订验证：三份文档的相对链接、代码围栏、A/B任务及步骤编号、接口归属和diff空白检查通过；独立复核未发现新的阻塞问题。`cargo test --workspace`退出0：**2749通过、0失败、14忽略，405个结果汇总**，日志 `/tmp/storage-plan-revision-tests.6GePwK`。本次仅修改方案文档，未实现或运行新增行为回归。
