#!/usr/bin/env bash
# =============================================================================
# crawler-media 一键测试环境脚本
#
# 用法:
#   ./start-test.sh start          启动全部服务（后端 / 前端 / mock 站点，默认清空旧日志）
#   ./start-test.sh restart        仅重启并构建后端（清空后端日志，前端与 qB 保持常驻运行）
#   ./start-test.sh restart-all    全量重启（后端、前端、mock 站点，清空所有日志）
#   ./start-test.sh stop           停止全部服务
#   ./start-test.sh status         查看各服务状态
#   ./start-test.sh logs           跟随查看后端日志（终端实时输出）
#   ./start-test.sh clean-logs     清空全部测试日志（.test-logs/*.log）
#
# 选项:
#   --clean / --clean-logs         显式指定清除日志（start/restart 已默认自动清理）
#   --no-clean                     启动/重启时不清除旧日志
#
# 启动后访问:  http://127.0.0.1:3334   账号 admin / 密码 test-admin-secret-password
# =============================================================================
set -euo pipefail

# ----------------------------- 配置区（按需修改） ----------------------------
BACKEND_PORT="${BACKEND_PORT:-18765}"
FRONTEND_PORT="${FRONTEND_PORT:-3334}"
QB_PORT="${QB_PORT:-8080}"
MOCK_PORT="${MOCK_PORT:-18090}"
DATA_DIR="${DATA_DIR:-data/live}"
TOKEN="${TOKEN:-changeme}"
ADMIN_PASSWORD="${ADMIN_PASSWORD:-test-admin-secret-password}"
QB_USER="${QB_USER:-admin}"
QB_PASS="${QB_PASS:-adminadmin}"
QB_DOCKER="${QB_DOCKER:-crawler-media-qb}"
QB_HOST_DOWNLOADS="${QB_HOST_DOWNLOADS:-$HOME/Downloads/crawler-media-test/qb}"
MOCK_SITE_PY="${MOCK_SITE_PY:-/tmp/mock-site/server.py}"

# 仓库根目录（脚本所在位置）
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BACKEND_BIN="$ROOT_DIR/target/debug/crawler-media"
FRONTEND_DIR="$ROOT_DIR/web"

# 非交互 shell 常缺的 PATH：rustup cargo + homebrew node + docker
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:$PATH"
command -v cargo >/dev/null 2>&1 || { echo "[!!] 找不到 cargo，请先安装 Rust: curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh"; exit 1; }

# 日志文件
LOG_DIR="$ROOT_DIR/.test-logs"
mkdir -p "$LOG_DIR"
BACKEND_LOG="$LOG_DIR/backend.log"
FRONTEND_LOG="$LOG_DIR/frontend.log"
MOCK_LOG="$LOG_DIR/mock.log"

clean_logs() {
  local target="${1:-all}"
  case "$target" in
    backend)
      if [[ -f "$BACKEND_LOG" ]]; then
        : > "$BACKEND_LOG"
        log "已清空后端日志: $BACKEND_LOG"
      fi
      ;;
    frontend)
      if [[ -f "$FRONTEND_LOG" ]]; then
        : > "$FRONTEND_LOG"
        log "已清空前端日志: $FRONTEND_LOG"
      fi
      ;;
    mock)
      if [[ -f "$MOCK_LOG" ]]; then
        : > "$MOCK_LOG"
        log "已清空 mock 站点日志: $MOCK_LOG"
      fi
      ;;
    all|*)
      for f in "$BACKEND_LOG" "$FRONTEND_LOG" "$MOCK_LOG"; do
        [[ -f "$f" ]] && : > "$f"
      done
      log "已清空所有测试日志 (.test-logs/*.log)"
      ;;
  esac
}

# ------------------------------- 工具函数 -----------------------------------
log()  { printf '\033[1;36m[test-env]\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m[ok]\033[0m %s\n' "$*"; }
fail() { printf '\033[1;31m[!!]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[warn]\033[0m %s\n' "$*"; }

local_curl() {
  command curl -q --noproxy "*" --connect-timeout 1 --max-time 2 "$@"
}

pid_file() { echo "$LOG_DIR/$1.pid"; }

read_pid() {
  local file="$1"
  [[ -f "$file" ]] || return 1
  local pid
  pid="$(tr -d '[:space:]' < "$file" 2>/dev/null || true)"
  [[ "$pid" =~ ^[0-9]+$ ]] || return 1
  printf '%s' "$pid"
}

is_running() {
  local pid
  pid="$(read_pid "$1")" || return 1
  kill -0 "$pid" 2>/dev/null
}

