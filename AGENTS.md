# AGENTS.md

本仓库 coding agent（Codex / Claude / Cursor 等）的首要指令。本地文档优先于训练数据。术语以 `CONTEXT.md` 为准（`_Avoid_` 的词不要用）。

## 项目概览

crawler-media：自托管媒体抓取/订阅/入库/播放服务。Rust workspace（edition 2024）+ React/Vite 前端。SQLite 为唯一事实来源（ADR-0008）。

### Crate map（`crates/`）
| crate | 职责 |
|---|---|
| `domain` | 纯类型，无 IO |
| `store` | SQLite system of record（ADR-0008），可依赖 `domain`、`library` |
| `indexer` | 站点索引/抓取（ADR-0006；CDP 浏览器 opt-in，ADR-0007） |
| `media` | TMDB 等 catalog 客户端与缓存 |
| `downloader` | qBittorrent 等下载器适配 |
| `library` | Library 账本、扫描、文件 → Media 识别（ADR-0001） |
| `subscribe` / `filter` / `release` | 订阅轮次、过滤原子、Release 解析 |
| `hooks` | 外部 hooks（ADR-0002，非插件运行时） |
| `jobs` | 任务队列（ADR-0009） |
| `marker` | 媒体信息/标记读取；`marker::media_read` 远程读闸门 |
| `playback` / `media-server` | 播放会话；Jellyfin/Emby 兼容协议层（ADR-0005） |
| `cover-generator` | 库封面生成 |
| `api` | axum HTTP 服务，组装以上所有 crate |

依赖方向：其他 crate 只能依赖 `domain`/`store`，禁止依赖 `api`。

### 前端（`web/`）
Vite + React + TS，pnpm。`web/src/main.tsx` 入口，`web/app/` 路由页面，`web/components/` 组件，`web/lib/` 逻辑，`web/lib/api/*` API 客户端，`web/test/` 测试（`node --test`）。dev server :3334，`vite.config.ts` 把 `/api`、`/posters` 代理到后端 :18765 —— 前端始终同源调用 `/api/v1`，不要写死后端地址。

### 关键位置
- **单写者**：`Arc<Mutex<Store>>`，写入一律走 `store_mutate`（`crates/api/src/http/store_wait.rs`，等待预算 5s，`STORE_READ_WAIT`）。
- **读池**：`StoreReaders`（`crates/api/src/store_readers.rs`，`STORE_READERS = 8`），读一律走 `store_read` / `store_read_handle`。
- **远程读闸门**：`marker::media_read::configure_remote_reads(REMOTE_READ_LIMIT, REMOTE_READ_COOLDOWN)`，在 `crates/api/src/probe_manager/worker.rs` 配置。
- **图片代理**：`crates/api/src/http/image_proxy.rs`（+`runtime.rs`，`IMAGE_CONCURRENCY = 8`）。
- **任务 SSE 流**：`crates/api/src/http/jobs.rs`（`Sse::new(ReceiverStream)`）。
- **路由表**：`crates/api/src/http/mod.rs`（`/api/v1`）+ `http/routes/{mod,member,admin}.rs`。API 约定：`docs/api-contracts/self.md`。
- ADR：`docs/adr/`；E2E：`docs/testing/e2e-runbook.md`、`docs/testing/system-regression-cases.md`。

## 硬性规则

1. **TDD**：先写失败测试（red）再实现（green）。测试放在行为的 public seam，断言可观察行为，不测私有 helper / SQL 文本 / mock 调用次数。
2. **零向后兼容**：不写兼容层、迁移垫片、旧字段兜底；直接选最优设计。
3. **锁纪律**：
   - 读永不拿 writer 锁（走 `store_read`）。
   - 写锁粒度最小，只包住真正的写。
   - 任何锁都不得跨 `.await`、网络或文件 IO 持有。
4. **不可改的常量**：strm media-info probe 并发必须保持 1（`REMOTE_READ_LIMIT = 1`），远程读冷却 500ms（`REMOTE_READ_COOLDOWN`）。不要调整。
5. **删除 Library（ADR-0012）**：先隐藏 → 立即取消该库所有任务 → 删除 → 清理孤儿 Media（含 favorites / playback 进度）。某 Media 最后一个文件被删时，其 favorites / 进度一并清除。
6. **Git**：只在当前分支提交；绝不 push `main`，绝不 force push，未经询问不开 PR。不要提交 `docs/test-reports/`、`.e2e/`，不要夹带无关改动。
7. **静态图片端点不鉴权**：`/posters/*`、`/fanart/*`、`/stills/*`、`/chapters/*`、`/libraries/{id}/cover` 必须留在 public router（`<img>` 不带 `Authorization`）。需要用户上下文时注入 `Option<axum::Extension<Option<domain::UserId>>>`。Jellyfin 视频流按 Jellyfin 协议鉴权。
8. **Issue**：除非用户明确要求，不创建/评论/关闭 GitHub issue。只做用户当前请求，不扩散。

