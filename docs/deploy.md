# Deploy

API + Jellyfin 兼容服务是单个 Rust 二进制（`crates/api`）。前端是 Vite + React SPA（`web/`，pnpm）。

## 部署形态

- **开发**：两个端口。Vite dev（3334）经 `VITE_API_BASE_URL` 直连后端（18765），
  跨域由后端 CORS 层放行（`CRAWLER_MEDIA_CORS_ORIGINS`，默认放行 127.0.0.1:3334 / localhost:3334）。
- **生产**：一个端口。Rust 监听 `CRAWLER_MEDIA_LISTEN`（默认 127.0.0.1:18765），同时托管：
  - `/` —— 前端静态产物（`CRAWLER_MEDIA_UI` 指向 `web/dist`，SPA fallback 到 index.html）
  - `/api/v1/*` —— 自有 API
  - Jellyfin 兼容路径（/Items、/Videos/...、/Sessions/...）—— 播放器直连同一端口

## 本地运行

```bash
# 1. 后端（数据目录默认 ./data，可用 CRAWLER_MEDIA_DATA 覆盖）
cargo run -p api --bin crawler-media -- serve

# 2. 前端（开发模式，直连后端，跨域走后端 CORS）
cd web
VITE_API_BASE_URL=http://127.0.0.1:18765/api/v1 pnpm dev   # 监听 127.0.0.1:3334

# 3. 在仓库根目录另开终端：生产构建 + 单端口烟测
pnpm --dir web build             # 产出 web/dist/
CRAWLER_MEDIA_UI="$PWD/web/dist" cargo run -p api --bin crawler-media -- serve
# 打开 http://127.0.0.1:18765；启动成功不是完整播放/协议验收。
```

默认监听：后端 `127.0.0.1:18765`，前端 `127.0.0.1:3334`。首次创建管理员时须独立设置 `CRAWLER_MEDIA_ADMIN_PASSWORD`；已有管理员使用当前保存的口令，不能把该启动变量当作每次重启都会覆盖口令。CLI token 与网页登录口令独立。

构建要求：rustc ≥ 1.88（`idna`/`icu` 依赖要求；`Dockerfile` 使用 `rust:bookworm`，自建镜像需确认版本）。

## Docker Compose

```bash
export CRAWLER_MEDIA_TOKEN=your-cli-bearer-token
export CRAWLER_MEDIA_ADMIN_PASSWORD=your-distinct-admin-password
docker compose up --build
```

镜像内单进程（`crawler-media`）监听 18765：静态 UI（`web/dist` 拷入
`/usr/share/crawler-media/ui`）+ API + Jellyfin 兼容，浏览器和播放器都打这一个端口。
镜像内默认 **无 Chromium**。实例级 Browser 开关决定是否使用 CDP：**关闭**时所有请求走 HTTP，不探测端点、不下载、不启动任何浏览器进程，Site 的 `cdp_url` 也不生效；**开启**时端点按 Site `cdp_url` → 实例 `cdp.url` → 受管 Obscura 解析，浏览器只用于 Site 的登录与签到（Site 上选择「浏览器」方式，`maintenance_via = browser`）；**搜索与 RSS 始终走 HTTP**。失败回退规则见下段。端点只接受 `http` CDP discovery URL（不接受 `https`/`ws`/`wss`、内嵌凭据或 query）。

回退规则：登录与签到只在**导航之前**失败（端点不可达、排队超时、target/WebSocket 建立失败、cookie 注入失败）时回退 HTTP 并 warn；导航之后失败直接报错，不会再发一次 HTTP 签到。回退不等于成功。浏览器全局并发默认 3，同一可注册域按公共后缀表串行。每个会话创建独立 BrowserContext，结束时 dispose；不发送 cookie 删除/清空命令，也不修改第三方浏览器的默认上下文。Site cookie 以当前 URL 经 `Network.setCookies` 做 host-only 注入，并经 `Network.getCookies{urls}` 定向回读；当前 Obscura 的回读另限制 path=`/`。不支持 BrowserContext 的端点在导航前失败并回退 HTTP。公共后缀 cookie 拒收能力的实测仍未完成，不能宣称该项已验证。浏览器会话不使用 Site 代理，`maintenance_via = browser` 与 Site 代理在保存时互斥。实现见 `crates/indexer/src/cdp_page.rs`，历史实测见 ADR-0007。

