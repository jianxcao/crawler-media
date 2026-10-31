# 性能闸门

测量环境为 Mac mini、release 构建、由 `scripts/e2e/stress-up.sh` 启动的独立实例 `http://127.0.0.1:18766`。修复基线使用 `4c507440`；该提交只改文档。其他端口的日常实例不参与测量。每次报告记录服务 `/api/v1/health` 返回的 commit，不能用另一份二进制冒充当前版本。

## 数据集

- `gen-stress.py` 默认生成 3000 部电影、300 部 TV Media × 70 集，共 24000 个 strm，附带 NFO。strm 指向本机不可达端口，不产生外网媒体流量。
- 50 个固定用户名 `perf-user-01` 至 `perf-user-50`；全部通过公开 API 创建和登录。
- 每个用户 300 条进度：100 条看完、200 条部分播放。60 部 TV Media 各使用前四集，加 60 部电影；40 部电影和每部 TV Media 的第一集完成，产生跨 60 部 TV Media 的 NextUp。
- 每个用户 100 条收藏、5 个收藏夹（每个 50 项）、400 条播放记录，共 20000 条记录。先写 100 条记录，再遍历 300 个目标写最终状态，保证最终进度的完成比例。
- 一个绑定 `stress-tv` 的 Subscribe；关闭片头检测、声纹、章节截图、实时监视及缩略图生成。必需的 media-info probe 仍会排队、运行，并按现有远程读闸门访问本机不可达 strm 目标；基线实际观察到大量这类任务。`idle` 表示没有全量扫描运行，并不表示后台队列完全静止。`REMOTE_READ_LIMIT = 1`、远程读冷却 500ms 保持不变。
- 本地海报、背板、单集缩略图来自仓库封面素材的 `/tmp` 副本，硬链接到数据集目录。不会回源 TMDB，也不会把可写数据集硬链接到仓库文件。

首次启动脚本在创建压力 Library 之前，通过公开 Jobs API 停用可选的全局 `scrape` Job；仅匹配该 kind，不停用必需的 media-info probe，也不影响 perfgate 主动触发的 full scan。`restart` 不重新配置 Job，使用已持久化状态。原因是全局 Scrape 会为每集所在的 Season 目录补图，在虚构 catalog ID 下产生大量重复回源，且退出时其阻塞 worker 不响应取消。

`seed-activity.py <base_url> [admin_token]` 全部通过 API 写入，用户名固定；已存在的用户和收藏夹复用。优先通过 `CRAWLER_MEDIA_TOKEN` 环境变量提供 token，避免出现在进程参数中。使用 `PERF_USER_PASSWORD` 指定隔离实例的测试用户密码。脚本先顺序登录，避免密码哈希服务的并发准入上限，再用最多 8 个独立用户工作线程写入。

临时清单位于 `/tmp/e2e-stress/perf-dataset.json`，只存真实 ID、不存凭据；检查点位于同目录 `seed-checkpoints/`，权限 0600，可从已确认完成的 API 操作继续。崩溃发生在成功响应与检查点落盘之间时，重跑可能多写一条记录；正式测量前必须确认总记录数和每用户分布。全新实例删除整个临时根后重建。

## SLO（毫秒）

| Class | 接口 | idle p95 | idle p99 |
|---|---|---:|---:|
| W 交互写 | 心跳、进度、标记已看、收藏、搜索历史 | 50 | 150 |
| D 详情 | Self 详情、70 集分集列表、Jellyfin Items 详情、PlaybackInfo、静态图片 | 100 | 250 |
| L 列表 | 50 项列表及各 sort/filter、Latest、NextUp、Resume、up-next、favorites、collections、missing | 150 | 400 |
| S 搜索 | Library 标题、前缀、演员搜索及 Jellyfin SearchTerm | 200 | 500 |
| A 聚合/管理 | facets、观看统计、jobs、activity、sites | 300 | 800 |
| U 鉴权 | auth/me、UserViews、用户资料、System/Info/Public | 100 | 250 |
| U* 登录 | auth/login、Users/AuthenticateByName，含密码哈希 | 500 | 1000 |
| P 首屏 API 集合 | 首页、Library 页、详情页所有初始 API 的墙钟时间 | 800 | 1500 |

## 路由覆盖与样本

`list-routes.py` 从 `crates/api/src/http/**/*.rs`、`crates/media-server/src/routes*.rs` 及其子模块提取 `.route(...)`，包括多行注册、链式 `.get/.post`、限定名称 `axum::routing::get`，排除测试模块。输出 `method/path/surface` JSON。Jellyfin 的 `/emby` 为相同 router 的挂载别名，不重复计数。

