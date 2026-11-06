# 2026-11-06 存储重构最终候选验证记录

最新补项：扫描同值源版本更新优化 `342224a7` 已按 TDD 提交推送，Store 全套 208 个结果组加总为 1038 passed、0 failed、12 ignored（含嵌套结果、未去重），Clippy/fmt 通过。新普通二进制完整 busy 为 **164/166**：所有绝对预算及业务通过，已看和进度写入相对门通过，进度持久化核对通过；剩余推荐和详情推荐完整结果相对失败。来源及副本清理全部核验通过。另有两个固定空推荐缓存目标的 idle/busy 各 200 样本绝对门通过、零 pending；没有相对比较，不替代原失败。此为新版本的独立 busy 轮，不改此前历史失败，也不宣布整体验收通过。详见[源版本优化与实际复验](2026-11-06-scan-source-version-work.md)。

状态：**尚未整体验收通过**。最终候选三轮性能均完成，但均有原相对门失败；最终候选的有限页面与图片 smoke 已完成，来源和清理核验通过。结果保留历史失败及未验证项，不将局部通过推广为整体验收。

## 版本与自动化

最终产品为 `da68921a1a5d1830f6d1bed3e65e538e135493e2`，性能工具为 `159934b25405f38de755295eb566df03d11a96f8`，冻结 fixture 来源为 `1c3af0236c14a13753884f6164f9962a34edb5fa`，三者身份独立。最终 release workspace 在主工作区执行并退出 0：602 结果组汇总 3976 passed、0 failed、25 ignored，包含嵌套子进程汇总、未去重。前端 build 和 release binary build 均退出 0；实际构建 cwd 为主工作区，clean checkout 用于源码身份核对。二进制 58,029,104 字节，SHA256 `23a3220acd279476f903408b8bd01ff995c8b4d0547fd33d446b860b02fa072a`。收据见 `/tmp/cm-final-workspace-da68921a/receipt.json` 和 `/private/tmp/crawler-final-build-da68921a/build-receipt.json`。

离线准入完成：双方路由覆盖各 154 GET、166 cases、10 exemptions；每侧 50 用户、400 项业务断言 verified，canonical oracle 输出与 business artifact 哈希一致。首次从复制布局运行 raw oracle 因 Git root 错误退出 1，后续从字节相同的 canonical tool checkout 运行两版均退出 0；原失败保留。见 `/tmp/cm-final-admission-da68921a/receipt.json` 和 `/tmp/cm-final-admission-completion-da68921a/receipt.json`。离线准入不代表性能通过。

## 最终性能

候选分别对已准入的历史旧报告比较；idle/busy 每项 200 个准入样本，cold 每项 1 个样本并逐用例重启。三轮均 complete、业务 verified、绝对阈值及响应正确性通过，退出 1，原相对门失败。busy 保留窗口外尝试。此为候选对历史基线比较，不是新执行的六阶段配对通过。

| 模式 | 通过用例 | 相对失败 |
| --- | ---: | --- |
| idle | 164/166 | Similar、page-detail |
| busy | 161/166 | chapters、favorite、played、Similar、page-detail |
| cold | 160/166 | genres、nextup、ledger、played、Similar、page-detail |

| 模式 / 用例 | 旧 p95 ms | 新 p95 ms | 变化 |
| --- | ---: | ---: | ---: |
| idle Similar 完整响应 | 869.448 | 1114.672 | +28.20% |
| idle page-detail 完整响应 | 803.649 | 1096.867 | +36.49% |
| busy chapters | 3.780 | 5.778 | +52.88% |
| busy favorite | 0.952 | 3.601 | +278.28% |
| busy played | 1.249 | 3.568 | +185.73% |
| busy Similar 完整响应 | 890.633 | 1151.636 | +29.31% |
| busy page-detail 完整响应 | 794.949 | 1194.737 | +50.29% |
| cold Similar 完整响应 | 841.542 | 1186.487 | +40.99% |
| cold page-detail 完整响应 | 775.239 | 1111.158 | +43.33% |

