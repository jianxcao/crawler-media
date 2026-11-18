# E2E 测试手册

测试脚本位于 `scripts/e2e/`。单元/集成、页面交互、真实客户端、性能与容量是不同验证层，结果分别记录。优化与规模验证见 [存储性能测试](storage-performance.md)，延迟预算见 [性能闸门](performance-slo.md)，交互清单见 [系统回归](system-regression-cases.md)。

## 隔离与数据恢复

每轮使用 `mktemp -d /tmp/crawler-e2e-XXXXXX` 创建独立目录，独立数据库、STRM/侧车副本、测试用户、端口与进程记录。绝不通过删除真实数据库准备 fixture。原媒体目录只作为复制来源；测试写入只落在副本中。不要把可写副本硬链接到原 NFO、图片或配置。

开始前记录原数据库及 sidecar、STRM、pan-302 相关文件的路径、类型、大小和内容摘要。数据库一致快照须在所属实例正常停服后复制，或采用明确的一致备份流程；不能只复制运行中 `.db` 而漏掉 WAL。前后清单覆盖原本不存在的 sidecar，才能发现测试新增文件。macOS 的 `/tmp` 与 `/private/tmp` 指向同一位置，清单统一规范路径。

优先新建数据库，只将必要的 TMDB 等配置通过受限文件或 API 写入。凭据可在仓库外的私有配置中保留，不输出值，不放进命令行参数、截图、报告或 Git。不得复用真实用户的观看记录进行写测试。

测试结束先正常停止自有进程并等待退出，确认端口和数据库句柄关闭，再核对原数据库/STRM/pan 清单。隔离副本删除后下一轮重新创建；原文件若发生变化，保留差异并定位后按一致备份恢复，不用 `--ignore-existing` 把部分恢复称作完整还原。仅操作已核实归属本轮的路径/PID，保留用户已有服务与客户端连接。

## 构建与自动化质量门

按仓库资源规则取得锁；严格测量独占两槽，`TMPDIR=/tmp` 由包装器设置。先跑受影响 crate 的定向测试，稳定代码树最后一次全量使用 `--no-fail-fast`。

```sh
python3 scripts/with-build-lock.py --exclusive cargo fmt --all -- --check
python3 scripts/with-build-lock.py --exclusive cargo clippy -j8 --all-targets -- -D warnings
# 将 <crate> / <target> 换成此次行为对应的实际目标。
python3 scripts/with-build-lock.py --exclusive cargo test -j8 -p <crate> --test <target> -- --test-threads=8
python3 scripts/with-build-lock.py --exclusive cargo test -j8 --workspace --no-fail-fast -- --test-threads=8
python3 scripts/with-build-lock.py --exclusive pnpm --dir web typecheck
python3 scripts/with-build-lock.py --exclusive pnpm --dir web lint
python3 scripts/with-build-lock.py --exclusive pnpm --dir web test
python3 scripts/with-build-lock.py --exclusive pnpm --dir web build
```

测试位于 public seam，先写失败行为，再实现。Rust 常规测试使用 fake/fixture，不访问真实 TMDB、Downloader 或 Chromium。页面测试与真实媒体验证另行执行，不能由上述命令代替。

## 启动与真实媒体链

`start-test.sh start|restart|restart-all|stop|status|logs` 管理本地开发实例，默认后端 `:18765`、前端 `:3334`。测试文件统一位于 `~/Downloads/crawler-media/`：

```text
crawler-media/
├── data/       数据库、缓存、图片等应用数据
│   └── logs    指向 log/ 的符号链接
├── log/        backend.log、frontend.log、mock.log 及应用的滚动日志
├── run/        服务 PID 文件
├── qb/         qBittorrent 下载文件（容器中的 /downloads）
└── mock-site/  可选的 mock 站点 server.py
```

`TEST_ROOT` 可指定另一套测试根目录，`DATA_DIR`、`LOG_DIR`、`QB_HOST_DOWNLOADS`、`MOCK_SITE_PY` 可单独覆盖；绝对路径直接使用，相对路径从仓库根目录解析，与调用时的工作目录无关。例如 `TEST_ROOT=/tmp/crawler-e2e-XXXXXX ./start-test.sh restart` 使用独立实例目录。`status` 显示实际数据、日志和下载路径；`python3 scripts/e2e/logstats.py [日志路径]` 默认读取相同 `TEST_ROOT` / `LOG_DIR` 下的 `backend.log`。

脚本在启动时将 `data/logs` 链接到 `log/`，应用日志导出也读取该目录。若已有不同的 `data/logs` 文件或目录，脚本会报错并保留内容；停服整理后再启动。`clean-logs` 清空三个服务输出日志，应用滚动日志保留。脚本不搬移旧数据库或日志；已有 qB 容器的 `/downloads` 挂载需与 `QB_HOST_DOWNLOADS` 一致。

使用前检查脚本配置、现有进程与目录。后端构建通过 `scripts/with-build-lock.py` 使用普通构建槽与 `-j4`；脚本还会释放指定端口。严格性能测量按上文独占构建，再用已构建二进制 `serve` 和绝对 `CRAWLER_MEDIA_DATA` 启动独立实例。不要重启另一轮正在使用的实例。

验收前记录实际代码与构建身份，刷新 `crates/api/build.rs` 的构建戳后按锁规则构建，核对 `/api/v1/health` 与实际被测二进制一致。运行报告需要可追溯身份，但本手册不固定某个提交为永久验收依据。

