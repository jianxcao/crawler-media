# main 基线数据库重构 Review

日期：2026-10-23。结论：**Request changes；保留架构方向，暂不接受“零兼容、并发安全、生产闭环已完成”的结论。** 本轮只 review 和技术方案，没有修改业务代码、测试或真实实例。

## 范围与方法

- 比较：`git diff main...HEAD`；main=`86122d73a4a23439e1d83234590fdd98b388fc50`，HEAD=`263114d854bca9515642053f024f3816efa2a9d6`；1,010 文件，+106,286/-45,897。初始工作树干净。
- 规范：[存储重设计](../superpowers/specs/2026-10-18-library-storage-redesign.md)、CONTEXT、ADR-0008、ADR-0012；已有 10-22 报告只用作线索，已修项目不重新报告。
- 四个独立只读切片：锁/测试并发、数据模型及写入不变量、前后端切换、SQL 成本；父审查重新读关键源码并运行以下隔离反例。
- 运行：系统 temp 下临时 Rust crate，path 依赖当前源码，`cargo run --offline --quiet --manifest-path <temp>/Cargo.toml --target-dir <repo>/target --bin <runner>`。真实 Store、真实 Router/registry/ProbeManager；只在 HTTP 场景复用仓库 Catalog/Downloader 外部 fixture。不访问用户数据，不启动 Chromium/FFmpeg/真实 Downloader。
- 两个死锁 runner 使用独立进程、完成 channel 截止时间和外部命令 deadline；观察成立后 `process::exit`，不让被锁住的 lease 析构/线程 join 再次挂住测试进程。ABBA 用持久 removal 标记确定交错，不靠 sleep 猜测锁已获取。
- rust-analyzer references 请求异常退出（code 0），已报工具问题；本轮使用源码/路由/SQL核对，未声称完成 LSP 全引用证明。
- 本轮**未运行** crate/workspace 测试、Web typecheck/test/build、浏览器交互、NAS/真实客户端压力或完整规模验收。编译 runner 有既有 unused/dead-code warnings，不称 lint 全绿。
- 技术方案：[实施计划](../superpowers/plans/2026-10-23-storage-review-remediation.md)。本文件的反例输出代表缺陷被观察到，不代表产品验收通过。

## 设计判断

| 部分 | 判断 | 决策 |
|---|---|---|
| 五文件生命周期 | 合理 | app 凭证/设置；library 业务原子事实；subscribe 政策；jobs 在途事实；cache 外部响应。不可把非缓存库当可随意清空的缓存。 |
| Media/Library/Playback 同库 | 合理 | 恢复包、会话、历史、公共及用户摘要需要局部原子发布；不为降低表数重新拆库。 |
| 显式 ownership、复合 FK、file/unit 分离 | 合理 | 文件多集、单集多版本成立；但规范路径必须从写入到重归属一致。 |
| items/units/稀疏 user_items + 冷详情 | 合理但有维护成本 | 保留；避免逐 episode 元数据更新重建全部覆盖/用户摘要；checker 不能把现有错误当期望。 |
| SQLite WAL + 全局 Store mutex | 有条件可用 | WAL 不消除进程 mutex 死锁，也不能令一个 Store mutex 下的读取并发；先修锁序/短事务并测争用，不先引入连接池或新服务。 |
| 零兼容切换 | 未完成 | 旧 browse 持久链和 LedgerRow 已删除，但自有 API aliases、旧响应投影及无效前端 API 仍在；分清合法 Jellyfin 协议与旧自有合同。 |

## Standards findings

### S01 [P1，实测] 删除文件后的 Probe 重试自死锁

位置：`crates/api/src/probe_manager/retry.rs:49,69-73`。

`match self.store.lock().file_coverage_by_id(file_id)` 的临时 guard 存活到 match arm 结束；`Ok(None)` 分支调用 retire，又获取相同非重入 mutex。真实竞态是 claim 返回 owned stage 后文件被删除；无需数据库永久保留孤儿 stage。job loop 同步调用该派发器，随后全部 Store 使用者可能阻塞。