**数据库启动边界（不迁移旧布局）**：当前 Store 只初始化空数据库或重开当前布局，不能将旧版本数据目录原地升级。启动先只读检查已存在的四个 Store 文件的 `application_id` / `user_version`（cache 另校验结构），不匹配时报 layout mismatch；错误不等于应用已重置目录。不要删除现有实例数据来试错：先停机、一致备份并保留原目录，使用独立的新目录评估；实际生产切换须另行批准。应用不会自动删除旧数据，也没有旧库导入器或迁移阶梯。当前布局可正常重启，并非每次启动都要求空目录。新 Site 默认 `maintenance_via = http`，CDP 默认关闭；旧 `cdp.sync.enabled`、`obscura.enabled/url` 不作为新开关使用，Profile `render` 不再控制搜索。依据：`crates/store/src/open.rs`、`schema/layout.rs`、`schema.rs`。

**受管 Obscura 启动条件**：开关开启、未配置 `cdp.url`、至少有一个启用的 Site 选择浏览器登录/签到且没有自己的 `cdp_url`、二进制可用。不满足时设置页显示未启动原因（`no_managed_browser_sites`、`not_installed` 等）。受管 daemon 每次启动使用随机空闲端口并只绑定 `127.0.0.1`；只管理本进程拉起的 daemon，关闭开关或应用退出会停止它，外部服务不受影响。

**安装策略**（`obscura.install`，启动默认值 `CRAWLER_MEDIA_OBSCURA_INSTALL`）：

- `preinstalled`（默认）：不联网。推荐把 v0.2.3 对应平台原始发布归档（如 `obscura-x86_64-linux.tar.gz`）放为 `<data>/bin/obscura.tar.gz`，应用按代码固定 SHA-256 与大小校验后解包。实现也接受 `obscura`、`obscura-worker` 加同目录部署者提供的 `obscura.sha256sums`；该方式只能校验与部署者清单一致，不能提供代码固定归档哈希的信任保证，须保证整个目录来源可信。
- `download`：首次需要时从 GitHub Releases 下载对应平台归档，按写死的 SHA-256 与大小校验，上限 200 MiB，通过后原子落盘。

归档安装的信任根是代码中写死的哈希与大小；部署者清单方式的信任根则是部署者提供的目录，二者不能混称同一供应链保证。实现见 `crates/api/src/obscura_install.rs`，决策与历史实测见 ADR-0007。

**`network_mode: host` 风险**：受管 daemon 虽只绑定回环，但同宿主的任何进程都能访问该无鉴权 CDP 端口并读取站点 cookie；在共享宿主上请使用 bridge 网络或改用独立容器中的外部端点。

也可自行运行外部 Browser 并在设置中填 `cdp.url`。非法端点在保存时会被拒绝；保存合法地址只表示配置成功，不等于连通性验证。

## 下载器

qBittorrent / Transmission 通过 `/api/v1/downloaders` 配置（持久化到 SQLite）；进程也接受 `CRAWLER_MEDIA_QB_URL/USER/PASS` 环境变量覆盖。live 环境用 `docker-compose.yml` 起 qB（8080）/ TR（19091）容器。

下载器可停用（`enabled = false`）：停用后不再被自动投递选中，手动提交仍可用。

## 环境变量

主要服务启动配置由 [config](<../crates/api/src/config.rs>) 解析，并对未注册的 `CRAWLER_MEDIA_*` 打 warning。Web 构建、类型检查与测试要求 **Node.js ≥ 22**（`web/package.json`）；Docker 构建镜像使用 Node 22。部分运行时工具仍有独立读取点，下面是部署常用变量，不是全部运行时配置的唯一注册表；typed settings 的完整收敛仍待实施。

