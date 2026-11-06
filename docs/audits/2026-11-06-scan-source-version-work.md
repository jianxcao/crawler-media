# 2026-11-06 扫描源版本同值更新

## 问题和修改

普通重扫会为新 operation 写入自己的断点。`record_scan_seen_stamps` 同时把版本写入 `library_files`，即使已存的纳秒时间、大小和内容摘要与本次赋值完全相同，也执行该行更新。

修改只在这三个字段至少一个不同时更新 durable 源版本。比较使用实际写入的 `mtime_ms * 1000000` 和空值安全的 `IS NOT`，因此仍会把带亚毫秒部分的旧时间推进到原先规定的毫秒精度。operation 断点仍在同一事务中写入，路径别名快照、执行顺序和回滚边界保持原语义。

## TDD 和工作量

新增公开 Store 集成测试 `scan_seen_work`。100 个已登记文件先写初始版本，再分别用新 operation 重扫同值版本、推进变化版本。原代码的两条路径均消耗 8,608 个 SQL VM steps，checkpoint 和 durable 读回正确，只有“同值路径应少做工作”的断言失败。

修改后，同值路径为 5,508 steps，比原来少 3,100（约 36.0%）；变化路径为 9,308，比原来多 700（约 8.1%）。这是所测操作的 SQLite VM 工作量，不能换算为物理写入量、锁持有时间或 HTTP 延迟改善。真实变化路径承担新增字段比较成本，此限制与收益一并保留。

三个新测试还检查：mtime、size、digest 分别变化，NULL 与摘要的双向转换，亚毫秒时间精确处理，重开数据库后的版本和断点，以及第 100 个 stamp 输入非法大小时整批断点和源版本回滚。断言通过公开 Store 读写接口完成；初稿的私有 SQLite trigger 失败注入在审查后删除。已有别名消失后的路径身份测试及扫描源版本测试共 12 项通过。

定向命令在两把 Cargo 槽锁内执行，设置 `TMPDIR=/tmp`，使用现存 `/tmp/cm-marker-build.a22APb` 缓存：

```sh
cargo test --offline -j8 -p store \
  --test scan_seen_work --test scan_recorded_keys --test scan_operations \
  -- --test-threads=8 --nocapture
```

定向结果为 3 + 1 + 11 项通过；修正公开失败注入后，新文件 3 项再次通过。独立静态审查无剩余发现，记录在 `/tmp/cm-scan-seen-test-review.md`；RED 二进制为 `/tmp/cm-scan-seen-work-red-f67b4229`。随后 Store 全套实际退出 0：208 个结果组汇总 1,038 passed、0 failed、12 ignored（含嵌套结果，非去重数）。`cargo clippy --offline -j8 -p store --all-targets -- -D warnings`、`cargo fmt --all -- --check` 及 `git diff --check` 均退出 0。全套原始日志和汇总分别为 `/tmp/cm-scan-seen-store-full.log` 与 `/tmp/cm-scan-seen-store-full-summary.json`。

## 与剩余验收的关系

此修改关闭的是已证实的同值更新工作浪费。此前分流诊断中的 played writer 等待与 scan 区间重叠，没有定位到本函数，不能据此宣布该慢尾或 progress 相对门已修复。原正式相对失败、首次上游推荐的完整响应耗时及 FULL 同步尾延迟仍按原记录保留。

源码与现有数据复核还排除了两项猜测：两版 TMDB 都持久缓存 typed 404 七天，分流轮的 340 个 pending 窗口对应不同 key；正常已绑定的 self/Jellyfin progress 和 played 均只取得一次 Library writer，没有发现新增重复 summary reduction。实际失败的 progress 用例是 self API，明确携带 duration，不会进入缺少 duration 的 probe 入队分支。这些结论不证明全部请求无其他问题，也不把外网 HTTP 区间归因于某个网络组件。只读报告分别为 `/tmp/cm-similar-source-comparison.md` 和 `/tmp/cm-progress-source-comparison.md`。


## 普通二进制实际 busy 复验

修复提交 `342224a70d969a8a1059e046cd2ed5b742de63bc` 已推送当前分支。刷新构建戳后，受两把槽锁保护的 `cargo build --offline --release -j8 -p api --bin crawler-media` 实际退出 0；二进制 SHA256 为 `107c482179eedd4b23363634161dabdeb2975554c660f8062eefb1e14942c653`，没有加入诊断插桩。启动后 health 精确返回 `342224a70d96`，另核对进程出生时间、可执行文件、监听端口和独立 app.db。

沿用冻结的六份测量工具、原 166 项顺序、每项 200 样本和原 busy 基线；未改 SLO、准备请求、后台工作或样本筛选规则。约 626 秒后完整结束，driver 与 gate 均退出 1，业务 verified，所有绝对预算通过，相对门 **164/166**。以下是本轮结果，不能与此前诊断拼成同一轮，也不能只据两轮差异归因于本次 SQL 修改。

| 用例 | 本轮 p95 ms | 相对基线 | 本轮判定 |
| --- | ---: | ---: | --- |
| 收藏写入 | 1.168500 | +22.75%，绝对增量低于 1ms | 通过 |
| 已看写入 | 1.425291 | +14.14% | 通过 |
| 进度写入 | 3.248959 | −21.95% | 通过，200 个计时样本且公开读回持久化成立 |
| 推荐完整结果 | 1211.775208 | +36.06% | 相对失败 |
| 详情页推荐完整结果 | 1298.249500 | +63.31% | 相对失败 |

