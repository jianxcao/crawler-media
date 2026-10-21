#!/usr/bin/env bash
# 存储干净切换（docs/superpowers/plans/2026-10-20-storage-clean-cutover.md）
#
# cache.db 只承载外部响应体：它是派生数据、不是事实来源
# （docs/superpowers/specs/2026-10-18-source-separated-cache.md）。
# 切换前布局（旧索引名 *_cache_recent_idx / *_cache_retention_idx）被 Store 启动按计划拒绝，
# 本脚本按 runbook 把旧 cache.db 移到备份目录，让 Store 用当前 DDL 重建它。
#
# 绝不触碰事实库 app.db / library.db / subscribe.db / jobs.db；不删除任何文件。
set -euo pipefail

DATA_DIR="data/live"
DRY_RUN=0
FORCE=0

usage() {
  cat <<'USAGE'
用法: bash scripts/storage-cutover-rebuild.sh [--data-dir <dir>] [--dry-run] [--force]

  --data-dir <dir>  数据目录（默认 data/live）
  --dry-run         只打印将要执行的动作，不改动文件
  --force           后端仍在运行、或无法确认布局时也继续

动作: 停止服务后，把 <dir>/cache.db(含 -wal/-shm) 移入 <dir>/backup-cutover-<时间戳>/，
      重启服务时 Store 以当前 DDL 重建 cache.db（application_id=1129139251, user_version=1）。
回滚: 把 <dir>/backups/cache-cutover-<时间戳>/ 中的 cache.db(含 -wal/-shm) 移回 <dir>/。
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --data-dir) DATA_DIR="${2:?--data-dir 需要一个目录}"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    --force) FORCE=1; shift ;;
    -h|--help) usage; exit 0 ;;
    *) echo "[!!] 未知参数: $1" >&2; usage >&2; exit 2 ;;
  esac
done

CACHE="$DATA_DIR/cache.db"
if [[ ! -d "$DATA_DIR" ]]; then
  echo "[!!] 数据目录不存在: $DATA_DIR" >&2
  exit 1
fi
if [[ ! -f "$CACHE" ]]; then
  echo "[ok] $CACHE 不存在，无需重建（启动时会按当前 DDL 新建）"
  exit 0
fi

if [[ "$FORCE" != "1" ]] && command -v lsof >/dev/null 2>&1; then
  if [[ -n "$(lsof -- "$CACHE" 2>/dev/null | tail -n +2)" ]]; then
    echo "[!!] $CACHE 仍被进程持有，请先停服（确需继续可加 --force）:" >&2
    lsof -- "$CACHE" 2>/dev/null | tail -n +2 >&2
    exit 1
  fi
fi

NEEDS_REBUILD=1
if command -v sqlite3 >/dev/null 2>&1; then
  declared="'idx_tmdb_cache_retention','idx_douban_cache_retention','idx_tvdb_cache_retention','idx_bangumi_cache_retention','idx_anilist_cache_retention','idx_fanart_cache_retention'"
  found=$(sqlite3 "$CACHE" "SELECT count(*) FROM sqlite_schema WHERE type='index' AND name IN ($declared);" 2>/dev/null || echo "0")
  app_id=$(sqlite3 "$CACHE" "PRAGMA application_id;" 2>/dev/null || echo "?")
  user_ver=$(sqlite3 "$CACHE" "PRAGMA user_version;" 2>/dev/null || echo "?")
  echo "[..] $CACHE: application_id=$app_id user_version=$user_ver 已声明保留索引 $found/6"
  if [[ "$found" == "6" && "$app_id" == "1129139251" && "$user_ver" == "1" ]]; then
    NEEDS_REBUILD=0
  fi
else
  echo "[..] 未找到 sqlite3，无法只读确认布局，按需要重建处理（cache.db 是派生数据）"
fi

if [[ "$NEEDS_REBUILD" == "0" && "$FORCE" != "1" ]]; then
  echo "[ok] cache.db 已是当前布局，无需重建"
  exit 0
fi

BACKUP="$DATA_DIR/backups/cache-cutover-$(date +%Y%m%dT%H%M%S)"
echo "[..] 备份目录: $BACKUP"
for suffix in "" "-wal" "-shm"; do
  src="$CACHE$suffix"
  [[ -f "$src" ]] || continue
  if [[ "$DRY_RUN" == "1" ]]; then
    echo "     [dry-run] mv $src $BACKUP/"
  else
    mkdir -p "$BACKUP"
    mv "$src" "$BACKUP/"
    echo "[ok] 已移动 $src -> $BACKUP/"
  fi
done

if [[ "$DRY_RUN" == "1" ]]; then
  echo "[dry-run] 未改动任何文件；事实库 app/library/subscribe/jobs 不在本脚本范围内"
  exit 0
fi

echo "[ok] 事实库 app.db / library.db / subscribe.db / jobs.db 未被触碰（只重建派生缓存）"
echo "[next] 启动服务，Store 会新建 cache.db；核对:"
echo "       sqlite3 $CACHE 'PRAGMA application_id; PRAGMA user_version;'  # 期望 1129139251 / 1"
echo "[rollback] mv $BACKUP/cache.db* $DATA_DIR/"