cold 其余四项单样本相对变化：`jellyfin-genres` +74.01%、`jellyfin-shows-nextup` +26.17%、`self-ledger` +43.91%、`jellyfin-items-played` +24.15%；单样本不能描述延迟分布。Similar 的初始响应可能 pending，表格比较完整响应；本轮推荐观察为空，不代表非空推荐性能。busy chapters 总尝试 201 次，其中 1 次窗口外；其余两个短写各 200 次。扫描准入依据请求两侧 operation 状态，不能证明请求期间每一时刻的扫描阶段。

报告为 `/private/tmp/crawler-perfgate-matched-candidate-1c3af0236c14/retests/final-da68921a-{idle,busy,cold}/report.json`；复核见 `/tmp/cm-final-relative-failures-review.md`。各轮 source manifest 一致，stage exit 1，listener、owner 和 live fixture 均已清理，收据分别为 `full-matched-audit-20261106-92068-18nm7l0`、`full-matched-audit-20261106-98117-qm3464`、`full-matched-audit-20261106-6490-4ls7e3` 下的 `receipt.json`。

这些相对失败是真实测量结果，尚无已证实、可操作的产品代码缺陷。源码复核未发现成功首尝试路径的固定 sleep；聚合日志无法关联具体用例和样本，不能把所有失败归因于 retry 或网络。后续若定位，应在安静环境按固定样本量关联请求、缓存、origin 尝试、reader/writer 等待及扫描状态，保留窗口外样本；诊断不能替代正式门。

## 页面与图片

最终产品 `da68921a1a5d`、同版本构建的 UI bundle 在 `127.0.0.1:18766` 运行，启动健康检查、二进制摘要与 UI 逐文件摘要匹配；浏览器使用获授权的新测试空间，结束时关闭。有限 smoke 实际检查：桌面媒体库首页与电影列表、Movie/TV 详情、390×844 手机布局、S01E01→S01E02 分集选择、返回与刷新、空库、媒体库不存在及返回恢复。首页显示 4 个可见 Library、3000 Movie、300 TV，所选剧集显示 70/70 集；电影与剧集标题、简介、选择集和空媒体轨道提示可见。手机首页和剧集详情实测 `scrollWidth=innerWidth=390`。这不是全部页面、全部详情字段或新的 VidHub 实播验收。

图片检查取得 poster、fanart、still、Library cover 四项 HTTP 200、`image/jpeg`；前三项完整字节与对应本地副本文件的 SHA256 一致，分别为 21,796、21,796、21,401 字节，cover 为 88,455 字节并实际渲染。夹具使用其既有占位画面，不代表真实影片海报或人物头像。抓到的 31 次静态图片请求均无 Authorization，应用数据请求使用独立测试会话。无章节帧的已知文件及不存在 poster 的无鉴权请求均为预期 HTTP404；**该轮成功章节帧和真实慢源图片 smoke 为 Not run**，没有捏造对应夹具。受控超时、共享填充与缓存恢复的公开 Router 回归仍是独立自动化证据。

已捕获的 TV 与错误路径浏览器事件没有未处理 JavaScript 异常；5 条控制台网络错误均来自主动访问不存在 Library 的 API 请求。Movie 事件捕获不含 Runtime console 订阅，不能据此声称全程零 console error。正常页面网络事件、图片字节及截图保存在 `/private/tmp/cm-final-page-service-da68921a/`，主收据为 `browser-receipt.json`；`browser-actions.json`、`public-image-results.json` 与各 `events-sanitized.json` 记录实际范围。

