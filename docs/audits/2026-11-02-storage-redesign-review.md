# feat/storage-redesign 合并审核（固定基线 `a28249a3`）

## 最新状态（2026-11-06）

- 有界COMMIT诊断已结束：2f5cd923的独立副本各推进224,978步，诊断覆盖100,000次Users→Users，无≥100ms的COMMIT PROFILE样本；控制同一步数/状态，两轮退出0，源码绑定、原夹具元数据及副本清理核验通过。只保留16个正常样本的VFS明细，没有复现或归因历史2.735秒尖峰，不改SQLite策略，也不追认为完整容量门通过。详见[专项记录](2026-11-06-commit-tail-observation.md)。下一项为图片504回归与分段观察。

- 收藏分段诊断已完成：`e7a12033` 普通/临时诊断构建各 200 次收藏 HTTP200，p95 为 1.250875/1.179750ms，均未复现原 3.509875ms。临时诊断的两个 3.8ms 尾部请求主要等待 Library writer，自有事务及 COMMIT 较短；未识别竞争 writer 的具体操作，不能追溯归因原失败。两轮保留原166项顺序，但循环边界30秒软限时使部分前序用例样本不足，gate均退出1；只作为诊断，不替代正式性能门。业务、请求ID/扫描窗口及来源/清理核验通过，无产品性能改动。详见[收藏分段诊断](2026-11-06-favorite-writer-contention.md)。下一项为有界COMMIT尾延迟观察。

- 本轮按用户确认的顺序先完成 TMDB 有限重试：最多三个实际尝试（含双栈），上游六秒预算贯穿刷新等待、origin 准入、完整正文和退避，并继承调用方截止/取消；429 遵守 Retry-After，永久错误与重定向停止。共享上游超时与个人取消分开，保留有效 stale、typed 404 和缓存发布错误。TDD、共享取消/限流/断流/真实本机 HTTP 等回归及独立 review 已完成；media 全套94通过，API单元与六组相关集成合计352通过，release all-target Clippy通过。策略、回归位置及同步网络取消限制见[TMDB 请求回归](../testing/tmdb-retry-regressions.md)。本项未重跑正式 perfgate、491/UI、容量或内存，不改变下述历史失败；下一项为收藏 busy 性能定位。

- 用户于2026-11-06明确允许TMDB网络问题先暂缓处理；已确认的外网请求超时不再反复阻塞本轮本地修复。原HTTP失败、无效基线和Not run仍保留，不改原SLO或准入规则；代理根因尚未确认，图片代理504也未证明全部来自外网，仍需排查其内部等待。后续优先定位收藏写入波动和2.735秒COMMIT尖峰。

- 后续按预先限定的停止条件，仅做[一次完整busy对照](/private/tmp/crawler-matched-final-hint100-post-v4/favorite-full-busy-replay-20261106T031247Z/campaign.json)。旧版完整166项、162通过、stage退出1、业务verified，但严格基线准入失败；协调器及quiet wrapper实际退出2，新版**Not run**，未继续重复对照。人物图片和poster的准备请求各504，人物详情准备502；Similar仅记录84个窗口内样本，86次尝试为85次200、1次502，无法作为200样本基线。原失败报告全部保留。
- 本次旧版收藏写入为200次HTTP200、p95 3.436208ms，接近此前新版3.509875ms；只能记录旧版也出现该量级耗时，整轮旧基线无效，不能由此作有效版本比较或确认抖动/扫描阶段等根因。原网络复测的新busy收藏相对Fail不变，后续聚焦结果亦不替代该失败。
- 本次日志将TMDB推荐失败记为`timeout: global`（`/movie/9001506/similar`），人物详情记为`timeout: connect`（`/person/3234`），两者均无有效stale缓存；该客户端固定访问`api.themoviedb.org`。图片日志仅有`image delivery deadline reached`，没有具体连接阶段证据。结束后对fixture记录的`image.tmdb.org`头像地址做3次独立curl诊断，均HTTP200、0.242–0.356秒；保持系统路由、IPv4，无显式代理环境变量，未验证图片正文。curl与产品链路不同，不能据此确认代理根因，或把两项504直接归因图片CDN。
- 此次来源清单匹配，无listener/owner/live残留；23份绑定输入摘要未变，没有产品、门槛、请求重试或fixture变化。预启动工具的公开CLI回归4项通过，独立复核关闭基线附属证据哈希缺口后才运行。原始结果汇总为`/tmp/crawler-favorite-followup-20261106-summary.json`，图片诊断为`/tmp/crawler-tmdb-image-recheck-20261106.json`。以下网络复测与更早结果保留各自时点，不拼成单轮通过。

- 用户怀疑代理不稳定并要求再试后，独立复测[本轮 idle/busy](/private/tmp/crawler-matched-final-hint100-post-v4/campaign-20261106T020030Z/campaign.json)已完成。产品`97cc6b1b`、工具`159934b2`、fixture来源`1c3af023`及原门槛不变；仅衍生driver的模式与独立轮元数据变化，没有新增请求重试、重seed或覆盖原失败。

| 本轮阶段 | 实际终态 | 关键结果 |
|---|---|---|
| 旧idle | 完整166项、164通过，退出1；严格基线准入verified | Similar及page-detail各200次200；初始p95 869.336/803.530ms，绝对失败保留 |
| 新idle | 完整166项、166通过，退出0 | Similar/page-detail初始p95 2.484/21.244ms；完整结果p95 856.394/876.744ms，相对−1.501%/+9.095% |
| 旧busy | 完整166项、165通过，退出1；严格基线准入verified | Similar取得200个窗口内样本，203次尝试全200；page-detail取得200个窗口内样本，202次尝试全200；仅Similar绝对失败 |
| 新busy | 完整166项、165通过，退出1 | 唯一失败为收藏写入：200次200，p95由0.951917增至3.509875ms，相对+268.716%，绝对预算通过、相对门失败；Similar/page-detail完整结果p95 949.996/859.837ms，相对+6.665%/+8.163%，均通过 |

- 本轮四阶段业务均verified、来源清单均匹配且无listener/owner/live残留。两种推荐用例在各阶段均取得200个有效完整结果；busy边界外成功尝试另留，不删样本。所有推荐观察均为空，不能推广为非空推荐性能；也不能由延迟推断每次回源或缓存命中。本轮未复现原EOF/502，但新busy收藏相对门失败使`all_candidate_passed=false`，整体验收仍未通过。汇总见`/tmp/crawler-tmdb-network-repeat-20261106-summary.json`。
- 此前失败的`/tv/9500258/similar`和`/movie/9000417/similar`又各做3次独立curl诊断，6次均完成TLS并返回HTTP404（请求使用合成ID），耗时0.626–0.821秒；未验证正文，不冒称typed NotFound合同测试。诊断保持系统路由，使用IPv4且无显式应用代理；curl并非产品ureq传输，不能据此确定此前EOF连接阶段或代理根因。凭据仅在内存及stdin使用，冻结app hash未变，见`/tmp/crawler-tmdb-failed-path-recheck-20261106.json`。
- 收藏相对失败的只读核对尚不能归因：两版marks handler的相关行为与mutation SQL相同，重复`favorite=true`仍进入事务、upsert及summary刷新；普通POST日志只有整数毫秒响应头耗时，且与played共用路由，不能拆出writer等待、SQL或COMMIT。不以单轮差异断言代码缺陷或抖动，见`/tmp/crawler-favorite-busy-relative-root-cause-review.md`。
- 后续[收藏聚焦诊断](/tmp/crawler-favorite-focused-campaign-20261106T025249Z/campaign.json)已完成旧→新→旧→新四轮busy测量，仅运行原`self-write-favorite`，每轮200次HTTP200；p95依次为1.287375、1.359042、1.204958、1.319708ms。四轮均退出0，来源清单匹配且无listener/owner/live残留；没有复现原完整busy的3.509875ms。该诊断明确`diagnostic_only=true`、`acceptance_eligible=false`；两个新版诊断仍使用原完整旧busy报告作基线，不把相邻聚焦旧轮当完整基线。聚焦轮改变前序请求与扫描时机，不能据此定位根因、追认原165/166为通过，或替代完整166项验收。
- 以下为本轮之前三个独立campaign的原始终态，失败及Not run保持当时语境，不拼称单次六阶段通过。产品、工具、fixture分别绑定，没有改写原baseline、binding及历史报告。

| 轮次 | 终态 | 基线与后续 |
|---|---|---|
| [旧idle](/private/tmp/crawler-matched-final-hint100-post-v4/campaign-20261106T011849Z/campaign.json) | 166项完整，164通过，退出1 | Similar p95 842.140ms绝对失败；page-detail 199次200/1次502、p95 829.924ms；旧基线无效，新idle Not run，该原campaign其余五阶段未执行 |
| [独立旧busy](/private/tmp/crawler-matched-final-hint100-post-v4/campaign-20261106T013035Z/campaign.json) | 166项完整，165通过，退出1 | Similar仅80个窗口内成功样本，全部尝试80次200/1次502；旧基线无效，新busy Not run；page-detail另取得200个有效样本、203次200（含3次窗口外成功） |
| [独立旧/新cold](/private/tmp/crawler-matched-final-hint100-post-v4/campaign-20261106T013903Z/campaign.json) | 旧165/166、退出1；新166/166、退出0 | 旧版只有Similar绝对预算失败（841.261ms），HTTP/业务有效、严格基线准入verified；新版完整通过，但每项仅1个冷样本 |

- 四个实际stage的业务均verified、原source清单均匹配，无listener/owner/live残留；新版报告commit为`97cc6b1bfedd`，正式runner启动身份核验通过。新cold Similar/page-detail的初始响应为1.322/6.480ms，完整结果为775.980/794.663ms，相对完整结果变化为−7.791%/+2.506%，均过原相对门；本轮这些推荐结果均为空，不推广为非空推荐或idle/busy结论。字幕旧idle、busy各200次200，旧/新cold各1次200，POST/source准备实际有效。
- 两次502均有TMDB推荐请求意外EOF、无有效stale缓存的直接日志（TV 9500258、Movie 9000417），更深连接原因未确认。不能把正常NotFound等同于EOF：旧新版源码均会持久缓存typed NotFound并映射为空列表，不能断言每个样本都回源。失败、无效基线和未运行项原样保留，没有添加请求重试或丢弃失败样本。汇总见`/tmp/cm-final-hint100-post-v4-results-summary.json`、`/tmp/cm-final-hint100-post-v4-eof-diagnosis.json`和`/tmp/cm-final-hint100-post-v4-busy-origin-errors.json`。
- 新包准入已实际退出0：30项绑定测试、154 GET/166 cases/10 exemptions、两版各50用户400项业务断言verified，static/business freeze匹配，见`/tmp/cm-final-hint100-post-v4-admission.json`。准入通过不等于实际性能通过，仍须核对各阶段health、HTTP/业务、有效旧基线、来源及清理。
- 字幕工具修复`0f1296f6`和`159934b2`均已独立提交推送：先按PlaybackInfo唯一匹配所需外置字幕的source，再用新旧两版共同支持的POST准备；畸形JSON、缺失或歧义来源使case失败，准备记录`outside_timing`及SID存在布尔。旧工具GET准备的实际busy轮保留405/零字幕样本；page-detail该轮200样本有效，不能把整轮改记通过。POST修复有有效RED、根组合128项GREEN及精确路径33项GREEN，独立复审已关闭发现。
- 产品`97cc6b1b`已独立提交推送：Similar仅将JSON重取提示250ms改为100ms，HTTP 202、`Retry-After: 1`及原资源上限不变。公开Router先RED后9/9 GREEN，相关API all-target Clippy、fmt和Web轮询8/8通过，限定独立复审无发现；二进制已构建冻结。八个Movie的[诊断](/tmp/cm-similar-poll-diagnostic-20261105T182122Z-61020/diagnostic-lifecycle.json)两轮均8/8 ready且结果全为空；250/100ms的完整响应p95为1042.364/876.098ms，p50为785.990/866.141ms，请求35/69次。网络未控制、顺序固定、TMDB来源唯一性未核实；8样本p95即最大值，不能替代正式gate或证明非空推荐性能。
- 历史产品`1c3af023`、工具`433`的[v2 campaign](/private/tmp/crawler-matched-final-contract-aware-v2/campaign-20261105T173705Z/campaign.json)旧idle164/166、新idle163/166、旧busy164/166；旧busy的page-detail仅12次200和1次503、样本不足，基线无效，新busy未执行。[独立cold轮](/private/tmp/crawler-matched-final-contract-aware-v2/campaign-20261105T180633Z/campaign.json)两版均165/166。五阶段退出1、业务verified、source及清理正常；新idle Similar完整响应p95为1046.078ms，Similar/page-detail相对失败及字幕404仍保留，初始响应不能替代完整响应。
- 严格六行Playback比较仍为Fail，原driver退出状态Unverified；2.735秒COMMIT尾延迟、有限内存观测窗口与缺失的10-29外部报告仍未闭合。