# lsof 可能吐出多个 PID（换行分隔）。逐个 SIGTERM，仍占端口再 SIGKILL。
# 等到 LISTEN 消失再返回：Python TCPServer 默认不 SO_REUSEADDR，立刻 bind 会 EADDRINUSE。
free_port() {
  local port="$1"
  local pids pid i
  pids="$(lsof -tiTCP:"$port" -sTCP:LISTEN 2>/dev/null || true)"
  if [[ -n "$pids" ]]; then
    warn "端口 $port 被占用 (PID $(echo "$pids" | tr '\n' ' '))，先停掉再启动"
    for pid in $pids; do
      kill "$pid" 2>/dev/null || true
    done
    sleep 1
    pids="$(lsof -tiTCP:"$port" -sTCP:LISTEN 2>/dev/null || true)"
    if [[ -n "$pids" ]]; then
      for pid in $pids; do
        kill -9 "$pid" 2>/dev/null || true
      done
    fi
  fi
  for i in $(seq 1 10); do
    pids="$(lsof -tiTCP:"$port" -sTCP:LISTEN 2>/dev/null || true)"
    [[ -z "$pids" ]] && return 0
    sleep 0.3
  done
}

# 后台启动并把「真正的子进程」PID 写入 pid 文件。
# 不用 ( cmd & echo $! )：在 macOS bash 3.2 下 $! 经常是包装 shell，
# stop 只杀掉 wrapper，next-server / python 会残留占端口。
launch() {
  local name="$1"
  shift
  "$@" >>"$LOG_DIR/${name}.log" 2>&1 &
  echo $! > "$(pid_file "$name")"
}

# -------------------------------- 各服务 ------------------------------------
start_backend() {
  local force="${1:-false}"
  if [[ "$force" != "true" ]] && local_curl -sf --connect-timeout 1 -m 2 "http://127.0.0.1:$BACKEND_PORT/api/v1/health" >/dev/null 2>&1; then
    log "后端已在运行且健康 (端口 $BACKEND_PORT)，跳过重复启动"
    return 0
  fi

  if is_running "$(pid_file backend)"; then
    log "停止旧后端进程 (PID $(read_pid "$(pid_file backend)")) 并释放端口..."
    stop_service backend
  fi
  free_port "$BACKEND_PORT"

  log "构建后端..."
  (cd "$ROOT_DIR" && cargo build -p api 2>>"$BACKEND_LOG") || { fail "构建失败，见 $BACKEND_LOG"; return 1; }
  
  # 确保端口彻底释放，无任何 TIME_WAIT 或残留监听
  free_port "$BACKEND_PORT"

  log "启动后端 (端口 $BACKEND_PORT)..."
  printf '\n=== [START BACKEND %s] ===\n' "$(date)" >>"$BACKEND_LOG"
  launch backend env \
    RUST_LOG="${RUST_LOG:-info,api=debug,crawler_media=debug,domain=debug,indexer=debug,media=debug,downloader=debug,library=debug,subscribe=debug,filter=debug,release=debug,hooks=debug,jobs=debug,playback=debug,marker=debug,html5ever=off,selectors=warn}" \
    CRAWLER_MEDIA_FFMPEG_LOG_LEVEL="${CRAWLER_MEDIA_FFMPEG_LOG_LEVEL:-verbose}" \
    CRAWLER_MEDIA_DATA="$ROOT_DIR/$DATA_DIR" \
    CRAWLER_MEDIA_TOKEN="$TOKEN" \
    CRAWLER_MEDIA_ADMIN_PASSWORD="${ADMIN_PASSWORD:-test-admin-secret-password}" \
    CRAWLER_MEDIA_QB_URL="http://localhost:$QB_PORT" \
    CRAWLER_MEDIA_QB_USER="$QB_USER" \
    CRAWLER_MEDIA_QB_PASS="$QB_PASS" \
    CRAWLER_MEDIA_QB_PATH_MAP="/downloads=$QB_HOST_DOWNLOADS" \
    CRAWLER_MEDIA_CORS_ORIGINS="http://127.0.0.1:$FRONTEND_PORT,http://localhost:$FRONTEND_PORT" \
    CRAWLER_MEDIA_LISTEN="127.0.0.1:$BACKEND_PORT" \
    "$BACKEND_BIN" serve

  # 等待后端 HTTP 存活探针就绪
  local i
  for i in $(seq 1 30); do
    if local_curl -sf "http://127.0.0.1:$BACKEND_PORT/api/v1/health" >/dev/null 2>&1; then
      ok "后端就绪 http://127.0.0.1:$BACKEND_PORT"
      return 0
    fi
    # 如果进程已经意外退出，立刻报错并打印日志，绝不空等
    if ! is_running "$(pid_file backend)"; then
      fail "后端进程已提前退出，见 $BACKEND_LOG"
      tail -20 "$BACKEND_LOG"
      return 1
    fi
    sleep 0.5
  done
  fail "后端启动超时（已等待 15 秒），见 $BACKEND_LOG"
  tail -20 "$BACKEND_LOG"
  return 1
}

