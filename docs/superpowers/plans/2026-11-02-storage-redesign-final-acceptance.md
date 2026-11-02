# Storage redesign 最终验收执行清单

状态：已进入最终验收；尚未验收通过，不执行用户实例切换、合并或发布。

本清单承接 [审核修复计划](2026-10-29-storage-redesign-fixes.md)，验收合同为 [原设计第 8–9 节](../specs/2026-10-18-library-storage-redesign.md)、[性能 SLO](../../testing/performance-slo.md)、[系统回归](../../testing/system-regression-cases.md) 和 [E2E 手册](../../testing/e2e-runbook.md)。原设计 H1–H6 有历史修复记录，不把旧 unchecked 或历史状态当成重新实施的理由。

## 1. 分工和版本

- Codex：代码与 Task 对账、单元/集成及性能工具验证、idle 证据核对、修复实际发现的问题。
- 按原分工，由对应执行者完成 busy/cold、页面/E2E、VidHub/实际设备和现场验证；本清单不代表这些项目已执行，也不向其他执行者自动发送消息。
- 功能代码基准为 `b2b902a0`；后续文档和 `stress-up.sh` 构建并发修正不构成新功能实现。新一轮实际运行版本为 `df8c50bdf2e3`，结果索引见本地 `.e2e/perf/final-20261102-summary.md`；所有历史报告保留真实 health commit，不改写为新 HEAD。
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

## 3. 必须保留的未完成项

- [ ] 取得 `docs/superpowers/specs/2026-10-29-storage-redesign-review.md` 后核对 S-DB5 逐列 CHECK、Z4 六个函数、Z3 image-proxy 精确行为、报告 5.2 及 D1–D13 的未知细项/桩文件。收尾已执行 `git pull --ff-only`，报告仍缺失；不猜清单、不标全完成。可选优化与必须完成项分别按报告原要求判定。
- [ ] 当前候选的完整 idle 仍需判定：保留阶段 D 两份完整报告及后续定向验证；新增当前构建每项 200 样本的完整报告，仍非全绿。Similar/详情集合中历史 ready 与本轮 pending 后回源的条件差异，以及人物缓存相对失败均保留在本地证据中；不能拼接成“一轮全部通过”，也不提高预算或删掉失败样本。
- [ ] busy/cold 当前候选与当前缓存口径重测。现有 `phaseC-busy.json` / `phaseC-cold.json` 是旧提交且失败，不能替代当前版本结果。
- [ ] 页面/E2E、真实客户端、NAS/实际来源、完整容量及故障恢复证据按下节补齐；未运行明确写 Not run。

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

## 5. 已取得的局部规模与内存证据

- 现有 Store release 集成测试已验证：无关库扩大到百万条，固定目标库的同页输出不变、Fullscan/Sort 为零，VmStep 不增长。
- 现有 release 查询矩阵已验证 5k 与 100k 规模：固定 Movie 库的类型读取不随无关 TV 增长放大；简单分类分页工作量稳定。精确交集及 facets 的实际线性成本另记，不推广成所有查询 O(1)。
- 独立实例已记录从空库入库、用户数据准备、完整 idle 到结束后静置的 RSS 与 vmmap；物理占用有明显回落。此夹具关闭实时监听，不代表 active watcher、目录批事件、错误密集扫描或实际 Transfer 的内存门已经通过。
- 这些结果只关闭对应的局部测量缺口；上节完整容量、业务矩阵和外部执行项继续待验。原始日志与数字只保存在未提交的 `.e2e/perf/`。

## 6. 资源、报告与交付

- 单 cargo/test/build，`-j4`、`-- --test-threads=4`；用 `/tmp/cm-cargo.lock` 的 mkdir 锁串行协调。先定向，稳定功能树 workspace 全套仅一次；仅改本清单/文档/构建并发参数不重建全套 debug 缓存。
- 独立临时目录 `/tmp/crawler-storage-fixes`。使用非默认夹具根时，给 perfgate 的临时 cases 配置同步替换 `fs/browse` 的固定目录，避免把旧 `/tmp/e2e-stress/media` 的 404 当功能失败。不修改 SLO 或历史报告。
- `stress-up.sh` 启动前先完成受锁保护的 release 构建；启动时设 `PERF_SKIP_BUILD=1`，避免脚本再构建。远程读并发 1、冷却 500ms、Library readers 8 不改。
- 所有原始结果留 `.e2e/perf/` 或 `docs/test-reports/`，均不提交；密钥只内部传递，不打印，也不放命令行参数或报告。用例结果必须区分 Pass / Fail / Not run。
- 当前 256G 磁盘不同时保留多套 debug/release 大规模夹具；容量测试分批准备，先评估 DB/WAL 峰值，空间不足记录 Not run，不删除用户媒体来腾空间。
- 每次运行只停止自有进程、清理自有临时文件；报告保留。结束检查端口、git status、磁盘及分支同步。
- 所有必需门及报告依赖闭合，或由用户明确接受具体遗留项后，才能宣布最终验收通过。当前不合并、不发布、不切用户实例；现有授权只允许提交推送 `feat/storage-redesign`。