页面副本与正式性能夹具分开：前两次准备分别因停止态 WAL 格式只读打开、发现待处理 probe 而退出，均未启动服务，副本未变、来源一致，原失败保留。第三次仅在 LIVE 副本按生产计数规则取消 23,669 个 active probe job/unit，删除 20,058 个 probe obligation，关闭两库 realtime watch，并禁用 8 个 job definition、取消 8 个 queued job；已成功的 removal 未改。停止态无 WAL/SHM/journal 才使用 immutable 只读检查，运行中的 DB 使用普通只读连接。仅选择有效章节缓存的 Movie 与 TV S01E01/S01E02，避免重新探测。结束前独立检查 probe job/unit、chapter frame task、probe obligation 均为 0，约每 5 秒采样时 enabled definition 与 queued/running job 均为 0，不证明任意瞬间均为空闲。

页面准备改变的是自有副本的后台工作状态，不能称其与性能工作量等价。`performance-slo.md` 明确保留必需 media-info probe，idle 仅表示没有 full scan；两份性能 baseline 的 queued probe 分别为 23,681/23,668、running 各 1，obligation 为 20,227/20,058。最终 idle/busy 全日志分别出现 896/1085 次媒体信息探测开始；这些计数不与具体请求关联，既不能否定正式结果，也不能证明性能失败由 probe 造成。业务 oracle 和字节绑定不证明后台负载相等，详见 `/tmp/cm-final-fixture-background-review.md`。

正常 TERM 后 DB-family 句柄、18766 listener 和 owner 均关闭，自有 LIVE 副本已删除，冻结 baseline、原 app/sidecar 及 STRM/pan 清单仍一致；`stopped.json`、`cleanup.json` 记录核验。测试密码和原始含请求头事件仅保存在私有临时记录，公开文档不含凭据。

### 后续章节图片页面补测

使用同一最终二进制/UI 的新独立副本，生成 320×180 的 JPEG 测试图并只给该副本的一条有效章节缓存填入明确标注的合成章节。生产 `/api/v1/chapters/{file_id}/0` 无凭据请求返回 HTTP 200、`image/jpeg`、7,526 字节，与生成文件 SHA256 一致；浏览器直接图片及 Movie 详情章节卡片均实际显示 320×180 图像，截图已检查。此为本地图片交付/展示验证，不是视频抽帧或实际影片章节识别验证；真实慢源页面仍为 Not run。

准备阶段先因用户授权清理的旧 `97cc6b1b` 二进制不再存在而拒绝，服务/副本均未创建；随后只对该未使用的历史路径及精确哈希核对清理清单，其他冻结输入、当前二进制及 UI 摘要继续完整核验，原 freeze 未改。该准入补项先失败后通过，3 项离线回归通过。第二次准备把停止态 sidecar 误判为活跃数据库，未启动服务；来源一致、句柄检查后删除副本并保留失败。最终准备改为检查进程/端口/全部 DB-family 句柄，允许正常 SQLite 处理停止态 WAL family。

本轮点击章节的播放按钮曾打开合成媒体播放器，显示无法直接播放提示后返回，没有确认实播；不计为播放验收或大图灯箱通过。返回后未等待第二次懒加载，复入图片状态为 Unverified，首次详情显示证据独立保留。测试空间 2 已关闭；服务正常 TERM、生命周期脚本退出 0，原清单一致，端口/句柄/owner 关闭且 LIVE 已删除。证据在 `/tmp/cm-final-chapter-page-da68921a/` 的 `public-image-result.json`、`detail-result.json`、`browser-receipt.json`、`stopped.json` 和 `cleanup.json`。

## 容量与内存边界

历史 `61ee9207` 百万规模完整插桩轮完成 4,500,001 推进，100k Media、1M 文件、1M Playback 的前后 oracle 及外键检查通过；这是限定工作量的一致性证据。最大覆盖事务 2.735212417 秒，advance 的 COMMIT PROFILE 聚合 2.735 秒，因时钟粒度及 PROFILE 不覆盖后续 WAL callback，尚不能归因 checkpoint、fsync 或其他原因，也不能声称严格容量性能通过。`2f5cd923` 有界诊断及控制各 224,978 步、100,000 次 Users COMMIT 未见 >=100ms PROFILE，未复现或解释原峰值，且 `acceptance_eligible=false`。见 `docs/audits/2026-11-06-commit-tail-observation.md`。