公开 seam 反例：ProbeManager 创建后，公开 claim/finish 写入缺失 UUID 的失败阶段，到 deadline 调 dispatch_due。输出：`RETRY_SELF_DEADLOCK dispatch_blocked=true store_mutex_unavailable=true`（artifact://59）。

修复：先在独立语句/词法 scope 得到 owned lookup result，释放 guard 后 match。保留 `dispatch_due(&self, i64, usize)->Result<usize,StoreError>`；回归要求返回0、阶段退役、Store可重新获取；再补 claim→删除真实文件竞态。不能增加 timeout 掩盖自锁。

**状态：已修复（Task1）。** lookup 在独立块内取得 owned 结果后再 match，退役在 guard 外。回归 `api/tests/probe_retry_deleted_file.rs`（子进程+60s 外部 watchdog）：缺失 UUID、文件删除后残留重试两例；修复前两例均被 watchdog 判定自锁，修复后 0.06s 通过、dispatch 返回0、Store 1s 内可再锁、阶段已退役。claim 与删除的精确交错用“删除后残留阶段”表示，未加入生产检查点。

### S02 [P1，实测] 跨 Library organize 与 freeze 构成 ABBA

位置：`api/src/http/library_organize.rs:175-178,203-211`；`api/src/library_lifecycle.rs:260-265,315-325,372-377`；`marker/src/work_control.rs:14-29`。

organize 持源 publication gate 再 acquire 目标 registry；freeze/block 持 registry 再 cancel 源 gate。两者互等；lease Drop 也获取 registry，故只把 acquire 提前、Drop 仍放 gate 内并不够。

真实 public registry/Store 交错输出：`LOCK_CYCLE freeze_committed=true organize_acquire_blocked=true freeze_cancel_blocked=true`（artifact://56:181）。这证明底层锁循环；未将其冒充实际 HTTP 并发回放。

修复：外层持有全部源/目标 lease，gate 内不得 acquire/drop lease；按统一 registry→publication→Store 顺序做短发布，磁盘 IO 不包在整批 publication gate 内。回归使用独立子进程 watchdog，要求 organize 与 freeze 均结束或明确拒绝。

**测试代码审查边界：** 未证明额外独立 fixture 死锁。BlockingBudget permit 随实际 blocking work 持有、wait_idle 先注册通知再检查且 await 前释放 guard、Job 执行在队列锁外；这些未发现本轮可证明的循环。测试绿不证明没有竞态；昂贵规模用例和时序抖动不应直接归类死锁。`media_reassign.rs:375` 另有固定错误产品行为的断言，见 R03。

## Spec findings

### R01 [P1，实测] 旧 rename 意图可移动已经替换的源文件

位置：`store/src/library_write/rename_durable.rs:74,100-118,500-528,187-192,253-260`。

begin 记录 size/inode，但 `move_rename_bytes` 不校验；源 A 被原子替换成无关 B 后，旧意图仍移动 B；complete 把内容变化当正常新源推进，不能撤销已移动的错误字节。

输出：`RENAME_REPLACED_SOURCE disk_ok=true complete_ok=true moved_replacement=true old_path_exists=false`（artifact://32/42）。

修复：持久 intent 捕获完整源 identity/content token、目标和 config；锁外磁盘执行必须重验源，拒绝旧意图，目标已存在的恢复也必须验证它属于原意图，不能仅 `!from && to_exists` 成功。使用受控 no-clobber 发布，保持恢复幂等。跨文件系统 copy 如无字节同一性证据保守新源，不能将任意替换冒充合法 copy。

### R02 [P1，实测] canonical path_key 与 display 根混比，嵌套库重归属漏文件

位置：`store/src/roots.rs:155-180`；`store/src/libraries.rs:448-450`。

files 读 canonical path_key，root_path/候选根却取 display_path。A 的根经 symlink alias 注册后，新增 alias/nested 的 B，字面前缀不匹配 canonical file key，已有文件不迁移；严格 resolver 已选 B。

输出：`ROOT_REASSIGN persisted_in_A=true resolved_in_B=true persisted_matches_resolver=false`（artifact://32/42）。文件查询/权限范围与当前归属解析不一致。

