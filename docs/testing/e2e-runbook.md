# crawler-media E2E 测试手册

辅助脚本在 `scripts/e2e/`，运行时的临时文件统一写到 `/tmp/e2e/`（先执行 `mkdir -p /tmp/e2e && cp scripts/e2e/* /tmp/e2e/`）。zsh 下空 glob 会报错，批量操作请用 `find … -exec`。**任何输出中都不得打印 key 或 token 的值。**

## 1. 单元测试
```sh
cargo fmt --all -- --check
cargo test --workspace -j4 -- --test-threads=4     # 限制并发，避免挤占机器上其他 rustc
cd web && pnpm install && pnpm typecheck && pnpm lint && pnpm test   # typecheck 即 tsc
```

## 2. 接口测试
- 路由清单：`routes.txt`，格式为 `method path`（共 201 条，其中 GET 108 条）。从 router 源码重新生成后，用 diff 检查是否有新增路由。
- `python3 bench.py <phase> 20`：所有 GET 路由各请求 20 次，输出 p50/p95/max 和状态码，写入 `bench-<phase>.json`。路径中的 `{id}` 用真实的库、条目、文件 ID 代入，其余用占位 ID（此时 404/400 属于预期）。
- 测三种状态：**空闲**（探测队列为空，`probestat.sh` 中没有 queued）、**探测期**、**Scrape 期**（`POST /api/v1/jobs/{scrape_job_id}/run`）。报告里用一张表对比三者。
- 详情页持续采样：后台运行 `nohup python3 sampler.py &`，每 2s 轮询所有库的详情，输出到 `samples.jsonl`，最后计算 p50/p95/max 和 5xx 数量。
- 后端视角：`python3 logstats.py` 解析 `.test-logs/backend.log`，统计慢请求、5xx 和探测各阶段耗时。

### 压力测试（隔离实例，不影响正式数据）
```sh
zsh scripts/e2e/stress-up.sh /tmp/e2e-stress     # 生成 3000 部电影 + 300 剧×70 集（共 24000 个 strm，带 NFO），在 :18766 启动独立实例
# 建两个库 stress-movie / stress-tv，路径分别为 /tmp/e2e-stress/media/{movie,tv}，等待入库完成（约 1 分钟）
python3 /tmp/e2e/stressbench.py 40 idle          # 测详情、分集、列表、排序、类型、年代、facets、搜索（标题/前缀/演员）
python3 /tmp/e2e/stressbench.py 8 busy           # 在扫描或探测队列滚动时再跑一次
kill $(cat /tmp/e2e-stress/pid); rm -rf /tmp/e2e-stress
```
strm 指向 127.0.0.1:9，探测会快速失败，不会产生外网流量。

## 3. E2E 步骤
1. **备份**：`rsync -a ~/Downloads/crawler-media/strm/ /tmp/e2e/strm-backup/`。如需保留旧数据库，把 `data/*.db*` 也复制到 /tmp/e2e。
2. **重置**：`DATA_DIR=../../../Downloads/crawler-media/data ./start-test.sh stop`，然后删除 `~/Downloads/crawler-media/data/{app,library,jobs,cache,subscribe}.db*`。
   注意 DATA_DIR 相对于仓库根目录解析，写成 `../../../../` 会变成 /Users/Downloads，是错的。
3. **启动**：`DATA_DIR=../../../Downloads/crawler-media/data ./start-test.sh`。启动后检查 `/api/v1/health` 返回的 commit 与 `git rev-parse --short HEAD` 一致；如不一致，先 `cargo build`。
4. **注入 TMDB key**：key 保存在仓库外的文件里（例如 `~/.config/crawler-media/tmdb.env`，内容为 `TMDB_API_KEY=…`）。用 `source` 读进变量，调用 `PUT /api/v1/settings/metadata {"tmdb_api_key":$TMDB_API_KEY}`，再调用 `POST /api/v1/settings/metadata/test {"source":"tmdb"}` 验证。全程只打印 key 的长度。
5. **pan-302**：确认 `/Users/jianxcao/work/fun/pan-302` 的服务在 :8082 运行（`lsof -iTCP:8082 -sTCP:LISTEN`）。strm 内容是指向 :8082 的 URL。
6. **建库**：`strm/disk/media/{movie,tv}` 下每个子目录各建一个库（movie-* 和 tv-*），然后触发扫描。
7. **检查**：
   - 标题、年份与目录名一致
   - 分集数与磁盘一致，没有重复的 unit（`resid.py` 可查残留）
   - media info（分辨率、编码、音轨）
   - 片头片尾时间点（intro/outro）
   - `probestat.sh` 查看探测状态分布
8. **删除测试**：运行中和完成后各做一遍，分别覆盖库、单剧、单影片、单个 strm（`rm` 后重新扫描），以及删除带收藏/已播放标记的条目。每次删除后检查三项：HTTP 状态和耗时；DB 残留（media / media_aliases / media_metadata_sources / media_episodes / probe_jobs / user_library_items）；磁盘上的侧车文件。结束后用 `rsync -a --ignore-existing /tmp/e2e/strm-backup/ ~/Downloads/crawler-media/strm/` 恢复，然后重建库。
9. **完整性**：`python3 complete.py final` 检查所有电影、剧、季、集的海报、背景图、剧照、简介和演员。每张图片 URL 都要请求一次，确认返回 200 且是有效图片。
10. **ffmpeg 与网络耗时拆分**：
    - 从后端日志的 timings 汇总里取 `time_to_first_pcm`、`pcm_read_wait`（网络）、`chromaprint_consume`（CPU）和远程读门等待。
    - 手动对照：分别用 `/opt/homebrew/bin/ffprobe` 跑 302 URL、`curl -sI` 解析出的直链 URL 和本地文件，比较三者耗时。