- 审核日期：2026-11-02；HEAD `a28249a3`；范围 123 commits / 2122 files / +257k −84k
- 方式：只读审核，未改动任何源码/文档、未运行 cargo。SQL 结论用 `sqlite3` 对 `crates/store/src/schema/v1/*.sql`、`crates/jobs/src/schema.sql` 建临时库跑 `EXPLAIN QUERY PLAN`（EQP），并用合成数据（10 万 items / 25k logs / 20 万 jobs 历史）计时；耗时绝对值仅供参考，复杂度阶数可靠。
- 按约定不计为问题：probe 并发 = 1、远程读冷却 500ms、ADR-0012 删库清理孤儿 Media 及其收藏/进度、library 读池容量 8。
- 本文件是受版本控制的审核基线，位于 `docs/audits/`；后续修复状态见下方记录，原始运行产物另存未提交的 `.e2e/` 或 `docs/test-reports/`。

## 修复状态（2026-11-02）

- B1/B2：`18bad154` 改为按可见 Library 的索引查询，观看统计使用用户等值条件；Store/API 回归和 10k Media 工作量回归已通过。
- B3：`2e378ca2` 将 Transfer/Watch 与原始 Scrape 配置的唯一权威移至 Library；同库快照观察、单调 revision 和发布事务内校验关闭配置变化窗口。Store 943、API 2109 项通过（各 10 项忽略），独立审查通过；旧布局按零兼容规则拒绝，未重建用户实例。
- B4：ADR-0008、数据库字典及当前入口已按现有 ReaderPool/每库 writer 更新；10-28 锁审计明确为历史快照。
- §2 建议项和最终运行验收继续按计划执行；本文的基线数值与旧结论保留原提交语境，不代表新一轮性能结果。

### 后续修复与证据（2026-11-03–05）

下方条目按事件先后保留当时版本和时点；“正在”“尚未”等阶段性状态以后续记录为准。

- 2026-11-05阶段候选为`1c3af0236c14a13753884f6164f9962a34edb5fa`。SQLite初始化修复`cd80d58e`已提交；原workspace退出101的20个失败目标因本机loopback EPERM进行授权定向核对，后续共1096 passed、0 failed、2 ignored，原101与失败分别保留；新进程初始化等7项通过，未再观察到原生崩溃。此前三项修复后的原workspace亦退出101，`executor_not_blocked`在完成前SIGSEGV；不得把定向重跑改写成单轮workspace全绿。详见[SQLite专项记录](2026-11-05-sqlite-startup-race.md)。`1c3af023`的TMDB人物头像NFO修复另有214项相关门、all-target Clippy、fmt与限定复审，见[专项记录](2026-11-05-nfo-person-thumbnails.md)；其构建清单为`/private/tmp/crawler-storage-candidate-1c3af0236c14/manifest.json`，验证依据是cd80原始workspace及定向核对加NFO受影响门，并非新候选重新跑全workspace。

- `cd80d58e`的有限Native复验中，管理员和成员无结果前缀均返回空；管理员长剧S1的163个Episode身份与坐标精确匹配，实际UI观察了分页边界及末尾，详情仍显示Movie 3:38、Episode 3:25。独立观察确认恢复停止副本的核心事实、catalog及全部491条file_details与阶段基线一致、491 probes成功；没有重扫，见`/tmp/crawler-real491-cd80-bounded-service-report.md`。严格六行Playback基线比较为**Fail**：未知操作者的真实管理员VidHub《功夫女足》从0到2122ms使终态为7 units/7 logs/7关闭SID/0活跃；原六行及Movie 218284ms、Episode 205907ms未变。未归因或重置此增量。原driver因工具宿主中断，退出状态Unverified；另行受控TERM恢复退出0，端口/句柄关闭，原app/sidecar、STRM及pan清单一致。7个测试VidHub连接已删，原有Emby连接保留，Reqable抓包已停。

- 最终匹配性能工具的早期准入补强先有6个无效基线生命周期副作用RED，后有2项新增、22项runner和6项audit GREEN；限定复审与实际绑定的30项及Ruby/shell/AST检查通过。旧版原8 worker seed失败并保留43次503；根因未确认。双方使用仅准备阶段`max_workers`从8改为1的独立衍生脚本，50用户、每人300 unit/400 log等工作量不变，无重试或测量修改；旧版、新版serial seed分别退出0、耗时194秒及202秒，均原清单匹配且无listener/owner/live（`/tmp/cm-final-old-serialprep-seed-stage.json`、`/tmp/cm-final-candidate-serialprep-seed-stage.json`）。原binding的generator hash不代表衍生脚本，衍生freeze为`3f77994f8a26065e6dcc191a2d4a2653144897cb652762a5f718e8a9095a6f77`。

- seed后发现WAL sidecar及3302个work缺少studio，不得放宽oracle。旧版首次category enrichment因以`mode=ro`打开无sidecar的WAL格式library失败退出1，服务未启动、NFO未改，原清单匹配且无owner/listener；live保留并移至该轮audit的`failed-live-retained`。只读打开诊断见`/tmp/cm-final-category-sqlite-open-diagnosis.json`；helper经3项RED/GREEN，只在无WAL内容且无journal时使用immutable，非空WAL仍为只读模式。后续结果见下方。历史73da freeze中仓库`perfgate.py`摘要被已提交`8473955d`替代，保留该不匹配；9087、audit及新freeze匹配。缺失的10-29外部报告仍为Unverified。

- 后续旧、新enrichment分别退出0、耗时84秒及91秒；两边各3302个NFO与3次scan的关键Playback、身份、文件和覆盖事实完全一致，原清单匹配且无owner/listener/live。`checkpoint-v3`退出0，双根`cold-checkpoint-publication.json`记录五库schema/table/rowid前后一致、integrity通过、外键错误0、sidecar不存在，原baseline保留。四份独立semantic/seed核对均退出0（`/private/tmp/crawler-matched-final-fixture-evidence-1c3af023/`）；50用户、各300 target/400 logs/100 favorites/5个各50成员的collections及精确身份已核对。两边语义除随机TV文件选择旧E16、新E51外一致，衍生dataset已固定S1E1。jobs审计最初查错表产生false；公开GET jobs/id读取`job_defs`，两边RSS定义均存在，不归为产品缺陷。

- 首次stage oracle复制后ROOT错误指向临时目录，调用已改为字节相同的clean checkout副本，原冻结工具不变；旧/新app/library四种schema均GREEN并经独立审查。finalizer v3退出0并生成材料，但双方各400项业务断言全部Unverified：oracle错误拒绝合法local/merge-survivor provisional，原材料及freeze保留且未准入测量（`/tmp/cm-final-initial-business-unverified-receipt.json`）。仅oracle及CLI测试的修正先在双schema RED，再有27项GREEN；允许明确confirmed/provisional，merging、retired、unknown仍为unknown，缺失Media外键拒绝。父任务正在重跑oracle和业务门并独立复审，准备独立工具提交及新包绑定产品`1c3af023`；未修改Rust、Web或DDL，也未重做seed/build。六阶段实际性能仍Not run。

- 后续工具修复`c55cd325`、`485b524e`已各自提交推送，根任务相关61/62项测试退出0及各自独立复审通过。新包`/private/tmp/crawler-matched-final-provisional-oracle`绑定30项测试退出0；v4原始oracle两边各50用户、400项断言全部verified（`/tmp/cm-final-corrected-business-admission.json`）。产品Rust/Web/DDL仍对应`1c3af023`，工具版本单独记`485b524e`。

- 首次实际匹配campaign `campaign-20261105T170045Z`在旧idle阶段主动中止：旧ce3 Similar返回HTTP 200且`data`为裸列表，新工具只认可pending/ready合同。旧报告保留`complete=false`、162 cases、161 Pass/1 Similar Fail；Similar 200次请求均HTTP 200，p95为950.702833ms，也实际超过150ms绝对预算，不能全部归因于解析器或改记Pass。根任务仅向自有perfgate子进程发送SIGINT，wrapper正常TERM停副本；`/private/tmp/crawler-perfgate-matched-old-ce3f189e5d3d/full-matched-audit-20261106-17802-1fop497/receipt.json`记录stage_exit 254、source match、无listener/owner/live，外层campaign退出1。此轮为Partial/中止而非完整idle，其余五阶段Not run。旧版显式legacy-list合同正按TDD准备，当前版仍严格使用stateful合同，不改SLO或20%规则；独立新包准备中，不重建产品或重做seed。

- 后续性能工具修复`920`、`c8d`、`433`分别独立提交，根任务对应106、32、108项测试通过并分别经独立复审。旧contract包coverage准入因三个视频流别名未覆盖而失败，该结果保留。v2新包绑定30项测试通过；真实路由清单154个GET、166个case、10个exemption，旧、新各50用户的400项业务断言均verified。产品身份仍为`1c3af023`，工具身份单独记`433`。新六阶段campaign为`/private/tmp/crawler-matched-final-contract-aware-v2/campaign-20261105T173705Z/campaign.json`；截至启动时旧idle为RUNNING，其余五阶段NOTRUN，尚无完整性能结论。

- `af40b58a` 的真实491来源联合验收已正常结束。两端管理员12个Movie/18个TV及成员12个Movie的完整ID集合与独立事实一致；隐藏TV、动画分类、五季切换、代表性详情图片/演员/媒体信息、收藏与已播放的跨端设置和撤销均有实际观察。真实STRM Movie与Episode完成Web→VidHub→Web出画、停止落库和续播，最终分别218284ms、205907ms；同一数据库正常重启后，四张Playback表逐行一致，两端详情分别显示3:38、3:25。335ms/1ms的首次Native起点偏差保留，不宣称帧精确。终态6个unit、6条log、6个关闭SID、0个活跃session；原app/sidecar与STRM/pan前后清单一致，自有服务正常停止。GUI浏览使10条`file_details.chapters_json`从NULL变成`[]`，其余16张核心表与原049事实一致；不能写成17表全部不变。隔离副本和两个临时Native连接仅为下述修复复验保留，最终清理待完成。
- 同轮新增两个实际失败：Native无结果搜索在空`SearchTerm`响应后改发`NameStartsWith`，服务端忽略后者并返回全部30项；Native长TV的Episodes请求不带分页参数，服务端默认仅返回100/163，客户端不继续翻页。公开Router定向RED已分别观察到2项前缀过滤失败及5项分页截断失败；额外单集显示名前缀RED也已复现。`f21ff307`与`b47290d1`现已分别修复、独立复审、单独提交推送；前缀过滤使用同一Library快照的显示字段，Episodes未指定Limit时不再截断、显式205亦不被200上限截断。公开Router回归通过，新候选实际复验仍待执行，不将页面轮标为全部通过。
- 最终perfgate的正确性补测已复现工具缺口：HTTP200的错误空Genres被判为PASS。`8473955d`已按TDD补齐独立期望合同和原始SQLite事实oracle并提交推送；最终98项工具回归及限定独立复审通过，绑定六份工具hash与配置，分类无法独立证明时保持Unverified。实际旧/新seed与匹配idle/busy/cold尚未运行。既有容量和内存结果及2.735秒COMMIT尾延迟边界保持原记录。

