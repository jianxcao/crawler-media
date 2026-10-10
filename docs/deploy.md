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
cargo run -p api

# 2. 前端（开发模式，直连后端，跨域走后端 CORS）
cd web
VITE_API_BASE_URL=http://127.0.0.1:18765/api/v1 pnpm dev   # 监听 127.0.0.1:3334

# 3. 生产构建 + 单端口冒烟
cd web
pnpm build                       # 产出 dist/
CRAWLER_MEDIA_UI=$PWD/dist cargo run -p api   # 浏览器打开 http://127.0.0.1:18765 即完整应用
```

默认监听：后端 `127.0.0.1:18765`，前端 `127.0.0.1:3334`。登录：`admin` / `${CRAWLER_MEDIA_ADMIN_PASSWORD}`（首次启动必须独立设置）。

## Docker Compose

```bash
export CRAWLER_MEDIA_TOKEN=your-cli-bearer-token
export CRAWLER_MEDIA_ADMIN_PASSWORD=your-distinct-admin-password
docker compose up --build
```

镜像内单进程（`crawler-media`）监听 18765：静态 UI（`web/dist` 拷入
`/usr/share/crawler-media/ui`）+ API + Jellyfin 兼容，浏览器和播放器都打这一个端口。
镜像内默认 **无 Chromium**。当前 Browser 仅支持外部 HTTP(S) CDP discovery；JS 渲染 Site 可配置专属 `cdp_url`，或在 Browser 设置中启用 Obscura/全局 CDP 地址。自行运行外部 Browser，保存配置不会安装或启动进程。`CRAWLER_MEDIA_BROWSER=1` 请求不支持的 managed 能力，会在启动时明确报错。渲染路由为 Site CDP → 启用的 Obscura → 启用的全局 CDP；保存即时生效，端点配置不等于可用性验证。

## 下载器

qBittorrent / Transmission 通过 `/api/v1/downloaders` 配置（持久化到 SQLite）；进程也接受 `CRAWLER_MEDIA_QB_URL/USER/PASS` 环境变量覆盖。live 环境用 `docker-compose.yml` 起 qB（8080）/ TR（19091）容器。

下载器可停用（`enabled = false`）：停用后不再被自动投递选中，手动提交仍可用。

## 环境变量

所有 `CRAWLER_MEDIA_*` 在启动时集中解析（`crates/api/src/config.rs`）；无法识别的
`CRAWLER_MEDIA_*` 变量会打印 warning，便于发现拼写错误。

| 变量 | 默认 | 说明 |
|---|---|---|
| `CRAWLER_MEDIA_DATA` | `data` | 数据目录（SQLite / 缓存 / 图片 / 日志） |
| `CRAWLER_MEDIA_TOKEN` | 必填 | 实例访问令牌（用于 CLI 与受信服务调用） |
| `CRAWLER_MEDIA_ADMIN_PASSWORD` | 首次必填 | 管理员初始登录口令（不可与 TOKEN 相同，Argon2id 加盐哈希存储） |
| `CRAWLER_MEDIA_LISTEN` | `127.0.0.1:18765` | 监听地址 |
| `CRAWLER_MEDIA_UI` | 空 | 前端静态产物目录 |
| `CRAWLER_MEDIA_CORS_ORIGINS` | 空 | CORS 白名单（逗号分隔） |
| `CRAWLER_MEDIA_BROWSER` | 关 | 旧 managed 开关；`1`/`true`/`yes` 现因能力不支持而拒绝启动，请配置外部 CDP |
| `CRAWLER_MEDIA_TMDB_KEY` | 空 | TMDB API Key（同时可写入设置 UI） |
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

站点凭证（cookie / API key / RSS URL）明文存 SQLite（ADR-0003），仅存在于 gitignored `data/live/.env` 或 UI 配置，绝不入库。