六批 Scan、三批 Transfer 和 `cc7c997be6ff` 独立 heap 轮提供有限内存观察；后者 rescan 后 60 秒活 heap 12,220,032→12,202,000 字节，RSS 为 53,008 KiB。分配点分类不是完整所有权清单，有限窗口不能证明长期无泄漏；Transfer 未覆盖同一 Media 多版本，先前第二批 409 原因未定。合成容量工具的采样 RSS 不代表服务 RSS。

## 历史实播与未闭合项

`af40b58a` 的真实 491 来源轮取得 Movie/Episode 的 Web→VidHub→Web 播放、停止持久化和同库重启证据；`cd80d58e` 有限 Native 复验取得前缀、163 集身份和保存位置证据。它们均保留实际版本，不冒称最终 commit 新运行。后者严格六行 Playback 基线 Fail：未知操作者的真实播放新增第七条 unit/log/关闭 SID，终态 7/7/7/0；不推断操作者或归因代码。原 driver 因工具宿主中断退出 Unverified，独立受控 TERM 恢复退出 0 不改变原退出状态。见 `/tmp/crawler-real491-cd80-bounded-service-report.md`。

原引用的 2026-10-29 审核报告缺失，其 S-DB5、Z4、Z3、5.2 及 D1–D13 原始细项仍为 unknown，不据猜测标记完成。历史容量 COMMIT 尾延迟、内存长期归属、三轮性能相对失败仍未闭合；有限页面 smoke 已完成，不替代其余门。

历史实播受控恢复记录原 app/sidecar 和 STRM/pan 清单一致、测试连接及自有进程清理；本次三轮性能各自 source/cleanup 收据通过。页面 smoke 的独立来源与正常停机/句柄/副本清理收据均通过。

## 后续 Playback 保留验证与临时数据清理

最终产品 `da68921a` 新增独立保留/重启轮通过：完整复制恢复停止态的 DB、WAL 和 SHM，实际基线为 7 条 unit、7 条 log、7 个关闭 SID、0 个 active session。只在自有副本停用后台监听和定时任务，连续两次正常启动、登录、通过 marks/resume 读回管理员六条记录并正常 TERM；四张 Playback 表的全部行在两轮后均与七行基线精确一致，包括另一用户的数据。二进制摘要和两次 health 身份匹配，各次服务退出 0；原恢复源 DB family、原 app/sidecar 及 STRM/pan 清单一致，端口与句柄关闭，测试副本已删除。收据为 `/tmp/cm-final-playback-preservation-da68921a-v3/receipt.json`。

这轮验证没有新增 Native 实播，不改写历史严格六行基线 Fail 或原 driver 退出 Unverified。前两次工具失败分别报告 `'position_ms'` 和 `unable to open database file`，原失败收据保留。后续脚本位置从 resume 读取，只有自有副本已停止且无 sidecar/句柄时才使用 immutable，运行中及有 WAL 的数据库保持普通只读。

用户授权清理历史 `/tmp` 测试数据后，18 份已结束后端日志经无损压缩、解压 SHA256 核验才删除原日志，释放 6,274,760,296 字节；另在确认无打开句柄后删除过期候选二进制和六份已结束测试快照的数据库，共 170 个文件、1,342,450,328 字节。小型报告、摘要及清理清单保留；旧日志应通过 `.log.gz` 读取，已删除的旧二进制/数据库不能再声称可重放。清单为 `/tmp/cm-evidence-compression-20261106.json` 和 `/tmp/cm-ended-history-cleanup-20261106.json`。当前构建缓存、最终产品、正式基线及待诊断的容量和恢复源均保留，原用户数据库与媒体未参与删除。

## 后续性能诊断与增量清理（2026-11-06）