- `af40b58a`已单独提交并推送，关闭9087复审发现的五类协议缺陷：PlaybackInfo响应来源与SID竞态、缺DeviceId的身份不一致、字幕随首选来源漂移、未知/隐藏ParentId扩大分类范围，以及旧SID遗漏Library配置/生命周期复查。均有公开Router有效RED→GREEN，并补充实际Range/字幕字节、授权/设备/用户/单元隔离、配置变更和fresh SID正控；协议合同详见[Jellyfin记录](../api-contracts/jellyfin.md)。最终稳定树release workspace含doc-tests实际退出0，596结果组汇总3936 passed、0 failed、25 ignored（含子进程汇总、非去重数），1977项代码hash前后一致并与提交对应；release all-target Clippy、fmt及文件/函数硬门通过，提交后限定复审无可操作发现。初轮workspace的六项fixture授权失败及loopback权限失败分别保留；夹具仅调整为有权限时创建真实历史再撤销访问，不削弱生产授权。新候选二进制已冻结，491真实库联合页面/播放与最终匹配perfgate尚未通过，不将73da实播或049探测改记为af40。

- `9087d4c5`已单独提交推送：公开Router先复现来源A会话在仍可见有效时因B成为首选而无法激活/保存、Range返回404，再按用户/设备/SID的绑定来源解析未指定来源的事件与取流。89项Jellyfin、6项取流绑定、14项事件身份、12项播放读取通过；公开负例验证未知/关闭SID、错误用户/设备/Media、隐藏Library不写入或取流，同物理文件覆盖两集时不能跨集借SID。release Clippy/fmt、独立审查通过；稳定树release workspace含doc-tests实际退出0，596个结果组汇总3925 passed、0 failed、25 ignored（含子进程汇总、非去重数），测试文件哈希与提交一致。Work/Season聚合入口的同类来源切换未直接实测。新候选491完整页面与匹配性能仍待执行，73da四源实播不改记为新候选通过。

- 049最新491轮自然探测全部成功，但正式GUI准备因两个空默认Library未隐藏而失败，成员可见库精确断言不满足，未到S0/ready。前置admin12 Movie/18 TV身份及member隐藏TV404已观察，播放/标记表均空；正常停服、完整五库冷快照和原app/sidecar、STRM/pan清单一致。保留fixture失败与049探测版本；后续同隔离轮显式切换9087并修正fixture，不恢复DB、不重扫/重probe，GUI证据独立记新候选。

- 9087实际切换首启/fixture修正/17事实表比较通过并冻结完整S0，但重启后比较失败，未ready/正式GUI。完整一次性副本诊断定位相同11个TV Media的27条metadata input及180条unit revision变化；启动时原已排队的全局Scrape运行，RSS只新增后继排队行，无新增probe。sample全树和原app/sidecar、STRM/pan清单一致，服务正常退出。失败、完整S0及逐字段差异保留；下一阶段须明确整套S0恢复与启动前暂停副本无关的Scrape/RSS，不能放宽17表比较或追认旧轮。

- 9087宽面复审的四项问题均已由公开Router实际复现：PlaybackInfo来源/SID竞态和缺DeviceId取流均404而非206，A字幕URL返回B实际字节，未知ParentId返回全局分类而非空页；渲染后隐藏成员Library仍签发200也已先RED。统一修复已有聚焦GREEN，未完成最终整波次验收。定向复审另发现旧SID未复查Library配置/生命周期，实际字幕200而非404；common video/subtitle guard修复后聚焦GREEN，当前补负例及三crate验证。首轮API测试因四个loopback bind PermissionDenied退出101（284通过），不计产品RED或全绿。9087匹配性能仍仅静态准备，未seed/测量；后续最终候选须另绑。

- `049389ac`已单独提交推送，修复73da后复审新增的P2：播放事件忽略明确MediaSourceId、按ItemId选择首选来源，旧revision无SID、非首选来源和SID来源冲突均可导致错误归属；同Media隐藏来源及另一Media有效来源也由公开Router复现错误写入。先RED后改为严格来源选择与绑定核验，来源8项GREEN，相关矩阵、release Clippy/fmt/尺寸及独立复审通过。最终稳定工作树release workspace含doc-tests退出0：596个结果组汇总3919 passed、0 failed、25 ignored（含子进程汇总、非去重数），测试代码哈希与提交一致。不把此缺陷当作之前Native无上报的原因。隐藏详情/PlaybackInfo及POST Delete的跨用户/隐藏条目负例首次通过，属于覆盖补强，未证明原授权绕过。首轮media-server两项回环监听EPERM与授权仅重跑user_isolation目标3/3通过分别保留。

- `73da134e`正式四源实播：本地Movie/HTTP STRM Movie/本地E01/HTTP STRM E02均收到真实Sessions204并关闭SID，停止ticks按毫秒转换后与DB/log一致。Movie与E02分别完成Native→Web→Native→Web连续链，各次新日志起点等于前次持久停止；正常同DB重启后四张Playback表逐行一致，再次Web/Native续播通过；成员B实际播放不改变A四个单元。终态5units/14logs/14closed SID/0active，正常停止、原app/sidecar和STRM/pan清单一致，无强杀。具体数值见最终验收清单。Native首帧提前和E02画面烧录与报告约5.8秒差异保留，不宣称帧精确；实际Native直取HTTP origin的Range未捕获。491完整页面另验，不与控制片混同。

- `73da134e`已修复并推送：可播放单项详情返回与PlaybackInfo共用的MediaSources，STRM本地读取移到锁外阻塞任务，Series/list不增加源读取。Movie/Episode缺字段红测及Season字幕错绑红测均先失败后通过；Jellyfin76、binding5、media-server去重40、相关release Clippy/fmt及独立复审通过。首轮两项loopback EPERM与授权定向重跑分别保留。稳定候选最终release workspace含doc-tests退出0：596个结果组汇总3911 passed、0 failed、25 ignored，含子进程汇总、非去重数，不把忽略项计作通过；新候选实际File/STRM/Episode与Web双向验收继续，不把隔离桥正向当成生产通过。

- `61ee9207`详情MediaSources单变量正向：完整coldS0五库/13文件恢复及oracle一致，仅选中File详情追加原PI源数组、所有既有身份/Auth/PI原样。真实Playing1/Progress4/Stopped1均204，停止758080044ticks与DB/log75808ms一致、SID关闭，VidHub显示01:16续播。原f1源被实际PI/stream消费，定位到详情媒体源初始化缺失；生产TDD及File/STRM/Episode/Web矩阵继续，临时桥结果不等于生产通过。

- `61ee9207`单File GUID真实诊断：条目Id/UserData.ItemId改变，详情、PI及stream实际消费并回译，Auth/PI及保护字段字节审计通过；实播0.167→快进60.500→暂停70.333秒，正常退出后仍无Sessions、SID pending、单位/日志为空。无映射拒绝，两次响应头后取消单独保留。仅否定该双字段变化足以恢复上报，不改生产ID；后续区分detail.MediaSources缺失与source ID格式，详见客户端合同。

- `61ee9207`插桩fullB完整退出0：4,500,001次推进、1719秒、前后100k Media/1M文件/1M Playback及外键oracle通过；覆盖样本4,500,002、缺失0，克隆删除、原fixture metadata一致，最低采样空闲98,304,000,000字节。最大覆盖事务2,735,212,417ns位于第4,080,709步`Users→Users`，对应整个advance的PROFILE为1 BEGIN/14正文/1 COMMIT、计时0/0/2,735,000,000ns。保留2.735秒提交尾风险；语句聚合及毫秒时钟不能证明峰值事务细分、checkpoint或fsync原因，插桩诊断不等于strict SLO通过。该轮不替换先前中断或未归因的历史结果。
- `61ee9207`完整报文对照：官方Jellyfin 12.4 Docker与独立产品实例播放相同600秒File/HTTP STRM，Reqable MCP保留真实VidHub请求/响应。官方File/STRM的Playing/Progress/Stopped均204，停止报告与持久位置分别1431165307/3535199618ticks；产品File真帧、seek、正常退出后无Sessions，单元/日志空、SID pending，保留Fail。产品STRM点击时锁屏且无新PlaybackInfo，播放未确认，不判阴性。登录、条目ID/MediaSources及播放source等差异已记录，但根因未证。锁屏后可逆停服并保留私有冷S0与抓包，原清单及媒体SHA一致、私有端口/句柄清空；origin退出137例外保留，其余服务正常退出，不宣称全部正常退出。详见[客户端合同](../api-contracts/jellyfin.md)。
- `61ee9207`容量复验：独立百万文件副本的Header与小Movie检查退出0、前后计数/外键断言通过，覆盖事务最大4.696/32.374ms；完整TV B在949718步的`VerifyReverse→VerifyReverse`出现2641.865ms事务及2641.870ms writer持有，按阈值中止退出130，未到终态、无after oracle。原夹具元数据未变，副本均清理，最低空闲约95.0GB。该阶段查询在reader准备，writer仅校验和推进游标，缺少SQL/commit分段证据，尚不能归因慢样本或宣布容量通过。
- `61ee9207`：补Movie/Episode从真实登录token到目录、绑定Range、三种播放事件及列表/详情位置读回的连续HTTP回归，两协议前缀均覆盖；Episode另验证两用户/两集隔离。新链首次通过，未伪称产品修复RED。完整套件曾75/76因两项日志观察并发空捕获失败，精简并发复现后只调整测试编排、保留全部断言，最终75/75、相关release Clippy/fmt及独立审查通过，已提交推送。实际VidHub进度仍未验收通过。
- `32591af5`：季列表独立构造的UserData缺少Key/ItemId，现补充稳定身份。公开API回归先因ItemId为null失败，再通过Jellyfin集成74项、media-server全套40项、相关release Clippy和fmt；独立审查无阻断项，已单独提交推送。当前491诊断服务仍运行冻结的bd3候选，不能把新Season修复或未执行的GUID实播当成Native进度问题已解决。
- `bd3c16f9` 同一停止态的独立字段诊断B/C均为阴性：B实际只补登录根ServerId，C登录字节一致且11个实际条目响应的77个目标条目仅补ServerId；两轮真帧、seek、正常退出后均无Sessions入站，Native SID仍pending、持久值233489ms，Web刷新03:53。C首帧HUD03:54，末HUD未知。两字段单独补齐不足以恢复报告，尚未确认根因；用户指定的本地movieclaw实现已作只读对照，其32hex实体ID及完整登录DTO是待验证差异，不是修复结论。协议和回归说明见[客户端合同](../api-contracts/jellyfin.md)。
- `bd3c16f9` 增加合并协议路由外层的安全入站观察，覆盖直接18767和经18768转发的到达及响应头状态，不记录实际路径/query/凭据/正文。新隔离轮491个来源probe全部自然成功，12个Movie/479个TV文件均有实际轨道且无排队/运行项。《四喜猫》S01E01 Web真实出画、seek及退出后保存233489ms、SID关闭；VidHub同用户读到03:53并真实续播，正常退出后DB仍233489ms、Native SID pending、无Native日志，Web刷新仍03:53。后端58次到达/58次完成没有Sessions请求；登录阶段Users401不能当作报告被拒绝。Native截图无可读时间，精确停止位置未知。第二片《七龙珠超：比鲁斯》实际probe为H264/AAC，但Web首次加载失败且未确认帧；通用编码提示不证明编码不支持。B/C单字段诊断阴性另记。该轮最终到达固定7200秒测试窗口并退出1；注册后端/观察器及原进程组正常TERM退出，原app/sidecar及pan/STRM清单一致，4份快照和私有副本已删除，无force或残留。D因再次锁屏未运行，不将这些局部结果或清理通过标成整轮通过。
- `047e7d1e` 的独立本地600秒视频实测：Web真实出帧/seek后退出保存341008ms，刷新重开出帧、再停346511ms；同DB重启后源revision、轨道、进度及日志一致，另一用户无这些记录。VidHub同源也已真帧并读到Web续播点，但实播退出后签发SID仍pending、无native日志，进度保留Web旧值，保留Fail；18768观察器未见Sessions不能排除其他入口。该轮正常清理，两份私有停止态快照及DB/凭据均删除，原视频哈希一致。另起独立DB的HTTP STRM已通过实际产品probe及Web出帧/seek，保存262601ms、实际重开并再停268161ms，重启后逻辑事实一致、不同用户隔离；Native及真实491来源仍待执行，不将合成控制片当真实库通过。最终限定范围代码静态复核无新增可复现阻断项，不代表运行验收通过。
- `047e7d1e`：49cce隔离本地视频的真实Native请求仍404，安全观测确认发送了`MediaSourceId=`，此前诊断解析忽略空查询值而误记为缺失。相同认证、SID、设备、参数及32字节Range的只读差分中，原样404，仅删除这一空key即206且字节一致。现仅把精确空源参数按未指定处理，非空无效值、空白及旧源revision仍拒绝；保留现有默认文件选择及会话绑定校验。公开API先404失败，再5项及Jellyfin71项通过，media-server全套40项、定向clippy/fmt和独立review通过，已单独提交推送。新候选真实出帧及进度仍待验证。
- `c690956e`：49cce实际Web首个sessions请求200，首个视频流401，随后降级请求400并显示通用编码提示；浏览器H264/AAC能力检查均通过。真实网络布尔确认URL凭据等于POST Bearer，而自动Cookie与二者不同。取流鉴权现按Bearer、既有窄范围query凭据、Cookie选择，显式失败不回退其他用户；普通API的query鉴权范围未扩大。公开POST签发URL回归先2项失败，再通过播放读12项、存储端到端5项、观看投影8项、播放/撤销21项及auth单测1项，定向clippy/fmt及独立review通过。另保留一次错误测试target名称造成的工具退出101。已单独提交推送，真实Web出帧、保存和续播仍待新候选复验。
- `49cce686`：隔离本地视频实测确认 VidHub 请求 `/videos/{id}/stream.mp4` 返回404，现有 `/Videos/{id}/stream` 返回相同源的正确Range字节。四种大小写/容器后缀入口统一复用既有鉴权、设备撤销、可见性及播放会话绑定，不改实际文件或STRM跳转。公开API回归先2项失败（404对206/401），修复后4项及Jellyfin集成71项通过；STRM重定向及撤销矩阵3项通过。media-server全套首次38通过/2项本机WebSocket监听EPERM失败，仅失败目标授权重跑3项通过，去重40项；定向release clippy、fmt、尺寸门及独立review通过。已单独提交推送，真实Web/VidHub的本地视频与STRM播放、持久进度及恢复仍须分别验收。
- `eb87a4dc`：隔离Web点击电影播放进入404，因路由要求作品ID后存在通配单元段；改用可选单元参数，电影与单集共用当前播放入口。路由回归先复现电影不匹配，修复后新增3项通过；完整Web940项、typecheck、lint及build通过，独立review无新增must-fix，已单独提交推送。该回归不替代实际播放器的进度保存、刷新及重启恢复验证。
- `8928568123d7` 的独立491来源轮完成全部probe后，基于完整停服S0做Type单字段诊断。透明A及仅补`MediaSources[0].Type=Default`的B均有真实帧、32次右移seek，退出后成员进度仍0、无active session/log、签发SID为pending；代理未见Sessions入站且无proxy error。B首次未出画独立保留，唯一实播重试seek后约92秒，A为约73秒，不把两次时长称为完全一致。仅能否定补Type足以恢复报告，未定位根因、未改生产；恢复清理另记。用户新增的自有Web Movie/Episode退出保存、刷新/重启恢复、切集独立进度及Web/VidHub一致性须另作实际验收。