`perf-cases.json` 为每个 GET 路由提供具体用例，或只针对视频流、SSE、WebSocket、外网图片代理提供理由明确的豁免。有限大小的字幕与静态图片不豁免。没有配置上游 catalog、没有生成的章节图或没有可用的特定资源，会明确报告非 2xx 或缺失数据前置条件，不能用 404 当成功，也不能扩大豁免。覆盖检查通过代表路由被登记；最终通过还要求所有登记用例实际成功并达标。Jellyfin 70 集列表按 D 类计量，并核对响应确实含 70 项；不支持的 DatePlayed 排序不伪装成 LastPlayed 用例，Self `last_played` 用例保留。

默认每个用例 40 次，轮换 50 个用户，并从真实数据集中随机选取 Movie/TV Media。Self 使用 `Authorization: Bearer …`，Jellyfin 先通过 `Users/AuthenticateByName` 获取 token，再使用 `Authorization: MediaBrowser Token="…"`。认证准备不计入普通用例延迟；登录用例单独测量。测量客户端在每个线程上复用一条 HTTP/1.1 连接。`origin_warmup` 用例（会 `get_or_fetch` 的 genres、routing-options、countries、languages、season）在 idle 和 busy 的计时样本前先请求一次同一 URL，该次不计入样本、状态码或错误；cold 不预热，仍测重启后的第一次请求。

P 类请求集合按当前 `web/components/library-view.tsx`、`library-detail-view.tsx`、`library-item-detail-view.tsx` 以及 `web/lib/api/` 静态核对，来源记在 cases 的 `source`。本轮用户明确要求页面/e2e 由别人负责，因此没有运行 `pages.mjs` 采集。P 类同时发起所列请求，详情的 `next_file_cursor` 后续文件页按实际游标继续请求并计入墙钟；P 只测 API 集合，不测浏览器渲染、图片队列或白屏。

输出逐用例 p50、p95、p99、max、状态码分布、SLO、相对上一阶段的 p95 变化和通过标记。分位数采用 nearest-rank。每个用例完成即写部分报告，最终报告 `complete: true`。缺少资源、超时、非 2xx 或无有效样本一律失败。报告不保存请求体、认证头或 token。

## 判定

- idle：p95 与 p99 均满足表中目标；默认 40 次，任何非 2xx 和超时失败。
- busy：对 `stress-tv` 触发完整扫描，测量前确认任务 `running`，测量后再确认仍在运行。p95 不超过对应 idle 目标的 2 倍；5xx（含 503）和超时必须为 0。跨越扫描完成边界的成功样本保存到 `invalid_samples`，计入 `outside_busy` 与 `attempts`，不参与分位数；在下一扫描窗口补测，直到取得 40 个完整落在 running 窗口内的样本。每用例最多尝试 80 次，若仍不足则失败。任何窗口内外的非 2xx、超时或准入检查错误都保留并使该用例失败，不为 HTTP 错误补测。`codes` 包含全部尝试的状态码，`precondition_codes` 另存检查请求错误；不能只看有效样本的状态码宣称零 503。
- cold：每个用例测一次，并在每个用例前重启进程。只允许健康检查作就绪探针；认证 token 在整轮开始前签发。依赖播放会话的用例在每个样本开始前重新签发并激活会话；cold 时该准备发生在重启之前，重启后直接测请求。心跳使用严格递增事件序号，并在计时结束后经公开 resume API 核对确切持久化位置；HTTP 200 的空操作不能通过，结果保存 `write_verified`。不能先用 50 次登录预热再声称冷启动。该首次用例墙钟不超过对应 idle p99 的 2 倍。必须传 `--restart-command`；该命令用参数数组执行，不经 shell 展开。
- 相对上一阶段，任一接口 p95 退化超过 20% 就失败，即便仍在绝对 SLO 内。绝对涨幅低于 1ms 记为通过。基线报告必须完整、模式一致，且含本轮全部选定用例，否则在测量开始前拒绝比较。阶段 A 要求 busy 没有 503；阶段 B 要求 busy 的各 class 达标；阶段 C 结束要求 idle、busy、cold 全部达标。
- 统计信息过期：同一进程先完成导入并测 idle，再重启后重新测 idle，按逐用例 p95 比较。若相差超过 2 倍，记录为统计信息过期候选，结合查询计划确认；扫描完成后优化统计信息属于写操作，必须走 `store_write`。冷启动单次延迟不能直接当作 40 次 idle p95 比较。

## 命令

同一时间仅允许运行一个 cargo 进程，build/test/clippy 串行，使用 `-j8`，测试 `-- --test-threads=8`。第一次建实例：