推荐和详情页初始响应 p95 分别为 0.770916 / 8.726959ms，不能替代上表的完整结果指标。两项 HTTP/响应校验通过，但仍保留原相对失败；此轮并未宣布全部性能通过，也未确认网络、代理或上游为唯一原因。既有 idle/cold 的版本限定结果继续按原记录保留，本次仅复验 busy。

物理 fixture 仍来自 `1c3af0236c14`，没有重新 seed；新的独立 manifest 只更新产品身份、二进制摘要及 business artifact 描述。按现有 oracle 合同，artifact 的 `fixture_commit` 表示被验证的目标产品，不冒称其为物理 seed 提交；原 `source_snapshot` 完全相同，新旧 artifact 的用户期望逐项相等，cases 原始字节相同。oracle 从新提交的干净源码副本运行，六份 canonical 工具与冻结副本逐字节一致。

回执 `evidence_valid=true`、`report_complete=true`：原 app/sidecar、STRM/pan 清单和冻结 baseline 匹配，所有绑定输入复核通过，服务正常停止，listener/owner/live 均不存在；独立副本已删除。父任务重新核对 166×200、全部绝对判定、进度持久化、cases、source_snapshot、用户期望及五个证据文件摘要。原始目录为 `/private/tmp/crawler-perfgate-matched-candidate-1c3af0236c14/retests/scan-source-version-342224a7-busy/`，包含 `receipt.json`、`report.json`、`parent-verification.json`、独立 manifest 与 artifact。

临时驱动在实际运行前经独立静态审查；五项局部失败路径回归退出 0，但服务停止只用 fake 验证调用，未验证真实服务归属，真实生命周期以上述实际轮回执为准。早期测试夹具路径错误及短命子进程组探测的间歇 EPERM 保留为工具验证限制；EPERM 不视为进程已退出。通过后编辑的临时测试增强版未运行，不能用于扩大五项通过的范围。冻结工具和生产代码均未因这些工具检查而修改。


### 剩余推荐失败的离线核对

Similar 的 200 个窗口内样本为 193 个初始 pending、7 个初始 ready；详情为 148 / 52。400 个推荐观察全部终态 ready/HTTP200、推荐列表为空，两项没有窗口外样本。与本轮时间一致的新后端归档 `1793979240200512000.log` 分别记录 193 个 Movie 和 148 个 TV Similar 的首次尝试 404，341 个 `(kind, TMDB ID, language)` key 均不同，数量及时间顺序与两组 pending 吻合。日志缺少请求起点和逐样本关联，不能据聚合数量认定每个请求的耗时归属，或进一步分解网络、客户端、排队、缓存和轮询耗时。

源码的 pending 去重 key 为 `(MediaKind, TMDB ID)`，客户端持久缓存另带 language；typed 404 会缓存并映射为 ready 空列表。当前证据没有确认去重失效或其他产品缺陷。`performance-slo.md` 对实际首次外网回源与缓存读取有不同验收范围，但这些聚合证据不足以把整组样本重新分类，原 gate 退出 1 不变。离线报告为 `/tmp/cm-scan-seen-remaining-recommendations.md` 和 `.json`。


### 明确命中空推荐缓存的定向补测

随后用同一普通 `342224a7` 二进制分别启动新独立 idle / busy 副本，固定一个 Movie 和一个 70 集 TV，追加两个 `cachecheck-*` 临时用例。原 166 项及六份工具逐字节保留，两个新用例沿用对应原用例的请求集合、class、预算和 50 用户轮换，只改变 ID、缓存标记及固定目标 URL；运行时用 `--only cachecheck- --n 200`。没有提供旧 default 范围基线，实际 `relative_status=not_requested`，不是相对门通过。

启动前的只读检查确认两目标有 TMDB 别名且原 baseline 的对应 Similar 缓存不存在。每轮先对两条精确 URL 等待 ready/空列表，记录准备状态和完整耗时；本轮服务日志分别确认对应 TMDB Movie/TV 路径的首次 404。计时期间额外要求所有推荐观察（包括任何窗口外尝试）均为初始 HTTP200/ready、零 pending、终态 HTTP200/ready/空列表；pending 会使本补测失败，不能被 gate 的成功遮盖。

| 模式 | 推荐初始 p95 ms | 推荐完整 p95 ms | 详情初始集合 p95 ms | 详情推荐完整 p95 ms |
| --- | ---: | ---: | ---: | ---: |
| idle | 0.636583 | 0.647000 | 4.608542 | 4.623708 |
| busy | 0.943708 | 1.188000 | 6.368209 | 6.617084 |

两轮 driver/gate 均实际退出 0，各项 200 个有效样本，800 个计时观察全部 ready、零 pending、无窗口外样本，原绝对预算和响应检查通过。每轮来源、原 app/sidecar、冻结 baseline 及绑定输入一致；listener/owner/live 均已清理。此切片没有选中业务 oracle 用例，报告 `business_status=null`，不冒称新跑了完整业务断言。也没有运行 cold、相对比较、非空推荐或其他 Media 的缓存测试。

该结果只证明两个固定 typed 404 空推荐缓存的读取与页面 API 集合预算，不证明先前全轮慢尾的唯一根因，不重分类旧样本，不替代原 164/166 的两项相对失败。独立审查为 `/tmp/cm-similar-cache-followup-review.md`，父任务逐观察及五类文件摘要核对在 `/tmp/cm-similar-cache-followup/parent-verification.json`；实际目录为前述 `retests/scan-source-version-342224a7-cache-{idle,busy}/`。两轮使用相同源码和普通二进制，没有新增产品修改。