- `89285681`：真实浏览器的 Library TV 搜索白屏由前端仍读取文件命中的 `units.map` 触发；当前 API 返回的是 Media 命中。前端按唯一 Media 合同显示类型、标题、年份和详情入口，并保留顶层游标以加载后续页，不加旧形状兜底。公开组件/客户端回归先复现原始 TypeError，再通过2项定向测试；完整 Web 937项、typecheck、lint（0 errors/81项其他文件 warnings）及build通过，独立复审无新增问题，已单独提交推送。首次完整Web测试的回环监听沙箱失败保留，授权重跑通过。新候选release构建成功、二进制及UI分别冻结；真实浏览器复验及完整页面阶段仍待执行，不将旧轮白屏追认为通过。
- `3fbbf445`：新的百万规模克隆诊断将一次1.688秒writer事务定位到 `VerifyHeader→Members`；该样本与旧完整轮未归因的2.047秒最大值分别保留。FTS actual证据改由Library reader按operation/library/revision准备并共享，writer仅比较当前Media的精确term/column/offset及零token文档；保留owner/CAS/重启语义，缓存析构位于writer外。公开writer工作量回归先为5,352→49,671而失败，修复后为3,747→3,749，后续Header writer为3,560→3,550；首轮全局工作移到reader，不据此宣称全流程为常数时间或所有事务低于1秒。54项相关release测试、Store/API受影响验证、workspace release all-target clippy及fmt/尺寸门已执行；首次受影响全套因10个API目标的本机监听权限及次生断言退出101，Store目标全部通过，授权仅重跑10目标后顶层1,010 passed/1 ignored、退出0，不称单轮全绿。FTS专项和最终跨任务静态复审未发现新增must-fix，代码已提交推送；实际Header/完整终态、性能及设备复验仍待独立新轮。
- `3fbbf445a10f` 隔离联合轮 `91261-3au6k3` 已结束：health及二进制身份、491来源扫描和12 Movie/18 TV oracle通过。VidHub选择Jellyfin，成员电影及管理员单集的两种标记SET与实际POST Delete均200，退出重进保持撤销；成员隐藏单集404、电影两用户标记独立。成员动画分类五项与独立Library membership/zh-CN genres oracle完全一致。电影实际seek至05:28并续播至06:57、暂停关闭后回读仍0且重入从头；第二次复现后该成员有3个pending签发会话，没有active session/log及单位进度或resume摘要，保留Fail且根因未定。后端access logger只覆盖自有API、未覆盖同级Jellyfin路由，代理也不能证明直连入口无请求；不据无日志猜测协议字段缺陷。页面矩阵部分通过，搜索白屏已另行修复，其他失败及缺少夹具项保留。正常退出后原app/sidecar与pan/STRM清单一致，无自有listener/live进程，两份私有副本已删除，脱敏日志及页面证据保留；本轮清理通过不改变功能失败。
- 官方Jellyfin 12.4.0独立对照已完成同一600秒视频的File及HTTP STRM实播。File收到Playing1/Progress22/Stopped1，均204，服务端保存约08:50并有resume；File实际重入未确认。STRM收到Playing2/Progress25/Stopped2，均204，SID/device/source关联一致，首次约07:47停止后VidHub重入从约07:51播放；最终保存5,323,575,794ticks、played=false、play_count=2且resume包含该项。容器内外同URL的Range字节、时长及长度已核验；官方服务、origin和代理正常停止，剩余自有容器为0。该功能诊断排除了HTTP/remote源必然不报告的宽假设，不证明本产品已修复，也不替代491真实来源验收。当前8928独立轮继续同状态快照下的单字段Type对照，尚无生产修复结论。
- `798606f3`：真实 VidHub 以 Jellyfin 接入时，取消收藏及取消已播放发送 `POST /Users/{userId}/{FavoriteItems|PlayedItems}/{itemId}/Delete`，旧服务返回405。新增两条受认证保护的协议路由，复用原用户范围、Library 可见性及写准入；测试路由树无静态回退时先复现404，再验证取消、重入和持久状态。Jellyfin 集成71项及 media-server 40项通过，相关clippy/fmt及独立审查通过，已提交推送；新的实际客户端撤销复验仍待运行。
- `f55776bd`：补偿清理将 retain 前中断的新建无引用 Media 纳入最后一窗的权威 orphan predicate，保留既有事实、并发身份变化和其他引用。公开 Store 回归先2项清理失败、2项保护对照通过，修复后补偿13项、相关clippy/fmt及独立审查通过，已提交推送。
- `192e1f32`：创建在 durable begin 前持有 Subscribe decision guard；启动及周期恢复逐候选非阻塞取得同一 guard，跳过活跃创建并继续处理其他候选。reader 选取和短 writer 游标推进分离，坏候选独立报错、游标继续推进，epoch/generation 检查保留。真实创建服务操作与公开HTTP tick 的确定性交错先失败后通过；API2项、Store补偿13项/retention23项、相关clippy/fmt及最终独立审查通过，已提交推送。未重复 workspace 全套，后续 Store/API 功能树还会变化。
- 新隔离轮 `26713-1qtztoy` 使用 `f886d46c1533`，真实491来源副本显示12部 Movie/18部 TV。VidHub 管理员和成员均选择Jellyfin登录，成员只见12部 Movie、直接访问隐藏TV返回404；电影/单集设置标记200，撤销405保留为Fail。搜索同参数复查管理员返回目标TV1项、成员0项，客户端随后请求无过滤列表导致显示其他授权电影，不归因为服务端搜索泄露。分类仅观察动画5项，精确metadata oracle、播放进度/恢复、完整artwork/轨道及Web矩阵仍未验完。该轮正常退出，原app/sidecar及pan/STRM前后清单一致，自有进程和两份私有副本已删除，临时VidHub连接已移除；不能由此追认旧轮清理或进度通过。
- `ce85e0a3`：补充仅供测试使用的普通浏览首页 EXPLAIN 诊断，复用生产查询构造器、绑定和 Library reader。行为红测2项失败后定向3项通过；Store release 汇总1023 passed、0 failed、12 ignored（207个结果组，非去重数），clippy/fmt/尺寸门通过，提交推送后独立review无发现。该诊断尚不代表完整容量门已执行。
- `cc7c997b` 事务诊断补项已提交推送，提交后独立review无发现。默认10k分集的release测量：100个100文件发布批次、99个完整扫描窗口心跳，全部100次HTTP尝试均200、busy为0。心跳p95为564ms；扫描writer等待p95为3.681ms、持有p95为568.541ms，扫描已覆盖事务最大593.104ms；native请求内writer等待之和p95为559.420ms、已覆盖事务最大3.876ms，扫描吞吐346.8文件/秒。计时范围从尝试BEGIN到commit/rollback/drop，仅成功BEGIN计数，排除其他raw事务；RSS因采集权限不可用而未测得。Store release汇总1025 passed/12 ignored，API原全套10个目标在沙箱失败后授权定向重跑全部通过，media-server全套及三crate的clippy/fmt通过；测试打印参数的最后风格调整仅由all-target clippy及fmt验证。统计为结果组求和，非去重数。本项测试通过仅指其工作量/一致性断言及测量完成，不能解释为严格SLO或最终perfgate通过。
- `cc7c997be6ff` 独立内存轮 `round-20261104-51289-1miams0` 已完成 Movie 4/4/4、同源重扫和额外60秒空闲，12个唯一文件probe均成功；六阶段heap/完整malloc_history正常退出并保留原始数据。活分配10,827,104→峰值检查点13,433,664→重扫12,220,032→空闲12,202,000字节；最后两阶段RSS均53,008 KiB。分配栈包含日志缓冲字符串、SQLite reader/页面缓存及网络连接/传输缓冲；网络分类重扫后下降。仍有未归属余量，不把分配点分类当完整对象所有权，也不据有限批次证明长期无泄漏。原app/sidecar及pan/STRM清单一致，无forced/live自有进程，私有副本已删除。09:15已可读取VidHub实际窗口，剩余设备操作尚未重验。
- 对上述保留的六阶段原始栈作离线细分：baseline至最后idle净增1,374,896字节，主要为SQLite +426,480、ureq +603,360和String +224,768；原other的72,672字节中62,784对应rustls符号，原分类外44,736字节补充解析覆盖43,632，剩余净额1,104字节及other中的6,080字节未细分。这些互斥分类与LogBuffer、ReaderPool连接缓存、HTTP/TLS池及redirect cache的源码生命周期相符；LogBuffer上限3,000条、redirect cache上限4,096项且TTL不主动驱逐。已定位该轮主要分配增长来源，但聚合栈不提供逐个allocation ID，不能证明所有当前引用或追溯已清理原始栈的491/Transfer旧轮，也不能宣称长期无泄漏。
- `cc7c997be6ff` 的完整idle perfgate已实际完成：166项、每项200样本，164项通过、2项失败。Similar初始p95为2.545292ms、相对+92.746%，完整结果199项ready、1项502；日志导出p95为23.969584ms，满足300ms绝对预算但相对+32.228%失败。报告保持原相对门及失败，未解释为通过。原app/sidecar及pan/STRM清单一致，无残留listener/owner/live副本。测试脚本此前两次因`/tmp`与`/private/tmp`路径比较误判而启动失败，原失败审计保留，规范化路径后24k STRM seed完成。
- `cc7c997be6ff` 的完整busy perfgate已完成166项、每项200样本，155项通过、11项相对基线回退失败：两种Jellyfin计数、ledger、gallery、up-next、日志导出、两种登录、PlaybackInfo、收藏及进度写入。Similar本轮200个完整结果均成功，不能回填idle的一次502。原app/sidecar及pan/STRM清单一致，正常清理，无listener/owner/live残留。提交后的`2b3c160d..cc7c997be6ff`只读增量review未发现具体新跨任务问题，Spec/Quality限定范围通过；不代表运行/容量/页面门通过。
- `cc7c997be6ff` 的cold逐用例重启轮已完成全部166项，160项通过、6项失败（日志导出、两种登录、PlaybackInfo、收藏及已看写入）；原app/sidecar及pan/STRM清单一致，无listener/owner/live残留。idle的Similar 502对应TMDB `/movie/9002490/similar` connect timeout且无有效stale缓存，更深网络原因未确定；保留失败及相对门结果，不追认为通过。
- cold六项失败中，两种登录、PlaybackInfo、收藏及已看写入的旧基线实际均为`code=0`连接失败；工具错误地将约0.1ms失败耗时作为可比性能。原报告保留，但这五项不能据此认定产品回退，缺少有效相对基线仍不能验收通过。日志导出的成功HTTP基线相对失败独立保留；后续工具修复与证据核对见`f886d46c`记录。
- `cc7c997be6ff` 的完整合成容量工具退出0，确认100k Media、1M文件、1M Playback及10用户，固定目标页VmStep为1590→1590、Fullscan/Sort均0；分集分页oracle、事实/摘要计数及外键检查通过。完整轮约2956秒，DB/WAL最终8,642,501,368字节，索引dbstat为6,427,488,256字节。Integrity B共4,500,001步、2284秒，单次已覆盖事务最大2,047,298,167ns，未记录其阶段，风险仍待归因。独立默认100k Media Integrity gate通过，1,600,001步、检查233.42秒、总338.81秒，最大writer约712.63ms；不据工具退出0宣称所有最终门已通过。
- realistic release gate通过（1项、58.35秒），实际21k Media/61k文件/45k分集。完整容量工具采样RSS峰值435,184 KiB，不是实际服务内存测量。后续在APFS克隆的full副本上对50项Movie A作check-only诊断，最大事务875.94ms出现在`VerifyHeader→Members`，终态9.02ms；其路径调用全局FTS prepare，但不能把原B修复轮2.047秒样本据此归因。诊断退出0、克隆删除，原full副本保留。
- `f886d46c` 修复perfgate基线有效性：失败HTTP/业务验证或不足窗口样本的历史耗时不再作为相对基线，当前错误响应不再显示相对通过，空基线在测量前拒绝；保留已授权缓存口径例外和20%/1ms规则。公开main先RED后GREEN，相关离线74项通过、静态复核通过，已提交推送。原始三轮报告保持原样，不据工具修复追认历史验收通过。
- 按`f886d46c`规则离线核对原报告：idle日志导出仍为有效相对失败，Similar因终态502业务校验失败而不能成立相对比较；busy仍有11项有效相对失败。cold除前述五项外，chapters及progress的旧基线也有零耗时和前置失败，共七项相对比较不可用；日志导出仍有效失败。busy/cold各14项缓存口径例外只执行原绝对预算及正确性校验。该核对不是新性能运行，原始报告及失败不改写。
- `4d681e8f`：Subscribe 子表 FK 级联及 Collection 排序错误传播；`b89769d3`：Jobs 有界准入和待同步意图；`c476b43a`：结构化 Jobs 身份；`fa0f8e05`：引用保留、合并及清理 generation fence；`df1f473d`：毫秒时间、严格 cursor 与 token 索引。各库分别提交，持久意图恢复不构成跨库原子性。已关闭 fence、终态 epoch 和 tombstone 可累积，尚无压缩上限保证。
- `cf012c99`：结果值逻辑字节和实际 writer 诊断、P1 覆盖索引、P4 批量及精确 TMDB 键、P5 每定义索引 top-1。P1 精确聚合仍随目标库数据线性增长；P2/P3/P6–P10 的排序、扫描、OFFSET、精确计数或短子串规模依赖未整体消除。P8 无过滤样本工作量稳定，带残差过滤仍线性。
- Task9 本地 Mac debug、进程内 Axum 合成测量：每协议4阶段，每阶段20次100文件扫描/发布与20次重叠进度请求；native 与 protocol 各80次持久进度、0个503。native 后索引 writer hold p95 在扫描分散/单Media为73.627/39.274ms，发布为78.512/52.521ms；对应 HTTP p95 为73.043/38.764/77.437/51.145ms。protocol provider 的 writer timing 未传播，不记为0；raw Playback 事务不在 owned `write_tx` 覆盖内。单次本地合成测量不是实际设备或最终 SLO 验收。
- `2b3c160d`：退订预览和清理、Downloader/Playback 字段、严格 User removal version 与构造副作用清理。Task10 四 crate 测试退出0，448个结果组、3225 passed、22 ignored（含子进程汇总，非去重数）；Web 935 passed，lint 0 errors/81既有 warnings，typecheck/build、workspace all-target clippy 与 fmt 通过。Task9 五 crate 测试退出0，481个结果组、3351 passed、23 ignored（同样含子进程汇总）。这些是各自版本局部证据，不是最终 workspace 或现场验收。
- `938a9b89` 完成五库数据字典与生命周期合同对账。最终 workspace 全套已执行一次：命令开始于 `2b3c160d`，期间仅文档/注释变化至 `938a9b89`；退出0，588个结果组、3842 passed、0 failed、26 ignored（含子进程汇总，非去重数）。不包含后续 Rust 修复。
- `9a9dcbbb`：分集选源限定到当前单元覆盖，固定单库/Media/季的分集升序复用索引路径。固定50项页面的额外 VM 工作在100→1500集样本中由73972→1053972改为约7005；精确计数和协议 OFFSET 的线性成本仍保留。新增3项和既有10项定向回归通过，独立审查通过。
- `dabf77a6`：媒体探测显式使用自身活跃 lease 的取消控制，修复借用其他同类任务 control 后逃出删除取消集合的 NFO 迟到写。公开 worker/删除边界的10项定向回归通过，独立审查无问题；远程读1/500ms及删除不等待旧 worker 的语义不变。
- `52522549`：Integrity 在单个 Media 内按持久游标分页，补齐事实写屏障和 Favorites/roster 读取屏障，父子清理共用100次实际 Delete 预算。初轮 Store/API 全套的失败已分别定向补绿，最终补包 Store14/API23项、clippy/fmt/尺寸门通过，独立主审和增量审查通过；不宣称最终修改后全套单轮全绿。规模测试仅 N=3 验证工具，不能代替100k验收。
- 早期候选 `938a9b89809e` 已运行491个真实 STRM/pan 来源副本的功能轮及独立 Transfer 轮；Transfer 使用真实视频与 fake qB。该候选删除成功约441ms后 NFO 仍被补写流信息的失败证据保留。最终候选 `52522549fbef` 的真实删除竞态复测通过：9次约50ms文件清单采样、6个原有文件，未发现丢失、STRM变化或观测到成功后的内容变化。这仅覆盖该次观测窗口，不是取消延迟或长期无变化保证。
- 最终候选首轮491功能断言通过，但读取原 app.db key 前缺少 app.db/WAL/SHM 基线，原 app 恢复证据为 Unverified，不能追认通过。工具改用私有 key 后的新491轮主断言亦通过，原 app/sidecar 与 STRM/pan 的前后清单一致且自有进程已清理。该轮仅覆盖扫描及指定业务断言，结束时 probe jobs 仍有79 queued/1 running，另有1项 ffprobe TLS IO超时（根因未定）；不能声称所有491文件的 probe 均已完成或成功。原轮副本已按清理约定删除，后续终态观察必须另记新轮证据，不回填旧轮。
- 最终候选独立 Transfer 轮通过（真实 worker、视频哈希/NFO/重启，fake qB）。CLI/global Scrape 首轮退出1的未知失败保留；后续工具错误分别为把未来 scheduled queued 当作活跃工作、硬编码英文标题不符实际中文标题。修正临时工具后的独立新轮退出0，精确跟踪 expedited child 实际开始/完成、验证新 ledger、NFO、TMDB alias及重启后 CLI/API 事实；原 app/sidecar 与 STRM/pan 清单一致，自有进程及副本已清理。Transfer 与独立 global Scrape 结果分别记录。
- 独立全来源 Scan 轮 `12916-11ts08a` 在同一 backend 完成 Movie 4/4/4、TV 160/160/159 六批，每批均达到持续10秒的持久空闲；491个唯一 file_id 均有成功 probe unit，491次尝试全部成功，无失败或活跃项。原 app/sidecar 与 STRM/pan 清单一致、自有进程及副本已清理。这是新的成功轮，不改变前述 TLS 失败、不完整终态或原 app 基线缺失结论。
- 多批内存诊断已执行：上述 Scan 的空闲 RSS 从64,288 KiB，经 Movie 三批至53,088 KiB、TV 三批至73,984 KiB；SQLite 栈分类存活字节3,933,088→8,975,472、String clone 181,520→1,323,744。独立 Transfer 轮 `34495-1ksgvu8` 在同一 backend 完成三个独立 Media/Subscribe 的 fake-qB 提交及真实发布，哈希/NFO、每批持久空闲与原清单均通过；空闲 RSS 66,128→68,944→49,536→49,616 KiB，SQLite 分类约3.85→3.65 MiB。8 MiB malloc 正控证明采集可用，但栈分类和有限批次不能证明完整对象归属或无泄漏。该 Transfer 轮不覆盖同一 Media 的多版本；前一轮第二批409未保存具体错误，根因仍未知。
- 浏览器与用户授权的 VidHub 联合轮 `72487-z6wo3u` 已结束。真实 Movie 卡片与图片就绪：idle 20/20、p95 176ms；TV Scan running 期间20/20、p95 182ms。Probe 阶段仅第1例通过，第2例未观察到 running，其余18例未运行；Scrape 第1例185ms，第2例5439ms并出现 `/api/v1/unidentified` 504，其余18例未运行。此前仅标题出现的 p95 177ms及等待失败保留，不冒充业务内容就绪结果。
- 联合轮 Web 确认12个 Movie、18个 TV Media/479文件、长安详情与媒体信息、火线S1的13集/S5的10集、仙逆163集；首次部分图片未就绪不记全图通过。VidHub admin 列表、搜索、部分详情/演员图片及实际播放画面、暂停/跳转/恢复/关闭已观察；member隐藏库、全部季切换及服务端播放记录仍未闭合。真实播放后服务端无进度，会话请求尚未捕获，不能归因于已独立复现的 null PositionTicks 缺陷。后续 native 操作受 macOS 锁屏阻挡。
- 该联合轮491个唯一探测单元全部成功，但退出时需强制终止自有进程组，清理判定 Fail。原 app/sidecar 与 STRM 清单一致；原 pan 的两份任务数据库和日志发生变化，时间不同于私有 pan 退出、归因未确认，未覆盖原文件。失败轮与证据保留；临时轮工具已补每轮复制前、复制后、退出后清单及存活PID快照，新的真实轮验证另记，不能追认旧轮通过。
- 当前候选 perfgate、完整容量、设备及多批内存归属门仍待补齐。缺失的10-29 review spec 细项仍未知；不能由旧 unchecked 反推未实现，也不能据局部通过宣称最终验收通过。
- `ab362a82`：空 Unidentified 列表不再申请文件系统容量；公开HTTP回归先在耗尽两份容量时超时失败，修复后21项Library管理回归通过，非空stat行为保留。该缺陷可独立复现，不能据此断言旧504当时的容量持有者已查明。
- `4ae4a85e`：Jellyfin缺失/null PositionTicks仍处理会话开始、暂停及结束，保留单元恢复/已看/次数，未知位置打断观看时长采样；binding/source/Library/ACL守卫保留。6项公开回归先4失败、后全通过。Store release汇总1020 passed/12 ignored、API2169 passed/9 ignored，均退出0；media-server两个沙箱端口失败经授权定向3/3通过，其余目标及doc通过。fmt、release全target clippy及尺寸门通过；提交推送后的独立review无发现。统计为结果组求和，非去重数，不是再次执行workspace全套，也不替代新的真实客户端/页面回归。
- `4ae4a85efb52` 的新隔离轮 `57540-e64827` 已完成：491个来源probe全部成功，公开HTTP null Playing后可保存1300000000 ticks，null Stop结束会话并保留进度。Movie卡片及图片就绪各20样本：idle p95 177ms、Scan/Scrape均176ms；Scrape期间Unidentified20次均200，但未复现旧轮文件系统容量饱和条件。Probe首次逐样本新建任务的第2例因排队超过60秒未观察到running，失败保留；改用同一真实39单元任务覆盖20导航后p95 176ms，每例任务前后均running，任务最终39/39成功。后台排空后正常退出、无强制终止，原app/sidecar和pan/STRM清单均匹配，私有副本已删除。该结果只覆盖所列页面与协议控制；VidHub实际播放记录仍因锁屏未复验，旧轮清理及来源异常不追认。
- §3 除本记录具体指出的测量与文档修正外，索引合并、checkpoint、reader identity 缓存、offset 替换和逐项死代码等可选优化未在本轮统一测量或实施，保留后续评估；不将整节标为已修复。

