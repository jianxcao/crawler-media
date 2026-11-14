# 性能闸门

使用 release 构建和独立实例测量，默认压力实例为 `http://127.0.0.1:18766`。报告记录硬件、系统、产品与工具构建、配置、数据和数据库摘要，并核对 `/api/v1/health`。正式测量期间不运行其他构建/测试；无法排除其他负载时记录干扰，结果只作诊断。

整体测试流程、容量与内存验证见 [存储性能测试](storage-performance.md)。本文定义可重复测量口径，不保留固定提交的性能排行榜，也不代表某个构建已经达标。

## 数据集与准备

- `gen-stress.py` 默认生成 3000 个 Movie、300 个 TV Media × 70 集，共 24000 个 STRM，附 NFO。目标是本机不可达端口，避免外网媒体流量。
- `seed-activity.py` 经 API 创建和登录 50 个测试用户；每人 300 条进度（100 完成、200 部分播放）、100 条收藏、5 个各 50 项的收藏夹、400 条播放记录。记录数和每用户分布需再次核对，不能仅依据 seed 退出码。
- 固定数据包括 Movie/TV、NextUp、绑定压力 TV Library 的 Subscribe，以及本地海报/背板/剧照。可写图片使用 `/tmp` 自有副本，不硬链接到仓库素材。
- 关闭可选片头检测、声纹、章节截图、实时监听和缩略图工作。`stress-up.sh` 在首次启动时通过 Jobs API 停用全局 Scrape；restart 沿用已保存状态。必需 media-info probe 仍按并发 1、冷却 500ms 运行。
- perfgate 的 idle 是“没有 full scan 正在运行”，不保证 probe 队列为空。报告须另列后台队列状态；需要完全空闲对照时，先明确并核验该前置条件。
- `CRAWLER_MEDIA_TOKEN` 和 `PERF_USER_PASSWORD` 从私有环境提供，不输出认证值。活动播种的检查点需保护；崩溃可能发生在成功响应和落检查点之间，因此恢复后检查重复记录。

压力数据用于公开 HTTP 性能，不替代 100k Media / 1M 文件 / 1M Playback 的组合容量门。对比双方按各自正式 DDL 独立准备等价数据，不能把一份数据库直接复制给不匹配的布局。

## 测量范围

严格毫秒预算用于本地数据库、已命中缓存、已存在本地图片及其组成的页面 API 集合。实际首次外网回源另记耗时，并检查正确性、超时和有界资源；后续缓存读取仍受严格 SLO 约束。不能因为端点有回源能力就豁免全部请求，也不能根据慢响应猜测发生回源。

`perf-cases.json` 的 `cache_read:true` 表示外网数据缓存读取：先执行同 URL 的准备请求，完整保存 `cache_preparation` 的状态、耗时和响应校验，再严格计时。准备失败进入 `precondition_codes` 并使整项失败。cold 在重启前准备持久缓存，重启后只做健康检查再测第一次读取。本地 Library/Jellyfin 投影、计数、列表、搜索与本地图片不加准备请求。

报告以 `latency_scope:cached` 绑定缓存口径。不同缓存条件不可直接比较 p95；需先建立同口径有效基线。缓存标题搜索须确认目标提供方成功且结果非空；包含提供方错误的 HTTP 200 不算通过。

Similar 分别记录首次 pending/ready 响应与完整 `ready.items` 的等待时间、请求数和终态。不能把快速 202 当成完整推荐性能，不能把上游错误转换成空成功。typed NotFound 的空推荐缓存与首次真实回源、非空推荐是不同场景，应分别报告。

## SLO（毫秒）

| Class | 接口 | idle p95 | idle p99 |
|---|---|---:|---:|
| W 交互写 | 心跳、进度、标记已看、收藏、搜索历史 | 50 | 150 |
| D 详情 | Self 详情、70 集分集列表、Jellyfin Items 详情、PlaybackInfo、静态图片 | 100 | 250 |
| L 列表 | 50 项列表及 sort/filter、Latest、NextUp、Resume、up-next、favorites、collections、missing | 150 | 400 |
| S 搜索 | Library 标题、前缀、演员及 Jellyfin SearchTerm | 200 | 500 |
| A 聚合/管理 | facets、观看统计、jobs、activity、sites | 300 | 800 |
| U 鉴权 | auth/me、UserViews、用户资料、System/Info/Public | 100 | 250 |
| U* 登录 | auth/login、Users/AuthenticateByName，含密码哈希 | 500 | 1000 |
| P 首屏 API 集合 | 首页、Library 页、详情页初始 API 的墙钟时间 | 800 | 1500 |

P 类按 Web 当前调用者维护请求集合，详情文件续页按实际游标继续并计入墙钟。它不包含浏览器渲染、图片排队、懒加载和白屏，页面必须独立验收。

## 覆盖、正确性与独立期望

`list-routes.py` 从 API 和 media-server 路由提取 method/path/surface；`/emby` 是同一 router 的挂载别名，不重复计数。`perf-cases.json` 为 GET 路由配置实际用例或理由明确的豁免。视频流、SSE、WebSocket 等不能强行套普通 JSON 测量；有限字幕与静态图片不能因准备困难而豁免。

路由登记通过只是覆盖检查。缺资源、404、准备错误、超时、无有效样本和响应业务不符都不能通过。Jellyfin 70 集详情须确实含 70 项；不支持的排序不能改名成另一排序后冒充覆盖。每次修改路由或 Web 初始请求集合后重新核对覆盖。

