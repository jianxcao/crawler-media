# 存储复审修复执行记录

状态：2026-10-24，用户在只读复审后明确授权修复。实施基线 `feat/storage-redesign@96f7445`，本文与实现同提交发布；原问题代码基线为 `cf7fb70`。原始发现保留在[固定版本复审](../../audits/2026-10-24-storage-rereview.md)，5k–100k 数据形状和结果见[测量记录](../../benchmarks/2026-10-24-storage-query-rereview.md)。本文件是唯一当前执行状态入口，原只读技术方案可用 `git show 96f7445:docs/superpowers/plans/2026-10-23-storage-review-remediation.md` 读取。

五文件边界、显式归属、事实/摘要同事务、冷详情与持久意图继续使用。旧数据库与旧自有 API 不提供导入、转发 shim、双读写或字段别名；静态图片 public，Jellyfin 保留协议鉴权及合法行为。

## 修复对应表

| 发现 | 已实现 | 公开行为/工作量证据 |
|---|---|---|
| S01 / P1 | Jobs 独占唯一 DDL/身份/只读预检，单 IMMEDIATE 事务创建全部对象、单例和双头；重开校验定义，不修补旧布局 | 表/索引/触发器/单例/两头故障整体回滚可重试；损坏/未知/未来布局零写拒绝；独立 Queue→ApiState 保留在途 Job |
| S02 / P1 | 根队列有限窗口 driver、通知+兜底、公平轮转、稳定 operation/revision/state/error；配置和 Watch/filesystem 观察移锁外 | 真实 ApiState 冻结→解冻后无手动 drain 收敛；blocked A 不挡 B；释放实例后预算可回收；301 文件/NFO跨批重开守恒 |
| S03 / P1 | 删除 directory 聚合/roots 写、旧 root setter、重复 Naming 表和组合拆解；app 只读拒绝额外退役表；Web/CLI同批切当前接口 | Transfer/Watch 严格 PATCH revision CAS，null 清空、旧 body/route 拒绝；根操作202/轮询；Naming四段设置 |
| S04 / P1 | Self progress 必填签发 session/sequence；签发失败500，不返回空ID或旧流地址；删除无绑定暂存与补当前源路径；Jellyfin 适配层先绑定后提交 | 缺失/伪造/错设备身份400零写；旧r1事件不污染r2；Jellyfin无ID新Playing在换源后签发r2、Progress/Stop不凭空绑定 |
| S05 / P2 | 删除 Downloader settings JSON、SavedDownloader/from_saved；移除确定不可达的 nullable 播放日志分支及失效 browse 路径谓词 | 仅旧配置 key 不再选出 Downloader；环境覆盖/当前默认行保留；SQL 按显式 library_id 查询 |
| S06 / P2 | 删除先短事务 freeze，按目标 Library 索引逐100文件持久快照/清理，API批间释放锁 | 空A+坏B不耦合；301文件每批100+重开；配置变更拒绝且零删除；保留原始用户事实及磁盘字节 |
| S07、S10 / P2 | Unidentified 登记独立 ID/path_key/library/root/revision；SQL按库过滤，Claim发布同事务CAS消费原来源；候选按不可变ID索引seek，正确新记录不重置游标 | TV列表/过滤正确；Movie停放→显式TV Transfer合法；NFO观察期间来源更新、删后重停ABA、Move复制后源替换均拒绝且保留新来源；352行重开/游标前新增守恒及下一批继续推进100行；10万前缀+10万无关窗口VM固定10154 |
| S08 / P2 | 同事务dirty card批末同步协议terms/search；修复整批最后可用文件转pending的摘要边界 | 100文件/100演员 INSERT10000→100、DELETE9900→0，最终人物/分类/搜索语义保留 |
| S09 / P2 | kind先收窄授权库，facets单快照维度矩阵；genre名称精确谓词；完整page/count/辅助SQL统计与HTTP锁/预算日志 | fixed Movie+无关TV工作量稳定；filtered facets 11语句→1；release5k/20k/50k/100k精确total/多维/All/kind曲线入库 |

## 关键合同