---

2026-11-04晚间续验使用冻结候选`61ee92078a4b`。新的独立491来源probe自然全部成功；Web实播《四喜猫》S01E01、seek并正常退出，服务端位置及播放日志终点200531ms，签发SID关闭。当前Native真实登录/播放及新进度回读仍待完成；旧测试连接的缓存凭据401已与当前随机凭据的setup-only登录200区分，不能将登录准备失败当作本轮播放上报阴性。Header/small容量复验退出0、前后oracle及原fixture metadata一致；完整fullB在VerifyReverse第949718步观察到2.642秒写事务后被中断，未达终态，独立克隆已删除。SQL正文、COMMIT及后续SQLite工作尚未有实测归因，保留失败/未完成边界。最终perfgate及剩余页面仍待验，详见最终验收清单与Jellyfin合同。

## 基线结论

以下结论描述 `a28249a3`，不代表修复后的分支状态。

**基线暂不可合并。** 阻塞项 4 个（B1–B4），全部修完即可合并；"建议修复"可在合并后紧随处理。

| # | 阻塞项 | 一句话 |
|---|---|---|
| B1 | 播放历史可见性过滤 O(L×V) | 相对 main 的性能回退，可见 Media 数千即秒级、数万即数十秒，占读池 |
| B2 | 观看统计同一问题 ×≈13 条查询 | 单请求可达数十秒 |
| B3 | `with_plan_write` 持 App writer 贯穿 Library 发布事务、并在 writer 下读 | 直接违反"读不拿写锁 / 写锁粒度最小 / 读写互不阻塞" |
| B4 | ADR-0008 及其引用的锁复审文档描述的是已删除的架构 | 本分支核心 ADR 失真，AGENTS 规定"本地文档优先"，会误导后续实现 |