start_frontend() {
  if is_running "$(pid_file frontend)"; then
    log "前端已在运行 (PID $(read_pid "$(pid_file frontend)"))"
    return
  fi
  if [[ ! -x "$FRONTEND_DIR/node_modules/.bin/vite" ]]; then
    fail "找不到 vite，请先: cd web && pnpm install"
    return 1
  fi
  free_port "$FRONTEND_PORT"
  log "启动前端 (端口 $FRONTEND_PORT)..."
  # Vite dev 直连后端(不走反代):VITE_API_BASE_URL 指到后端端口,
  # 跨域由后端 CORS 层放行(start_backend 里按 FRONTEND_PORT 配了白名单)。
  # 先跑 copy-jassub 复制字幕渲染资产,再 exec vite,避免 pid 文件记到 shell 包装器。
  (
    cd "$FRONTEND_DIR"
    if [[ -f scripts/copy-jassub.mjs ]]; then
      node scripts/copy-jassub.mjs >>"$FRONTEND_LOG" 2>&1 || true
    fi
    launch frontend env VITE_API_BASE_URL="http://127.0.0.1:$BACKEND_PORT/api/v1" \
      node "$FRONTEND_DIR/node_modules/vite/bin/vite.js" \
      --host 127.0.0.1 --port "$FRONTEND_PORT" --strictPort
  )
  local _
  for _ in $(seq 1 90); do
    if local_curl -sf -o /dev/null "http://127.0.0.1:$FRONTEND_PORT/" 2>/dev/null; then
      ok "前端就绪 http://127.0.0.1:$FRONTEND_PORT"
      return 0
    fi
    sleep 1
  done
  fail "前端启动超时，见 $FRONTEND_LOG"; tail -20 "$FRONTEND_LOG"; return 1
}

start_qb() {
  # 假设 qBittorrent 容器常驻运行，不每次检查/拉起，减少启动耗时
  return 0
}

start_mock() {
  if is_running "$(pid_file mock)"; then
    log "mock 站点已在运行 (PID $(read_pid "$(pid_file mock)"))"
    return
  fi
  if [[ ! -f "$MOCK_SITE_PY" ]]; then
    warn "mock 站点脚本不存在 ($MOCK_SITE_PY)，跳过（不影响真实站点）"
    return
  fi
  free_port "$MOCK_PORT"
  log "启动 mock 站点 (端口 $MOCK_PORT)..."
  (
    cd "$(dirname "$MOCK_SITE_PY")"
    launch mock python3 "$MOCK_SITE_PY"
  )
  http_up() {
    local code
    code="$(curl -s -o /dev/null -w '%{http_code}' --connect-timeout 1 -m 2 "$1" 2>/dev/null || true)"
    [[ "$code" =~ ^[1-5][0-9][0-9]$ ]]
  }
  local _
  for _ in $(seq 1 10); do
    if http_up "http://127.0.0.1:$MOCK_PORT/torrents.php"; then
      ok "mock 站点就绪 http://127.0.0.1:$MOCK_PORT"
      return 0
    fi
    # 进程已经挂掉就别空等
    if ! is_running "$(pid_file mock)"; then
      warn "mock 站点进程退出，见 $MOCK_LOG"
      return 0
    fi
    sleep 1
  done
  warn "mock 站点未响应，见 $MOCK_LOG（不影响真实站点）"
}

# -------------------------------- 停止 --------------------------------------
stop_tree() {
  local pid="$1"
  local child
  # 先杀子进程（next-server、python 等），再杀自己
  for child in $(pgrep -P "$pid" 2>/dev/null || true); do
    stop_tree "$child"
  done
  kill "$pid" 2>/dev/null || true
}

stop_service() {
  local name="$1"
  local file pid
  file="$(pid_file "$name")"
  pid="$(read_pid "$file" || true)"
  if [[ -n "${pid:-}" ]] && kill -0 "$pid" 2>/dev/null; then
    stop_tree "$pid"
    sleep 1
    if kill -0 "$pid" 2>/dev/null; then
      kill -9 "$pid" 2>/dev/null || true
      local child
      for child in $(pgrep -P "$pid" 2>/dev/null || true); do
        kill -9 "$child" 2>/dev/null || true
      done
    fi
    log "已停止 $name (PID $pid)"
  fi
  rm -f "$file"
}

