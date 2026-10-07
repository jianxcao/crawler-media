use rusqlite::{Connection, OptionalExtension, params};

use super::StoreError;

/// Schema version constants. Bump when adding new migrations.
/// The version is stored via `PRAGMA user_version` on each database file.
pub const APP_SCHEMA_VERSION: i64 = 3;
pub const CATALOG_SCHEMA_VERSION: i64 = 1;
pub const LIBRARY_SCHEMA_VERSION: i64 = 4;
pub const SUBSCRIBE_SCHEMA_VERSION: i64 = 2;

/// Read the current schema version from a database file.
fn schema_version(conn: &Connection) -> i64 {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap_or(0)
}

/// Set the schema version after successful migration.
fn set_schema_version(conn: &Connection, version: i64) -> Result<(), StoreError> {
    conn.pragma_update(None, "user_version", version)?;
    Ok(())
}

pub fn migrate_app(conn: &Connection) -> Result<(), StoreError> {
    let _current = schema_version(conn);
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS users (
            id TEXT PRIMARY KEY,
            login TEXT NOT NULL UNIQUE,
            enabled INTEGER NOT NULL DEFAULT 1
        );
        CREATE TABLE IF NOT EXISTS media (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            title TEXT NOT NULL,
            year INTEGER,
            original_title TEXT,
            tmdb_id TEXT,
            douban_id TEXT,
            tvdb_id TEXT,
            bangumi_id TEXT,
            anilist_id TEXT
        );
        CREATE TABLE IF NOT EXISTS sites (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            url TEXT NOT NULL,
            profile_id TEXT NOT NULL,
            cookie TEXT,
            api_key TEXT,
            rss_url TEXT,
            proxy TEXT,
            rate_limit_per_minute INTEGER,
            cdp_url TEXT,
            downloader_id TEXT,
            enabled INTEGER NOT NULL DEFAULT 1
        );
        CREATE TABLE IF NOT EXISTS filters (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            atoms_json TEXT NOT NULL,
            keep_old_versions INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS user_tokens (
            token TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            created_at INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS search_snapshots (
            id TEXT PRIMARY KEY,
            vertical TEXT NOT NULL,
            query TEXT NOT NULL,
            payload TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS downloaders (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            kind TEXT NOT NULL,
            url TEXT NOT NULL,
            username TEXT,
            password TEXT,
            category TEXT,
            is_default INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS library_roots (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            path TEXT NOT NULL,
            is_default INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS libraries (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            name TEXT NOT NULL,
            is_default INTEGER NOT NULL DEFAULT 0,
            sort_order INTEGER NOT NULL DEFAULT 0,
            access_mode TEXT NOT NULL DEFAULT 'everyone',
            admin_visible INTEGER NOT NULL DEFAULT 1,
            member_ids_json TEXT NOT NULL DEFAULT '[]',
            detect_intros INTEGER NOT NULL DEFAULT 1,
            enable_fingerprint INTEGER NOT NULL DEFAULT 0,
            cover_path TEXT,
            match_rules_json TEXT NOT NULL DEFAULT '[]',
            default_filter_id TEXT,
            realtime_watch INTEGER NOT NULL DEFAULT 1,
            generate_thumbnails INTEGER NOT NULL DEFAULT 1,
            extract_chapter_images INTEGER NOT NULL DEFAULT 1,
            exclude_from_home INTEGER NOT NULL DEFAULT 0,
            auto_series_collections INTEGER NOT NULL DEFAULT 1
        );
        CREATE TABLE IF NOT EXISTS naming_templates (
            kind TEXT PRIMARY KEY,
            pattern TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS sites_enabled_idx ON sites(enabled);
        CREATE INDEX IF NOT EXISTS user_tokens_user_idx ON user_tokens(user_id);
        "#,
    )?;
    ensure_column(conn, "downloaders", "path_maps", "TEXT")?;
    // downloaders.enabled：停用的下载器不参与自动投递（手动提交仍可用）。
    ensure_column(conn, "downloaders", "enabled", "INTEGER NOT NULL DEFAULT 1")?;
    ensure_column(conn, "users", "role", "TEXT NOT NULL DEFAULT 'member'")?;
    ensure_column(conn, "users", "password", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(conn, "users", "enabled", "INTEGER NOT NULL DEFAULT 1")?;
    // 老库回填：把既有的 token 行当作密码（token 即密码的旧模型），
    // 保证升级后 admin 仍能用原来的口令登录（changeme 等）。
    conn.execute(
        "UPDATE users SET password = COALESCE(
             (SELECT token FROM user_tokens WHERE user_id = users.id LIMIT 1), '')
          WHERE password = ''",
        [],
    )?;
    ensure_column(
        conn,
        "filters",
        "keep_old_versions",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(conn, "library_roots", "library_id", "TEXT")?;
    ensure_column(
        conn,
        "library_roots",
        "sort_order",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(conn, "libraries", "detect_intros", "INTEGER")?;
    ensure_column(conn, "libraries", "enable_fingerprint", "INTEGER")?;
    ensure_column(conn, "libraries", "cover_path", "TEXT")?;
    ensure_column(
        conn,
        "libraries",
        "match_rules_json",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    ensure_column(conn, "libraries", "default_filter_id", "TEXT")?;
    ensure_column(
        conn,
        "libraries",
        "realtime_watch",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    ensure_column(
        conn,
        "libraries",
        "generate_thumbnails",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    ensure_column(
        conn,
        "libraries",
        "extract_chapter_images",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    ensure_column(
        conn,
        "libraries",
        "exclude_from_home",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "libraries",
        "auto_series_collections",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    // 会话 token 时效：老库补 created_at 列，并把存量 token 视为刚刚签发，
    // 升级后获得一个完整的 30 天窗口（Session TTL）而不是立即被过期掉。
    ensure_column(
        conn,
        "user_tokens",
        "created_at",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    conn.execute(
        "UPDATE user_tokens SET created_at = strftime('%s','now') WHERE created_at = 0",
        [],
    )?;
    // libraries 可见性列（老库需逐列补齐，否则 SELECT 报 no such column）。
    ensure_column(
        conn,
        "libraries",
        "access_mode",
        "TEXT NOT NULL DEFAULT 'everyone'",
    )?;
    ensure_column(
        conn,
        "libraries",
        "admin_visible",
        "INTEGER NOT NULL DEFAULT 1",
    )?;
    ensure_column(
        conn,
        "libraries",
        "member_ids_json",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS collections (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            name TEXT NOT NULL,
            sort_order INTEGER NOT NULL DEFAULT 0,
            created_at TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS collection_items (
            collection_id TEXT NOT NULL,
            media_item_id TEXT NOT NULL,
            sort_order INTEGER NOT NULL DEFAULT 0,
            added_at TEXT NOT NULL,
            PRIMARY KEY (collection_id, media_item_id)
        );",
    )?;
    set_schema_version(conn, APP_SCHEMA_VERSION)?;
    Ok(())
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    decl: &str,
) -> Result<(), StoreError> {
    let has: bool = conn
        .prepare(&format!("PRAGMA table_info({table})"))?
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == column);
    if !has {
        conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl};"))?;
    }
    Ok(())
}

pub fn migrate_catalog(conn: &Connection) -> Result<(), StoreError> {
    let _current = schema_version(conn);
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS catalog_cache (
            source TEXT NOT NULL,
            cache_key TEXT NOT NULL,
            body TEXT NOT NULL,
            fetched_at INTEGER NOT NULL,
            expires_at INTEGER,
            PRIMARY KEY (source, cache_key)
        );
        "#,
    )?;
    set_schema_version(conn, CATALOG_SCHEMA_VERSION)?;
    Ok(())
}

pub fn migrate_library(conn: &Connection) -> Result<(), StoreError> {
    let _current = schema_version(conn);
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS ledger (
            id TEXT PRIMARY KEY,
            media_id TEXT NOT NULL,
            root_id TEXT,
            path TEXT NOT NULL UNIQUE,
            season INTEGER,
            episode INTEGER,
            resolution TEXT,
            codec TEXT,
            hdr TEXT,
            quality_source TEXT NOT NULL,
            confidence TEXT NOT NULL,
            filter_score INTEGER,
            transfer_mode TEXT
        );
        CREATE INDEX IF NOT EXISTS ledger_path_idx ON ledger(path);
        CREATE INDEX IF NOT EXISTS ledger_media_id_idx ON ledger(media_id);
        CREATE TABLE IF NOT EXISTS scrape_sidecars (
            ledger_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            path TEXT NOT NULL,
            PRIMARY KEY (ledger_id, kind)
        );
        CREATE TABLE IF NOT EXISTS unidentified (
            path TEXT PRIMARY KEY,
            confidence TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS file_meta (
            ledger_id TEXT PRIMARY KEY,
            audio_json TEXT NOT NULL,
            subtitle_json TEXT NOT NULL,
            tracks_version INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS media_markers (
            media_id TEXT NOT NULL,
            season INTEGER NOT NULL,
            episode INTEGER NOT NULL,
            intro_start_ms INTEGER,
            intro_end_ms INTEGER,
            outro_start_ms INTEGER,
            outro_end_ms INTEGER,
            source TEXT NOT NULL,
            locked INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY (media_id, season, episode)
        );
        CREATE TABLE IF NOT EXISTS probe_jobs (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            media_id TEXT NOT NULL,
            season INTEGER,
            scope_key TEXT NOT NULL,
            status TEXT NOT NULL,
            total INTEGER NOT NULL,
            completed INTEGER NOT NULL DEFAULT 0,
            succeeded INTEGER NOT NULL DEFAULT 0,
            failed INTEGER NOT NULL DEFAULT 0,
            error TEXT,
            created_at_ms INTEGER NOT NULL,
            started_at_ms INTEGER,
            finished_at_ms INTEGER
        );
        CREATE TABLE IF NOT EXISTS probe_job_units (
            job_id TEXT NOT NULL,
            ledger_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            force_fingerprint INTEGER NOT NULL DEFAULT 0,
            overwrite_markers INTEGER NOT NULL DEFAULT 0,
            status TEXT NOT NULL,
            error TEXT,
            PRIMARY KEY (job_id, ledger_id),
            FOREIGN KEY (job_id) REFERENCES probe_jobs(id) ON DELETE CASCADE
        );
        CREATE UNIQUE INDEX IF NOT EXISTS probe_jobs_active_scope_idx
            ON probe_jobs(scope_key) WHERE status IN ('queued', 'running');
        CREATE UNIQUE INDEX IF NOT EXISTS probe_job_units_active_ledger_idx
            ON probe_job_units(ledger_id) WHERE status IN ('queued', 'running');
        CREATE INDEX IF NOT EXISTS probe_jobs_media_created_idx
            ON probe_jobs(media_id, created_at_ms DESC);
        CREATE INDEX IF NOT EXISTS probe_job_units_job_status_idx
            ON probe_job_units(job_id, status);
        "#,
    )?;
    // ledger.missing_at：文件核验标记（老库补列，幂等）。
    ensure_column(conn, "ledger", "missing_at", "INTEGER")?;
    ensure_column(conn, "ledger", "transfer_mode", "TEXT")?;
    ensure_column(conn, "ledger", "source_path", "TEXT")?;
    ensure_column(conn, "file_meta", "chapters_json", "TEXT")?;
    ensure_column(conn, "file_meta", "video_json", "TEXT")?;
    ensure_column(
        conn,
        "file_meta",
        "tracks_version",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(conn, "file_meta", "fingerprint_json", "TEXT")?;
    ensure_column(conn, "file_meta", "outro_fingerprint_json", "TEXT")?;
    set_schema_version(conn, LIBRARY_SCHEMA_VERSION)?;
    Ok(())
}

pub fn migrate_subscribe(conn: &Connection) -> Result<(), StoreError> {
    let _current = schema_version(conn);
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS subscribes (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            media_id TEXT NOT NULL,
            coverage_kind TEXT NOT NULL,
            season INTEGER,
            episode_from INTEGER,
            episode_to INTEGER,
            fetch_mode TEXT NOT NULL,
            filter_id TEXT NOT NULL,
            wash_cut INTEGER NOT NULL DEFAULT 0,
            wash_cut_filter_id TEXT,
            full_season_pack INTEGER NOT NULL DEFAULT 0,
            downloader_id TEXT,
            tracking_state TEXT NOT NULL DEFAULT 'active',
            follow_future INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS subscribe_facts (
            subscribe_id TEXT NOT NULL,
            season INTEGER NOT NULL DEFAULT -1,
            episode INTEGER NOT NULL DEFAULT -1,
            score INTEGER NOT NULL,
            path TEXT,
            PRIMARY KEY (subscribe_id, season, episode)
        );
        CREATE TABLE IF NOT EXISTS subscribe_wanted (
            subscribe_id TEXT NOT NULL,
            season INTEGER NOT NULL DEFAULT -1,
            episode INTEGER NOT NULL DEFAULT -1,
            search_attempts INTEGER NOT NULL DEFAULT 0,
            last_search_at INTEGER,
            grabbed_at INTEGER,
            imported_at INTEGER,
            grab_title TEXT,
            last_reject_reason TEXT,
            PRIMARY KEY (subscribe_id, season, episode)
        );
        CREATE TABLE IF NOT EXISTS pending_downloads (
            subscribe_id TEXT NOT NULL,
            enclosure TEXT NOT NULL,
            title TEXT NOT NULL,
            score INTEGER NOT NULL,
            torrent_json TEXT NOT NULL,
            PRIMARY KEY (subscribe_id, enclosure)
        );
        CREATE TABLE IF NOT EXISTS playback_sessions (
            user_id TEXT NOT NULL,
            device_id TEXT NOT NULL,
            media_id TEXT NOT NULL,
            season INTEGER,
            episode INTEGER,
            client TEXT,
            device_name TEXT,
            client_version TEXT,
            play_method TEXT NOT NULL DEFAULT 'local',
            position_ms INTEGER NOT NULL DEFAULT 0,
            start_position_ms INTEGER NOT NULL DEFAULT 0,
            duration_ms INTEGER,
            paused INTEGER NOT NULL DEFAULT 0,
            watched_ms INTEGER NOT NULL DEFAULT 0,
            rate_bps INTEGER NOT NULL DEFAULT 0,
            bytes_sent INTEGER NOT NULL DEFAULT 0,
            connections INTEGER NOT NULL DEFAULT 0,
            admin_ended INTEGER NOT NULL DEFAULT 0,
            started_at INTEGER NOT NULL,
            last_report_at INTEGER NOT NULL,
            PRIMARY KEY (user_id, device_id)
        );
        CREATE TABLE IF NOT EXISTS playback_logs (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            media_id TEXT NOT NULL,
            season INTEGER,
            episode INTEGER,
            client TEXT,
            device_name TEXT,
            play_method TEXT NOT NULL DEFAULT 'local',
            started_at INTEGER NOT NULL,
            ended_at INTEGER NOT NULL,
            watched_ms INTEGER NOT NULL DEFAULT 0,
            start_position_ms INTEGER NOT NULL DEFAULT 0,
            end_position_ms INTEGER NOT NULL DEFAULT 0,
            duration_ms INTEGER,
            completed INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS playback_metrics (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            media_id TEXT NOT NULL,
            tier INTEGER NOT NULL DEFAULT 0,
            engine TEXT,
            ttff_ms INTEGER,
            rebuffer_ms INTEGER NOT NULL DEFAULT 0,
            rebuffer_count INTEGER NOT NULL DEFAULT 0,
            seek_count INTEGER NOT NULL DEFAULT 0,
            dropped_frames INTEGER,
            total_frames INTEGER,
            watched_ms INTEGER NOT NULL DEFAULT 0,
            recorded_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS playback_units (
            user_id TEXT NOT NULL,
            media_id TEXT NOT NULL,
            season INTEGER NOT NULL DEFAULT -1,
            episode INTEGER NOT NULL DEFAULT -1,
            position_ms INTEGER NOT NULL DEFAULT 0,
            played INTEGER NOT NULL DEFAULT 0,
            favorite INTEGER NOT NULL DEFAULT 0,
            duration_ms INTEGER,
            audio_track TEXT,
            subtitle_track TEXT,
            play_count INTEGER NOT NULL DEFAULT 0,
            updated_at INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (user_id, media_id, season, episode)
        );
        CREATE INDEX IF NOT EXISTS subscribes_user_idx ON subscribes(user_id);
        CREATE INDEX IF NOT EXISTS subscribes_media_idx ON subscribes(media_id);
        CREATE INDEX IF NOT EXISTS subscribe_facts_sub_idx ON subscribe_facts(subscribe_id);
        CREATE INDEX IF NOT EXISTS subscribe_wanted_sub_idx ON subscribe_wanted(subscribe_id);
        CREATE INDEX IF NOT EXISTS playback_units_user_media_idx ON playback_units(user_id, media_id);
        CREATE INDEX IF NOT EXISTS playback_sessions_user_idx ON playback_sessions(user_id);
        CREATE INDEX IF NOT EXISTS playback_logs_user_started_idx ON playback_logs(user_id, started_at);
        CREATE INDEX IF NOT EXISTS playback_logs_started_idx ON playback_logs(started_at DESC, id DESC);
        "#,
    )?;
    migrate_playback_session_key(conn)?;
    ensure_column(
        conn,
        "subscribes",
        "tracking_state",
        "TEXT NOT NULL DEFAULT 'active'",
    )?;
    ensure_column(
        conn,
        "subscribes",
        "follow_future",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(
        conn,
        "subscribes",
        "search_interval_secs",
        "INTEGER NOT NULL DEFAULT 1800",
    )?;
    ensure_column(
        conn,
        "subscribes",
        "keep_old_versions",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    ensure_column(conn, "subscribes", "created_at", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(conn, "subscribes", "updated_at", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(conn, "subscribes", "library_id", "TEXT")?;
    ensure_column(
        conn,
        "pending_downloads",
        "submitted_at",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    // pending_downloads.state：'active'（在途）| 'imported'（已入库，保留为
    // 源链接——库文件缺失时据此重新硬链接）。必须在建表之后补列。
    ensure_column(
        conn,
        "pending_downloads",
        "state",
        "TEXT NOT NULL DEFAULT 'active'",
    )?;
    // Index on (subscribe_id, state) — must come after the state column exists.
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS pending_downloads_sub_state_idx \
         ON pending_downloads(subscribe_id, state);",
    )?;
    // 老库遗留的 playback_progress 表：数据并入 playback_units 后删除。
    migrate_legacy_playback_progress(conn)?;
    set_schema_version(conn, SUBSCRIBE_SCHEMA_VERSION)?;
    Ok(())
}

/// Drop the legacy `playback_progress` table after merging its rows into
/// `playback_units`. New installs never create it (it is absent from
/// `migrate_subscribe`'s initial batch), so this only fires on old databases.
fn migrate_legacy_playback_progress(conn: &Connection) -> Result<(), StoreError> {
    let has_table: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'playback_progress')",
        [],
        |row| row.get(0),
    )?;
    if !has_table {
        return Ok(());
    }
    // 旧表 (user_id, media_id) 主键 → 新表 (user_id, media_id, season, episode)；
    // 行键取 (-1, -1)（整部作品），已存在的行不覆盖（新表是权威）。
    conn.execute_batch(
        "INSERT OR IGNORE INTO playback_units (
             user_id, media_id, season, episode, position_ms, played, favorite, updated_at
         )
         SELECT user_id, media_id,
                COALESCE(season, -1), COALESCE(episode, -1),
                position_ms,
                COALESCE(played, 0),
                COALESCE(favorite, 0),
                0
         FROM playback_progress;
         DROP TABLE playback_progress;",
    )?;
    Ok(())
}

fn migrate_playback_session_key(conn: &Connection) -> Result<(), StoreError> {
    let mut primary_key: Vec<(i64, String)> = conn
        .prepare("PRAGMA table_info(playback_sessions)")?
        .query_map([], |row| {
            Ok((row.get::<_, i64>(5)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|(position, _)| *position > 0)
        .collect();
    primary_key.sort_by_key(|(position, _)| *position);
    let columns: Vec<&str> = primary_key.iter().map(|(_, name)| name.as_str()).collect();
    if columns == ["user_id", "device_id"] {
        return Ok(());
    }
    conn.execute_batch(
        r#"
        BEGIN IMMEDIATE;
        DROP TABLE IF EXISTS playback_sessions_new;
        CREATE TABLE playback_sessions_new (
            user_id TEXT NOT NULL,
            device_id TEXT NOT NULL,
            media_id TEXT NOT NULL,
            season INTEGER,
            episode INTEGER,
            client TEXT,
            device_name TEXT,
            client_version TEXT,
            play_method TEXT NOT NULL DEFAULT 'local',
            position_ms INTEGER NOT NULL DEFAULT 0,
            start_position_ms INTEGER NOT NULL DEFAULT 0,
            duration_ms INTEGER,
            paused INTEGER NOT NULL DEFAULT 0,
            watched_ms INTEGER NOT NULL DEFAULT 0,
            rate_bps INTEGER NOT NULL DEFAULT 0,
            bytes_sent INTEGER NOT NULL DEFAULT 0,
            connections INTEGER NOT NULL DEFAULT 0,
            admin_ended INTEGER NOT NULL DEFAULT 0,
            started_at INTEGER NOT NULL,
            last_report_at INTEGER NOT NULL,
            PRIMARY KEY (user_id, device_id)
        );
        INSERT INTO playback_sessions_new (
            user_id, device_id, media_id, season, episode, client, device_name,
            client_version, play_method, position_ms, start_position_ms, duration_ms,
            paused, watched_ms, rate_bps, bytes_sent, connections, admin_ended,
            started_at, last_report_at
        ) SELECT
            user_id, device_id, media_id, season, episode, client, device_name,
            client_version, play_method, position_ms, start_position_ms, duration_ms,
            paused, watched_ms, rate_bps, bytes_sent, connections, admin_ended,
            started_at, last_report_at
        FROM playback_sessions;
        DROP TABLE playback_sessions;
        ALTER TABLE playback_sessions_new RENAME TO playback_sessions;
        COMMIT;
        "#,
    )?;
    Ok(())
}

pub fn seed_defaults(app: &Connection, data_dir: &std::path::Path) -> Result<(), StoreError> {
    let movie_root = data_dir.join("library/movies");
    let tv_root = data_dir.join("library/tv");
    std::fs::create_dir_all(&movie_root)?;
    std::fs::create_dir_all(&tv_root)?;
    app.execute(
        "INSERT OR IGNORE INTO naming_templates (kind, pattern) VALUES (?1, ?2)",
        params![
            "movie",
            "{title} ({year})/{title} ({year}){part} - {resolution}{ext}"
        ],
    )?;
    app.execute(
        "INSERT OR IGNORE INTO naming_templates (kind, pattern) VALUES (?1, ?2)",
        params![
            "tv",
            "{title} ({year})/Season {season}/{title} - {season_episode}{part}{ext}"
        ],
    )?;
    let movie_id = uuid::Uuid::new_v4().to_string();
    let tv_id = uuid::Uuid::new_v4().to_string();
    app.execute(
        "INSERT OR IGNORE INTO library_roots (id, kind, path, is_default)
         SELECT ?1, 'movie', ?2, 1 WHERE NOT EXISTS (SELECT 1 FROM library_roots WHERE kind='movie')",
        params![movie_id, movie_root.display().to_string()],
    )?;
    app.execute(
        "INSERT OR IGNORE INTO library_roots (id, kind, path, is_default)
         SELECT ?1, 'tv', ?2, 1 WHERE NOT EXISTS (SELECT 1 FROM library_roots WHERE kind='tv')",
        params![tv_id, tv_root.display().to_string()],
    )?;
    let filter_id = uuid::Uuid::new_v4().to_string();
    // JSON atom form (see maps::serialize_atoms); the legacy DSL is still read.
    let atoms = r#"[{"priority":100,"rule":"resolution","exclude":false,"value":"2160p"},{"priority":50,"rule":"resolution","exclude":false,"value":"1080p"}]"#;
    app.execute(
        "INSERT INTO filters (id, name, atoms_json)
         SELECT ?1, '默认', ?2 WHERE NOT EXISTS (SELECT 1 FROM filters)",
        params![filter_id, atoms],
    )?;
    app.execute(
        "INSERT OR IGNORE INTO settings (key, value) VALUES ('default_filter_id', ?1)",
        params![filter_id],
    )?;
    seed_libraries(app)?;
    Ok(())
}

/// Backfill the `libraries` entity from `library_roots`: one default library per
/// kind, roots attached in current order (first root = primary). Idempotent —
/// existing libraries are left untouched, orphaned roots (library_id NULL) are
/// re-attached to their kind's default library.
fn seed_libraries(app: &Connection) -> Result<(), StoreError> {
    let kinds: Vec<String> = app
        .prepare("SELECT DISTINCT kind FROM library_roots")?
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for kind in kinds {
        let has_library: bool = app
            .prepare("SELECT 1 FROM libraries WHERE kind = ?1 LIMIT 1")?
            .query_row(params![kind], |_| Ok(()))
            .optional()?
            .is_some();
        if !has_library {
            let name = if kind == "tv" {
                "剧集库"
            } else {
                "电影库"
            };
            app.execute(
                "INSERT INTO libraries (id, kind, name, is_default, sort_order)
                 VALUES (?1, ?2, ?3, 1, 0)",
                params![uuid::Uuid::new_v4().to_string(), kind, name],
            )?;
        }
    }
    crate::library_defaults::ensure_library_defaults(app)?;
    Ok(())
}

pub const MEDIA_COLS: &str =
    "id, kind, title, year, original_title, tmdb_id, douban_id, tvdb_id, bangumi_id, anilist_id";