完整 COMMIT 插桩轮已完成 4,500,001 次推进、前后规模/外键 oracle 与副本清理，退出 0。最大已覆盖写事务约 1.728 秒，主要处于 WAL `xSync` 墙钟区间，同库 writer 至少被持有相应时长；未测精确最大持锁时间。另有 PROFILE 后数据库同步长区间，设备等待与调度尚不能区分。该轮是 Integrity 诊断、`acceptance_eligible=false`，不替代正式容量门或归因历史峰值。首次第 41 步因 read ring 溢出的失败与后续聚合修正分别保留，见[COMMIT 观察](2026-11-06-commit-tail-observation.md)。

首次完整 busy 关联诊断的 gate 已完成全部 166 项，每项 200 个准入样本，业务 verified、stage 退出 1，性能结果仍有失败；随后 driver 在解析混写的 writer JSON 时退出，原收据保持 `complete=false`，未生成有效关联审计。原日志与性能报告保留在 `/private/tmp/crawler-perfgate-matched-candidate-1c3af0236c14/retests/causal-da68921a-reviewed-20261106T100015Z/` 及该 root 的 `backend-logs/1793960016213790000.log`。严格离线恢复未能证明完整拆分，因此没有补写派生通过结论。源码核查确认临时启动脚本把 stdout tracing 与 stderr 结构化事件重定向到同一文件；当时分流修复尚在验证，后续补跑结果见下。此前因工具摘要绑定拒绝的零样本尝试也独立保留，不计入完整诊断。

本次忙碌轮正常清理，自有 listener、owner、LIVE 均不存在，原 app/sidecar、STRM/pan 和 baseline 清单一致。关联审计缺失不能被业务 verified 或 report complete 替代，原正式相对失败不变。

用户再次要求检查 `/private/tmp` 后，确认 `/tmp` 是同目录链接。清理了无打开句柄的旧 Go 缓存，并对另外五份历史 backend 日志无损压缩、解压 SHA256 核验后删除原文本，回收逻辑字节 1,890,459,955。随后取得两把 Cargo 槽锁、确认无 cargo/rustc 进程和打开句柄，仅删除三处 Rust incremental 子目录，逻辑字节 16,952,378,165；编译产物、源码和测试事实仍保留。两步的可用空间增量分别为 1,965,051,904 和 15,860,408,320 字节，不能把逻辑体积当成 APFS 实际回收量。核验时 `/private/tmp` 约 33GiB、磁盘可用约 93GiB；同卷其他进程可能改变后续值。收据为 `/tmp/cm-history-cleanup-20261106T102325Z.json` 和 `/tmp/cm-incremental-cache-cleanup-20261106T102606Z.json`。旧 Go 只读目录首次删除失败后，仅将缓存目录设为当前用户可写并重试，失败记录保留。当前容量源、性能基线、恢复源、凭据和新原始日志均未删除，原用户数据库和媒体未参与清理。

### 分流修复后的完整补跑

临时启动脚本仅将 stderr 从合并的 `backend.log` 分到 `backend-diagnostics.log`，沿用同一个插桩二进制，产品及六份冻结性能工具均未修改。公开 subprocess 回归确定性复现旧合并写入会打断 JSON；失败后实现分流和审计，3 项 CLI 回归通过，覆盖真实 ANSI 格式的脱敏日志、缺失/损坏记录、重复编号、水印/丢失计数及 page-detail 缺少子请求的拒绝。父任务独立重跑 3 项均退出 0、核对源码/冻结工具摘要与独立静态复审后，补跑原 166 项顺序和每项 200 个样本。

新轮实际 559.563 秒，采集 driver 退出 0、`complete=true`、`capture_valid=true`；性能 gate 仍退出 1，业务 verified、所有绝对门通过，相对门 163/166。其中 played p95 5.395708ms（+332.09%）、progress p95 5.985708ms（+43.80%）、page-detail **完整响应** p95 1053.018334ms（+32.46%）未通过；page-detail 初始 bundle 的 8.735250ms 不是该相对门比较值。progress 保留额外一次窗口外尝试。本轮收藏相对门通过，不追溯解释或改写旧失败。此为插桩诊断，不能替代正式性能验收。