修复：全部归属比较使用同一 canonical key + 目录组件边界；display 仅展示/实际定位。新增更深根也走可恢复、限定受影响范围的归属操作，更新 files/coverage/NFO/摘要，避免同步读取同 kind 全部文件。

### R03 [P1，实测] 文件重新识别清除了原 Media 进度

位置：`store/src/library_write/reassign.rs:186-211`；`store/tests/media_reassign.rs:371-375`。

关闭会话之外还 UPDATE playback_units，清空 position/duration/原源绑定/轨道。违反规范4.6“文件改挂不迁移 A 的原始进度”，以及4.4“文件移除保留最后恢复包”。现有测试反而固定 position=0。

输出：`REASSIGN_PROGRESS before_position=5000 after_position=0 after_duration=None favorite=true`（artifact://66）。

修复：关闭相关活动会话，保留 A 原始恢复包/收藏/历史；B 不继承。选源依照新覆盖决定是否可复用，不能破坏事实防止不合法选源。删除错误归零断言，改为完整包保留及旧会话拒绝。

### R04 [P1，实测及源码] 元数据写者绕过 merging/retired 屏障

位置：`store/src/metadata/adopt.rs:28-35,170-188`；`metadata/overrides.rs:18-60`；`api/src/http/media.rs:771-778`。

两公开事务未校验 identity_state；retired 墓碑仍满足 FK。adopt 还不消费 `MetadataAdoption.expected_revision`，不事务内校验当前 refresh target，旧网络响应可覆盖新决议。

输出：真实 merge 完成后 `RETIRED_METADATA override_accepted=true`（artifact://66）。expected_revision/目标竞态是源码证明，未运行。

修复：事务开头统一校验可写身份；adopt 同事务校验捕获的 metadata revision、source/kind/provider_work/current target。捕获→网络→CAS，旧响应拒绝且零写。retired 仅报明确错误/survivor，不重定向或继续写入。

### R05 [P1，实测] merge 选中了快照，但未重合成有效元数据

位置：`store/src/media_operations/references.rs:108-120,164-167,183-209,228-230`。

来源快照按目标选定，但 adopted work 仍优先保留 survivor，season/episode 碰撞也直接保 survivor。真实 A采用p2/A、B采用p3/B，合并选p3：`MERGE_EFFECTIVE composed_title=Some("Adopted B") persisted_title=Adopted A`（artifact://66）。同一事实的实时合成与落盘/卡片不一致。

修复：merge 事务固定 entity/locale 决议，归并来源及 overrides 后，对受影响键调用同一 write_effective，再更新卡片/terms。目标无快照时仍遵守规范 pending fallback，不把普通 survivor 优先伪装成目标快照。

### R06 [P2，实测] locale operation 成功但卡片仍旧语言；重扫会重新标记 NFO 语言

位置：`store/src/library_operations/locale.rs:24-41`；`metadata/nfo_input/reconcile.rs:54-55`；`metadata/nfo_input/record.rs:126-140`。

locale 步骤只 UPDATE 配置并 count 输入，不重算 locale-dependent items/terms。NFO reconcile 重新取当前 display locale，再删除旧locale输入，违背导入时持久绑定规则。

实测：`LOCALE_OPERATION state=succeeded`；`LOCALE_CARD card_title=中文 detail_title=Some("English")`（artifact://66）。NFO 重扫是源码证明：扩展 runner 在读取预期初始 NFO 输入时得到 QueryReturnedNoRows（artifact://69，exit101），尚未执行重扫，不将其算作运行复现；不能仅用 locale operation 的输入保留测试关闭后续扫描路径。

修复：持久 locale operation 分批按 Media 重算 display 摘要/terms，完成复核前保持 reindexing；无语言 NFO 使用已持久导入 locale，普通重扫不重新解释。实体变化/显式语言变化按单独事实处理。

### R07 [P2，实测] checker/repair 对反向孤儿用户摘要假 clean

位置：`store/src/library_write/reduce/card.rs:61-69`；`library_operations/integrity.rs:308-320`。

期望探针复用 reducer，但 reducer 只枚举 raw playback_units 用户，不处理仅有 user_library_items 的用户。孤儿因此同时留在 actual/expected，修复也不删。

