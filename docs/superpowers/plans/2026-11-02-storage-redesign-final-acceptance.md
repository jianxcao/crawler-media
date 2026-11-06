# Storage redesign 最终验收执行清单

## 最新状态（2026-11-06）

后续完整 COMMIT 插桩轮已完成 450 万步及双 oracle，最大已覆盖写事务约 1.728 秒定位到 WAL 同步墙钟区间，只证明 writer 至少被持有相应时长；Integrity 推进与扫描/Transfer 发布严格区分，诊断仍不能替代正式性能门。单次 busy 的 166 项 gate/业务检查已完成，后处理因混写 JSON 退出，关联审计无效、原 `complete=false` 和相对失败保留；分流修复正在验证，新实际轮尚未运行。用户要求的增量清理已完成，核验时 `/private/tmp` 约 33GiB、磁盘可用约 93GiB。详见[最终验证追补](../../audits/2026-11-06-storage-redesign-final-verification.md)。整体验收状态不变。

后续独立补测：最终产品的七行 Playback 副本连续两次正常重启、公开 marks/resume 读回和四表逐行保留通过；新隔离页面轮的本地合成章节图片无鉴权交付及首次详情卡片显示通过，服务正常退出且原清单/句柄/副本清理通过。保留历史六行基线 Fail、原 driver Unverified、页面复入懒加载未复验及真实慢源 Not run。用户授权的历史 `/tmp` 日志压缩和过期二进制/数据库清理已释放约 7.1 GiB，详见[最终验证追补](../../audits/2026-11-06-storage-redesign-final-verification.md)。性能相对门及 COMMIT 尾延迟仍未闭合，整体状态不变。

最终候选验证已完成记录，**整体验收仍未通过**：`da68921a` 的 release workspace 实际退出0（602组汇总3976 passed、0 failed、25 ignored，含嵌套汇总未去重），Web lint/typecheck/940项测试/build及相关release all-target Clippy通过。正式候选对历史准入基线的 idle/busy/cold 三轮分别164/166、161/166、160/166，均完整、业务和绝对预算通过，但原相对门失败；不是新配对六阶段通过，也未归因为代理。最终二进制有限桌面/390px页面、分集切换、空库/404恢复及四类静态图检查完成，31次图片请求无Authorization；该轮章节帧成功与真实慢源页面为Not run，后续章节图片补测见上。每轮来源清单一致，自有服务/句柄/副本均清理。详见[最终验证记录](../../audits/2026-11-06-storage-redesign-final-verification.md)。历史COMMIT/有限内存/严格Playback及缺失引用的限制保留。

图片截止修复及分段诊断已完成：后加入者保持自己的两秒预算，初始排队截止、名额及worker所有权不变；公开Router覆盖排队共享超时、个人截止区别及缓存完成后的离线恢复。图片单元26/HTTP集成19、release all-target Clippy/fmt与Web940项通过，采集和清理补项的原失败保留；详见[专项记录](../../audits/2026-11-06-image-delivery-deadlines.md)。历史504具体归属仍不能一概而论，最终workspace、正式性能及实际页面验收另记。

有界COMMIT诊断完成：2f5cd923的诊断/无插桩控制各224,978步、同状态、退出0；十万次Users COMMIT PROFILE未出现≥100ms样本。源码摘要、原夹具元数据与副本清理通过；此为acceptance_eligible=false的部分观察，未复现或解释历史2.735秒风险，不替代完整容量终态。见[专项记录](../../audits/2026-11-06-commit-tail-observation.md)。继续图片504回归及最终受影响验收。

收藏分段诊断已完成，正式门仍待最终复测：e7普通/临时诊断构建各200次HTTP200，p95为1.250875/1.179750ms，未复现原3.509875ms。本次3.8ms尾部主要为Library writer等待，不能追溯归因历史失败或识别竞争操作。原166项顺序保留，诊断软限时导致部分前序样本不足，gate均退出1，不追认为正式通过；业务、逐请求关联、来源及清理通过，没有性能产品改动。证据与限制见[专项记录](../../audits/2026-11-06-favorite-writer-contention.md)。继续有界COMMIT尾延迟观察，再处理图片504与最终受影响验收。

用户确认的收尾顺序为 TMDB 有限重试 → 收藏 busy 性能 → COMMIT 尾延迟 → 图片504定位 → 最终受影响验收。第1项已按TDD实现：最多三次、上游六秒含刷新/名额等待与正文、继承调用方取消/截止、429 Retry-After和抖动、永久错误停止、共享 origin 超时与个人取消区分；stale、typed 404及缓存发布错误保留。media全套94通过，API单元与六组相关集成合计352通过，release all-target Clippy及独立review通过。细节见[TMDB 请求回归](../../testing/tmdb-retry-regressions.md)。同步网络尝试受剩余超时约束，不能立即强制取消；本项不追认任何旧性能/页面失败，也未重跑491、容量和内存。下一项定位收藏原相对门失败。

用户于2026-11-06明确允许TMDB网络问题先暂缓：已确认的外网请求超时不再反复阻塞本轮本地修复，原失败与Not run仍如实保留，原SLO和基线准入规则不变。代理根因尚未确认；图片代理504的内部等待仍待排查，不能一并排除。当前优先定位收藏写入波动和2.735秒COMMIT尖峰，此项暂缓不代表整体验收通过。

后续[单次完整busy对照](/private/tmp/crawler-matched-final-hint100-post-v4/favorite-full-busy-replay-20261106T031247Z/campaign.json)在旧版完整166项后停止：162通过、stage退出1、业务verified，但人物图片504、人物详情502、poster504及Similar不足样本/502使基线无效。协调器及quiet wrapper实际退出2，新版**Not run**，没有继续重复对照。旧版收藏本次200次HTTP200、p95 3.436208ms；虽然接近此前新版3.509875ms，整轮无效基线不能支持版本比较或根因结论，原新busy相对Fail保留。

本次TMDB推荐日志为整体请求超时、人物为连接超时，客户端域名是`api.themoviedb.org`；图片日志仅能定位到交付截止超时。事后对fixture的`image.tmdb.org`头像地址做3次独立curl请求，均200、0.242–0.356秒，正文未验证；不能据此确认代理根因或图片504的具体来源。来源及清理通过、23份绑定输入未变，产品和原门槛不变。证据为`/tmp/crawler-favorite-followup-20261106-summary.json`及`/tmp/crawler-tmdb-image-recheck-20261106.json`。以下为此前各轮实际结果，保留历史失败与Not run。

用户要求再次验证疑似代理波动后，[独立idle/busy复测](/private/tmp/crawler-matched-final-hint100-post-v4/campaign-20261106T020030Z/campaign.json)已完成：旧idle164/166、新idle166/166、旧busy165/166、新busy165/166，各阶段完整。两份旧基线HTTP/业务有效且严格准入verified，旧版绝对延迟失败保留；新版busy唯一失败是收藏写入p95由0.951917增至3.509875ms（+268.716%），200次请求全200，绝对预算通过但原相对门失败。故本轮`all_candidate_passed=false`，匹配性能和整体验收仍未通过，不把失败归为已确认代码缺陷或测量抖动。

本轮推荐和详情各阶段均取得200个有效完整结果，无原EOF/502；新idle两项完整p95为856.394/876.744ms（−1.501%/+9.095%），新busy为949.996/859.837ms（+6.665%/+8.163%），均过原门。busy旧版额外3/2次窗口外成功尝试保留；推荐观察全部为空，不推广为非空推荐性能。四阶段业务、来源及清理通过，产品97/工具159/fixture1c3与原门槛不变，没有新增请求重试或覆盖旧失败。详见[审核记录](../../audits/2026-11-02-storage-redesign-review.md)及`/tmp/crawler-tmdb-network-repeat-20261106-summary.json`。

此前两个失败TMDB路径另各做3次curl诊断，6次均完成TLS并返回HTTP404，请求使用合成ID、正文未验证；使用IPv4、无显式应用代理、系统路由未改，curl不等于产品ureq链路。当前可达不足以定位此前EOF或确认代理根因，冻结app hash未变，凭据未输出。证据为`/tmp/crawler-tmdb-failed-path-recheck-20261106.json`。