- `GET/PATCH /settings/transfer`：独立实例设置，PATCH 必填 expected_revision；未提供字段不改，Watch null/空白清空，错误请求零写。根只通过 Library operation，Naming 只通过当前 Scrape 四段设置。
- 根重配置的主 Library operation 成功表示配置及目标库内部重配完成；新增覆盖其他 Library 的根还会登记跨库归属队列。完整收敛同时观察 Library DTO 的 `root_reassignments` 到 succeeded。状态持久且可重启查询，不能只把 202 或配置成功当成所有归属完成。
- 每个根/删除窗口最多处理100文件或100 Unidentified候选，API driver批间释放 Store并yield；冻结根轮转，失败保留状态/错误并允许重试。取消/退出不承诺中断已经在磁盘或 SQLite 中开始的单条操作。
- Claim 固定待认领来源的独立 ID 和登记 library/root/path/revision/config；明确动作可选择不同最终 kind/Library。来源消费、文件事实、覆盖及摘要在一个事务提交；观察或提交失败不会丢来源。Transfer 在复制前固定原源版本、复制后固定目标版本；发布与 Move 删除前复核，拒绝删除后出现的替换源，回滚仅清理本次相同版本的目标。版本观察与外部进程的路径替换不构成跨文件系统事务，检查到删除之间仍依赖目录写入协调，不能宣称任意外部改名都被原子阻止。
- Self Playback 只提交签发绑定，坏身份400；已签发但失效绑定执行 IgnoredStale。Jellyfin协议分类下沉适配层；合法客户端不依赖自有旧入口。
- 精确 total 和大命中 facets/genre 仍有规模成本。QueryWorkStats记录完整调用内 SQL 工作量，HTTP日志同时记录等待、持锁和查询时间；不能把100行页面或0 fullscan当成常量请求成本。

## 验证记录

公开行为先 RED→GREEN；各模块以 fake Fetcher/Downloader/Probe、临时数据目录和本机SQLite验证，无实时TMDB、Downloader或Chromium依赖。原先要求旧入口成功的夹具已显式迁移当前API/签发会话，没有在通用HTTP helper补身份。

- 单 crate：Jobs 61通过/1忽略、Store 523通过/4忽略、API 1599通过/6忽略，均零失败。Store 完整门后追加的 app 退役表校验定向4项通过（其中3项与原门重复）；当前版本以最终 workspace 总计为准。
- `cargo test --workspace`：2749通过、0失败、14忽略，405个结果汇总；最终完整日志 `/tmp/storage-final-workspace.log`。测试文件按合同职责拆分后，`cargo test -p api --test storage_end_to_end` 额外5项通过（与全量门重复，不累加）；格式、Rust文件/函数硬上限、diff空白与本地文档链接检查通过。
- Web：`pnpm test` 845通过；`pnpm typecheck`、`pnpm build`通过，build有既有chunk-size提示。
- 性能：release 5k–100k完整矩阵通过；bundled SQLite3.50.2，含首轮共7次，报告p50/max，不把max当可靠p95。Unidentified公开drain工作量5k/50k/100k前缀+同规模无关数据均10154VM。
- 测量源码、方法和详细统计见[测量记录](../../benchmarks/2026-10-24-storage-query-rereview.md)，结构来自[唯一生产DDL/字典](../../database/README.md)。

## 剩余现场验收

以下是单独的环境/容量验收门，不是本次仍保留的已知旧逻辑：真实浏览器/Jellyfin设备、NAS慢盘、完整停启/清cache、100k Media/1M文件/1M Playback组合、WAL/busy/RSS和单变量连接参数A/B尚未现场执行。当前合成查询数据不能替代这些场景。

没有依据修改 synchronous/cache_size/mmap/temp_store/WITHOUT ROWID；保留现有明确耐久策略。S09的精确查询随有效命中规模增长仍是后续测量方向，不改近似total，也不引入长期缓存掩盖一致性。

本次修复使用隔离临时数据库，没有部署、清空或重建用户实例，没有推送分支。历史A8竞争写者、A12NFO区分性及原测试退出反例不在本轮虚报通过；已通过回归与现场未执行项分开记录。