1. 通过公开设置 API 配置必要的 Metadata source，验证连接。验证失败保留错误，不能只因稍后 curl 成功就认定产品链路正常。
2. 真实 STRM 可使用现有 pan-302 服务；其项目通过 `sh run.sh` 启动。先检查 STRM 实际目标及已有监听，沿用已授权服务。记录「设置 → 刮削与整理 → 媒体信息探测」的三个实际值；默认媒体信息并发 1、共享远程并发 1、间隔 500ms。验收沿用默认值，不为加速验收提高并发或降低间隔；配置变更用隔离本地源验证，结束后恢复原值。设置范围与热生效见[探测设置契约](../api-contracts/probe-settings.md)。
3. 将选定的真实 STRM 树和 NFO/图片/字幕复制到隔离路径，建立 Movie/TV Library；另准备可本地播放的真实视频及浏览器可解码的 STRM。
4. 扫描并逐文件核对路径、Media/Season/Episode、别名、数量和覆盖，不以总数相等代替身份集合相等。记录所有 probe 的 queued/running/succeeded/failed/cancelled 终态及每个来源，队列未排空不算全来源成功。
5. 检查媒体信息、音轨、字幕、片头片尾、章节图与完整元数据。`complete.py`、`probestat.sh`、`resid.py` 是诊断辅助，先核对其 URL、凭据和数据路径配置。
6. 分别执行 Scan、Transfer、CLI/global Scrape，核对真实 worker、目标文件哈希、NFO、别名与重启读回。Transfer 可接 fake Downloader，但发布与文件操作必须真实；Transfer 输出的 NFO 不能代替独立 Scrape 验证。
7. 按 ADR-0012 覆盖运行中/完成后删库、单项、单文件和最后引用：先隐藏并取消任务，成功终态前不能宣称删除完成；孤儿 Media 的收藏/进度清理，仍有其他 Library 引用的 Media 保留。磁盘保留/删除取决于对应操作合同；删库本身保留媒体字节。
8. 每个阶段记录首帧、响应、任务终态、错误与来源清单。STRM 错误持续增长或 pan-302 连续 5xx 时停止本轮负载并定位，不继续堆积请求。

网络耗时拆分使用实际 timings：远程闸门等待、首 PCM、PCM 读取等待、Chromaprint CPU。对照 302 URL、解析出的直链和本地文件时保持相同音轨与时间窗口，并保护敏感完整 URL。差值只说明观察区间，不自动证明代理或上游根因。

批量任务停服修复时使用 `./start-test.sh stop`，保留数据库、STRM、NFO、图片和日志；再次启动用 `./start-test.sh start --no-clean`。恢复前核对数据目录与健康检查构建身份，分别检查 Scan/Scrape 与持久 probe 队列，不能以元数据刮削结束推断媒体信息已完成。确认源未变而丢失媒体信息时，通过管理员 `POST /api/v1/libraries/{id}/media-info/refresh` 的 `mode:"missing"` 补排，不删除数据库或伪造成功结果。分页请求使用同一 `request_id`，以响应的 `next_cursor` 作为后续 `after`；已存在有效结果会被跳过。

## 页面与 VidHub

浏览器操作遵循可用浏览器工具的说明。`mkurls.py`、`pages.mjs`、`net2.mjs` 可辅助建立页面清单、采集和网络诊断；使用前核对脚本与当前浏览器工具 API，不能因脚本退出成功就省略真实页面检查。

页面覆盖首页、Library 列表/详情、Movie/Series/Season/Episode、搜索、分类、合集、发现、Subscribe、Transfer、Job、缓存、活动和设置。分别在空闲、probe、Scan/Transfer 和 Scrape 实际运行窗口执行，记录可操作内容与图片就绪时间、p50/p95/max、白屏/错误卡、挂起请求和断图。未观察到目标任务正在运行时，该样本不能代表对应负载阶段。

首入、离开后重入、刷新、窄屏和滚动触发的图片都检查。图片必须实际解码且尺寸正确；200、存在 URL 或缓存文件不能代替图片验证。API 集合性能不包含浏览器渲染与图片连接排队；页面慢而后端快时分别观察 queueing、stalled、TTFB 和正文，不从单一总耗时猜根因。

VidHub 添加 **Jellyfin 类型**的测试服务器，与 Web 使用相同隔离实例和用户。按 [Jellyfin 回归合同](../api-contracts/jellyfin.md) 和系统回归 VH/PLAY 清单检查：

- 全部授权 Library、隐藏库、列表分页、长集数、Season 切换、搜索/无结果与分类浏览。
- 详情的简介、年份、评分、人物及头像、海报/背板/剧照、媒体信息、音轨/字幕与章节。
- 收藏/取消、已播放/取消及跨端刷新；两个 User 的状态隔离。
- 本地视频与 STRM，Movie 与 Episode 的 Web → VidHub → Web 实播、seek、正常退出和续播；服务正常重启后再次回读。

Web 不转码，选取浏览器支持的源完成正向播放，并单列不支持编码的提示行为。锁屏、没有首帧、缺少可读 HUD 或未收到 Sessions 都应如实记录。只删除本轮创建的测试服务器，保留用户日常服务器；停止本轮抓包并恢复临时系统设置。

## 报告与收尾

报告放 `/tmp` 本轮目录、`.e2e/` 或 `docs/test-reports/`，后两者不提交。记录环境/构建/工具身份、数据规格、实际命令与退出码、用例结果、性能样本和错误、页面截图、真实播放持久化、资源使用与清理核验。

结果区分 Pass、Fail、Blocked、Not run；证据无法确认写 Unverified。失败重跑另立结果，不覆盖失败原件；环境失败不当产品缺陷，也不追认通过。有限样本只证明已覆盖范围，不能推导全量准确率或长期无泄漏。

交付前核对自有服务、监听、数据库句柄、后台采样、浏览器测试空间及 VidHub 测试连接均已收尾，原数据清单一致。交付所需报告后删除本轮独立数据库、STRM副本、私有报文和临时凭据文件；保留已约定长期使用的私有 key。