总体评价：存储重构主干是对的——每库单 writer + `try_lock_for` 有界准入、`ReaderPool` query_only + deferred 快照读不碰 writer、fs/网络 IO 已分阶段移出临界区（observe → 短写 + CAS）、删除与 retention 分批、DB 布局只校验不迁移、无旧路由别名与 serde 兼容字段。问题集中在少数回退查询、一处跨库 writer 嵌套、jobs 锁、死代码与文档失真。

---

## 一、阻塞合并

### B1 播放历史可见性过滤 O(L×V)（性能回退）
- 位置：`crates/store/src/playback/log_scope/connection.rs:38`（page）、`:49-51`（hidden_count）
- 原因：`WITH request_visible_media AS (SELECT value AS id FROM json_each(?))` 未物化，在 `EXISTS / NOT EXISTS` 中对每条日志重扫一次 JSON 数组。main 上该处是 `CREATE TEMP TABLE request_visible_media (id TEXT PRIMARY KEY)`（`main:crates/store/src/playback/log_scope.rs:123`），本分支改写后退化。
- EQP（已复现）：
  ```
  SEARCH l USING INDEX logs_user_started (user_id=?)
  CORRELATED SCALAR SUBQUERY 2
    SCAN json_each VIRTUAL TABLE INDEX 1:
  ```
- 实测：25k logs、V=2000 可见 Media：**1535ms**；同 SQL 加 `MATERIALIZED`：**7ms**。子 agent 数据：V=20000 → 19.8s，外推 V=10 万 ≈ 100s，期间独占一个 library reader lease。
- 触发：普通用户（有可见库限制）打开 `GET /api/v1/playback/history`，库中 Media 数千以上。
- 修复（推荐顺序）：
  1. 不再传 id 列表，改为 `EXISTS (SELECT 1 FROM library_items i WHERE i.media_id = l.media_id AND i.library_id IN (<可见库>))`，走 `items_media(media_id, library_id)` 覆盖索引（实测 45.9ms），同时可删除 `api/http/playback_logs.rs:89` → `visible_scope.rs:12` → `store/ledger/connection.rs:172 media_ids_in_libraries` 的每请求 O(N) 全量 id 加载与 JSON 编码。
  2. 最小改动：CTE 改为 `AS MATERIALIZED`。
  3. 补一条回归测试：可见 Media 1 万条时请求在预算内完成（或断言 EQP 不含 `CORRELATED ... json_each`）。

### B2 观看统计同一问题，乘以约 13 条查询
- 位置：`crates/store/src/playback/watch_aggregate.rs:18-21`（VISIBLE/HIDDEN 子句）、`:84-98`（`scoped_cte`）；查询在 `watch_aggregate/connection.rs:70,132,163,208,243,278,310`；handler `api/http/playback_stats.rs:23`
- 原因：与 B1 同一未物化 `json_each` CTE；每个窗口约 7 条 × 2 窗口 + hidden_titles。另 `(:user IS NULL OR l.user_id = :user)` 使 `logs_user_started` 用不上，只能走 `logs_started` 范围扫描。
- 实测：V=20000、30 天窗口，单条 totals 6483ms（物化后 16ms），整请求估计 > 80s。
- 触发：`GET /api/v1/playback/stats/watch`。
- 修复：同 B1；并在 Rust 端按"有/无 user"拼两种 SQL，去掉 `IS NULL OR`。

### B3 跨库 writer 嵌套：App writer 被持有贯穿 Library 发布事务
- 位置：`crates/api/src/worker/transfer/plan.rs:252-278`（`with_plan_write`：`writers().with_app(|app| { 读 transfer_settings/scrape 设置; store.library_write_with_error(...) })`）
- 调用方：`crates/api/src/management/subscribes/run.rs:584`（一个事务内循环本轮全部文件 + `queue_transfer_removals`）、`worker/transfer/run.rs:78`、`worker/transfer/recorder.rs:139`
- 原因：
  - 锁序 app→library 合法，但 App writer 持有时长被放大为整个 library 发布时长；
  - 在 writer 锁下做读（配置 fence），违反 AGENTS 硬性规则 3"读永不拿 writer 锁"与"写锁粒度最小"，也违反本次审核要求的"读写锁完全独立"。
- 触发：订阅轮次 / transfer 发布大量文件期间，登录 token 签发、设置保存、用户管理、站点编辑等 app.db 写入在 `STORE_WRITE_WAIT=1s` 后返回 503。
- 修复：
  1. 用 reader 读配置并带上 revision（或内容哈希），library 事务内只做 library 侧校验；
  2. 提交后用 reader 再短读一次 app revision，若变化则把本次发布标记为需重排/补偿（发布本身是幂等的），或将 revision 镜像进 library.db 在事务内 CAS；
  3. 立规则并加测试：任何 writer 闭包内不得再获取另一个库的 writer（见第五节锁设计）。`library_deletion/cleanup.rs:16 with_reference_write`（subscribe writer 下读 `subscribed_media` 再拿 library writer）同类，一并改为 revision fence（可列为建议修复）。

### B4 ADR-0008 与锁复审文档描述的是已删除的架构
- 位置：
  - `docs/adr/0008-sqlite-system-of-record.md:3`：把 `docs/audits/2026-10-28-storage-lock-review.md` 当作现状引用；
  - `:43-49`：`StoreReaders`、`crates/api/src/store_readers.rs`、`Store::open_reader`、`store_mutate`（5s）、`LIBRARY_READERS = 2`——均已不存在；
  - 实际：`ReaderPool` app/subscribe/library/cache = 4/2/8/2（`crates/store/src/open.rs:95-98`）、`STORE_WRITE_WAIT=1s`（`crates/api/src/http/store_wait.rs:28`）、`STORE_READ_WAIT=5s`（`:21`）、无 Store Mutex。
  - `docs/audits/2026-10-28-storage-lock-review.md` 全文描述 `StoreReaders` / `store_mutate` / `Arc<Mutex<Store>>` 与"目标结构（提案）"（已实现），被 8 处当现状引用：`docs/README.md:8`、`adr/0008:3`、`adr/0012:5`、`database/library.md:3`、`deploy.md:132`、`api-contracts/self.md:143,151`、`superpowers/specs/2026-10-18*:3`。
- 原因：ADR-0008 是本分支的核心决策文档，AGENTS 规定"本地文档优先于训练数据"，错误描述会直接误导后续 agent/人类实现（例如重新引入 Store Mutex 或按 5s 写等待设计）。
- 修复：重写 ADR-0008 的"读写分离"节为现状（每库 writer + rank 顺序、ReaderPool 容量、`store_write` 1s 准入/复查、`with_read_snapshot` 顺序、jobs_readers 容量 2）；锁复审文档标注为历史快照或删除，并更新上述 8 处引用。

---

## 二、建议修复

### 2.1 数据库设计

| # | 位置 | 问题 / 触发 | 修复 |
|---|---|---|---|
| D1 | `crates/store/src/schema/v1/subscribe.sql`（`subscribe_facts`/`subscribe_wanted`/`pending_downloads`）；`crates/store/src/subscribes.rs:205-219` | 同库父子表没有外键，`delete_subscribe` 手写 `for table in [...] DELETE`；`subscribes/connection.rs:27-72 upsert_pending_row` 不校验父行，`list_all_pending_routed`（`:252-256`）不 join 父表。删除订阅与下载回调/reconcile 交错时可产生孤儿 pending 行被持续轮询；新增子表易漏删。 | 三表 `subscribe_id REFERENCES subscribes(id) ON DELETE CASCADE`（PK 已以 subscribe_id 开头，无需新索引），`delete_subscribe` 只删父行。 |
| D2 | `crates/jobs/src/store.rs:207,225,335,348,361`（`WHERE payload = ?`）；`crates/jobs/src/schema.sql` | job 身份 = payload 字符串字节相等且不带 kind；`job_defs.payload` 无索引（EQP `SCAN job_defs`）、不唯一。现有 5 个不同 kind 的 def payload 都是 `'{}'`，按 `'{}'` 调 `set_def_enabled/set_def_schedule/delete_defs_for_payload` 会误伤所有全局任务；payload 由 `subscribe_schedule.rs:11`/`deletion.rs:219`（`format!`）与 `jobs_api.rs:59`（`json!`）各自拼接，键序/空白一变即静默匹配 0 行。 | `job_defs` 加结构化身份列 `(kind, scope_key)` + `UNIQUE(kind, scope_key)`，所有定位 API 改按 `(kind, scope_key)`；payload 只作参数。 |
| D3 | `crates/store/src/library_write/orphan_media.rs:40 purge_orphan_media_in_tx` 仅在文件删除路径被调用 | 最后一个文件删除时 Media 因订阅/收藏被保留；之后删除订阅（`api/http/subscriptions/deletion.rs:415`）或移除收藏条目（`collections/connection.rs:87-90,151`）不会再清理，无文件 Media 及其 aliases/metadata/playback 行永久残留，全库无周期清理。 | 删除订阅、移除收藏后对受影响 media_id 调同一 `purge_orphan_media_in_tx`（跨 subscribe→library 两步，按锁序分开写）。 |
| D4 | `crates/store/src/collections/connection.rs:50`、`:125` | `query_row("SELECT COALESCE(MAX(sort_order),0) ...").unwrap_or(0)` 吞掉 busy/IO 错误且无日志，排序错乱；违反 AGENTS"IO 路径禁止吞错/日志不能静默"。 | 改 `?` 传播（COALESCE 已保证非 NULL）。 |
| D5 | `subscribes.created_at/updated_at`（TEXT RFC3339，写于 `subscribes/admission.rs:114-115`、`subscribes/connection.rs:347`）；`collections.created_at`、`collection_items.added_at`（`library.sql:269-272`） | 其余表均为 INTEGER 毫秒/秒；`collection_items_order(collection_id, sort_order, added_at, media_id)` 用 TEXT 秒级做 keyset 次序键。 | 统一 INTEGER 毫秒，直接改 DDL（零兼容）。 |
| D6 | `app.sql` `playback_device_credentials` PK `(user_id, device_id, token)`，FK `token → user_tokens ON DELETE CASCADE` | 无 token 前导索引，EQP 实测每删一个 user_token 都 `SCAN playback_device_credentials`；且 `user_id` 与 `user_tokens.user_id` 冗余无一致性约束。 | 加 `INDEX(token)` 或 PK 改为 `(token, device_id)` 并去掉冗余 user_id。 |
| D7 | `crates/jobs/src/media_refs.rs:9,26,54` | 注释写"index acceleration"，实际 `payload LIKE '%'||?||'%'` 对 `job_defs` 全表扫（jobs 侧靠部分索引 `jobs_live_def_idx` 只扫活跃行，可接受）。 | 修正注释；随 D2 改为结构化 media_id 列定位。 |