| 变量 | 默认 | 说明 |
|---|---|---|
| `CRAWLER_MEDIA_DATA` | `data` | 数据目录（SQLite / 缓存 / 图片 / 日志） |
| `CRAWLER_MEDIA_TOKEN` | 必填 | 实例访问令牌（用于 CLI 与受信服务调用） |
| `CRAWLER_MEDIA_ADMIN_PASSWORD` | 首次必填 | 管理员初始登录口令（不可与 TOKEN 相同，Argon2id 加盐哈希存储） |
| `CRAWLER_MEDIA_LISTEN` | `127.0.0.1:18765` | 监听地址 |
| `CRAWLER_MEDIA_UI` | 空 | 前端静态产物目录 |
| `CRAWLER_MEDIA_CORS_ORIGINS` | 空 | CORS 白名单（逗号分隔） |
| `CRAWLER_MEDIA_BROWSER` | 关 | 启动期 Browser 开关默认值（`1`/`true`/`yes`/`on`），仅在未保存过开关时生效。**只控制开关**：受管浏览器还需预置二进制或 `CRAWLER_MEDIA_OBSCURA_INSTALL=download`，且至少一个选择浏览器登录/签到的 Site，否则不会启动（设置页显示原因） |
| `CRAWLER_MEDIA_OBSCURA_INSTALL` | `preinstalled` | 受管 Obscura 安装策略启动默认值：`preinstalled`（不联网）或 `download`（按写死哈希校验后下载） |
| `CRAWLER_MEDIA_TMDB_KEY` | 空 | 非空 env／_FILE 固定 TMDB API Key 时，UI PUT 返回409；清除env pin后才可由UI修改 |
| `CRAWLER_MEDIA_TVDB_KEY` | 空 | TVDB API Key |
| `CRAWLER_MEDIA_QB_URL` / `_USER` / `_PASS` / `_CATEGORY` | 空 | qBittorrent 覆盖 |
| `CRAWLER_MEDIA_QB_PATH_MAP` | 空 | qB 路径映射 `容器路径=宿主路径,...` |
| `CRAWLER_MEDIA_TR_PATH_MAP` | 空 | Transmission 路径映射 |
| `CRAWLER_MEDIA_METADATA_PROXY` | 空 | 元数据代理 URL (如 `socks5://127.0.0.1:1080`) |
| `CRAWLER_MEDIA_METADATA_PROXY_USER` | 空 | 元数据代理认证用户名 |
| `CRAWLER_MEDIA_METADATA_PROXY_PASS` | 空 | 元数据代理认证密码 (支持 `_FILE`) |

### 密钥文件模式

保密变量都支持 `_FILE` 变体，值为存放密钥的文件路径；直接变量优先于文件。
用于 Docker Swarm / Kubernetes Secret 卷，避免密钥出现在 `docker inspect`
或 `/proc/<pid>/environ`：

```bash
CRAWLER_MEDIA_TOKEN_FILE=/run/secrets/cm_token
CRAWLER_MEDIA_QB_PASS_FILE=/run/secrets/qb_pass
CRAWLER_MEDIA_TMDB_KEY_FILE=/run/secrets/tmdb_key
CRAWLER_MEDIA_TVDB_KEY_FILE=/run/secrets/tvdb_key
CRAWLER_MEDIA_METADATA_PROXY_PASS_FILE=/run/secrets/proxy_pass
```

文件内容会被 trim；空文件视为未设置（不会用空值覆盖已有配置）。

## 凭证

Site 凭证（cookie / API key / RSS URL）按 [ADR-0003](<adr/0003-plaintext-site-credentials.md>) 明文持久化于 SQLite，须保护数据目录与一致备份。部署 secret 文件／env 必须不进入 Git 或 Docker 构建上下文；“不进版本库”不等于“不进数据库”。

## 数据库维护与升级注意

当前默认文件共五个：Store 管理 `<data>/app.db`（User/token、Site、Downloader、Filter、设置）、`cache.db`（按来源分表的 HTTP 响应缓存）、`library.db`（Media/别名/采用元数据、Library/根、文件/覆盖/摘要、Playback、Probe/Scan）、`subscribe.db`（Subscribe）；另有独立队列 `<data>/jobs.db`（定义、调度配置、在途任务与历史）。`catalog.db`、`fanart.db` 是旧布局名称，不是当前另需创建的库。`jobs.db` 不是可丢弃缓存。连接间无跨文件原子事务，备份需保持整体一致。DDL 与布局入口见 `crates/store/src/schema/layout.rs`、`crates/store/src/schema/v1/*.sql`，队列入口见 `crates/api/src/management/state.rs`。

默认注册数据库轻量 optimize（每6小时）、Jobs历史清理（每天、90天、启用）、Playback历史清理（每天、90天、**关闭**），以及分类历史保留 `data_retention`（默认关闭、dry_run=true，各窗口90天）。默认只在首次注册时应用，重启保留已有 def 配置。首个启用任务可能立即执行，Jobs超90天终态历史可能在首次后台调度即删除；各def保留latest/tie组，运行／排队行不删。VACUUM默认关闭，绝不因每次DELETE自动重建库。依据 `crates/api/src/maintenance/mod.rs`。