后续[收藏聚焦诊断](/tmp/crawler-favorite-focused-campaign-20261106T025249Z/campaign.json)完成旧→新→旧→新四轮busy测量，仅运行原收藏写入用例，每轮200次HTTP200；p95依次为1.287375、1.359042、1.204958、1.319708ms。四轮退出0、来源及清理通过，没有复现完整busy中的3.509875ms。两个新版诊断仍绑定原完整旧busy基线；本诊断标记`diagnostic_only=true`、`acceptance_eligible=false`，不能替代完整166项验收。由于前序请求和扫描时机不同，根因仍未确认，原正式失败保留。

以下保留本轮之前的匹配结果。被测产品`97cc6b1b`、测量工具`159934b2`、冻结fixture来源`1c3af023`分别绑定，没有重seed或改写原baseline、binding及历史报告。新包准入退出0：30项绑定、154 GET/166 cases/10 exemptions、两版各50用户400项业务断言verified及freeze匹配；准入不替代实际性能。

[旧idle轮](/private/tmp/crawler-matched-final-hint100-post-v4/campaign-20261106T011849Z/campaign.json)完整166项、164通过：Similar超预算，page-detail有199次200和1次502，基线无效，新idle Not run。[独立旧busy轮](/private/tmp/crawler-matched-final-hint100-post-v4/campaign-20261106T013035Z/campaign.json)完整166项、165通过：Similar仅80个窗口内成功样本、全部尝试80次200和1次502，基线无效，新busy Not run。两次502对应TMDB推荐请求意外EOF且无有效stale缓存；更深连接原因未知。没有请求重试、删样本或追认通过。

[独立cold轮](/private/tmp/crawler-matched-final-hint100-post-v4/campaign-20261106T013903Z/campaign.json)旧165/166、退出1，仅Similar绝对预算失败，HTTP/业务有效、严格基线准入verified；新版166/166、退出0。新cold Similar/page-detail初始响应为1.322/6.480ms，完整结果775.980/794.663ms，相对完整结果变化−7.791%/+2.506%，均过原门。每用例仅1个冷样本，推荐均为空，不能推广成idle/busy或非空推荐性能。字幕旧idle/busy各200次200，旧/新cold各1次200，POST/source准备实际有效。四个stage的业务、来源及清理均通过，新版启动身份及报告commit为97；各独立轮不拼成单次六阶段通过。完整结果与原始回执索引见[审核记录](../../audits/2026-11-02-storage-redesign-review.md)及`/tmp/cm-final-hint100-post-v4-results-summary.json`。

字幕工具`0f1296f6`按PlaybackInfo匹配所需source，`159934b2`将准备改为两版共同支持的POST，均已独立提交推送。旧工具GET准备的实际busy轮保留405和零字幕样本，page-detail该轮200样本有效；不能追认为整轮通过。POST修复有有效RED、根组合128项GREEN、精确路径33项GREEN和独立复审。

产品`97cc6b1b`仅将Similar的JSON重取提示250ms改为100ms，HTTP 202、`Retry-After: 1`及原资源上限不变；公开Router先RED后9/9 GREEN，API all-target Clippy、fmt、Web轮询8/8及限定独立复审通过，二进制已构建冻结。八个Movie的[诊断](/tmp/cm-similar-poll-diagnostic-20261105T182122Z-61020/diagnostic-lifecycle.json)两轮均8/8 ready且结果全为空；250/100ms的完整响应p95为1042.364/876.098ms，p50为785.990/866.141ms，请求35/69次。网络未控制、顺序固定且TMDB来源唯一性未核实；8样本p95即最大值，不能替代正式gate或证明非空推荐性能。

历史产品`1c3af023`、工具`433`的[v2 campaign](/private/tmp/crawler-matched-final-contract-aware-v2/campaign-20261105T173705Z/campaign.json)旧idle164/166、新idle163/166、旧busy164/166；旧busy的page-detail仅12次200和1次503、样本不足，基线无效，新busy未执行。[独立cold轮](/private/tmp/crawler-matched-final-contract-aware-v2/campaign-20261105T180633Z/campaign.json)两版均165/166。五阶段退出1、业务verified、source及清理正常，新idle Similar/page-detail相对失败及字幕404保留。严格六行Playback仍Fail，原driver退出Unverified；2.735秒COMMIT尾延迟、有限内存窗口及缺失10-29外部报告仍未闭合。详细修复链和原始证据见[审核记录](../../audits/2026-11-02-storage-redesign-review.md)。

状态：已进入最终验收；尚未验收通过，不执行用户实例切换、合并或发布。

进展按事件先后记录；较早条目的“正在”“尚未”等状态以后续记录为准。

2026-11-05真实491来源轮：`af40b58a`的Movie和Episode完成Web→VidHub→Web真实播放、停止持久化及续播，同库正常重启后四张Playback表逐行一致；目录身份、隐藏库、分类、季切换、详情及跨端标记有实际证据。`f21ff307`、`b47290d1`修复Native前缀过滤和Episodes截断；`cd80d58e`有限Native复验已确认管理员及成员无结果、管理员163集身份/坐标精确匹配、实际UI分页边界及两项保存位置详情。独立观察确认恢复停止副本的核心事实、catalog及491条file_details与该阶段基线一致，491 probes成功；没有重扫。10条章节缓存NULL→`[]`、Native首次位置微差仍保留。

该有限复验的严格六行Playback基线为**Fail**：未知操作者的真实管理员VidHub《功夫女足》0→2122ms增加第七条unit/log/关闭SID，终态7/7/7/0；原六行及Movie 218284ms、Episode 205907ms仍在，不归因、不重置。原driver因工具宿主中断，退出Unverified；独立受控TERM恢复退出0，原app/sidecar、STRM/pan清单一致，端口和句柄关闭。7个测试VidHub连接已删，既有Emby保留，Reqable抓包已停。独立观察与原始回执见`/tmp/crawler-real491-cd80-bounded-service-report.md`。

2026-11-05阶段候选`1c3af0236c14a13753884f6164f9962a34edb5fa`的SQLite初始化修复和NFO人物头像修复均已提交推送。cd80稳定workspace原退出101，20个失败目标因loopback EPERM授权定向核对为1096 passed、0 failed、2 ignored，初始化相关7项通过；原101不可改称单轮全绿。NFO受影响214项、all-target Clippy/fmt及限定复审通过，详见[SQLite](../../audits/2026-11-05-sqlite-startup-race.md)和[NFO](../../audits/2026-11-05-nfo-person-thumbnails.md)专项记录。新二进制清单为`/private/tmp/crawler-storage-candidate-1c3af0236c14/manifest.json`，验证链为cd80原workspace加失败目标核对，再加NFO受影响门，不是新候选全workspace。

匹配perfgate工具已完成早期准入的6个无效基线RED及2项新增、22项runner、6项audit GREEN，绑定包30项和Ruby/shell/AST检查通过。旧版首次独立seed在8用户并发准备Playback时出现43次503并退出1；原清单一致且正常清理，失败保留。共享任务槽争用是待核实解释，原脚本未保留503正文，不能写成已确认根因。两版随后采用仅准备阶段`max_workers`从8改1的衍生脚本，50用户、每人300 unit/400 log等工作量不变，无重试或测量修改；旧、新serial seed分别退出0、194秒与202秒，原清单均匹配且无listener/owner/live（`/tmp/cm-final-old-serialprep-seed-stage.json`、`/tmp/cm-final-candidate-serialprep-seed-stage.json`）。原binding的generator hash不代表衍生脚本，衍生freeze为`3f77994f8a26065e6dcc191a2d4a2653144897cb652762a5f718e8a9095a6f77`。

seed后发现WAL sidecar及3302个work缺少studio，不能放宽oracle。旧版首次category enrichment以`mode=ro`打开无sidecar的WAL格式library失败退出1，服务未启动、NFO未改，原清单匹配且无owner/listener；保留live并移入该audit的`failed-live-retained`。诊断`/tmp/cm-final-category-sqlite-open-diagnosis.json`和helper 3项RED/GREEN记录了只在无WAL内容且无journal时用immutable、非空WAL继续只读的边界，后续结果见下方。历史73da freeze中的仓库`perfgate.py`摘要被`8473955d`替代，保留不匹配；9087、audit及新freeze匹配。容量和内存沿用已有结果；2.735秒COMMIT尾延迟、有限观测窗口和缺失10-29外部报告仍为Unverified。各次实际运行保留自己的构建身份：