### 2.2 锁

| # | 位置 | 问题 / 触发 | 修复 |
|---|---|---|---|
| L1 | 写入 `crates/store/src/playback/event.rs:57-73`（`with_library_write`）；调用 `api/http/playback/heartbeat.rs:87,301`、`media_server_provider/playback.rs:102`、`session.rs:156`、`watch_state.rs:368` | 高频、延迟敏感的播放心跳与扫描发布 / transfer 发布 / 删除批 / probe 结果 / VACUUM 共用 library 单 writer，准入只等 1s；`store_wait.rs:17-20` 注释自称"扫描一批 ≈1.3s，其中约 950ms 发布"已 > 1s。触发：大库扫描或订阅发布时播放，心跳 503、进度丢失。**若实测发布单事务 > 1s 应升级为阻塞。** | 心跳改内存 latest-wins 合并缓冲 + 后台 flusher 短事务批量落库（推荐），或拆独立 playback.db；同时把后台批量写单事务控制在 50–100ms 并实测更新该注释。 |
| L2 | `crates/api/src/jobs_writer.rs:18-24` `lock()` 无界；调用点 `http/jobs.rs:105,211,270,346`（`run_to_completion` 内，占 http_work permit）、`http/subscriptions/patch_jobs.rs:9`、`http/subscriptions.rs:525`（持 `subscribe_guard` 时）、`subscription_missing.rs:69`、`job_maintenance.rs:121`、`job_loop.rs:379,459,463`、`fs_watcher.rs:496,508`、`worker.rs:98`、`subscribe_create.rs:65` | jobs.db 维护（VACUUM/backup 页拷贝不响应 interrupt，`maintenance/database.rs:26,36-44`）持锁期间，所有 jobs HTTP 无限阻塞并占满 permit，进而让其它请求 503；订阅 PATCH 在 guard 内被卡住。 | 删除 `lock()`，统一 `try_lock_for(STORE_WRITE_WAIT)`，WriteBusy→503；后台路径超时退避重试。 |
| L3 | `crates/store/src/writer_rank.rs:26`（`debug_assert!`）、`:40`（`debug_assert_eq!`） | 锁序/同库重入检查只在 debug 生效；release 下 store writer 反转表现为 1s 后 WriteBusy（根因被掩盖），jobs writer 因 `lock()` 无界会永久死锁。 | release 也检查：返回 `StoreError::LockOrder` 并 `error!`（附持有栈与目标库），成本仅一次 thread-local 比较。 |

### 2.3 性能

| # | 端点 / 位置 | SQL / EQP | 复杂度 | 修复 |
|---|---|---|---|---|
| P1 | `GET /api/v1/libraries`、`GET/PUT /libraries/{id}`：`api/http/library.rs:172`、`library_statistics.rs:33` → `store/library_stats/connection.rs:5-28` | 每库 `COUNT(*),SUM(size_bytes) FROM library_items WHERE library_id=?`（EQP `SEARCH ... items_resolution (library_id=?)`，非覆盖）+ `library_files` 计数（`files_presence`） | 每库 O(items+files)，按库 N 次；8 万 items 库约 165ms/库/请求 | 写路径维护计数（并入 `library_query_counts` 或单独统计行）；最低限度建覆盖索引 `library_items(library_id, size_bytes)`、`library_files(library_id, presence_state, publication_state, path_key)` |
| P2 | `GET /libraries/{id}/items` 未看优先：`store/library_query/items/page.rs:84 WATCH_TIER_SQL` | `USE TEMP B-TREE FOR ORDER BY` | 每页 O(n log n)，8 万 items 122ms | 在 `user_library_items` 维护 `watch_tier` 并纳入排序索引，或按 tier 三段各走索引拼接 |
| P3 | `GET /libraries/{id}/item-index`：`items/page/connection.rs:43` | 全库扫描分桶 | O(n)，120–137ms | 维护首字母分桶计数，或 `(library_id, index_key)` 覆盖索引 + GROUP BY |
| P4 | `GET /subscriptions`：`api/http/subscriptions/snapshot.rs:96-105`；`views.rs:226 cached_show_body` → `store/catalog/connection.rs:43 list_catalog_cache` | 每订阅 `get_media`/facts/pending/times/progress 4–6 次查询；缓存未命中的开放季订阅对 6 张 cache 表 UNION ALL（含 body）全扫 + 排序，再 Rust 线性 `contains` | N+1；最坏 O(S×C×body) | facts/pending/times 改 `WHERE subscribe_id IN (...)` 批量；`cached_show_body` 改 `cache_key` 范围精确查找只取一行 |
| P5 | `GET /jobs`：`crates/jobs/src/read.rs:87 collect_last` | `ROW_NUMBER() OVER (PARTITION BY def_id ...)` 扫每个 def 全部终态历史 | O(历史)，20 万行 187ms | 每 def 相关子查询 `ORDER BY finished_at DESC, run_after DESC LIMIT 1` 命中 `jobs_last_def_idx`，实测 0.44ms |
| P6 | Jellyfin `Items` 不带 ParentId（多库）：`store/library_query/protocol/page.rs` | `ROW_NUMBER()` 窗口 + 两次 `USE TEMP B-TREE FOR ORDER BY` | 每页 O(n log n)，233ms | 按库各走排序索引后 k 路归并去重 |
| P7 | Jellyfin `StartIndex` 深翻页（单库） | `INDEXED BY items_protocol_* ... LIMIT ? OFFSET ?` | O(offset)，OFFSET 6 万 70ms | 协议只给 offset：缓存 offset→keyset 锚点 |
| P8 | `GET /libraries/{id}/facets`：`members/facets.rs:152-203` | 公共过滤为空时仍 `MATERIALIZE b`（整库）再 INTERSECT | O(n log n)，71.8ms→去掉后 52.1ms | 公共过滤为空时跳过 `b` |
| P9 | items `include_total` 仅残差过滤时 | `COUNT(*)` 全扫 | O(n)，36ms | 仅首页计算或改 has_more |
| P10 | Jellyfin `SearchTerm`：`protocol/predicates.rs search_clause`、`protocol/sql.rs` work_search | 词长 <3 无 FTS 预过滤，全库 `instr(casefold(...))`，每行 NFO 相关子查询 | O(n)+每行子查询 | 短词复用 `items_search_prefix`；NFO 子查询移到分页之后 |

已确认良好：自有搜索 `GET /search/library-items`（FTS5 trigram，1.2ms）、items 标题/添加时间/评分 keyset 分页（约 0.1ms，O(log n)）、收藏按 favorited_at（0.22ms）、`GET /jobs/{id}`（O(log n)）、collect_live（0.1ms）。按 kind 过滤为残差谓词，单库单 kind 设计下无需索引。

### 2.4 零兼容 / 遗留代码（待删除）

后端：
- `crates/api/src/management/state.rs:97-98` `#[allow(dead_code)] library_root`（单根目录模型残留，构造 `:209-363`、`:288 create_dir_all`）——删字段、参数与目录创建。
- `crates/api/src/user_removal.rs:13-22` `#[serde(default = "default_version")] version`（本分支新增的缺字段兜底，从不校验）——删除或改为严格校验拒绝。
- 本分支新增且全仓零引用：`store/src/library_write/rename_intent.rs:133 begin_file_reconciliation`、`library_write/rename.rs:56 check_file_rename`、`library_query/continue_watching/home.rs:64 home_continue_query_plans`、`library_query/details/metadata.rs:23 instance_default_locale`、`marker/src/redirect.rs:104 invalidate_remote`、`domain/src/metadata_fields.rs:24 is_value`、`api/src/storage_operations.rs:177 library_operation`/`:207 write_verdict`、`store/src/metadata/export_driver.rs:160 resume_nfo_export`、`domain/src/library_storage.rs:235 season_or_one`、`api/src/user_removal.rs:54 start_removal`。
- 本分支删掉调用方后成孤儿：`api/src/playback_events.rs:37 classify_event`、`playback_events/clock.rs:18 clamp_elapsed`、`management/state.rs:548-605` browse_page 计数器整套（`#[allow(dead_code)]`）、`store/src/users.rs:43 update_user_and_password`。
- 其它 `#[allow(dead_code)]`：`api/src/http/notify.rs:64 send`、`http/playback_logs.rs:278 _err_shape`、`store/src/library_deletion/mod.rs:42 terminal_operation_predicate`、`management/state.rs:149 statistics_role_lookups`。
- `crates/api/src/http/downloaders/task_listing.rs:328`、`:236-238` 固定输出 `"sources": []`、`error_message/site_name/resolution: null` 旧前端占位字段——删除或真实填充，前端同步。
- `crates/api/src/worker.rs:98` `let _ = state.jobs.lock().delete_defs_for_payload(...)` 静默吞错——至少 `warn!`（并随 L2 改有界锁）。
- `crates/store/src/lib.rs:100-102` `pub use probe_dispatch::PENDING_DISPATCH_SQL` 把 SQL 文本放进公开 API（仅测试使用）；`Store::delete_media`（`store/src/media/mod.rs:286`）无生产调用者且不处理引用——降为 crate 内/测试钩子。

前端（`web/`）：
- `web/lib/api/subscriptions.ts:754-799` 按旧形状补默认值：后端 `subscription_depth/removal_preview.rs:68` 不返回 `hit_and_run_count`/`recycle_retention_days`/`retained_cross_season`。**用户可见 bug**：`:795` 用 `?? 0` 填充，使 `components/subscription-cancel-dialog.tsx:89` 的 `?? 7` 永不生效，弹窗提示"保留 0 天"；`season-cleanup-dialog.tsx:82,109` 依赖字段恒为空。——按真实契约收紧类型，后端补齐或删 UI。
- `web/lib/api/downloaders.ts:283-352` + `components/downloader-config-section.tsx:706-751` + `lib/downloader-limits-capabilities.ts`：`alt_speed_enabled/queue_enabled/max_active_*` 后端不返回不接受，UI 被永久关闭；注释引用 Python 时代 schema——删除。
- `web/lib/api/downloaders.ts:80-123` + `task-center-view.tsx:112,239,456`：`purpose/upgrade_keep_old/units[].replaced/content_missing/enters_library/task_count` 后端不返回，分支不可达——删除。
- `web/lib/api/playback.ts:565-610` + `components/player/video-player.tsx:3583`：`PlaybackDiagnostics/ArtifactUpload` 无后端端点，调用处写死 `null`——删除。

已确认无问题：前端约 150 处调用全部能在路由表找到，无调用已删除端点；DB 布局闸门只校验 + 拒绝（`store/src/schema/{layout,definitions}.rs`、`jobs/src/schema.rs`、`media/src/response_cache`），无迁移代码；`password.rs:35-47` 明文行 fail closed；无旧路由别名（media-server `routes/system.rs:16,18` 的 `alias` 属 Jellyfin 协议）。

### 2.5 文档