stop_all() {
  stop_service mock
  stop_service frontend
  stop_service backend
  # 端口兜底：pid 文件错了或上次异常退出时，清掉本脚本占用的监听口
  free_port "$FRONTEND_PORT"
  free_port "$BACKEND_PORT"
  free_port "$MOCK_PORT"
  sleep 1
  log "全部已停止（qB 容器保留运行）"
}

# -------------------------------- 状态 --------------------------------------
status_all() {
  printf '\n=== 服务状态 ===\n'
  local backend="✗ 未运行" frontend="✗ 未运行" qb="✗ 未运行" mock="✗ 未运行"
  local_curl -sf -o /dev/null "http://127.0.0.1:$BACKEND_PORT/api/v1/health" 2>/dev/null && backend="✓ 运行中 (http://127.0.0.1:$BACKEND_PORT)"
  local_curl -sf -o /dev/null "http://127.0.0.1:$FRONTEND_PORT/" 2>/dev/null && frontend="✓ 运行中 (http://127.0.0.1:$FRONTEND_PORT)"
  local_curl -sf -o /dev/null -X POST "http://localhost:$QB_PORT/api/v2/auth/login" -d "username=$QB_USER&password=$QB_PASS" 2>/dev/null && qb="✓ 运行中 (http://localhost:$QB_PORT)"
  mock_code="$(local_curl -s -o /dev/null -w '%{http_code}' --connect-timeout 1 -m 2 "http://127.0.0.1:$MOCK_PORT/torrents.php" 2>/dev/null || true)"
  if [[ "$mock_code" =~ ^[1-5][0-9][0-9]$ ]]; then
    mock="✓ 运行中 (http://127.0.0.1:$MOCK_PORT)"
  elif is_running "$(pid_file mock)"; then
    mock="· 进程在、端口未响应 (http://127.0.0.1:$MOCK_PORT)"
  fi
  printf '  后端: %s\n  前端: %s\n  qB:   %s\n  mock: %s\n' "$backend" "$frontend" "$qb" "$mock"
  printf '\n  登录页面: http://127.0.0.1:%s   (账号 admin / 密码 %s)\n' "$FRONTEND_PORT" "$ADMIN_PASSWORD"
  printf '  API 接口: http://127.0.0.1:%s   (CLI Bearer Token: %s)\n' "$BACKEND_PORT" "$TOKEN"
}

# -------------------------------- 主入口 ------------------------------------
ACTION="${1:-}"
CLEAN_MODE="${CLEAN_LOGS:-auto}"

# 解析后续可能传入的标志，例如: ./start-test.sh start --no-clean 或 ./start-test.sh restart --clean
for arg in "${@:2}"; do
  case "$arg" in
    --clean|--clean-logs)
      CLEAN_MODE="true"
      ;;
    --no-clean|--keep-logs)
      CLEAN_MODE="false"
      ;;
  esac
done

case "$ACTION" in
  start)
    if [[ "$CLEAN_MODE" != "false" ]]; then
      clean_logs all
    fi
    start_qb
    start_backend
    start_frontend
    start_mock
    # 预热：Vite 首次访问每个路由会现场按需编译依赖图（transform），
    # 全部页面包在首屏模块里，先访问一遍让依赖编译进缓存，避免浏览器首开卡顿。
    log "预热前端路由（触发 Vite 依赖编译）..."
    local_route=""
    for local_route in / /login /library /subscriptions /activity /discover/movie /settings /settings/overview /health; do
      local_curl -sf -o /dev/null -m 30 "http://127.0.0.1:$FRONTEND_PORT$local_route" 2>/dev/null && printf '  ✓ %s\n' "$local_route" || printf '  · %s (跳过)\n' "$local_route"
    done
    ok "路由预热完成"
    status_all
    printf '\n  打开 http://127.0.0.1:%s 开始测试\n' "$FRONTEND_PORT"
    ;;
  stop)
    stop_all
    ;;
  restart)
    # 快速重启：仅重启并重构后端，前端 Vite 与 qB 保持常驻运行
    log "正在仅重启后端..."
    stop_service backend
    if [[ "$CLEAN_MODE" != "false" ]]; then
      clean_logs backend
    fi
    start_backend true
    status_all
    ;;
  restart-all)
    stop_all
    sleep 2
    if [[ "$CLEAN_MODE" != "false" ]]; then
      clean_logs all
    fi
    exec "$0" start --no-clean
    ;;
  clean-logs)
    clean_logs "${2:-all}"
    ;;
  status)
    status_all
    ;;
  logs)
    tail -f "$BACKEND_LOG"
    ;;
  *)
    sed -n '2,19p' "$0"
    exit 1
    ;;
esac
