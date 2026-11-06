# 2026-11-06 存储重构最终候选验证记录

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

图片检查取得 poster、fanart、still、Library cover 四项 HTTP 200、`image/jpeg`；前三项完整字节与对应本地副本文件的 SHA256 一致，分别为 21,796、21,796、21,401 字节，cover 为 88,455 字节并实际渲染。夹具使用其既有占位画面，不代表真实影片海报或人物头像。抓到的 31 次静态图片请求均无 Authorization，应用数据请求使用独立测试会话。无章节帧的已知文件及不存在 poster 的无鉴权请求均为预期 HTTP404；**成功章节帧和真实慢源图片 smoke 为 Not run**，没有捏造对应夹具。受控超时、共享填充与缓存恢复的公开 Router 回归仍是独立自动化证据。

已捕获的 TV 与错误路径浏览器事件没有未处理 JavaScript 异常；5 条控制台网络错误均来自主动访问不存在 Library 的 API 请求。Movie 事件捕获不含 Runtime console 订阅，不能据此声称全程零 console error。正常页面网络事件、图片字节及截图保存在 `/private/tmp/cm-final-page-service-da68921a/`，主收据为 `browser-receipt.json`；`browser-actions.json`、`public-image-results.json` 与各 `events-sanitized.json` 记录实际范围。

页面副本与正式性能夹具分开：前两次准备分别因停止态 WAL 格式只读打开、发现待处理 probe 而退出，均未启动服务，副本未变、来源一致，原失败保留。第三次仅在 LIVE 副本按生产计数规则取消 23,669 个 active probe job/unit，删除 20,058 个 probe obligation，关闭两库 realtime watch，并禁用 8 个 job definition、取消 8 个 queued job；已成功的 removal 未改。停止态无 WAL/SHM/journal 才使用 immutable 只读检查，运行中的 DB 使用普通只读连接。仅选择有效章节缓存的 Movie 与 TV S01E01/S01E02，避免重新探测。结束前独立检查 probe job/unit、chapter frame task、probe obligation 均为 0，约每 5 秒采样时 enabled definition 与 queued/running job 均为 0，不证明任意瞬间均为空闲。

页面准备改变的是自有副本的后台工作状态，不能称其与性能工作量等价。`performance-slo.md` 明确保留必需 media-info probe，idle 仅表示没有 full scan；两份性能 baseline 的 queued probe 分别为 23,681/23,668、running 各 1，obligation 为 20,227/20,058。最终 idle/busy 全日志分别出现 896/1085 次媒体信息探测开始；这些计数不与具体请求关联，既不能否定正式结果，也不能证明性能失败由 probe 造成。业务 oracle 和字节绑定不证明后台负载相等，详见 `/tmp/cm-final-fixture-background-review.md`。

正常 TERM 后 DB-family 句柄、18766 listener 和 owner 均关闭，自有 LIVE 副本已删除，冻结 baseline、原 app/sidecar 及 STRM/pan 清单仍一致；`stopped.json`、`cleanup.json` 记录核验。测试密码和原始含请求头事件仅保存在私有临时记录，公开文档不含凭据。

## 容量与内存边界

历史 `61ee9207` 百万规模完整插桩轮完成 4,500,001 推进，100k Media、1M 文件、1M Playback 的前后 oracle 及外键检查通过；这是限定工作量的一致性证据。最大覆盖事务 2.735212417 秒，advance 的 COMMIT PROFILE 聚合 2.735 秒，因时钟粒度及 PROFILE 不覆盖后续 WAL callback，尚不能归因 checkpoint、fsync 或其他原因，也不能声称严格容量性能通过。`2f5cd923` 有界诊断及控制各 224,978 步、100,000 次 Users COMMIT 未见 >=100ms PROFILE，未复现或解释原峰值，且 `acceptance_eligible=false`。见 `docs/audits/2026-11-06-commit-tail-observation.md`。

六批 Scan、三批 Transfer 和 `cc7c997be6ff` 独立 heap 轮提供有限内存观察；后者 rescan 后 60 秒活 heap 12,220,032→12,202,000 字节，RSS 为 53,008 KiB。分配点分类不是完整所有权清单，有限窗口不能证明长期无泄漏；Transfer 未覆盖同一 Media 多版本，先前第二批 409 原因未定。合成容量工具的采样 RSS 不代表服务 RSS。

## 历史实播与未闭合项

`af40b58a` 的真实 491 来源轮取得 Movie/Episode 的 Web→VidHub→Web 播放、停止持久化和同库重启证据；`cd80d58e` 有限 Native 复验取得前缀、163 集身份和保存位置证据。它们均保留实际版本，不冒称最终 commit 新运行。后者严格六行 Playback 基线 Fail：未知操作者的真实播放新增第七条 unit/log/关闭 SID，终态 7/7/7/0；不推断操作者或归因代码。原 driver 因工具宿主中断退出 Unverified，独立受控 TERM 恢复退出 0 不改变原退出状态。见 `/tmp/crawler-real491-cd80-bounded-service-report.md`。

原引用的 2026-10-29 审核报告缺失，其 S-DB5、Z4、Z3、5.2 及 D1–D13 原始细项仍为 unknown，不据猜测标记完成。历史容量 COMMIT 尾延迟、内存长期归属、三轮性能相对失败仍未闭合；有限页面 smoke 已完成，不替代其余门。

历史实播受控恢复记录原 app/sidecar 和 STRM/pan 清单一致、测试连接及自有进程清理；本次三轮性能各自 source/cleanup 收据通过。页面 smoke 的独立来源与正常停机/句柄/副本清理收据均通过。

## 复核与交付边界

四个顺序项已分别提交推送：TMDB 有界重试 `e7a12033`、收藏争用诊断 `2f5cd923`、有界 COMMIT 观察 `590d96dd`、图片观察者截止及分段日志修复 `da68921a`。最终产品独立源码复核未发现新增必须修改项；本记录经独立数值/状态及页面证据复核，未发现必须修改的实质性不一致（`/tmp/cm-final-closeout-doc-review.md`、`/tmp/cm-final-page-evidence-review.md`）；性能失败复核尚无足够因果证据支持新的产品修复。没有改门槛、删除失败样本、把初始响应替代完整响应、调整 SQLite 持久化策略或资源常量。

结论仍为 **最终验收未通过**：当前三轮相对性能门失败；历史 COMMIT 尾延迟、有限内存归属、严格 Playback 额外第七行和原 driver 退出 Unverified 等保持原状。本文保存本轮可复核结果，不把未知项写成完成，也不进行用户实例切换、合并或发布。剩余定位应使用已建议的一次有界请求分段关联；目前没有证据把这些失败统称为代理问题。