两通道完整审计及父任务独立复核均通过：257,337 条唯一 writer、10,174 条 Similar 事件、3,987 次目标请求、800 条 stdout 请求诊断、3,187 个 Similar caller，最终数量水印匹配、丢失为 0。原收据的 `diagnostic_valid=false` 明确保留逐阶段分析与采集有效性之间的区别，不能将其解释为采集再次失败。原 app/sidecar、STRM/pan 及 baseline 清单一致，自有 listener、owner、LIVE 和进程均已清理。原混写轮的失败与原始材料保持不变。

200 个 played 请求均能按 request ID 和采集样本 ID 唯一关联到同一 Library writer 实例。writer 等待 p95 4.778458ms，自有 COMMIT p95 0.059959ms；这是各自独立分位，不能相加。客户端 attempt 的 p95 样本等待 4.793709ms，其中与 scan 确定持锁区间重叠 4.786417ms，自有 COMMIT 0.034125ms；最大 attempt 样本等待 5.017875ms，scan 重叠约 5.010–5.012ms。按全部同实例候选区间的并集计算，200 个请求中 174 个有 scan 候选，另有 3 个 unknown 候选；支持本轮扫描竞争解释，不证明唯一因果，不定位到具体扫描子阶段。progress 没有同等逐请求仪表，原因仍 unknown。

推荐分析覆盖独立 Similar 的 193 个 pending 和 page-detail 的 147 个 pending 窗口；其余分别 7/53 个初始 ready。340 个同 key 窗口各观察到一组 worker/origin 边界及一次 404 attempt，未见重试；origin p95 分别约 909.796/968.457ms，后台 ready 到最后一次 poll 进入的 p95 约 103.812/97.050ms。HTTP 调用区间占主要耗时，但未区分本地 HTTP 客户端、网络与上游处理；key 与时间窗口不证明唯一请求所有权。初始 ready 样本不能填入虚构刷新阶段。监控期间曾误完整读取一次当时 stdout 文件，实际字节数 unknown，潜在时序干扰无法量化，已作为诊断限制保留；没有因此重跑或追认为正式通过。

新收据、报告、客户端事件和父核验在 `/private/tmp/crawler-perfgate-matched-candidate-1c3af0236c14/retests/causal-da68921a-separated-v3/`；两份原始日志为该 root 的 `backend-logs/1793962507692359000.stdout.log` / `.stderr.log`。`parent-verification.json` 绑定原始字节及摘要。采集工具与回归位于 `/tmp/cm-final-separated-capture-v3/`；逐请求分析为 `/tmp/cm-final-separated-played-analysis-union.json` 及 `/tmp/cm-final-separated-similar-analysis.json`，报告同名 `.md`（played 报告不带 `-union`）。本次只关闭混写采集缺陷，既有严格验收和尾延迟限制保持不变。

## 复核与交付边界

四个顺序项已分别提交推送：TMDB 有界重试 `e7a12033`、收藏争用诊断 `2f5cd923`、有界 COMMIT 观察 `590d96dd`、图片观察者截止及分段日志修复 `da68921a`。最终产品独立源码复核未发现新增必须修改项；本记录经独立数值/状态及页面证据复核，未发现必须修改的实质性不一致（`/tmp/cm-final-closeout-doc-review.md`、`/tmp/cm-final-page-evidence-review.md`）；性能失败复核尚无足够因果证据支持新的产品修复。没有改门槛、删除失败样本、把初始响应替代完整响应、调整 SQLite 持久化策略或资源常量。

结论仍为 **最终验收未通过**：当前三轮相对性能门失败；历史 COMMIT 尾延迟、有限内存归属、严格 Playback 额外第七行和原 driver 退出 Unverified 等保持原状。本文保存本轮可复核结果，不把未知项写成完成，也不进行用户实例切换、合并或发布。剩余定位应使用已建议的一次有界请求分段关联；目前没有证据把这些失败统称为代理问题。