后续两版enrichment分别退出0（旧84秒、新91秒）；两边各3302个NFO及3次scan的关键Playback、身份、文件和覆盖事实相同，原清单匹配且无owner/listener/live。`checkpoint-v3`退出0，双根五库schema/table/rowid前后一致、integrity通过、外键错误0且sidecar不存在，原baseline保留。四份semantic/seed核对退出0，50用户的300 target、400 logs、100 favorites、5个各50成员的collections及精确身份均已核对；仅随机TV文件旧E16、新E51不同，衍生dataset固定S1E1。jobs审计曾查错表而显示false，公开GET jobs/id使用`job_defs`，两边RSS定义均存在，不是产品缺陷。

首次stage oracle复制后ROOT错指临时目录，调用已改用字节相同的clean checkout副本，原冻结工具未改；旧/新app/library四种schema GREEN并经独立审查。finalizer v3虽退出0并生成材料，两边各400项业务断言全部Unverified：oracle错误拒绝合法local/merge-survivor provisional；材料和freeze保留且未准入测量（`/tmp/cm-final-initial-business-unverified-receipt.json`）。仅oracle及CLI测试的修正经双schema RED和27项GREEN，明确confirmed/provisional可接受，merging/retired/unknown仍为unknown，缺失Media外键拒绝；父任务正在重跑oracle与业务门并独立复审，拟独立提交工具并将新包绑定产品`1c3af023`。未修改Rust/Web/DDL，未重做seed/build；六阶段性能仍Not run。

后续工具修复`c55cd325`、`485b524e`已分别提交推送，根任务相关61/62项测试退出0并各经独立复审。新包`/private/tmp/crawler-matched-final-provisional-oracle`绑定30项测试退出0，v4原始oracle两版各50用户、400项断言全部verified（`/tmp/cm-final-corrected-business-admission.json`）；产品Rust/Web/DDL仍为`1c3af023`，工具单独记`485b524e`。

首次实际匹配campaign `campaign-20261105T170045Z`在旧idle主动中止：旧ce3 Similar的HTTP 200 `data`为裸列表，新工具只认可pending/ready合同。旧报告保留`complete=false`、162 cases、161 Pass/1 Similar Fail；Similar 200次均HTTP 200，p95 950.702833ms，实际也超过150ms绝对预算，不得全部归因解析器或改为Pass。根任务仅向自有perfgate子进程发送SIGINT，wrapper正常TERM停副本，`/private/tmp/crawler-perfgate-matched-old-ce3f189e5d3d/full-matched-audit-20261106-17802-1fop497/receipt.json`记录stage_exit 254、source match、无listener/owner/live；外层campaign退出1。此轮为Partial/中止，不称完整idle，其余五阶段Not run。旧版显式legacy-list合同正按TDD准备，当前版stateful合同仍严格，不修改SLO或20%规则；新包准备中，产品`1c3af023`不重建、不重做seed。

后续性能工具修复`920`、`c8d`、`433`分别独立提交，根任务对应106、32、108项测试通过并分别经独立复审。旧contract包的coverage准入因三个视频流别名未覆盖而失败，该结果保留。v2新包绑定30项测试通过，真实路由清单为154个GET、166个case和10个exemption；旧、新各50用户的400项业务断言均verified。产品身份仍为`1c3af023`，工具身份单独记`433`。新六阶段campaign为`/private/tmp/crawler-matched-final-contract-aware-v2/campaign-20261105T173705Z/campaign.json`；截至启动时旧idle为RUNNING，其余五阶段NOTRUN，尚无完整性能结论。

`af40b58a`已单独提交推送，关闭下述9087宽面/定向复审的五类协议缺陷。公开Router均有有效RED→GREEN，实际Range/字幕字节与来源、SID、设备/用户/单元、隐藏库及配置revision的正反例已补齐；字幕私有模块拆分和typed请求通过原审查者复核。最终稳定树release workspace含doc-tests必要重测实际退出0：596结果组汇总3936 passed、0 failed、25 ignored（含子进程汇总、非去重数），1977项代码hash稳定并与提交一致；release all-target Clippy、fmt、尺寸门通过，提交后限定复审无可操作发现。初轮workspace仍保留3930 passed、6 failed、25 ignored及退出101；六项fixture修正为授权有效时创建并关闭真实历史，再撤销Library访问，原查询/断言不变。候选二进制57561936字节、SHA256为`82b8051a16975c381107ff38c96b982d3c2aa6bd1d7b5697f489102630b5f813`，UI未变；新候选491联合页面/真实来源与最终匹配性能尚待实际验证，不能据自动化通过宣布最终验收完成。

`9087d4c5`已单独提交推送，修复首选来源变化后仍有效的旧来源会话被忽略、Range返回404的问题。两条公开Router回归先RED，修复后未指定来源的事件/取流按该用户、设备及SID的来源绑定解析，旧revision、冲突来源和失效授权仍拒绝。定向矩阵89项Jellyfin、6项取流绑定、14项事件身份、12项播放读取通过；新增公开负例覆盖未知/关闭SID、错误设备/用户/Media、签发后隐藏Library，以及同一物理文件覆盖两集时跨集SID不混用。独立审查无新增必须修复项，release Clippy/fmt通过。最终稳定树release workspace含doc-tests退出0：596个结果组汇总3925 passed、0 failed、25 ignored（含子进程汇总、非去重数）；代码哈希与提交一致。Work/Season聚合入口的同类来源切换尚未直接实测，不扩大单集验证范围。

049候选的最新491轮已自然完成全部491探测且0失败，但准备阶段因漏处理两个空默认Library而未通过成员可见库精确断言，未到正式GUI S0/ready。前置页面已观察admin的12 Movie/18 TV身份与member隐藏TV的404，标记/播放表均空；正常停服并保留完整五库冷快照，原app/sidecar及STRM/pan清单一致。此为fixture配置失败，保留原结果。9087将在同隔离轮显式切换生产二进制、修正fixture后另做GUI验证；不恢复DB、不重扫/重探测，probe版本仍记049，不将其重标为9087。

9087显式切换第一次启动、fixture修正和17表事实核对通过，完整S0冻结成功；第二次启动后严格事实比较失败，未发ready、未做正式GUI。诊断在完整一次性副本上确认：17表中仅library_metadata_inputs的27行与library_units的180行发生revision变化，属于相同11个TV Media；原已排队的全局Scrape在第二次启动时运行，另有RSS生成后继排队行，没有新增probe。sample全树、原始app/sidecar及STRM/pan清单均一致，自有服务正常停止。保留失败与首次完整S0；后续需明示新独立阶段、整套恢复S0并在启动前暂停该副本无关的定时Scrape/RSS，不能忽略事实差异或把旧失败追认为通过。

9087宽面复审的四项缺陷均已通过真实provider/Store的公开Router失败回归确认：PlaybackInfo来源/SID竞态、缺DeviceId取流、字幕来源漂移、未知ParentId扩大范围；渲染后Library授权变化亦有失败回归。统一修复已有聚焦GREEN，尚未完成最终回归。定向复审新增旧SID未复查Library配置/生命周期，先复现字幕200而非404，再修common guard并验证旧SID字幕/视频拒绝、无SID合法字幕可读；fresh SID及其余负例、三crate和稳定workspace继续执行。首轮API lib在四个回环监听测试因PermissionDenied退出101，284通过、4失败，后续integration未运行；保留环境失败并另记后续结果。9087匹配性能只完成静态准备，未seed/实际测量，待新候选另绑。

最新复审新增的事件来源绑定P2已由`049389ac`修复并单独提交推送：事件忽略明确MediaSourceId，可能将旧来源或非首选来源的进度写错。公开Router先复现来源/会话冲突及隐藏来源、跨Media来源错误写入，再实施严格来源选择与绑定核验；来源8项GREEN，相关矩阵、release Clippy/fmt/尺寸和独立审查通过。稳定工作树最终release workspace含doc-tests退出0，596个结果组汇总3919 passed、0 failed、25 ignored（含子进程汇总、非去重数），测试代码哈希与提交一致。首轮两项本机监听EPERM与授权定向重跑分别保留。隐藏条目详情/PlaybackInfo及两类POST Delete授权负例首次通过，只计覆盖补强。新候选实际491页面与最终性能尚待执行，不能把下述73da实播改记为049。

`73da134e`已将详情媒体源修复单独提交推送。公开Movie/Episode缺字段与Season字幕错绑均先RED后GREEN；Jellyfin76、绑定5、media-server去重40、相关release Clippy/fmt及独立复审通过。首轮loopback权限失败保留。稳定候选最终release workspace（含doc-tests）已退出0：596个结果组汇总3911 passed、0 failed、25 ignored，含子进程汇总、非去重数；正式候选实播另记，不将忽略项计作通过。