公开 favorite 建摘要后外部删除其 raw facts，check→repair→check 均：`has_issues=false orphan_count=0 stale_favorite_survives=true`（artifact://42）。这是规范要求 checker 可修外部破坏的反例，不声称正常 API 一定生成该破坏。

修复：候选用户取当前 raw 用户 UNION 当前 library/media 的 derived 用户；期望为空则删孤儿，其他用户/库不受影响；保留合法稀疏行缺省及报告cap。

### R08 [P2，实测] 显式无效 file_id 被降级为自动选其他版本

位置：`api/src/http/playback/selection.rs:34-37,59-65,78-82`。

解析失败或 visible_file=None 都转换为 None，后续自动选源。输出：malformed/unknown/wrong_type 均 `status=200` 并选择有效另一文件（artifact://63:175-177）。

修复：区分省略与无效/不可用的显式选择；坏类型/UUID返回400，缺失/无权/不在位/不覆盖目标拒绝且不签发session。只有省略或明确允许的 null 自动选源，不泄漏隐藏库信息。

### R09 [P1，源码+Router请求实测] 下一集保留上一集物理 file_id

位置：`web/components/player/player-page.tsx:117-125,166-179`。

fileId 从 URL 持续进入每个 unit，next/prev 只改坐标。HTTP实测 E01/A=200，E02仍带A=404，E02省略file_id=200正确选B（artifact://63:178-180）。未运行浏览器按钮回放；UI 状态传递为源码证明。

修复：活动目标成套保存逻辑坐标和可选显式源；next/prev/autoplay 用新目标替换，清除初始物理偏好。合并文件E01/E02仍需独立逻辑身份，不推断分段。

### R10 [P2，源码证明] 默认展示文件被误当用户显式选源，破坏自动继续观看

位置：`web/components/library-item-detail-view.tsx:571-574,955-964`；`store/src/playback/mutate.rs:95-109`。

未操作选择器时 selectedTrackFileId=null，但 selectedTrackFile回退第一文件A；播放链接仍强制携A。已有恢复包指B时，自动继续被转换成显式换A，从0/source_changed开始。

修复：默认展示与显式选择分离，只为真实选择传file_id；resume预览与开播共用源感知解析。`web/lib/api/playback.ts:886-894` 的resume查询还丢弃file_id，一并对齐。不用把所有版本进度视作相同时间轴。

### R11 [P1，源码证明] rename 提前删除 durable intent，跨库引用失败无恢复

位置：`store/src/library_write/rename_durable.rs:284-301`。

library提交删除intent后，`rewrite_fact_paths` 写subscribe.db失败被 `let _` 吞掉，返回成功。重启无intent可继续，路径事实永久旧。不是要求跨文件原子事务，而是要求明确持久阶段。

修复：intent保留 library_published→references_applied→done 阶段；library CAS提交后继续幂等收敛subscribe引用，失败保留intent并报错/记录恢复状态；仅所有阶段完成后ACK。磁盘/源/目标证据不因重试丢失。

### R12 [P2，当前残留，部分 main 已存在] 零兼容验收仍未满足

- `api/src/http/playback/selection.rs:8-9,26-27`、`session.rs:43-49` 接受 season/episode旧键；实际只传旧键返回200（artifact://63:181）。新自有合同只留 season_number/episode_number，旧键明确400。合法 Jellyfin 查询名不是此项。
- `web/lib/api/libraries.ts:2124-2132` 仍接受 chapters旧数组并制造新对象；生产 `http/library_chapters.rs:405-410` 已返回对象。
- 同文件 `1944-2024` 的 trash/旧duplicate导出仍返回空或假成功；当前duplicates UI已用新API。删除无消费者导出及专属DTO，不实现兼容no-op。
- `web/lib/api/playback.ts:1073-1100` 从URL猜数字file ID及token，而当前UUID/session/api_key合同不匹配；真实 subtitle-layer/video-player仍调用。属于既存全链路缺口，不能宣称本分支新引入或只改正则就完成：需先核实实际端点能力，再显式绑定可用asset URL，未实现能力不能伪造可用。