11. **strm 停止条件**：日志里 strm/远程读失败持续增长时（例如 1 分钟内超过 20 条，或连续出现 pan-302 5xx），**立即停止**，保留现场再排查，不要继续压测。

## 4. ego lite 页面测试
1. 读取 `~/.agents/skills/ego-browser/SKILL.md`，并运行 `ego-browser skill` 获取当前 API 说明。
2. `python3 mkurls.py <phase> quick|full` 生成 `urls-<phase>.json`，再执行 `cp urls-<phase>.json cur.json`。
   页面清单：/library、各库列表、电影详情、剧集详情（含大剧 仙逆）、合集、发现电影/剧集、订阅、媒体整理、定时任务、元数据缓存、活动、设置各页。
3. `ego-browser nodejs < pages.mjs`。脚本流程：用 `taskSpace` 开页，自动登录 admin，逐个 `page.goto`，等待 h1/h2 出现且骨架屏消失（上限 20s），记录 wall 时间，抓取页面文本、断图和挂起的图片请求，截图到 `docs/test-reports/<date>-e2e-rN/<phase>-NN-name.png`，结果写入 `pages-<phase>.json`。
4. 分别在探测期、Scrape 期、入库期和空闲时各跑一次，统计每个阶段的 p50/p95/max 和出现「加载失败」卡片的数量。
5. 页面慢但后端快时，用 `net2.mjs`（CDP Network 事件）查看每个请求的 queueing、stalled 和 TTFB。`performance.getEntries` 的资源缓冲区会满，不可靠。已知问题：浏览器到 :18765 的 HTTP/1.1 连接（每个 origin 最多 6 个）被 SSE 和慢图片请求占满。
6. 示例最小脚本：
```js
const task=await taskSpace("e2e");const page=task.page("p1");
await page.goto("http://127.0.0.1:3334/library");
await page.screenshot({path:"/tmp/e2e/x.png"});
console.log(await page.evaluate(()=>document.title));
```
ego 不需要系统辅助功能权限，也不需要「允许 Apple 事件中的 JavaScript」。如果为排查临时改过 Preferences，测试结束必须改回。

## 5. 报告模板与收尾
报告路径为 `docs/test-reports/<date>-e2e-rN.md`，不要提交。章节依次为：
- 环境与 commit
- 修复验证表（提交 / 结果 / 依据）
- 删除测试
- ffmpeg 与网络耗时拆分
- 数据完整性（数量 + 示例）
- 页面（各阶段 p50/p95/max 与失败数，附截图路径）
- 接口表（空闲 / 探测期 / Scrape 期）
- 压力测试
- 与上一轮对比
- 遗留问题

收尾：
```sh
pkill -f sampler.py; kill $(cat /tmp/e2e-stress/pid) 2>/dev/null
rsync -a --ignore-existing /tmp/e2e/strm-backup/ ~/Downloads/crawler-media/strm/   # 检查 strm 数量与备份一致
DATA_DIR=../../../Downloads/crawler-media/data ./start-test.sh stop
rm -rf /tmp/e2e /tmp/e2e-stress
git status   # 确认报告、手册和脚本都未提交
```

## 性能闸门

全路由性能基线、数据规格、各 class 的 SLO 和 idle/busy/cold 判定见 [performance-slo.md](performance-slo.md)。入口是 `scripts/e2e/stress-up.sh`（独立 release 实例 :18766）、`seed-activity.py`（只经 API 写 50 用户的活动数据）、`list-routes.py`（从源码提取路由）和 `perfgate.py`（覆盖检查、分位数、阶段对比）。

```sh
python3 -m unittest scripts/e2e/test_perfgate.py
python3 scripts/e2e/perfgate.py idle --dry-run
zsh scripts/e2e/stress-up.sh /tmp/e2e-stress
python3 scripts/e2e/perfgate.py idle --out .e2e/perf/baseline-idle.json
python3 scripts/e2e/perfgate.py busy --out .e2e/perf/baseline-busy.json
python3 scripts/e2e/perfgate.py cold \
  --restart-command 'zsh scripts/e2e/stress-up.sh /tmp/e2e-stress restart' \
  --out .e2e/perf/baseline-cold.json
```

每阶段与上一阶段 JSON 对比，任何用例 p95 退化超过 20% 即失败。cold 每个用例都重启，busy 样本必须落在扫描任务运行期间。不要把缺失资源的 404 当成功。性能报告放 `.e2e/perf/`，不提交；临时数据放 `/tmp`，用完停止实例并删除。cargo 只允许一个进程，release 构建 `-j8`。本轮页面/e2e 另由负责人执行，P 类这里只测静态核对的 API 请求集合，不代表页面白屏或渲染验收。