正式候选四源新轮已验证Native真实播放与停止持久化：本地Movie首停75723ms、重入再停75068ms，HTTP STRM Movie75262ms，本地E01为85895ms、HTTP STRM E02为82395ms；实际Sessions均204、SID关闭、其他单元及另一成员不变。同单元双向链已闭合：Movie的Native75068→Web144457→Native169317→Web183088ms；E02的Native82395→Web114586→Native160854→Web177745ms，每次新日志起点精确承接前次停止。正常同DB重启后四张Playback表逐行一致，无restore；重启后Web183088→201197ms，Native起点201197→停止194895ms。成员B实际从0播放至82792ms，A四单元完全不变，切回A详情仍显示其独立位置。终态5units/14logs/14closed SID/0active，正常停止、原app/sidecar及STRM/pan清单一致，无强杀。Native首帧可能早于请求位置，E02暂停烧录166.667秒与客户端报告160.854秒不一致，不称帧精确续播；Native直取HTTP origin的Range未捕获。491真实来源完整页面和性能仍分别验收。

2026-11-05新进展：仅给独立S0的File详情补原PlaybackInfo的MediaSources，即恢复VidHub真实报告（Playing1/Progress4/Stopped1均204）并保存75808ms，详情显示01:16；typed条目ID、f1源、会话和认证原样。已定位该场景的详情媒体源初始化缺失，生产TDD修复已提交推送，继续扩展真实来源/双端复验；不能将临时桥成功当作正式候选实播通过。

| 范围 | 已有证据 | 剩余工作 |
|---|---|---|
| Web | af40真实491来源Movie/Episode与VidHub双向续播、同库重启和页面/身份检查已有实际证据 | 保留实际范围及差异，不将历史候选结果重标为1c3 |
| VidHub/Jellyfin | af40真实播放及目录/标记证据；cd80有限前缀、163集及保存位置复验完成，测试连接已清理 | 严格六行Playback基线Fail及原driver退出Unverified保留，不能据此称整轮通过 |
| 性能 | v2及原EOF轮失败保留；97/159独立cold166/166；网络复测新idle166/166、新busy165/166；后续聚焦四轮未复现，单次完整旧busy基线无效、新版Not run | 收藏相对失败及网络/图片超时根因仍待定位；保留空推荐、各独立轮及无效基线边界 |
| 容量 | 100k Media、1M文件/Playback及查询工作量断言已执行；61ee的Header/small及插桩fullB终态、双oracle完成 | 保留2.735秒提交尾延迟风险；插桩诊断不能代替最终性能判定 |
| 内存 | 6批Scan、3批Transfer、独立heap轮及主要分配来源分析 | 保留对象归属与观测窗口边界，不宣称长期无泄漏 |
| 交付 | 已完成修复均独立提交推送 | 后续发现修复、最终证据对账/复审及文档提交推送 |

2026-11-04晚间新轮使用冻结的`61ee92078a4b`，实际health确认该版本且可写；独立491来源的probe自然全部成功。Web实际播放《四喜猫》S01E01，seek、暂停并正常退出，持久值为200531ms，签发SID关闭，日志终点同为200531ms。该值来自服务端回读，非按键或墙钟推算；Native透明对照及后续身份诊断仍待实际操作和回读，不能据此宣布双端同步通过。前轮bd3及其B/C阴性结果保留各自版本和233489ms停止态，不能混入本轮结果。

随后新建官方Jellyfin 12.4 Docker与独立产品数据库，使用相同600秒视频的File及HTTP STRM，经Reqable MCP取得真实VidHub完整请求/响应。官方两类来源均出画、seek并正常退出，Playing/Progress/Stopped均204，报告及读回分别为1431165307/3535199618ticks。产品File出画、seek并正常退出，登录/PlaybackInfo200、视频206，但没有Sessions报告，单元/日志仍空且SID pending，保留Fail；产品STRM点击时Mac锁定、无新PlaybackInfo，播放未确认。完整字段差异及未验证边界见[客户端合同](../../api-contracts/jellyfin.md)。锁屏后服务可逆暂停，数据库、冷S0、凭据和抓包保留；原清单和源SHA一致、私有端口/句柄清空。官方origin的docker stop退出137（非OOM）单独保留，其余服务正常退出，不能称本轮全部正常退出。随后单File GUID实播已完成：实际详情/PlaybackInfo/stream均消费alias并成功回译，Auth/PI和保护字段原样；首帧0.167秒、快进后60.500秒、暂停70.333秒，正常退出仍无Sessions、SID pending、单位/日志为空。该双字段变化不足以恢复上报，不支持生产ID改动；详情缺少MediaSources与source格式的竞争解释继续验证。

61ee容量复验中，Header与small均退出0，前后oracle通过、原fixture文件metadata一致、独立APFS克隆删除。先前完整fullB在VerifyReverse第949718步观察到约2.642秒的已覆盖写事务后中断，未达到终态；该轮结果独立保留。新增`/tmp`事务profile工具的4项测试（含公开Store集成及替换tracer负例）、release Clippy和格式检查均通过。其后独占双槽的完整插桩轮退出0：4,500,001次推进、操作1719秒、前后100k Media/1M文件/1M Playback及外键oracle通过；覆盖样本4,500,002、缺失0，临时克隆删除、原fixture metadata未变，最低采样空闲98,304,000,000字节。最大覆盖事务为2,735,212,417ns，位于`Users→Users`第4,080,709步；对应整个advance的PROFILE为1 BEGIN/14正文/1 COMMIT，计时0/0/2,735,000,000ns。PROFILE是advance语句聚合，SQLite时钟为毫秒粒度，COMMIT PROFILE不覆盖其后的WAL回调；不能直接证明checkpoint或fsync原因。本轮完成数据一致性诊断，但提交尾延迟风险保留，不称strict SLO通过。历史完整轮和FTS修复的工作量证据保持原结论。

`32591af5`补齐季列表UserData的Key/ItemId。公开回归先复现ItemId为null，再通过Jellyfin集成74项、media-server全套40项、相关release Clippy及fmt，独立审查无阻断项，已单独提交推送。此前该轮实际诊断服务保持冻结的bd3构建；当时GUID实播因桌面工具仍返回Mac锁定而未运行，不把Season合同修复与Native进度故障建立未经实测的因果关系。

`61ee9207`补充Movie与Episode的真实登录连续HTTP播放链，根路径和`/emby`均实际验证签发token、目录、PlaybackInfo、Range字节、Playing/Progress/Stopped及列表/详情进度；Episode另验另一用户和另一集。新链首次通过。日志观察并发空捕获曾使全套75/76失败，最小复现后改为单测试顺序执行原两段断言，最终75/75、相关release Clippy/fmt及独立审查通过，已提交推送；不是实际客户端进度通过证据。

后续B/C独立字段诊断均为阴性：B仅补登录根ServerId，C恢复同一停止态后仅补Movie/Series/Episode条目ServerId，实际Auth保持字节一致，11个条目响应的77个目标条目差异已审计。两轮实际播放/seek/退出后无Sessions入站，Native SID pending，持久值233489ms且Web刷新03:53。C首帧HUD03:54，末HUD不可读；不能将此说成位置精确相等的播放窗口。当前根因未确认；已按用户指定只读对照movieclaw的GUID、完整登录DTO和公开请求链回归，后续诊断与生产修复另记，详见[客户端合同](../../api-contracts/jellyfin.md)。

`bd3c16f9` 增加合并协议路由外层的安全入站观察，区分请求到达和响应头完成，不记录实际路径、query、凭据或请求正文。新491来源轮已自然完成全部491个probe，12个Movie和479个TV文件均有实际轨道，无queued/running或强制取消。《四喜猫》S01E01在Web真实出画、seek后退出，保存233489ms且SID关闭；VidHub同用户显示03:53并实际续播，但正常退出后仍为233489ms、Native SID pending、没有Native日志，Web刷新仍03:53。此次后端完整观察的58次到达/58次完成中没有Sessions请求；两条独立Users401发生在登录阶段，不归因为播放报告被拒绝。Native截图未显示可读首/末帧时间，不以按键或墙钟推算精确停止位置。第二片《七龙珠超：比鲁斯》虽实际probe为H264/AAC，Web首次加载仍失败且无确认帧，保留Fail；通用编码提示不构成编码不支持的证据。该轮最终到达固定7200秒测试窗口，runner退出1；注册后端/观察器及原进程组正常TERM退出，原app/sidecar及pan/STRM清单一致，4份快照和私有副本已删除，无force或残留。D因再次锁屏未运行；页面及客户端进度仍未验收通过，不据probe终态或清理通过宣称整体验收通过。