## 资源规则（Mac mini，磁盘/CPU 紧张）

- 同一时间只允许**一个** cargo / test / build 进程。
- cargo 一律 `-j4`，测试一律 `-- --test-threads=4`。
- subagent 可以并行改代码，但 build/test 必须串行。
- 优先跑定向测试（`-p <crate>`、`--test <name>`、测试名过滤）；全量套件只在最后跑一次，带 `--no-fail-fast`。
- 临时文件放 `/tmp`，用完删除。

## 命令

```sh
# Rust
cargo fmt --all -- --check
cargo clippy -j4 --all-targets -- -D warnings
cargo test -j4 -p <crate> -- --test-threads=4                 # 每个改动的 crate
cargo test -j4 -p api --test <file> <name> -- --test-threads=4 # 定向
cargo test -j4 --workspace --no-fail-fast -- --test-threads=4  # 收尾全量，仅一次

# Web（cd web）
pnpm lint
pnpm typecheck     # tsc --noEmit
pnpm test          # node --test
pnpm build

# 本地/E2E 环境
./start-test.sh start|restart|restart-all|stop|status|logs   # 后端 :18765，前端 :3334
# 完整 E2E 流程、DATA_DIR 写法、压测脚本：docs/testing/e2e-runbook.md（脚本在 scripts/e2e/）
```

## 测试约定

- 每个 crate 必须可单独测试：`cargo test -p media` 不得依赖 qBittorrent、真实 TMDB 或 Chromium。
- 集成测试放 `crates/<name>/tests/`；单测放模块旁 `#[cfg(test)]`。
- HTTP / TMDB / qB / CDP 通过注入 trait（`Fetcher`、catalog client、`Downloader`、`MediaProbe`）替换为 fake；`cargo test` 不访问外网。
- 解析器用 golden fixtures（`crates/<name>/tests/fixtures/`）。测试中不启动 Chromium。

## Rust 规范

- 文件/函数尺寸硬限制：`.rs` 软 200 / 硬 800 行；生产函数软 60 / 硬 120；测试函数软 80 / 硬 120；`lib.rs` 只放模块树与 re-export。按职责拆，不为凑行数拆。检查：
  `{ git ls-files 'crates/**/*.rs'; git ls-files --others --exclude-standard 'crates/**/*.rs'; } | sort -u | xargs wc -l | awk '$1>800 && $2!="total"'`
- 一个模块一个变更理由；public API 最小化，parse/SQL/HTTP 映射保持私有。
- 错误用 `thiserror`；IO/parse 路径禁止 `unwrap`/`expect`（测试除外）；`unsafe` 必须附注释。
- 版本继承 `workspace.package` / `workspace.dependencies`。
- 类型命名用 `CONTEXT.md` 术语（`Subscribe`、`Torrent`、`Release`、`Library`、`wash_cut`），不要发明 `SearchResult`/`MetaInfo`/`MediaMeta`。
- 日志不能静默：`error!` 意外失败（带完整上下文）、`warn!` 可恢复降级、`info!` 业务生命周期（任务起止、订阅轮次结果、下载添加、转移/硬链、刮削、登录）、`debug!` 解析细节与耗时。

## 调试提示

- **卡住/超时先找根因**，不要加大超时或重试掩盖：查锁等待（writer 锁是否跨 await/IO 持有、`store_mutate` 5s 超时）、读池耗尽（8 个 reader 是否被长查询占满）、远程读闸门排队、任务未被取消。
- 日志：`.test-logs/backend.log`；`python3 scripts/e2e/logstats.py` 统计慢请求/5xx/探测阶段耗时。
- E2E 前 `touch crates/api/build.rs` 刷新版本戳（`GIT_HASH`），再确认 `/api/v1/health` 的 commit 等于 `git rev-parse --short HEAD`。

## 工作流程

1. 只读与请求相关的 `CONTEXT.md` 段落、相关 ADR、本文件。
2. 先测试后实现；跑改动 crate 的测试；收尾跑一次全量。
3. 全绿后在当前分支提交；总结改了什么、用户可见效果、验证结果。
4. 子 agent 用于并行调研或独立切片，但父 agent 负责提交与总结。