```sh
zsh scripts/e2e/stress-up.sh /tmp/e2e-stress
python3 -m unittest scripts/e2e/test_perfgate.py
python3 scripts/e2e/perfgate.py idle --dry-run
python3 scripts/e2e/perfgate.py idle --out .e2e/perf/baseline-idle.json
python3 scripts/e2e/perfgate.py busy --out .e2e/perf/baseline-busy.json
python3 scripts/e2e/perfgate.py cold \
  --restart-command 'zsh scripts/e2e/stress-up.sh /tmp/e2e-stress restart' \
  --out .e2e/perf/baseline-cold.json
```

`stress-up.sh` 默认先运行 `cargo build --release -j8 -p api`；已有匹配的 release 构建时可设置 `PERF_SKIP_BUILD=1`，由操作者核对 health 的 commit。`restart` 模式仅停止并重启同一个隔离实例、等待健康，不重建、不扫描、不重新播种。运行中的实例拒绝重复启动。所有步骤可从仓库外调用。

后续阶段保持同一数据规格，替换 `phaseA/phaseE/phaseB/phaseC` 名称并指向上一阶段报告：

```sh
python3 scripts/e2e/perfgate.py idle --baseline .e2e/perf/baseline-idle.json --out .e2e/perf/phaseA-idle.json
python3 scripts/e2e/perfgate.py busy --baseline .e2e/perf/baseline-busy.json --out .e2e/perf/phaseA-busy.json
python3 scripts/e2e/perfgate.py idle --only jellyfin-shows-nextup
```

`.e2e/perf/`、`.e2e/` 和 `docs/test-reports/` 都不提交。结束后先通过 PID 文件停止隔离实例，再删除 `/tmp/e2e-stress`。保留要交付的性能报告至工作完成。

## 修复前基线（4c507440）

下表为每类所有用例中最慢的 p95，单位 ms。cold 每用例只有一次首次请求，因此该列为首次请求耗时。整轮超标数量按各自模式阈值判定。

| 模式 | 完成/超标用例 | A | D | L | P | S | U | U* | W |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 初始 idle | 165/29 | 1248.17 | 1431.20 | 1223.70 | 1164.94 | 369.42 | 1.56 | 10.51 | 1.75 |
| busy | 165/21 | 3314.43 | 1220.66 | 1294.94 | 1163.94 | 708.98 | 72.62 | 201.17 | 72.52 |
| cold | 165/13 | 3482.31 | 1285.95 | 1479.61 | 1270.55 | 674.74 | 247.42 | 265.70 | 256.07 |
| 重启后 idle | 165/24 | 3657.41 | 1950.04 | 1244.40 | 1113.60 | 416.27 | 1.84 | 10.41 | 1.56 |

busy 取得 6600 个有效样本，实际请求 6628 次，28 个成功的扫描边界样本保留为无效测量，全部 6628 个请求为 200，前置检查没有 HTTP 错误。cold 的 165 次请求全部 200。两轮心跳均通过精确持久化位置校验。

初始 idle 的海报夹具原用虚构 TMDB ID，已改为真实 catalog Media 并仅重测该项；原报告与修正报告均保留，正式报告带来源说明。真实冷缓存的 40 次请求在后台填充完成前全部收到 503；填充实际用时 398ms。该错误归入 Task20，不能依据约 0.6ms 的失败响应宣称达标。初始 idle 最终仍为29项失败，其他6560个请求返回200。

初始 idle/busy 测量时全局 Scrape 仍启用。第一次 cold 重启检查失败，进程已关闭 HTTP 但主线程等待 Tokio blocking pool，采样栈定位到全局 Scrape 的逐文件补图循环。保留失败检查与采样栈后，仅清理了本次隔离进程，并通过公开 API 停用该可选 Job；随后正常重启检查通过，才开始正式 cold。未增加退出超时，未给脚本加入强杀兜底，应用代码和二进制均未修改。cold 及重启后的 idle 使用 Scrape 停用的条件；因此两次 idle 的差异含后台负载和缓存变化，超过2倍只能列为候选，不能直接证明统计信息过期。必需的媒体探测在各轮保持开启。

重启后的 idle 共6600次请求，全部200，心跳持久化校验通过。12项p95变化超过2倍：8项改善（genres、countries、两种Jellyfin图片、languages、routing-options、TV season、health），4项变慢（用户Item详情963→1950ms、catalog-cache1.30→2.74ms、Douban638→1544ms、日志导出1248→3657ms）。这些包含后台负载、缓存、外网和日志体积差异，未证明统计信息过期；Task5保留扫描结束optimize的验证要求。原始逐项比较保存在`.e2e/perf/`对应报告，未提交。