`047e7d1e` 的隔离本地600秒视频已实际验证Web首帧、seek、自然退出落库341008ms、刷新重开至345.86秒，再停346511ms；同DB服务重启后进度、源revision/轨道及日志一致，另一用户无该进度。VidHub已能真实出画并读取该续播点，但seek及实播退出后没有新的native日志、签发SID仍pending，进度仍为Web旧值，保留Fail。观察器只覆盖18768，不据未见Sessions推断其他入口没有请求。本轮正常清理，原视频哈希一致、两份私有停止态快照及数据库/凭据副本均已删除。独立新DB的HTTP STRM经实际产品probe成功，Web已真实出画、seek后保存262601ms并重开出帧；其余恢复及Native仍在验收，不替代真实491来源。限定范围的最终静态代码复核未发现新增可复现阻断项，不构成运行验收通过。

49cce实际复验又定位两处取流问题，均按TDD单独修复并推送：`c690956e`让播放URL的显式凭据在既有窄范围内优先于浏览器自动Cookie，避免旧Cookie遮住有效URL而401；`047e7d1e`仅把VidHub实际发送的空`MediaSourceId=`按未指定处理，继续拒绝非空无效源、空白、旧revision及错误用户/设备/会话。真实请求的安全布尔及同认证32字节Range差分均已留证；对应公开回归、相关集成、clippy/fmt和独立review通过。此前49cce两端均无帧，不能据此评价进度保存；新候选必须重新实际播放后核对持久值与恢复。

`eb87a4dc` 与 `49cce686` 分别修复隔离实测的Web电影播放404和VidHub静态取流404，均单独提交推送并独立review。Web先复现路由不匹配，再通过新增3项及完整940项；协议先2项404失败，再通过绑定/Range/鉴权4项、Jellyfin71项及扩展的STRM跳转/撤销矩阵3项。media-server首次全套的两项WebSocket测试因本机监听权限失败，授权重跑相应目标3项通过，保留原失败，不称单轮全绿。真实进度保存和恢复尚未证明修复；新候选须分别验证本地视频、HTTP STRM及491真实来源，不能用前两次无帧加载失败当作进度阴性实验。

最新 Web 修复 `89285681` 已提交推送：实际 Library TV 搜索结果仍被前端当作文件命中，读取不存在的 `units.map` 导致白屏；现按当前 Media 合同展示并支持详情入口及游标分页，不保留旧形状。回归先复现原始异常后2项通过，完整Web937项、typecheck、lint（0 errors/81项其他文件warnings）、build及独立复审通过；首次沙箱回环监听失败与授权重跑分别保留。新候选release构建及二进制/UI冻结已完成，实际health和浏览器复验另记，不将旧轮搜索白屏或页面阶段追认为通过。

此前 Rust 候选 `3fbbf445` 已提交推送：FTS全局actual证据移至Library reader，writer只消费当前Media证据；实际RED writer工作量5,352→49,671，GREEN为3,747→3,749，后续Header writer为3,560→3,550。54项相关release回归及Store/API受影响验证已执行；首次受影响全套10个API目标因回环监听权限及次生断言失败，授权仅重跑这些目标后顶层1,010 passed/1 ignored、退出0，Store目标首次全部通过。workspace release all-target clippy、fmt和尺寸门通过；FTS专项及最终跨任务静态review无新增must-fix。新的1.688秒Header定位与原完整轮2.047秒未归因样本不能混用，后续完整阶段/终态及实际设备/性能门仍待复测，不据代码修复宣称最终验收通过。

该候选联合轮 `91261-3au6k3` 已正常结束：真实491来源、health和12 Movie/18 TV oracle已确认；VidHub电影与单集标记SET/POST Delete撤销及重入、标记用户隔离/隐藏单集404、动画分类五项精确oracle已通过。实际电影非零播放后仍未形成active session/log/单位进度，恢复从头，根因未定；协议路由的后端访问日志覆盖缺口已核实，不能从缺少日志推出未上报。页面矩阵部分通过，搜索白屏由8928修复，其他失败及缺少夹具项保留。首次旧TaskSpace失效的无效计时也保留。退出后原app/sidecar及pan/STRM清单一致，自有进程和私有副本已清理，脱敏日志与页面证据保留；完整页面阶段另起独立新轮。

官方Jellyfin 12.4.0同视频File/HTTP STRM对照已完成真实上报。File的Playing1/Progress22/Stopped1均204，服务端保存约08:50且有resume，实际GUI重入未确认；HTTP STRM的Playing2/Progress25/Stopped2均204，同SID/device/source关联一致，首次约07:47停止后VidHub重入从约07:51播放，最终保存5,323,575,794ticks且有resume。容器内外同URL Range字节及600秒时长通过，服务/origin/代理正常停止、自有容器0。该诊断否定HTTP/remote源必然不报告的宽假设，不算本产品进度修复或491来源验收。

8928新隔离轮491文件probe均成功，随后在完整停服快照S0等价恢复下完成Type单字段诊断。透明A实际播放、32次右移seek后约73秒退出，成员单位进度仍0、签发SID为pending且无active session/log；B只在PlaybackInfo增加缺失的`MediaSources[0].Type=Default`，其他JSON值深度校验一致。B首次未确认出画，另记无效尝试；唯一重试确认实际帧及同场景seek，约92秒后退出，仍无Sessions入站、proxy error为0、进度0且SID为pending。两次实际播放时长相差约19秒，保留该偏差；结论限定为仅补Type不足以恢复报告，不推断其他字段或认证根因。S0及SQLite完整DB/sidecar副本核验、首次沙箱/诊断工具失败均独立留证，最终恢复及清理另记。未修改生产协议响应，自有Web实际进度矩阵仍待执行。

本清单承接 [审核修复计划](2026-10-29-storage-redesign-fixes.md)，验收合同为 [原设计第 8–9 节](../specs/2026-10-18-library-storage-redesign.md)、[性能 SLO](../../testing/performance-slo.md)、[系统回归](../../testing/system-regression-cases.md) 和 [E2E 手册](../../testing/e2e-runbook.md)。原设计 H1–H6 有历史修复记录，不把旧 unchecked 或历史状态当成重新实施的理由。

当前进度（2026-11-04）：`4ae4a85e` 两项运行时修复已提交推送并通过提交后独立review。新隔离轮 `57540-e64827` 的491来源probe及公开Sessions回归通过；Movie卡片与图片各20样本，idle p95 177ms、Scan/Scrape及独立单任务Probe均176ms。正常退出、无forced，原app/sidecar及pan/STRM前后清单一致，私有副本已删除。最初Probe逐任务排队失败独立保留。这里的页面结果只覆盖Movie页相应阶段；09:15已可读取VidHub实际窗口，旧锁屏阻碍已解除，剩余操作尚未重验。当前候选perfgate、完整容量、完整页面/设备矩阵及缺失报告依赖仍未闭合。

诊断补项 `ce85e0a3` 已提交推送并经提交后独立review：普通浏览首页 EXPLAIN 复用实际生产SQL构造与绑定，仅供测试。定向3项、Store release（1023 passed/12 ignored，结果组求和）、clippy/fmt/尺寸门通过。完整容量仍待实际执行；并发扫描与心跳的实际锁等待及单事务最大时长见下项，既有批次墙钟不作为这两项的替代指标。

事务诊断补项 `cc7c997b` 已提交推送并经提交后独立review。默认10k分集并发测量：100个100文件发布批次、99个严格窗口心跳，全部100次HTTP尝试均200、busy为0；心跳p95为564ms，扫描writer等待p95为3.681ms、已覆盖事务最大593.104ms，native请求writer等待之和p95为559.420ms、已覆盖事务最大3.876ms，吞吐346.8文件/秒。其他raw事务不在该计时覆盖内，RSS未测得。Store release（1025 passed/12 ignored，结果组求和）、API原失败10目标授权重跑、media-server全套及三crate clippy/fmt已验证；最后打印参数风格修改仅经clippy编译和fmt验证。该测试没有证明严格延迟SLO达标，最终性能判定仍待perfgate。