需删除：
- `docs/benchmarks/2026-10-28-storage-lock-work.md`：基于已删除的 Store Mutex（`state.store().lock()`），含本机绝对路径，已被 `crates/store/tests/storage_lock_work.rs` 取代；同步删 `docs/README.md:8`、`docs/database/library.md:3` 的链接。
- `docs/audits/2026-10-28-storage-lock-review.md`：见 B4（删除或明确标为历史快照）。

需更新：
- `docs/adr/0008-*.md`：见 B4。
- `docs/adr/0012-library-removal.md:5`：仍把锁复审当现状引用。
- `docs/database/library.md:40`、`:910`：说"最后一个文件删除时随孤儿 Media 清理 collection 成员"，与代码 `orphan_media.rs:52-55` 及 ADR-0012:17（收藏条目保留 Media）矛盾。`:52,535` 写 v4（实际 user_version=1）；`:17` 说 `LedgerRow` 已删除但 `store::LedgerRow` 仍导出并被 `http/library_admin.rs:74` 使用。
- `docs/deploy.md`：`:66,95,96,144` 及 `docs/adr/0007-*.md:51` 的环境变量 `CRAWLER_MEDIA_OBSCURA_INSTALL` 代码不读取（实际是设置项 `obscura.install`，`api/src/settings_keys.rs:35`、`runtime_browser.rs:32`）；`:132,136,140` 仍描述两条只读连接与 Store 锁；`:79` 说 compose 起 qB/TR，实际 `docker-compose.yml` 只有 crawler-media；环境变量表缺 `CRAWLER_MEDIA_SLOW_REQUEST_MS`、`FFMPEG_LOG_LEVEL`、`LEASE_*`、`ALLOW_MULTI_INSTANCE`。
- `docs/api-contracts/self.md:143,151`：item-index 说依赖共享 Store，实际 `store_read(&[Library, App])`（`library_scan.rs:408`）。
- `docs/database/subscribe.md:60`（通知接收者实为最小 admin id，`users.rs:432-440`）、`:61`（删除用户更新 jobs.db 定义未实现）、`:124`（漏 `rejected` 状态，`subscribes/connection.rs:141-148`）。
- `docs/database/app.md:66-73`：漏 finalizing 阶段与 `user_removal.result.<op>`、`user_removal.done.<uid>` 键（`user_removal.rs:148,154,159,273`）。
- `docs/README.md:8`：标签过时；索引缺 `testing/performance-slo.md`、`superpowers/plans/2026-10-29-storage-redesign-fixes.md`、`superpowers/plans/2026-11-02-storage-redesign-final-acceptance.md`。
- `docs/superpowers/plans/2026-10-29-storage-redesign-fixes.md`：117 个复选框未勾但已完成；引用 StoreReaders（:7,143,146,159,315）、`app_reader.rs`（:243）、`Arc<Mutex<Store>>`（:206）。
- `docs/superpowers/specs/2026-10-18*:3,22`：引用锁复审并把全局串行化当未解决问题。
- `docs/audits/2026-10-26-perf-baseline.md`：:35（1/144→实际 1/50）、~:77（1000 发布槽→`fixture.rs:78` 为 2400）、:82（v4→1）、§4.1（23→25 实际 28，`gate.rs:149`）、§5 引用不存在的 `objects.rs`。
- `docs/agents/domain.md:11`：指向已删除的 `2026-10-28-storage-main-rereview.md`。
- `crates/api/src/http/store_wait.rs:17-20` 注释（"扫描一批 ≈1.3s"）需按实测更新（见 L1）。

---

## 三、可选优化

数据库 / 索引：
- `library.sql:185-199` `library_items` 15 个二级索引 + 1 唯一索引，近似重复两组：`items_protocol_default` vs `items_protocol_default_explicit`、`items_search_prefix` vs `items_protocol_name_asc`——确认 `title`/`title_sort` 语义可否合并，各留一个，降低投影重建写放大。
- `library.sql:499` `fingerprint_samples.sample_lookup` 与 UNIQUE 前 4 列同集合，planner 选 autoindex——删除。
- `playback_metrics` 无 `user_id` 索引，用户删除按 user_id 删除为全表扫（低频，可接受或加索引）。
- cache.db 6 张表 `INTEGER PRIMARY KEY AUTOINCREMENT`（`crates/media/src/response_cache/mod.rs:13-71`）无需"不复用"语义，改 `INTEGER PRIMARY KEY`；`idx_*_fetched` 与 `idx_*_retention` 仅方向不同，可合并为一个；`CREATE TABLE IF NOT EXISTS` 多余。
- `library.sql:243-247` 注释称 `protocol_people_name` 已删，`:533` 又建同名索引——改注释或改名。
- 只写不读列：`library_removals.total_files`（`library.sql:398`）、jobs `retention_cursors.cycle_started_at`（`schema.sql:52`）；无用 DEFAULT：`user_tokens.created_at DEFAULT 0`、`pending_downloads.submitted_at DEFAULT 0`。
- `items/filters.rs:110` `LOWER(i.language) IN (...)` 无法用索引——写入时归一化小写。

模块边界：
- `crates/api/src/maintenance/database.rs:322,364,465` 的 `PRAGMA optimize`/`VACUUM`/`incremental_vacuum` 与 `pragma_i64(...)` 下沉到 store/jobs 维护接口。
- `open.rs:330-345 connection_pragmas()` 为诊断拿 writer 锁——改走 reader。
- `BrowsePage::Offset` 仍被支持（`items/page.rs:303`、`last_played.rs:158`、`details/episodes/connection.rs:285`）——零兼容下只保留 keyset。

锁：
- WAL checkpoint：writer 使用默认 autocheckpoint（在提交线程、writer 临界区内同步执行，叠加 `synchronous=FULL` 造成提交尾延迟尖刺）。建议 writer `wal_autocheckpoint=0`，每库后台独立连接 PASSIVE checkpointer，维护窗口 TRUNCATE；reader lease 设最长持有时间防 WAL 无法 reset。
- `crates/store/src/writers.rs:89` admission deadline 是整个 `store_write` 作用域的总预算，闭包内先算后取 writer、或依次取多个 writer 会被挤占——文档写明，或改为按次获取独立 wait。
- `library_read/pool.rs:260-280 check_identity` 每次 checkout/归还 stat 文件——缓存 identity 定时/事件失效。
- 其它小项：`library_facts`（`api/http/library/assets.rs:49`）每库调 `removal_for_library`/`root_reassignments_for_library`；facets marginal count 每值一次查询（`members/plan.rs:156`、`facets.rs:36`）可合并 GROUP BY；收藏未看优先排序 O(F log F)、`playback_favorite_position` 用 OFFSET。

前端 / 后端死代码（main 已有）：`user_agent.rs:23`、`fingerprint_job.rs:168`、`poster_fetch.rs:51,503,551`、`http_agent.rs:112`、`catalog.rs:627,679`、`probe_manager/policy.rs:37`、`obscura_manager.rs:191`、`probe_manager.rs:212`、`http/library_scan.rs:81`、`probe_manager/stages.rs:89`、`media/src/douban.rs:119`、`state.rs:408,709`、`indexer/src/browser.rs:192`、`scrape_config.rs:141`；兼容表述注释 `scrape_config.rs:1-5`（"zero-migration promise"）、`downloader/src/client.rs:25-27`、`file_stat_read.rs:31`；前端 `web/lib/api/auth.ts:15-29,50-77`（`nickname/avatar_url/capabilities` 旧会话形状）、`discover.ts:21-23` 过时注释、旧书签页 `web/app/(app)/discover/movie/{top250,high-score}/page.tsx`、旧名 `MediaLibrary`/`JobView` 与 `schemas.subscription.*` 引用；无前端/Jellyfin 调用的端点（如 `routes/admin.rs:133,137` integrity-checks/repairs、`:234,266,268`、`member.rs:25,74,78,143,147,238,245,255`、`admin.rs:34-35,59-67,89,116,120,262,263`）逐个确认是否运维 API，否则删除。

文档：`database/cache.md:41-42` 未提 `definitions::validate`；`audits/2026-10-26-library-schema-audit.md:85,352,371` 引用不存在的 `objects.rs`；`adr/0009` 未写 jobs_readers（容量 2）与 `concurrency_key`/`timeout_secs`；`testing/system-regression-cases.md:251` 行号漂移；`agents/domain.md:23` 格式。

---

## 四、锁架构现状与目标设计

现状（已核实）：

```
HTTP handler (tokio)
 ├─ store_read ── spawn_blocking + 5s ──► ReaderPool app4 | sub2 | lib8 | cache2
 │                                        (query_only, deferred BEGIN 快照, 不碰 writer)
 ├─ jobs 读 ──► jobs_readers ReaderPool(2, 只读) ──► jobs::QueueRead
 └─ store_write ── http_work permit ── admission(deadline=now+1s, 实例可写, WriteGate)
       └─ Writers(Arc, 每库一把)  rank: app → subscribe → library → cache → jobs  [仅 debug_assert ← L3]
            app.db / subscribe.db / library.db / cache.db : Mutex<Conn> + try_lock_for → BEGIN IMMEDIATE
            library.db 同时承载 扫描发布/transfer/删除批/probe/播放心跳/VACUUM  ← L1
            jobs.db : JobsWriter Mutex<Queue>  lock() 无界 ← L2
 应用级：subscribe_guard(每订阅) → jobs.lock()  ← L2
         with_plan_write: APP writer ─持有贯穿─► LIBRARY 写事务  ← B3
         with_reference_write: SUBSCRIBE writer ─► LIBRARY writer（writer 下读 fence）
 SQLite：WAL, synchronous FULL(cache NORMAL), busy_timeout 5000(维护 250), journal_size_limit 64MiB,
         默认 autocheckpoint 在提交路径
```

做得好的部分：读写分离已成立（读不触 writer Mutex、只读连接 query_only）；fs/网络在锁外分阶段执行（`pending_file_deletions.rs:58/85/124-154`、`metadata/export_driver.rs:221-258`、`metadata/nfo_input/reconcile.rs:35-67`、`roots.rs:44`、`scan_seen.rs:153,173`、`library_write/rename_intent.rs:42`；订阅轮次的 downloader add 与 ffprobe 在 `subscribe_guard` 外）；删除每批 100、retention 每批 500 且有游标；`jobs::Queue::tick` 执行 job 时释放锁；HTTP 访问 store 经 `spawn_blocking` + 预算，`try_acquire` 失败即 503 不排队；VACUUM 期间维护会关闭 WriteGate（`maintenance/runner.rs:260`）。

目标设计（在现有骨架上收紧，不推翻）：
1. **单写者不变量**：每库一把 writer，统一 `try_lock_for`，删除一切无界 `lock()`（L2）。
2. **禁止跨库 writer 嵌套**：writer 闭包内不得获取其它库 writer；跨库一致性一律"reader 读 revision → 本库写事务内 CAS / 提交后复核 + 幂等重排"（B3、`with_reference_write`）。rank 检查在 release 下也生效，违规即报错（L3）。
3. **writer 下不读**：fence 读走 reader；writer 闭包内只允许本次写所需的 SQL。
4. **写事务时长预算**：后台批量写（扫描/发布/删除/retention/probe）按 50–100ms 切 chunk，chunk 间释放 writer；记录每次持有时长，> 500ms `warn!`，便于 `logstats.py` 发现。
5. **前台热写隔离**：播放心跳内存合并 + 后台 flusher（或独立 playback.db），使前台交互写只剩低频小写，1s 准入预算充足（L1）。
6. **checkpoint 移出临界区**：writer `wal_autocheckpoint=0`，后台 PASSIVE checkpointer 用独立连接；维护窗口 TRUNCATE；reader lease 设最长持有。
7. **预算语义**：admission deadline 明确为写作用域总预算，闭包内只放 SQL。

---

## 五、建议的修复顺序

1. B1/B2（同一修复，改 EXISTS + `items_media`，删 `visible_media_ids` 链路）+ 回归测试。
2. B3 去掉 `with_plan_write` 的跨库嵌套，加"writer 闭包内不得取其它 writer"测试。
3. B4 重写 ADR-0008 读写分离节，处理锁复审文档及 8 处引用。
4. 合并后：L1（先实测发布单事务时长）、L2、L3、D1–D4、P1、P4、P5，前端 `recycle_retention_days` 用户可见 bug，其余遗留代码与文档清理。