Genres、Studios、两种 Items/Counts、指定 Movie Library 的 Items/Latest，要求计时前加载独立业务期望。`perf_business_oracle.py` 要求已停服、已完成播种且有来源清单的 fixture，并校验 DDL、外键、数据与工具摘要。它的布局准入和固定 DDL 摘要目前不匹配当前生产定义，不能直接生成当前数据库的正式期望；再次完整验收前须先适配并验证该工具，不能绕过身份或结构检查。`business_expectations` 用绝对路径、合同版本和 SHA256 绑定产物。不能从被测投影反推期望。

期望按 User 的实际 Library 权限，从原始文件/单元关系计算 distinct 作品、集数和授权成员。Items/Latest 检查总量、页长、唯一性与成员归属；这不证明 Latest 精确顺序。Genres 对照来源/override 和持久显示字段；Studios 按显示字段与 NFO 的明确约定判断。多来源、locale、Missing/Empty 矛盾或证据缺失保持 Unverified，不能把未知当空集合。采纳流程本身还需独立回归。

报告绑定实际 cases 配置、选定用例和工具摘要；对比必须具有相同业务合同。夹具绝对路径仅在工具明确支持的范围内归一化，不能忽略其他配置差异。工具 CLI 测试通过不代表真实 seed 或业务 oracle 已通过。

## 采样与判定

- 默认每项 40 次，轮换 50 用户；本地用例从真实数据集取 Movie/TV，缓存读取按已声明的固定来源与查询取样。客户端在线程内复用 HTTP/1.1 连接，普通请求的认证准备在计时外，登录单列。
- 完整读取响应正文后停止计时，并执行业务校验。分位数使用 nearest-rank，记录 p50/p95/p99/max、全部尝试状态码、准备错误、无效样本、完整/未完成状态和退出码。
- **idle**：p95/p99 同时满足上表，非 2xx、超时及业务失败为零。
- **busy**：full scan 运行前后均确认 running；p95 不超过 idle 目标的两倍。跨扫描完成边界的成功样本记入 `invalid_samples/outside_busy/attempts`，补足有效窗口样本；默认 40 样本最多尝试 80 次。任何窗口内外 HTTP/准入错误保留并失败，不为错误补测。报告样本和尝试不能混淆。
- **cold**：每个用例前正常重启，只以 health 检查就绪，认证 token 提前签发。所需会话在重启前准备，重启后直接测；首次完整墙钟不超过 idle p99 的两倍。每项单样本不能当作 40 样本 p95，也不代表 OS 缓存冷。
- **相对门**：同条件、同模式的 p95 退化超过 20% 且绝对涨幅达到 1ms 即失败，即使绝对 SLO 通过。基线必须完整、样本有效、HTTP/业务正确，并包含全部选定用例；连接失败的零/极短延迟不能作为基线。基线绝对预算失败与基线证据无效是不同判断。
- **真实写入**：进度使用递增事件序号，计时后经 resume 回读确切位置，保存 `write_verified`。200/204 的空操作不算通过。
- **统计信息诊断**：同一进程导入后 idle 与重启后 idle 用相同数据/后台负载比较，超过两倍仅作为统计信息过期候选，需结合计划确认；不能拿 cold 单样本或不同外网条件直接归因。

不得提高门槛、丢弃失败或把不同独立轮次拼成一轮全绿。聚焦诊断、插桩轮和有监控干扰的轮次明确标为非正式验收。

## 执行入口

以下命令均从仓库根执行。先受锁构建，再设置 `PERF_SKIP_BUILD=1`，避免 `stress-up.sh` 内置构建使用不同并发参数。默认目录 `/tmp/e2e-stress` 必须确认归属本轮且没有存活实例；自定义根还需同步 `--manifest` 和临时 `--cases` 中的文件浏览路径。

```sh
python3 scripts/with-build-lock.py --exclusive cargo build --release -j8 -p api
python3 -m unittest discover -s scripts/e2e -p 'test_perf*.py'
python3 scripts/e2e/perfgate.py idle --dry-run
PERF_SKIP_BUILD=1 zsh scripts/e2e/stress-up.sh /tmp/e2e-stress
```

首次压力实例启动和播种不自动生成正式 oracle。生成器的参数帮助只列命令选项，完整准备合同见 [生成器源码](../../scripts/e2e/perf_business_oracle.py) 和 [CLI 契约测试](../../scripts/e2e/test_perf_business_oracle_cli.py)：停服快照目录为 `baseline/data`，相邻阶段目录需有 `seed-complete` 和基线清单且无活动 owner；manifest 绑定产品、来源文件、来源树、用户和工具摘要。应先适配当前 DDL 的准入与期望推导并通过正反例，再准备相符的停服快照与独立业务期望，启动工作副本并核对身份。工具适配或任一前置证据未完成时，当前树的完整性能门保持 Unverified，不能跳过校验或仅用 dry-run 充当通过。

准备完成、正式测量获得独占资源后：

```sh
python3 scripts/with-build-lock.py --exclusive python3 scripts/e2e/perfgate.py idle --out /tmp/e2e-stress/idle.json
python3 scripts/with-build-lock.py --exclusive python3 scripts/e2e/perfgate.py busy --out /tmp/e2e-stress/busy.json
python3 scripts/with-build-lock.py --exclusive python3 scripts/e2e/perfgate.py cold \
  --restart-command 'zsh scripts/e2e/stress-up.sh /tmp/e2e-stress restart' \
  --out /tmp/e2e-stress/cold.json
```

`--n` 改采样数、`--only` 按用例 ID 前缀选择、`--baseline` 指向同模式有效报告；`--out` 不能覆盖已有文件。cold restart 命令用参数数组执行，不经过 shell 展开。restart 仅停止和启动同一隔离实例、检查 health，不重建、不扫描、不播种。

结束后正常停止本轮 PID、核对端口/句柄和来源清单，交付报告再清理独立临时目录。`.e2e/`、`docs/test-reports/` 与私有凭据/报文不提交。