完整容量工具已退出0：确认100k Media/1M文件/1M Playback、10用户，分集分页oracle及计数/外键断言通过；固定目标页VmStep为1590→1590，Fullscan/Sort均0。DB/WAL最终约8.64GB、索引dbstat约6.43GB。百万规模Integrity共4,500,001次推进、2284秒，已覆盖单事务最大2.047秒；没有最大值所属阶段证据，不能据工具退出0声称所有writer临界区满足1秒准入风险边界。默认100k Media Integrity gate亦通过：1项、总338.81秒，检查1,600,001步、233.42秒，最大writer约712.63ms；这不替代百万规模的2.047秒风险分析。新harness构建及试跑均退出0，完整轮启动前按新试跑估算和30GiB保留空间核对了磁盘。

realistic scale gate已通过（1项、58.35秒），实际数据为21k Media/61k文件/45k分集。完整容量工具的5秒RSS采样峰值为435,184 KiB；它是合成工具进程，非实际服务的内存测量。后续在原full fixture的APFS独立克隆上检查50项Movie A，最大事务875.94ms出现在`VerifyHeader→Members`，该路径调用全局FTS prepare；终态事务9.02ms。没有函数内细分计时，不能将原B修复轮的2.047秒归因到该步骤。诊断退出0，克隆已删除，原full副本保留供风险核查。

测量工具修复`f886d46c`已提交推送：拒绝失败HTTP/业务验证或不足窗口样本的历史耗时作为相对基线，当前错误响应不再显示相对通过，空基线报告在测量前拒绝；保留缓存口径例外和20%/1ms规则。公开main回归先失败后通过，相关离线74项通过，静态复核通过。旧perfgate原报告不改写，失效基线项不能追认为通过。

按修复后规则离线核对旧报告：idle有1项有效相对失败（日志导出），Similar的502使当前业务校验失败，其相对比较不可用；busy仍有11项有效相对失败。cold有1项有效相对失败（日志导出）及7项无效历史基线：两种登录、PlaybackInfo、收藏和已看写入为连接失败，chapters和progress另有零耗时及前置失败。busy/cold各14项缓存口径例外仍执行绝对预算和正确性校验。这是旧证据的有效性核对，不是新一轮运行，也不改变原报告及退出状态。

2026-11-04 后续复审及实际客户端补项：`798606f3` 修复 VidHub 的两种 `POST .../Delete` 撤销标记；`f55776bd` 修复 retain 前新建无引用 Media 的补偿遗漏；`192e1f32` 以共同 decision guard 防止周期恢复中止活跃 Subscribe 创建。三项均先有实际失败，再通过对应定向测试、clippy/fmt和独立审查，分别提交推送。Jellyfin集成71项、media-server40项，Subscribe API2项及Store补偿13项/retention23项为相应修复的局部证据，尚非新的最终workspace或设备通过结论。

`f886d46c1533` 的真实客户端隔离轮 `26713-1qtztoy` 已正常结束，原app/sidecar及pan/STRM清单一致，自有进程、私有副本及临时VidHub连接均清理。管理员12部Movie/18部TV、成员仅12部Movie及隐藏TV直接访问404已观察；设置电影/单集标记200、撤销405是本次修复的真实触发。两账户精确搜索返回正确，成员无结果后VidHub额外加载普通列表，不将客户端显示回退当作服务端泄露。分类精确oracle、非零进度/恢复/用户隔离、完整artwork与轨道及Web矩阵仍待新候选实测；代理只覆盖18768且本轮后端日志未保留，不能据未见事件断言服务端未收到播放上报。

`61ee9207`容量复验：在原百万文件夹具的独立APFS副本上，Header及小Movie Library检查均退出0，前后100k Media/1M文件/1M Playback计数及外键断言通过，覆盖事务最大值分别4.696ms和32.374ms。完整TV B推进到949718步时，`VerifyReverse→VerifyReverse`出现覆盖事务2641.865ms、writer持有2641.870ms；按暂停阈值中止，退出130，未到终态且没有after oracle。原夹具元数据未变，三个副本均清理，最低空闲约95.0GB。该阶段查询在reader上准备，writer仅校验及推进游标；尚缺SQL/commit分段证据，不能把该慢样本归因为FTS或反向查询，也不能称容量门通过。此为后续插桩完整轮之前的中断结果；终态、双oracle及提交尾延迟见本文顶部，两轮证据分别保留。

## 1. 分工和版本

- Codex：代码与 Task 对账、单元/集成及性能工具验证、idle 证据核对、修复实际发现的问题。
- 2026-11-02原分工由对应执行者完成busy/cold、页面/E2E、VidHub/实际设备和现场验证；后续实际执行和版本见顶部记录，不向其他执行者自动发送消息。
- 功能代码基准为 `b2b902a0`；后续文档和 `stress-up.sh` 构建并发修正不构成新功能实现。2026-11-02轮实际运行版本为 `df8c50bdf2e3`，结果索引见本地 `.e2e/perf/final-20261102-summary.md`；所有历史报告保留真实 health commit，不改写为新 HEAD。
- 每次实际验收前记下 `git rev-parse HEAD`，刷新 `crates/api/build.rs` 后串行 release 构建，核对 health 的 commit。后续有 Rust/Web 行为修改，重跑对应测试和受影响验收。
- 现有 workspace/Web 全套、clippy、fmt、release 与 idle 证据见本地 `.e2e/perf/phaseD-verification-summary.md`。这是已执行证据索引，不把本次清单审核写成重新执行全套。

## 2. Task 与提交对账

“实现已核对”只表示存在实现及相关回归，不等于所有最终运行验收通过。

| Task | 主要提交 | 实现对账 |
|---|---|---|
| 0 | `9f8da717` | 基线、规模夹具、路由性能闸门；历史失败报告保留 |
| 1 | `30909393` | SAVEPOINT 回滚与 reader 事务清理 |
| 2 | `6168490e` | writer 锁内写准入 |
| 3 | `aff639df`、`6f1413a3`、`ab52bf1b` | Library 读池、乐观校验、请求快照及读池准入顺序 |
| 4 | `ffe3e379` | 纯读移出 writer |
| 5 | `4da98f2c` | probe 同步阻塞隔离与有界枚举 |
| 6 | `8ee6b6b0`、`a99c4ef8` | 删除索引、订阅解绑及投影索引 |
| 7 | `a0974f39`、`6f1413a3` | 每库 writer、统一读池、jobs 读写分离及借用开销修复 |
| 8 | `e46e2d18` | NextUp/Resume/up-next 投影 |
| 9 | `ad2e294c` | 收藏 keyset 与 LastPlayed 分段分页 |
| 10 | `02e19ef4`、`fb16a386` | missing/collection 批量分页；jobs N+1 同 Task 4 |
| 11 | `6d5a9b7d` | FTS、短前缀及可见性 |
| 12 | `c70131ff`、`9067ed46` | 计数、pinned 剧集与 Latest |
| 12.1 | `59fb452a`、`584d103f`、`ce3f189e` | 本地采纳事实、推荐刷新与季缓存 |
| 12.2 | `b03aacbb`、`2e072b0c`、`ec75c3d9` | 实体、计数、搜索提示与季投影 |
| 12.3 | `c6e47925` | Person 图片缓存与有界填充 |
| 12.4 | `e59b3b7e` | ledger 批量事实与页内观察 |
| 12.5 | `d0ff5bef`、`f0a968e7`、`f36888fe` | catalog 缓存、执行器及后台 season 容量隔离 |
| 12.6 | `38015d02` | 日志完整响应、有界导出 |
| 13 | `fee4302c`、`3ea354ff` | ownership FK/成员清理与重复清单删除完成；S-DB5 待报告 |
| 14 | `aa77e25c`、`17069b16`、`a80e07f0` | atoms/H&R 三态、旧创建字段与废列完成；Z3/Z4 等待报告 |
| 15 | `b2b902a0`、`f4c5d2db` | 数据字典、入口、注释、资源及删除规则；未知 D 清单待报告 |
| 16 | 见下方拆分提交 | 文件/函数尺寸与 lib.rs 模块树已核对 |
| 17 | `28c9004f` | 图片视口/并发/取消实现；页面白屏与延迟待现场验收 |
| 18 | `b6ae9bee`、`7abbffc7` | optional 探测取消、迟到发布及根退休观察清理 |
| 19 | `0a01419a` | 未入库单集 still 统一代理 |
| 20 | `08f4d2e1` | 图片共享冷填充、截止及真实错误 |