这些是用户“所有旧逻辑清零”的验收项，单列而不冒充全部为本分支新增回归。历史注释、合法磁盘布局、正常可选字段、retired拒绝墓碑不自动算兼容层。

## 数据库优化

### O01 [P2，当前实测] 深cursor的OR绑定不能使用完整复合范围

位置：`store/src/library_query/items/page.rs:119-131`。相同游标键每个OR分支重新分配不同placeholder，SQLite不知道值相同，当前计划仅按library_id定位。

100k 卡片当前DDL、相同投影/LEFT JOIN/游标/51行、第一项Movie050000（artifact://63）：

| 查询 | VM steps | FullscanStep | Sort | 范围计划 |
|---|---:|---:|---:|---|
| 公开 Store 首屏（50项，读第51项判断续页） | 1,538 | 0 | 未单独采集 | 公开入口 |
| 公开 Store 深cursor（同样50项） | 351,645 | 0 | 未单独采集 | 公开入口 |
| 精确生产OR形态、完整读取51行 | 351,647 | 0 | 0 | items_title(library_id=?) |
| tuple形态、完整读取51行 | 1,761 | 0 | 0 | items_title(library_id=? AND (title_sort,media_id)>(?,?)) |

约200倍是这组SQL的VM工作量差，不是API/NAS耗时承诺。FullscanStep=0本身不能证明深seek有界。修复同向标量排序用tuple，保留混合watch-tier方向的合法实现及NULL/tie语义；复用现有8个排序索引，不先加索引。

### O02 [源码确认额外工作，耗时未测] 元数据 scope 与归约范围不匹配

`metadata/adopt.rs:35`、`overrides.rs:60` 对每个work/season/episode更新均调用全Media归约；`reduce/card.rs:13-34` 重建units并刷新所有用户。`api/src/scrape_metadata/series.rs:200-236` 还按文件版本重复adopt同一episode。

方案：entity/locale采用按键去重；work展示变更只重合成展示列/terms，season/episode只更新对应事实及真实依赖字段。不重建未变化coverage或用户状态。先记录SQL/修改行数/WAL/事务时长，保留冷详情与卡片一致性，不以拆表掩盖多次归约。

### O03 [源码确认成本，优化收益未测] 心跳与协议过滤仍有整作品/冷JSON成本

- `playback/event.rs:107`→`user_summary/mod.rs:253-297,327,479-534` 每心跳读当前用户整Media facts和本库整Media units；多个遍历中Vec.contains使membership可达O(拥有集数×用户facts数)。最小先用HashSet，后按是否完成/断言/领先者变化做精确episode增量；season/whole仍保持正确范围和同事务语义。不能把写集合有界宣传成读集合有界。
- `library_query/protocol/facts.rs:110-163,172-199` 在LIMIT前枚举授权scope的cast/genre冷JSON并在Rust匹配。用已有terms模型在写路径维护**有效locale/NFO决议后的**人物/流派协议键，查询索引筛选；先证明语义与写成本，不缓存整库JSON或建立第二完整投影。

以上两项无本轮延迟/吞吐数字，只列后续有条件优化，不把静态复杂度说成已测性能事故。精确count/facets/包含搜索/深offset本来允许按scope增长，不删能力达标。

## 修复优先级与验证门

1. 先修 S01/S02：消除无限等待，回归进程必须有外部watchdog。
2. 再修 R01/R03/R04/R05/R11：字节与持久事实不可逆损坏、身份屏障、跨文件恢复。
3. R02/R06/R07：统一canonical归属、locale重算/导入绑定、checker正反向闭环。
4. R08/R09/R10/R12：新API严格选源、实际UI状态切换、删兼容而非加fallback。
5. O01立即做等价SQL优化；O02/O03先测再按依赖范围缩减工作。

实施时每个任务公开seam RED→GREEN、实际烟测；每触及crate独立cargo test，稳定树workspace一次；Web变更实际浏览器交互并运行项目门。真实NAS/客户端、100k Media/1M文件/1M Playback与并发扫描心跳另记结果，不引用历史测试数作为当前验收。只review的本轮不执行这些修复、不切换实例。
