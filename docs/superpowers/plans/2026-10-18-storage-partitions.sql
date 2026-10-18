-- Development-plan appendix, NOT runtime migration. Sections execute on separate empty databases.
-- BEGIN APP
PRAGMA foreign_keys=ON;
BEGIN IMMEDIATE;
CREATE TABLE instance_lock(id INTEGER PRIMARY KEY CHECK(id=1),owner TEXT NOT NULL,heartbeat_at INTEGER NOT NULL);
CREATE TABLE users(id TEXT PRIMARY KEY NOT NULL,login TEXT NOT NULL UNIQUE,enabled INTEGER NOT NULL DEFAULT 1,role TEXT NOT NULL DEFAULT 'member',password TEXT NOT NULL DEFAULT '');
CREATE TABLE sites(id TEXT PRIMARY KEY NOT NULL,name TEXT NOT NULL,url TEXT NOT NULL,profile_id TEXT NOT NULL,cookie TEXT,api_key TEXT,rss_url TEXT,proxy TEXT,rate_limit_per_minute INTEGER,cdp_url TEXT,downloader_id TEXT,enabled INTEGER NOT NULL DEFAULT 1,maintenance_via TEXT NOT NULL DEFAULT 'http' CHECK(maintenance_via IN ('http','browser')));
CREATE INDEX sites_enabled_idx ON sites(enabled);
CREATE TABLE filters(id TEXT PRIMARY KEY NOT NULL,name TEXT NOT NULL,atoms_json TEXT NOT NULL,keep_old_versions INTEGER NOT NULL DEFAULT 0);
CREATE TABLE user_tokens(token TEXT PRIMARY KEY NOT NULL,user_id TEXT NOT NULL,created_at INTEGER NOT NULL DEFAULT 0);
CREATE INDEX user_tokens_user_idx ON user_tokens(user_id);
CREATE TABLE playback_device_credentials(user_id TEXT NOT NULL,device_id TEXT NOT NULL,token TEXT NOT NULL,PRIMARY KEY(user_id,device_id,token));
CREATE TABLE settings(key TEXT PRIMARY KEY NOT NULL,value TEXT NOT NULL);
CREATE TABLE search_snapshots(id TEXT PRIMARY KEY NOT NULL,vertical TEXT NOT NULL,query TEXT NOT NULL,payload TEXT NOT NULL,created_at INTEGER NOT NULL);
CREATE TABLE downloaders(id TEXT PRIMARY KEY NOT NULL,name TEXT NOT NULL,kind TEXT NOT NULL,url TEXT NOT NULL,username TEXT,password TEXT,category TEXT,is_default INTEGER NOT NULL DEFAULT 0,path_maps TEXT,enabled INTEGER NOT NULL DEFAULT 1);
CREATE TABLE naming_templates(kind TEXT PRIMARY KEY NOT NULL,pattern TEXT NOT NULL);
PRAGMA application_id=1129139248;
PRAGMA user_version=1;
COMMIT;
-- END APP
-- BEGIN SUBSCRIBE
PRAGMA foreign_keys=ON;
BEGIN IMMEDIATE;
CREATE TABLE subscribes(id TEXT PRIMARY KEY NOT NULL,user_id TEXT NOT NULL,media_id TEXT NOT NULL,coverage_kind TEXT NOT NULL,season INTEGER,episode_from INTEGER,episode_to INTEGER,fetch_mode TEXT NOT NULL,filter_id TEXT NOT NULL,wash_cut INTEGER NOT NULL DEFAULT 0,wash_cut_filter_id TEXT,full_season_pack INTEGER NOT NULL DEFAULT 0,downloader_id TEXT,tracking_state TEXT NOT NULL DEFAULT 'active',follow_future INTEGER NOT NULL DEFAULT 0,search_interval_secs INTEGER NOT NULL DEFAULT 1800,keep_old_versions INTEGER NOT NULL DEFAULT 0,created_at TEXT NOT NULL DEFAULT '',updated_at TEXT NOT NULL DEFAULT '',library_id TEXT);
CREATE TABLE subscribe_facts(subscribe_id TEXT NOT NULL,season INTEGER NOT NULL DEFAULT -1,episode INTEGER NOT NULL DEFAULT -1,score INTEGER NOT NULL,path TEXT,PRIMARY KEY(subscribe_id,season,episode));
CREATE TABLE subscribe_wanted(subscribe_id TEXT NOT NULL,season INTEGER NOT NULL DEFAULT -1,episode INTEGER NOT NULL DEFAULT -1,search_attempts INTEGER NOT NULL DEFAULT 0,last_search_at INTEGER,grabbed_at INTEGER,imported_at INTEGER,grab_title TEXT,last_reject_reason TEXT,PRIMARY KEY(subscribe_id,season,episode));
CREATE TABLE pending_downloads(subscribe_id TEXT NOT NULL,enclosure TEXT NOT NULL,title TEXT NOT NULL,score INTEGER NOT NULL,torrent_json TEXT NOT NULL,submitted_at INTEGER NOT NULL DEFAULT 0,state TEXT NOT NULL DEFAULT 'active',last_progress REAL,last_progress_at INTEGER,info_hash TEXT,selective_missing TEXT,probe_wait_attempts INTEGER,probe_wait_since INTEGER,PRIMARY KEY(subscribe_id,enclosure));
CREATE INDEX subscribes_user_idx ON subscribes(user_id);
CREATE INDEX subscribes_media_idx ON subscribes(media_id);
CREATE INDEX pending_downloads_sub_state_idx ON pending_downloads(subscribe_id,state);
PRAGMA application_id=1129139250;
PRAGMA user_version=1;
COMMIT;
-- END SUBSCRIBE
-- BEGIN CACHE
PRAGMA foreign_keys=ON;
BEGIN IMMEDIATE;
CREATE TABLE tmdb_cache(entry_id INTEGER PRIMARY KEY AUTOINCREMENT,cache_key TEXT NOT NULL COLLATE BINARY UNIQUE,body TEXT NOT NULL,fetched_at INTEGER NOT NULL,expires_at INTEGER);
CREATE INDEX tmdb_cache_recent_idx ON tmdb_cache(fetched_at DESC,entry_id);
CREATE INDEX tmdb_cache_retention_idx ON tmdb_cache(fetched_at,entry_id);
CREATE TABLE douban_cache(entry_id INTEGER PRIMARY KEY AUTOINCREMENT,cache_key TEXT NOT NULL COLLATE BINARY UNIQUE,body TEXT NOT NULL,fetched_at INTEGER NOT NULL,expires_at INTEGER);
CREATE INDEX douban_cache_recent_idx ON douban_cache(fetched_at DESC,entry_id);
CREATE INDEX douban_cache_retention_idx ON douban_cache(fetched_at,entry_id);
CREATE TABLE tvdb_cache(entry_id INTEGER PRIMARY KEY AUTOINCREMENT,cache_key TEXT NOT NULL COLLATE BINARY UNIQUE,body TEXT NOT NULL,fetched_at INTEGER NOT NULL,expires_at INTEGER);
CREATE INDEX tvdb_cache_recent_idx ON tvdb_cache(fetched_at DESC,entry_id);
CREATE INDEX tvdb_cache_retention_idx ON tvdb_cache(fetched_at,entry_id);
CREATE TABLE bangumi_cache(entry_id INTEGER PRIMARY KEY AUTOINCREMENT,cache_key TEXT NOT NULL COLLATE BINARY UNIQUE,body TEXT NOT NULL,fetched_at INTEGER NOT NULL,expires_at INTEGER);
CREATE INDEX bangumi_cache_recent_idx ON bangumi_cache(fetched_at DESC,entry_id);
CREATE INDEX bangumi_cache_retention_idx ON bangumi_cache(fetched_at,entry_id);
CREATE TABLE anilist_cache(entry_id INTEGER PRIMARY KEY AUTOINCREMENT,cache_key TEXT NOT NULL COLLATE BINARY UNIQUE,body TEXT NOT NULL,fetched_at INTEGER NOT NULL,expires_at INTEGER);
CREATE INDEX anilist_cache_recent_idx ON anilist_cache(fetched_at DESC,entry_id);
CREATE INDEX anilist_cache_retention_idx ON anilist_cache(fetched_at,entry_id);
CREATE TABLE fanart_cache(entry_id INTEGER PRIMARY KEY AUTOINCREMENT,cache_key TEXT NOT NULL COLLATE BINARY UNIQUE,body TEXT NOT NULL,fetched_at INTEGER NOT NULL,expires_at INTEGER);
CREATE INDEX fanart_cache_recent_idx ON fanart_cache(fetched_at DESC,entry_id);
CREATE INDEX fanart_cache_retention_idx ON fanart_cache(fetched_at,entry_id);
PRAGMA application_id=1129139251;
PRAGMA user_version=1;
COMMIT;
-- END CACHE