Task 16：release `57386abb`；downloader `908e57bb` / `4f911f94`；library `89ecc311`；marker `f39aaca1`；cover-generator `4c8d8bd8`；media-server `65d14859`；jobs `a5b44c3e`；domain `0f81938b`；store `09a9a5a4`；api `b8bb316c`。

2026-11-03 审核修复另见[固定基线后的修复记录](../../audits/2026-11-02-storage-redesign-review.md)：`18bad154`、`2e378ca2`、`64432d3e`、`4d681e8f`、`b89769d3`、`c476b43a`、`fa0f8e05`、`df1f473d`、`cf012c99`、`2b3c160d`、`938a9b89`、`9a9dcbbb`、`dabf77a6`、`52522549`。Task9 五 crate 与 Task10 四 crate、Web 和 clippy 的局部结果各按对应版本记录。workspace 全套已执行一次，开始于 `2b3c160d`、仅文档/注释变化后结束于 `938a9b89`：退出0、588个结果组、3842 passed、0 failed、26 ignored（含子进程汇总，非去重数）；后续 Rust 修改另跑受影响测试，不改写该次版本。

早期候选 `938a9b89809e` 的491个真实 STRM/pan 来源副本功能轮与独立 Transfer 轮已执行；Transfer 使用真实视频和 fake qB。其删除成功约441ms后 NFO 迟到补写的失败保留，修复为 `dabf77a6`。最终候选 `52522549fbef` 的真实删除竞态复测通过：9次约50ms清单采样中，6个原有文件未丢失，STRM及观测成功后的内容未变化；不据此推断取消延迟或长期无变化。`52522549` 的单Media内部分页 Integrity 已通过定向回归和独立审查，完整规模门待实测。

最终候选首轮491功能断言通过，但原 app.db/sidecar 前清单缺失，原 app 恢复证据保留为 Unverified。改用私有 key 的新491轮主断言通过，原 app/sidecar 及 STRM/pan 前后清单一致，自有进程清理完成。该轮结束时仍有79个 queued/1个 running probe job，另有1项 ffprobe TLS IO超时、根因未定，所以只证明扫描及指定业务流程，不能证明全部 probe 终态通过；旧轮副本已清理，不重造其完成证据。页面、当前候选 perfgate、完整容量和多批内存归属仍未闭合；后续全来源探测/内存轮须独立记录，不能拼成旧轮通过。

最终候选独立 Transfer 轮通过（真实 worker、视频哈希/NFO/重启，fake qB）。CLI/global Scrape 历次工具失败与首轮未知失败保留；修正未来计划队列及本地化标题前置断言后，独立新轮退出0，核对 expedited child 实际完成、新 ledger/NFO/TMDB alias及重启后 CLI/API 事实。原 app/sidecar 与 STRM/pan 清单一致，副本与自有进程已清理；独立 global Scrape 结果不来自 Transfer 的 NFO。

独立全来源 Scan 轮 `12916-11ts08a` 已在同一 backend 完成 Movie 4/4/4、TV 160/160/159 六批，每批持续10秒持久空闲；491个唯一 file_id 均有成功 probe unit，491次尝试全部成功，无失败或活跃项。原 app/sidecar 与 STRM/pan 前后清单一致，进程及副本清理完成；不改变旧轮 TLS 失败、不完整终态或原 app 基线缺失结论。

`52522549` 的 Web/VidHub 联合轮 `72487-z6wo3u` 已结束。实际Movie卡片及图片就绪 idle20/20、p95 176ms，Scan running20/20、p95 182ms；Probe只完成1例，第2例未观察到running；Scrape第2例5439ms并有unidentified504，两阶段各18例未运行。VidHub已观察完整admin列表、搜索、部分详情/演员图片和播放操作；member隐藏库、全部季切换、服务端播放记录仍待验证，后续native受锁屏阻挡。491探测终态全部成功不代表整轮通过：退出需强制终止自有进程组，原pan三文件清单变化归因未定，清理Fail；原app/sidecar与STRM清单一致。新轮清单工具已有隔离测试，实际验证另记，旧轮失败不追认。

后续修复 `ab362a82`（空Unidentified跳过文件系统准入）和 `4ae4a85e`（nullable Jellyfin会话事件）分别经TDD提交推送，提交后独立review无发现。Store/API release分别汇总1020/2169 passed、12/9 ignored（结果组求和，非去重数）；media-server沙箱端口失败定向重跑后通过、其余目标及doc通过，fmt/clippy/尺寸门通过。新候选真实HTTP、页面及剩余性能门仍须独立记录；缺失位置协议缺陷尚未被证实为VidHub实际无进度的原因。

## 3. 未完成项与历史失败

以下`cc7c997be6ff`条目保留历史候选失败；产品`1c3af023`的v2五阶段实际失败见本文顶部，当时candidate busy因旧基线无效未执行。产品`97cc6b1b`与工具`159934b2`的原六阶段campaign因旧idle的502拒绝基线，随后独立busy亦因旧Similar 502拒绝基线；独立新cold166/166通过。本轮另取得有效旧idle/busy基线，新idle166/166、新busy165/166，唯一剩余性能失败为busy收藏写入相对门。各轮原失败及当时Not run均保留，不拼成单次六阶段通过。

- [ ] 取得 `docs/superpowers/specs/2026-10-29-storage-redesign-review.md` 后核对 S-DB5 逐列 CHECK、Z4 六个函数、Z3 image-proxy 精确行为、报告 5.2 及 D1–D13 的未知细项/桩文件。收尾已执行 `git pull --ff-only`，报告仍缺失；不猜清单、不标全完成。可选优化与必须完成项分别按报告原要求判定。
- 历史失败保留：候选`cc7c997be6ff`的完整idle未通过。该轮完整166项、每项200样本，164项通过、2项失败。Similar初始p95为2.545292ms、相对+92.746%，完整结果199项ready、1项502；日志导出p95为23.969584ms，满足300ms绝对预算但相对+32.228%失败。保留阶段D及各轮历史报告、ready/pending条件差异，不拼接成全绿，也不提高预算或删样本。本轮原app/sidecar及pan/STRM清单一致，无残留listener/owner/live副本；此前两次测试脚本把`/tmp`与`/private/tmp`误判为不同路径的启动失败独立保留，路径规范化后seed完成。
- 历史失败保留：候选`cc7c997be6ff`的busy/cold按当时缓存口径重测后仍未通过。该轮busy完整166项、每项200样本，155项通过、11项相对基线回退失败；两种Jellyfin计数、ledger、gallery、up-next、日志导出、两种登录、PlaybackInfo、收藏和进度写入的失败均保留。Similar本轮200个完整结果均成功，不回填idle的一次502。cold按用例分别重启，完整166项、160项通过、6项失败：日志导出、两种登录、PlaybackInfo、收藏及已看写入。两轮均正常清理，原app/sidecar及pan/STRM清单一致，无listener/owner/live残留。`phaseC-busy.json` / `phaseC-cold.json` 保留为旧提交的比较基线，不当成当前版本通过结果。
  cold原六项失败中，两种登录、PlaybackInfo、收藏及已看写入的旧基线均为`code=0`连接失败；工具曾误把其约0.1ms耗时作为可比性能。修复后离线核对另发现chapters和progress的历史基线耗时为零且前置失败，共七项相对门不可验证。原报告与退出1保留，工具已按TDD修复，尚未据此取得新的运行结果。日志导出使用成功HTTP基线，其相对失败仍成立。绝对预算及业务正确性不因缺少有效相对基线自动证明整项通过。
- [ ] 最终匹配性能：独立新cold166/166与网络复测新idle166/166已有有效旧基线、health、业务、来源和清理证据；该轮新busy165/166，收藏写入相对失败（0.951917→3.509875ms，200次200）待定位。聚焦四轮未复现；随后单次完整旧busy为162/166、HTTP/准备错误导致基线无效，新版Not run，不能替代原比较。原EOF/502、v2五阶段及首次中止的部分旧idle均保留。保留此前实际491/UI和容量/内存结果，不重复作为当前候选新运行。
- [ ] 汇总实际证据与剩余Unverified，复审并完成文档交付；严格六行Playback基线Fail及driver退出Unverified不可追认为Pass。

## 4. 最终验收矩阵