维护控制以当前实现为准，不沿用 2026-10-13 审计中全部“当前缺陷”的措辞：`maintenance/runner.rs` 已在批/库边界检查禁用、取消与停机；`jobs::update_def_config_cas` 同事务保存 payload/可选周期并刷新 queued payload，running 保持快照；预览已通过 blocking budget 执行。仍需注意：payload token 不含周期版本，不能检测仅改周期的冲突；2 秒清理预算只在批间检查，不是单条 SQL 的硬时间上限；VACUUM 的部分页拷贝无法被 SQLite interrupt 硬停止。历史发现逐项验收不能仅凭代码存在判定关闭，见 [历史复核](<audits/2026-10-13-database-api-review.md>)；当前存储切换验收与优化顺序见 [执行计划](<superpowers/plans/2026-10-22-storage-repair-remediation.md>)。

不要在运行时复制单个主体文件忽略WAL当备份；采用SQLite online backup或停机后对有关文件的一致备份。不要手工删除DB/WAL/SHM来缩空间。

### 干净切换：旧 `cache.db` 的停机重建（不导入旧数据）

**症状**：启动直接失败，日志末尾为 `invalid argument: cache database schema mismatch: legacy format error: cache schema is missing required index idx_*_cache_retention`。

**原因**：`bd6c3c8` 把 per-source cache 的索引名从 `*_cache_recent_idx` / `*_cache_retention_idx` 收敛为 `idx_*_cache_fetched` / `idx_*_cache_retention`。切换前创建的 `cache.db` 主体身份（`application_id=1129139251`、`user_version=1`）与六张源表都在，只有索引名是旧布局；Store 启动按计划**拒绝**旧布局——不提供兼容视图、双读或旧数据导入器，所以不会自动修复。

**处置**（只重建派生缓存）：

```bash
./stop-test.sh 2>/dev/null || true          # 先停服
bash scripts/storage-cutover-rebuild.sh --data-dir data/live
./start-test.sh start                       # 必须带子命令；裸执行只打印用法
```

脚本把 `cache.db`（含 `-wal`/`-shm`）整体移到 `<data>/backups/cache-cutover-<时间戳>/`，**不删除**；重启后 Store 以当前 DDL 新建 `cache.db`。回滚即把备份目录里的文件移回原位。

有 `sqlite3` 时脚本先用它探测布局：这是读写打开，若 `cache.db` 旁有已提交的 `-wal`，SQLite 会先做 WAL 恢复、把已提交帧折叠进 `cache.db`（实测 `1429504 + 16512` 字节的库折叠成单个 `1433600` 字节文件），因此备份自包含、回滚只需 `cache.db` 一个文件；`-wal`/`-shm` 的移动分支只在没有 `sqlite3` 时才会走到（实测该分支三个文件一并移入备份，事实库 sha256/mtime 前后不变）。

**为什么无损**：`cache.db` 只有外部响应体，清空/重建后 Library 列表、筛选、采用图片、播放与观看进度仍离线可用（见 [cache 规范](<superpowers/specs/2026-10-18-source-separated-cache.md>)）。事实库 `library.db` / `app.db` / `subscribe.db` / `jobs.db` **不要**重建，脚本也不触碰它们；它们的布局失配属于存储迁移问题，须按执行计划另行处理并先备份。

**注意**：脚本在检测到该文件仍被进程持有（`lsof`）时默认拒绝执行（确需继续可加 `--force`）；缺少 `sqlite3` 时无法确认布局，会直接按“需重建”处理并连 `-wal`/`-shm` 一起备份（仅影响派生缓存）。脚本按与 `media::response_cache::CACHE_INDEXES` 相同的 12 条声明索引（6 表 × fetched/retention）判定“已是当前布局”，只查一族会误判为无需重建。`app.db` 等事实库若报 `database layout mismatch`，本脚本不适用。


### Obscura 受管浏览器补充（r9）
- **预置方式**（`CRAWLER_MEDIA_OBSCURA_INSTALL=preinstalled`，默认值）：归档与部署者清单方式及其不同信任边界见上文「安装策略」，不要求同时准备两套输入。安装器会核对已有安装标记与二进制 hash，不能把同目录手写清单视为防篡改证据。
- **局域网或内网站点**：Obscura 默认禁止访问私网地址。如果确实需要，设置 `CRAWLER_MEDIA_OBSCURA_ALLOW_PRIVATE_NETWORK=1`（默认关闭，注意 SSRF 风险）。
- **版本核对**用 `obscura --version`；不符时显示在状态的 `last_error` 里。
- **进程回收**：daemon 以独立进程组运行，停止时整组回收。应用崩溃后，由下次启动时的 pidfile 回收。
- **权限**：Browser 设置和 Obscura 状态接口仅管理员可读，因为里面有 CDP 端口和二进制路径。