| 门 | 执行与观察 | 通过条件 |
|---|---|---|
| 本地/缓存性能 | idle、扫描 running 窗口 busy、逐用例重启 cold；分类/人物/计数/搜索/季/NextUp/ledger 重点比对 | 维持既有 SLO、相对退化规则及缓存口径；所有 HTTP/准备错误保留，心跳实际持久化；首次外网回源耗时单记，正确性/超时/有界资源仍必须通过 |
| 空库完整流程 | 配置→识别→扫描本地视频/注入 probe→浏览/图片→Playback/收藏/Subscribe→Transfer/Scrape→重启→清 cache→移除/重导入 | Web/CLI/Jellyfin 真实请求覆盖；清 cache 不丢业务事实，移除不删磁盘文件，孤儿清理与保留引用符合 ADR-0012 |
| 身份/Playback | F1、R1：多集独立 ItemId、同物理源、迟到/重复事件、换源/剪辑/同路径替换/轨道重排 | 两协议的 position/duration/轨道语义一致；恢复包有效性和已看/收藏不混用 |
| 合并/元数据 | F2–F4、R2–R3：文件改挂/Media 合并、played 三元组、survivor fallback、NFO work/season/episode 与多 locale、自产 NFO→外改→刷新 | 输入乱序、幂等、提交前后重启结果稳定；显式空与缺失区分、provenance 正确、刷新目标固定、pending 正常退出 |
| 完整性/竞态 | F5：事实/摘要缺行发现修复；扫描/删除/根修改/重新识别/worker 迟到；维护/只读排队写 | 每个写入口事实与投影事务一致；旧 worker 不复活引用，ACL 隔离，异常和未知 IO 不误删 |
| 页面与设备 | 空闲/探测/入库/Scrape 各阶段页面，真实播放客户端与 NAS/来源 | 每例记录构建、起止状态、截图/请求及失败；API 集合 P 类不代替浏览器；Task 17 页面预算与白屏单独判定 |
| 容量与增长 | 至少 100k Media、1M 文件、1M Playback 单元，超长 TV、多版本/多库/多用户 | 记录 VmStep/FullscanStep/Sort、SQL/解码行和字节、分配/内存、锁等待/事务、WAL/DB/索引体积及吞吐；满足原设计第 8 节的增长边界，24k STRM 不能替代此门 |
| 大批入库内存 | 多批扫描/Transfer，记录开始/峰值/完成后空闲及下一批，结合 heap/allocator/队列与缓存归属 | 定位持续存活对象及增长来源；RSS 未立即下降本身不等于泄漏，不能仅以清空进程或重启当修复。无证据前不宣称已解决 |

2026-11-04 用户补充的播放验收覆盖自有Web播放器和VidHub：af40实际Movie/Episode双向续播及重启后读回、73da多用户隔离分别有上文版本限定证据；保留SID/device、位置与实际响应，不以模拟heartbeat替代真实UI。cd80严格六行基线的新增真实播放和driver退出不确定性另按上文保留。

## 5. 已取得的局部规模与内存证据

- 现有 Store release 集成测试已验证：无关库扩大到百万条，固定目标库的同页输出不变、Fullscan/Sort 为零，VmStep 不增长。
- 现有 release 查询矩阵已验证 5k 与 100k 规模：固定 Movie 库的类型读取不随无关 TV 增长放大；简单分类分页工作量稳定。精确交集及 facets 的实际线性成本另记，不推广成所有查询 O(1)。
- 独立实例已记录从空库入库、用户数据准备、完整 idle 到结束后静置的 RSS 与 vmmap；物理占用有明显回落。此夹具关闭实时监听，不代表 active watcher、目录批事件、错误密集扫描或实际 Transfer 的内存门已经通过。
- 此前候选已完成上述六批真实来源 Scan 与三个独立 Media/Subscribe 的 Transfer 内存诊断（同一 backend、fake qB、真实发布及哈希/NFO、每批持久空闲、原清单一致）。Scan 空闲 RSS 从64,288 KiB，经 Movie 至53,088 KiB、TV 至73,984 KiB；SQLite 分类存活字节3,933,088→8,975,472、String clone 181,520→1,323,744。Transfer 空闲 RSS 为66,128→68,944→49,536→49,616 KiB，SQLite 分类约3.85→3.65 MiB。malloc 正控证明采集能力；这些有限批次及符号分类不证明完整存活对象归属或无泄漏。Transfer 不覆盖同一 Media 多版本，前一轮第二批409根因未定。
- `cc7c997be6ff` 的独立内存轮 `round-20261104-51289-1miams0` 已完成 Movie 4/4/4、同源重扫及额外60秒空闲，12个唯一文件的probe均成功。六阶段原始heap及完整malloc_history均正常退出、未截断并已保留。活分配10,827,104→13,433,664→重扫12,220,032→空闲12,202,000字节；重扫及空闲RSS均53,008 KiB。分配栈可见日志缓冲字符串、SQLite reader/页面缓存及网络连接/传输缓冲；网络分类从批次1,787,520降至604,832字节。分配点分类仍有未归属余量，不能当作完整所有权清单，也不能证明长期无泄漏；mmap虚拟大小不与heap/RSS相加。原app/sidecar及pan/STRM前后清单一致，无forced/live自有进程，独立副本已删除。
- 后续离线细分同轮保留原始栈：baseline至最后idle净增1,374,896字节，主要为SQLite +426,480、ureq +603,360、String +224,768；other主要为rustls，分类外补充解析后仍有1,104字节净变化及other中6,080字节未细分。结合源码确认日志buffer、连接缓存、网络/TLS及redirect cache的生命周期，完成该轮主要分配来源定位；聚合栈没有逐个allocation ID，当前引用和逐字节owner仍未全部验证，也不将此结果推广到已清理原始栈的491/Transfer旧轮或长期无泄漏结论。
- 这些结果只关闭对应的局部测量缺口；完整容量的终态与双oracle已在后续插桩轮取得，但提交尾延迟风险、业务矩阵及外部执行项尚未闭合。原始日志与数字保存在未提交的 `.e2e/perf/` 或权限受限的 `/tmp` 审计目录；敏感原始栈不入库。

## 6. 资源、报告与交付

- 后续执行以用户最新重贴的AGENTS为准：同一时间一个Cargo/test/build，`TMPDIR=/tmp`、`-j8`、`--test-threads=8`。既有两个普通锁槽继续保留，收尾独占两槽以避免并行构建；不删除或替换锁文件。下一条两组`-j4`试跑只描述此前授权窗口，不改变历史命令或结果。严格性能测量不与构建或其他测试并行，其他session负载无法排除时记录干扰。
- 2026-11-04 用户更新资源试跑：两个 session 最多并行两组普通构建，每组 `-j4`，每个 session 内仍串行；`TMPDIR=/tmp`、测试 `-- --test-threads=8`。用 `/tmp/cm-cargo.lock`、`/tmp/cm-cargo-2.lock` 两个普通文件的排他 `flock` 槽位协调，未获槽不启动命令，自有进程组完全退出后释放，不删除或替换锁文件。严格性能测量期间不并行构建，不能排除其他负载时如实记录干扰。既有单进程 `-j8` 结果保留其实际参数。先定向，稳定功能树 workspace 全套仅一次；仅改本清单/文档/构建并发参数不重建全套 debug 缓存。
- 独立临时目录 `/tmp/crawler-storage-fixes`。使用非默认夹具根时，给 perfgate 的临时 cases 配置同步替换 `fs/browse` 的固定目录，避免把旧 `/tmp/e2e-stress/media` 的 404 当功能失败。不修改 SLO 或历史报告。
- `stress-up.sh` 启动前先完成受锁保护的 release 构建；启动时设 `PERF_SKIP_BUILD=1`，避免脚本再构建。远程读并发 1、冷却 500ms、Library readers 8 不改。
- 所有原始结果留 `.e2e/perf/` 或 `docs/test-reports/`，均不提交；密钥只内部传递，不打印，也不放命令行参数或报告。用例结果必须区分 Pass / Fail / Not run。
- 当前 256G 磁盘不同时保留多套 debug/release 大规模夹具；容量测试分批准备，先评估 DB/WAL 峰值，空间不足记录 Not run，不删除用户媒体来腾空间。
- 每次运行只停止自有进程、清理自有临时文件；报告保留。结束检查端口、git status、磁盘及分支同步。
- 所有必需门及报告依赖闭合，或由用户明确接受具体遗留项后，才能宣布最终验收通过。当前不合并、不发布、不切用户实例；现有授权只允许提交推送 `feat/storage-redesign`。
