-- library.db layout v6 (measured file cleanup and operation-retention indexes).
--
-- A DDL change *is* a layout change: this file and `LIBRARY_USER_VERSION` in
-- `schema/layout.rs` move together, and an older file is refused and rebuilt
-- (`scripts/storage-cutover-rebuild.sh`), never migrated.
--
-- `auto_vacuum = INCREMENTAL` is part of this layout, but it cannot be declared
-- here: SQLite only rewrites that header flag while the file has no tables *and*
-- is not in WAL mode, so a `PRAGMA auto_vacuum` in this batch (inside a write
-- transaction, after the connection policy turned on WAL) would be a silent no-op.
-- The create path sets it before the first table exists
-- (`open.rs::apply_auto_vacuum_on_create`).

CREATE TABLE media (
 media_id TEXT PRIMARY KEY NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('movie','tv','video')),
 title TEXT NOT NULL, original_title TEXT, year INTEGER CHECK(year IS NULL OR year BETWEEN 0 AND 9999),
 identity_state TEXT NOT NULL CHECK(identity_state IN ('provisional','confirmed','merging','retired')),
 aliases_display_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(aliases_display_json)),
 metadata_revision INTEGER NOT NULL DEFAULT 0, survivor_id TEXT REFERENCES media(media_id),
 UNIQUE(media_id,kind), CHECK((identity_state='retired')=(survivor_id IS NOT NULL)), CHECK(survivor_id IS NULL OR survivor_id<>media_id)
);
CREATE INDEX media_title ON media(kind,title,year,media_id);
CREATE INDEX media_survivor ON media(survivor_id) WHERE survivor_id IS NOT NULL;
CREATE TABLE media_aliases (
 source TEXT NOT NULL CHECK(source IN ('tmdb','douban','tvdb','bangumi','anilist')),
 kind TEXT NOT NULL, external_id TEXT NOT NULL, media_id TEXT NOT NULL,
 is_refresh_target INTEGER NOT NULL DEFAULT 0 CHECK(is_refresh_target IN (0,1)),
 PRIMARY KEY(source,kind,external_id), FOREIGN KEY(media_id,kind) REFERENCES media(media_id,kind)
);
CREATE INDEX alias_media ON media_aliases(media_id,source);
CREATE UNIQUE INDEX alias_target ON media_aliases(media_id,source,kind) WHERE is_refresh_target=1;
CREATE TABLE media_metadata_sources (
 media_id TEXT NOT NULL REFERENCES media(media_id), entity_scope TEXT NOT NULL,
 season INTEGER NOT NULL DEFAULT -1, episode INTEGER NOT NULL DEFAULT -1,
 source TEXT NOT NULL CHECK(source IN ('tmdb','douban','tvdb','bangumi','anilist','fanart')),
 locale TEXT NOT NULL, provider_entity_id TEXT NOT NULL, provider_work_id TEXT NOT NULL,
 normalized_json TEXT NOT NULL CHECK(json_valid(normalized_json)),
 present_fields_json TEXT NOT NULL CHECK(json_valid(present_fields_json)),
 preferences_json TEXT NOT NULL CHECK(json_valid(preferences_json)),
 selection_origins_json TEXT NOT NULL CHECK(json_valid(selection_origins_json)),
 fetched_at INTEGER NOT NULL, source_revision INTEGER NOT NULL CHECK(source_revision>=0),
 PRIMARY KEY(media_id,entity_scope,season,episode,source,locale),
 CHECK((entity_scope='work' AND season=-1 AND episode=-1) OR
       (entity_scope='season' AND season>=0 AND episode=-1) OR
       (entity_scope='episode' AND season>=0 AND episode>=0))
);
CREATE TABLE media_metadata (
 media_id TEXT NOT NULL REFERENCES media(media_id), locale TEXT NOT NULL,
 title TEXT NOT NULL, original_title TEXT, year INTEGER, overview TEXT, release_date TEXT,
 rating REAL CHECK(rating IS NULL OR rating BETWEEN 0 AND 10), runtime_minutes INTEGER CHECK(runtime_minutes IS NULL OR runtime_minutes>=0),
 language TEXT, genres_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(genres_json)),
 countries_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(countries_json)),
 poster_url TEXT, backdrop_url TEXT, details_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(details_json)),
 field_origins_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(field_origins_json)),
 revision INTEGER NOT NULL CHECK(revision>=0), PRIMARY KEY(media_id,locale)
);
-- `media_seasons` was deleted by the v4 round (2026-10-26 schema audit): every
-- metadata adoption wrote a wide season row per season x locale and *nothing*
-- read it. Season display comes from `library_units` + `library_files`, episode
-- display from `media_episodes`. See
-- docs/audits/2026-10-26-library-schema-audit.md (finding H1).
CREATE TABLE media_episodes (
 media_id TEXT NOT NULL REFERENCES media(media_id), season INTEGER NOT NULL CHECK(season>=0),
 episode INTEGER NOT NULL CHECK(episode>=0), locale TEXT NOT NULL, title TEXT, overview TEXT, air_date TEXT,
 runtime_minutes INTEGER, rating REAL, still_url TEXT,
 details_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(details_json)),
 field_origins_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(field_origins_json)), revision INTEGER NOT NULL,
 PRIMARY KEY(media_id,season,episode,locale)
);
CREATE TABLE media_metadata_overrides (
 media_id TEXT NOT NULL REFERENCES media(media_id), entity_scope TEXT NOT NULL, season INTEGER NOT NULL,
 episode INTEGER NOT NULL, locale TEXT NOT NULL, fields_json TEXT NOT NULL CHECK(json_valid(fields_json)), revision INTEGER NOT NULL,
 PRIMARY KEY(media_id,entity_scope,season,episode,locale),
 CHECK((entity_scope='work' AND season=-1 AND episode=-1) OR (entity_scope='season' AND season>=0 AND episode=-1) OR (entity_scope='episode' AND season>=0 AND episode>=0))
);
CREATE TABLE libraries (
 library_id TEXT PRIMARY KEY NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('movie','tv','video')), name TEXT NOT NULL,
 lifecycle_state TEXT NOT NULL DEFAULT 'ready' CHECK(lifecycle_state IN ('ready','frozen','draining','reindexing','failed')),
 config_revision INTEGER NOT NULL DEFAULT 0, integrity_state TEXT NOT NULL DEFAULT 'unknown' CHECK(integrity_state IN ('unknown','clean','failed')),
 display_locale TEXT NOT NULL DEFAULT 'zh-CN', is_default INTEGER NOT NULL DEFAULT 0 CHECK(is_default IN (0,1)), sort_order INTEGER NOT NULL DEFAULT 0,
 access_mode TEXT NOT NULL DEFAULT 'everyone', admin_visible INTEGER NOT NULL DEFAULT 1,
 member_ids_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(member_ids_json)),
 detect_intros INTEGER NOT NULL DEFAULT 0, enable_fingerprint INTEGER NOT NULL DEFAULT 0,
 cover_path TEXT, cover_revision INTEGER NOT NULL DEFAULT 0, match_rules_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(match_rules_json)),
 default_filter_id TEXT, realtime_watch INTEGER NOT NULL DEFAULT 1, generate_thumbnails INTEGER NOT NULL DEFAULT 1,
 extract_chapter_images INTEGER NOT NULL DEFAULT 1, exclude_from_home INTEGER NOT NULL DEFAULT 0, auto_series_collections INTEGER NOT NULL DEFAULT 1,
 UNIQUE(library_id,kind)
);
CREATE UNIQUE INDEX library_default ON libraries(kind) WHERE is_default=1;
CREATE TABLE library_roots (
 root_id TEXT PRIMARY KEY NOT NULL, library_id TEXT NOT NULL REFERENCES libraries(library_id),
 path_key TEXT NOT NULL UNIQUE CHECK(substr(path_key,1,1)='/' AND instr(path_key,'//')=0 AND instr(path_key,'/./')=0 AND instr(path_key,'/../')=0 AND path_key NOT LIKE '%/.' AND path_key NOT LIKE '%/..' AND (path_key='/' OR path_key NOT LIKE '%/')), display_path TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
 sort_order INTEGER NOT NULL DEFAULT 0, UNIQUE(root_id,library_id)
);
CREATE INDEX roots_library ON library_roots(library_id,sort_order,root_id);
CREATE TABLE library_files (
 file_id TEXT PRIMARY KEY NOT NULL, library_id TEXT NOT NULL, root_id TEXT NOT NULL, media_id TEXT NOT NULL, kind TEXT NOT NULL,
 path_key TEXT NOT NULL UNIQUE CHECK(substr(path_key,1,1)='/' AND instr(path_key,'//')=0 AND instr(path_key,'/./')=0 AND instr(path_key,'/../')=0 AND path_key NOT LIKE '%/.' AND path_key NOT LIKE '%/..' AND (path_key='/' OR path_key NOT LIKE '%/')), display_path TEXT NOT NULL, presence_state TEXT NOT NULL CHECK(presence_state IN ('present','missing','unknown')),
 publication_state TEXT NOT NULL CHECK(publication_state IN ('pending','published')),
 file_revision INTEGER NOT NULL CHECK(file_revision>0), source_version TEXT NOT NULL,
 size_bytes INTEGER CHECK(size_bytes IS NULL OR size_bytes>=0), stat_known INTEGER NOT NULL DEFAULT 0,
 mtime_ns INTEGER, created_ns INTEGER, source_digest TEXT, missing_at INTEGER,
 source_path TEXT, transfer_mode TEXT CHECK(transfer_mode IS NULL OR transfer_mode IN ('hardlink','copy','move')),
 resolution TEXT, codec TEXT, hdr TEXT, hdr_observed INTEGER NOT NULL DEFAULT 0 CHECK(hdr_observed IN (0,1)),
 quality_source TEXT NOT NULL CHECK(quality_source IN ('probe','release')),
 confidence TEXT NOT NULL CHECK(confidence IN ('high','low')), filter_score INTEGER, release_quality TEXT,
 duration_ms INTEGER CHECK(duration_ms IS NULL OR duration_ms>=0), tracks_version INTEGER NOT NULL DEFAULT 0,
 poster_path TEXT, backdrop_path TEXT, still_path TEXT,
 has_poster INTEGER NOT NULL DEFAULT 0, has_backdrop INTEGER NOT NULL DEFAULT 0, has_still INTEGER NOT NULL DEFAULT 0,
 UNIQUE(file_id,library_id,media_id),
 FOREIGN KEY(root_id,library_id) REFERENCES library_roots(root_id,library_id),
 FOREIGN KEY(library_id,kind) REFERENCES libraries(library_id,kind), FOREIGN KEY(media_id,kind) REFERENCES media(media_id,kind)
);
CREATE INDEX files_library_media ON library_files(library_id,media_id,file_id);
CREATE INDEX files_library_file ON library_files(library_id,file_id);
CREATE INDEX files_media_library ON library_files(media_id,library_id,file_id);
CREATE INDEX files_root_path ON library_files(root_id,path_key);
CREATE INDEX files_kind_path ON library_files(kind,path_key);
CREATE INDEX files_presence ON library_files(library_id,presence_state,file_id);
CREATE TABLE file_details (
 file_id TEXT PRIMARY KEY NOT NULL REFERENCES library_files(file_id) ON DELETE CASCADE,
 source_revision INTEGER NOT NULL, source_version TEXT NOT NULL, tracks_version INTEGER NOT NULL,
 video_json TEXT, audio_json TEXT NOT NULL DEFAULT '[]', subtitle_json TEXT NOT NULL DEFAULT '[]', chapters_json TEXT,
 fingerprint_json TEXT, outro_fingerprint_json TEXT, format_duration_ms INTEGER,
 CHECK(json_valid(audio_json)), CHECK(json_valid(subtitle_json)), CHECK(video_json IS NULL OR json_valid(video_json)), CHECK(chapters_json IS NULL OR json_valid(chapters_json))
);
-- WITHOUT ROWID: the primary key is a composite of NOT NULL columns and the row
-- carries nothing but that key, so the table *is* its primary key. A rowid table
-- would need a second b-tree (the PK autoindex) holding the same bytes; here both
-- the PK seek and the PK-ordered walk use one b-tree, and the secondary indexes
-- already contain every PK column, so they do not widen.
CREATE TABLE library_file_units (
 file_id TEXT NOT NULL, library_id TEXT NOT NULL, media_id TEXT NOT NULL, season INTEGER NOT NULL, episode INTEGER NOT NULL,
 PRIMARY KEY(file_id,season,episode), UNIQUE(library_id,media_id,season,episode,file_id),
 FOREIGN KEY(file_id,library_id,media_id) REFERENCES library_files(file_id,library_id,media_id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED,
 CHECK((season=-1 AND episode=-1) OR (season>=0 AND episode>=0))
) WITHOUT ROWID;
CREATE INDEX coverage_media ON library_file_units(media_id,season,episode,library_id,file_id);
CREATE TABLE library_units (
 library_id TEXT NOT NULL, media_id TEXT NOT NULL, season INTEGER NOT NULL, episode INTEGER NOT NULL,
 file_count INTEGER NOT NULL CHECK(file_count>0), available_file_count INTEGER NOT NULL CHECK(available_file_count BETWEEN 0 AND file_count),
 preferred_file_id TEXT, unit_revision INTEGER NOT NULL,
 PRIMARY KEY(library_id,media_id,season,episode),
 FOREIGN KEY(library_id) REFERENCES libraries(library_id), FOREIGN KEY(media_id) REFERENCES media(media_id),
 FOREIGN KEY(library_id,media_id,season,episode,preferred_file_id) REFERENCES library_file_units(library_id,media_id,season,episode,file_id) DEFERRABLE INITIALLY DEFERRED,
 CHECK((season=-1 AND episode=-1) OR (season>=0 AND episode>=0)), CHECK(available_file_count=0 OR preferred_file_id IS NOT NULL)
);
CREATE INDEX units_media_scope ON library_units(media_id,season,episode,library_id);
CREATE INDEX units_protocol_order ON library_units(library_id,season ASC,episode ASC,media_id ASC);
CREATE TABLE library_metadata_inputs (
 library_id TEXT NOT NULL, media_id TEXT NOT NULL REFERENCES media(media_id), entity_scope TEXT NOT NULL,
 season INTEGER NOT NULL, episode INTEGER NOT NULL, locale TEXT NOT NULL, input_key TEXT NOT NULL,
 root_id TEXT NOT NULL, path_key TEXT NOT NULL CHECK(substr(path_key,1,1)='/' AND instr(path_key,'//')=0 AND instr(path_key,'/./')=0 AND instr(path_key,'/../')=0 AND path_key NOT LIKE '%/.' AND path_key NOT LIKE '%/..' AND (path_key='/' OR path_key NOT LIKE '%/')), parse_state TEXT NOT NULL CHECK(parse_state IN ('valid','invalid','unknown','missing')),
 source_version TEXT NOT NULL, fields_json TEXT NOT NULL CHECK(json_valid(fields_json)),
 present_fields_json TEXT NOT NULL CHECK(json_valid(present_fields_json)),
 provenance_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(provenance_json)),
 export_id TEXT, export_digest TEXT, export_fields_json TEXT CHECK(export_fields_json IS NULL OR json_valid(export_fields_json)), revision INTEGER NOT NULL,
 PRIMARY KEY(library_id,media_id,entity_scope,season,episode,locale,input_key),
 FOREIGN KEY(root_id,library_id) REFERENCES library_roots(root_id,library_id),
 CHECK((entity_scope='work' AND season=-1 AND episode=-1) OR (entity_scope='season' AND season>=0 AND episode=-1) OR (entity_scope='episode' AND season>=0 AND episode>=0))
);
CREATE INDEX metadata_input_path ON library_metadata_inputs(root_id,path_key);
-- `media_id` is an FK to media(media_id) but the PK starts with library_id, so
-- deleting a media row could not reach its NFO inputs through the PK prefix.
CREATE INDEX metadata_inputs_media ON library_metadata_inputs(media_id);
-- v4 (2026-10-26 schema audit) deleted three write-only columns, each of which
-- had a production writer and no production reader:
--   aliases_json  — a second copy of `media.aliases_display_json`, and the only
--                   reason `reduce::media_identity` ran five per-source queries
--                   on every card reduce. Those queries are gone with it.
--   added_at_ns   — a second copy of `added_at_sort` in nanoseconds; the sort key
--                   is what every ORDER BY reads.
--   has_nfo       — computed from `library_metadata_inputs` on every reduce, read
--                   only by tests.
CREATE TABLE library_items (
 library_id TEXT NOT NULL REFERENCES libraries(library_id), media_id TEXT NOT NULL REFERENCES media(media_id), kind TEXT NOT NULL,
 title TEXT NOT NULL, title_sort TEXT NOT NULL, year INTEGER, identity_state TEXT NOT NULL,
 metadata_revision INTEGER NOT NULL, item_revision INTEGER NOT NULL,
 protocol_title_key TEXT NOT NULL DEFAULT '', query_projection_revision INTEGER NOT NULL DEFAULT 0,
 query_config_revision INTEGER NOT NULL DEFAULT 0,
 release_date_sort TEXT NOT NULL DEFAULT '', rating_sort REAL NOT NULL DEFAULT -1, runtime_sort INTEGER NOT NULL DEFAULT 0,
 language TEXT, added_at_sort INTEGER NOT NULL, size_bytes INTEGER, size_sort INTEGER NOT NULL DEFAULT 0,
 resolution_rank INTEGER NOT NULL DEFAULT 0, hdr INTEGER NOT NULL DEFAULT 0, scraped INTEGER NOT NULL DEFAULT 0,
 missing INTEGER NOT NULL DEFAULT 0, stable_shuffle_key TEXT NOT NULL,
 file_count INTEGER NOT NULL CHECK(file_count>0), available_file_count INTEGER NOT NULL,
 missing_file_count INTEGER NOT NULL DEFAULT 0, unknown_file_count INTEGER NOT NULL DEFAULT 0,
 distinct_episode_count INTEGER NOT NULL DEFAULT 0, distinct_season_count INTEGER NOT NULL DEFAULT 0,
 preferred_file_id TEXT, poster_ref TEXT, backdrop_ref TEXT, has_still INTEGER NOT NULL DEFAULT 0,
 search_text TEXT NOT NULL DEFAULT '',
 PRIMARY KEY(library_id,media_id),
 FOREIGN KEY(preferred_file_id,library_id,media_id) REFERENCES library_files(file_id,library_id,media_id) DEFERRABLE INITIALLY DEFERRED,
 CHECK(available_file_count>=0 AND missing_file_count>=0 AND unknown_file_count>=0),
 CHECK(available_file_count+missing_file_count+unknown_file_count=file_count), CHECK(available_file_count=0 OR preferred_file_id IS NOT NULL),
 CHECK(missing=(missing_file_count>0))
);
CREATE INDEX items_media ON library_items(media_id,library_id);
CREATE INDEX items_title ON library_items(library_id,title_sort,media_id);
CREATE INDEX items_added ON library_items(library_id,added_at_sort,media_id);
CREATE INDEX items_release ON library_items(library_id,release_date_sort,media_id);
CREATE INDEX items_rating ON library_items(library_id,rating_sort,media_id);
CREATE INDEX items_runtime ON library_items(library_id,runtime_sort,media_id);
CREATE INDEX items_size ON library_items(library_id,size_sort,media_id);
CREATE INDEX items_resolution ON library_items(library_id,resolution_rank,media_id);
CREATE INDEX items_shuffle ON library_items(library_id,stable_shuffle_key,media_id);
CREATE INDEX items_protocol_name_asc ON library_items(library_id,protocol_title_key ASC,title ASC,media_id ASC);
CREATE INDEX items_protocol_name_desc ON library_items(library_id,protocol_title_key DESC,title ASC,media_id ASC);
CREATE INDEX items_protocol_default ON library_items(library_id,added_at_sort DESC,protocol_title_key DESC,media_id ASC);
CREATE INDEX items_protocol_default_explicit ON library_items(library_id,added_at_sort DESC,protocol_title_key DESC,title ASC,media_id ASC);
-- WITHOUT ROWID: composite NOT NULL primary key and no payload columns.
-- `media_id` is the *second* key column: with the old order
-- (library_id, term_kind, term_value, media_id) a per-card read could not seek,
-- so the table was stored three times (key b-tree + `terms_media` +
-- `terms_value`). This order makes the per-card read
-- (`query_projection/browse.rs::read_terms`), the term delete in `sync_terms`,
-- and the `library_items` FK child lookup plain key prefixes, which is what
-- `terms_media` used to supply — at the cost of a whole second b-tree.
CREATE TABLE library_item_terms (
 library_id TEXT NOT NULL, term_kind TEXT NOT NULL CHECK(term_kind IN ('genre','country','resolution','decade','runtime','language','rating','hdr','stock','collection','studio')),
 term_value TEXT NOT NULL, media_id TEXT NOT NULL, PRIMARY KEY(library_id,media_id,term_kind,term_value),
 FOREIGN KEY(library_id,media_id) REFERENCES library_items(library_id,media_id) ON DELETE CASCADE
) WITHOUT ROWID;
CREATE INDEX terms_value ON library_item_terms(term_kind,term_value,library_id,media_id);
-- WITHOUT ROWID: composite NOT NULL primary key, row is the key (secondary
-- indexes already carry every PK column, so none of them widens).
CREATE TABLE library_protocol_terms (
 library_id TEXT NOT NULL, media_id TEXT NOT NULL,
 term_kind TEXT NOT NULL CHECK(term_kind IN ('genre','studio')), term_value TEXT NOT NULL,
 PRIMARY KEY(library_id,term_kind,term_value,media_id),
 FOREIGN KEY(library_id,media_id) REFERENCES library_items(library_id,media_id) ON DELETE CASCADE
) WITHOUT ROWID;
-- `protocol_terms_value` was dropped by the 2026-10-26 schema audit: no statement
-- reaches it. The one term predicate that exists
-- (`library_query/protocol/membership.rs:17`) carries `library_id` AND `media_id`
-- as equalities, so it seeks the primary key directly; the projection read
-- (`query_projection/browse.rs::read_terms`) seeks `protocol_terms_media`.
-- A (term_kind, term_value) index has no reader, and on a 20k-card fixture it cost
-- a third of the table's own b-trees (1.63 MB of 5.0 MB).
CREATE INDEX protocol_terms_media ON library_protocol_terms(library_id,media_id,term_kind,term_value);
-- WITHOUT ROWID: composite NOT NULL primary key plus one narrow TEXT column
-- (`wire_id`). The PK prefix (library_id, media_id) read in
-- query_projection/people.rs becomes a direct b-tree seek instead of
-- autoindex-then-table.
CREATE TABLE library_protocol_people (
 library_id TEXT NOT NULL, media_id TEXT NOT NULL,
 role TEXT NOT NULL CHECK(role IN ('Actor','Director','Writer')),
 name_key TEXT NOT NULL, tmdb_id TEXT NOT NULL, wire_id TEXT NOT NULL,
 PRIMARY KEY(library_id,media_id,role,name_key,tmdb_id),
 FOREIGN KEY(library_id,media_id) REFERENCES library_items(library_id,media_id) ON DELETE CASCADE
) WITHOUT ROWID;
CREATE INDEX protocol_people_wire ON library_protocol_people(wire_id,library_id,media_id,role,name_key,tmdb_id);
-- `protocol_people_name` (role,name_key,...) was dropped by the 2026-10-26 schema
-- audit: nothing seeks it. The only person predicate
-- (`library_query/protocol/membership.rs:21`) starts from `wire_id`/`tmdb_id` and
-- compares `name_key` only as a suffix of an aliased row, which a
-- (role,name_key) prefix cannot serve.
--
-- `tmdb_id` leads this index (it used to lead with `role`): the only predicate is
-- `a.tmdb_id IN (...) AND a.role = p.role`, which cannot seek a `role`-first key
-- and degraded to a full index scan. Same bytes, usable key.
CREATE INDEX protocol_people_tmdb ON library_protocol_people(tmdb_id,role,library_id,media_id);
CREATE TABLE library_query_counts (
 library_id TEXT NOT NULL REFERENCES libraries(library_id) ON DELETE CASCADE,
 surface TEXT NOT NULL CHECK(surface IN ('browse','protocol_work')),
 dimension TEXT NOT NULL, value TEXT NOT NULL, member_count INTEGER NOT NULL CHECK(member_count>=0),
 PRIMARY KEY(library_id,surface,dimension,value)
);
CREATE TABLE library_query_config (
 id INTEGER PRIMARY KEY CHECK(id=1), default_locale TEXT, revision INTEGER NOT NULL DEFAULT 0
);
INSERT INTO library_query_config(id) VALUES (1);
CREATE TABLE collections (
 id TEXT PRIMARY KEY NOT NULL, user_id TEXT, visibility TEXT NOT NULL CHECK(visibility IN ('private','public')),
 name TEXT NOT NULL, sort_order INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL,
 CHECK((visibility='private' AND user_id IS NOT NULL) OR (visibility='public' AND user_id IS NULL))
);
CREATE INDEX collections_owner ON collections(user_id,sort_order,id);
CREATE TABLE collection_items (
 collection_id TEXT NOT NULL REFERENCES collections(id) ON DELETE CASCADE, media_id TEXT NOT NULL REFERENCES media(media_id),
 sort_order INTEGER NOT NULL DEFAULT 0, added_at TEXT NOT NULL, PRIMARY KEY(collection_id,media_id)
);
CREATE INDEX collection_media ON collection_items(media_id,collection_id);
CREATE TABLE playback_units (
 user_id TEXT NOT NULL, media_id TEXT NOT NULL REFERENCES media(media_id), season INTEGER NOT NULL, episode INTEGER NOT NULL,
 played INTEGER NOT NULL DEFAULT 0 CHECK(played IN (0,1)), has_played_assertion INTEGER NOT NULL DEFAULT 0 CHECK(has_played_assertion IN (0,1)),
 played_changed_at_ms INTEGER, favorite INTEGER NOT NULL DEFAULT 0 CHECK(favorite IN (0,1)), favorite_at INTEGER NOT NULL DEFAULT 0,
 play_count INTEGER NOT NULL DEFAULT 0 CHECK(play_count>=0), watch_activity_at INTEGER NOT NULL DEFAULT 0,
 position_ms INTEGER NOT NULL DEFAULT 0 CHECK(position_ms>=0), duration_ms INTEGER CHECK(duration_ms IS NULL OR duration_ms>=0),
 resume_file_id TEXT, resume_file_revision INTEGER, resume_source_version TEXT, timeline_mode TEXT CHECK(timeline_mode IS NULL OR timeline_mode='full_file'),
 tracks_version INTEGER, audio_track TEXT, subtitle_track TEXT,
 PRIMARY KEY(user_id,media_id,season,episode), CHECK((season=-1 AND episode=-1) OR (season>=0 AND episode>=-1)),
 CHECK((has_played_assertion=0 AND played=0 AND played_changed_at_ms IS NULL) OR (has_played_assertion=1 AND played_changed_at_ms IS NOT NULL)),
 CHECK((resume_file_id IS NULL AND resume_file_revision IS NULL AND resume_source_version IS NULL AND timeline_mode IS NULL AND position_ms=0 AND duration_ms IS NULL AND tracks_version IS NULL AND audio_track IS NULL AND subtitle_track IS NULL) OR
       (resume_file_id IS NOT NULL AND resume_file_revision IS NOT NULL AND resume_file_revision>0 AND resume_source_version IS NOT NULL AND timeline_mode IS NOT NULL)),
 CHECK((audio_track IS NULL AND subtitle_track IS NULL) OR tracks_version IS NOT NULL)
);
CREATE INDEX playback_unit_media ON playback_units(media_id,user_id,season,episode);
CREATE INDEX playback_activity ON playback_units(user_id,media_id,watch_activity_at DESC,season,episode);
-- WITHOUT ROWID: composite NOT NULL primary key over four small counters and
-- four nullable coordinates; both secondary indexes already carry every PK
-- column. PK reads (the summary read path) stop paying for a second b-tree.
CREATE TABLE user_library_items (
 user_id TEXT NOT NULL, library_id TEXT NOT NULL, media_id TEXT NOT NULL,
 seen INTEGER NOT NULL, played INTEGER NOT NULL, favorite INTEGER NOT NULL, last_played_at INTEGER NOT NULL DEFAULT 0,
 favorite_at INTEGER NOT NULL DEFAULT 0, owned_episode_count INTEGER NOT NULL DEFAULT 0, completed_episode_count INTEGER NOT NULL DEFAULT 0,
 next_season INTEGER, next_episode INTEGER, resume_season INTEGER, resume_episode INTEGER, summary_revision INTEGER NOT NULL,
 PRIMARY KEY(user_id,library_id,media_id), FOREIGN KEY(library_id,media_id) REFERENCES library_items(library_id,media_id) ON DELETE CASCADE,
 CHECK(completed_episode_count BETWEEN 0 AND owned_episode_count)
) WITHOUT ROWID;
CREATE INDEX user_items_media ON user_library_items(library_id,media_id,user_id);
CREATE INDEX user_items_activity ON user_library_items(user_id,library_id,last_played_at DESC,media_id);
CREATE TABLE playback_session_ids (
 user_id TEXT NOT NULL, device_id TEXT NOT NULL, play_session_id TEXT NOT NULL, item_id TEXT NOT NULL,
 media_id TEXT NOT NULL, season INTEGER NOT NULL, episode INTEGER NOT NULL, file_id TEXT NOT NULL, file_revision INTEGER NOT NULL CHECK(file_revision>0),
 source_version TEXT NOT NULL, library_id TEXT NOT NULL, config_revision INTEGER NOT NULL CHECK(config_revision>=0), timeline_mode TEXT NOT NULL CHECK(timeline_mode='full_file'),
 issued_at INTEGER NOT NULL, generation INTEGER NOT NULL CHECK(generation>=0), state TEXT NOT NULL CHECK(state IN ('pending','active','closed')),
 PRIMARY KEY(user_id,device_id,play_session_id)
);
CREATE INDEX session_id_expiry ON playback_session_ids(issued_at);
CREATE INDEX session_generation ON playback_session_ids(user_id,device_id,generation DESC);
CREATE TABLE playback_sessions (
 user_id TEXT NOT NULL, device_id TEXT NOT NULL, media_id TEXT NOT NULL, season INTEGER NOT NULL, episode INTEGER NOT NULL,
 client TEXT, device_name TEXT, client_version TEXT, play_method TEXT NOT NULL DEFAULT 'local',
 position_ms INTEGER NOT NULL DEFAULT 0, start_position_ms INTEGER NOT NULL DEFAULT 0, duration_ms INTEGER,
 paused INTEGER NOT NULL DEFAULT 0, watched_ms INTEGER NOT NULL DEFAULT 0, rate_bps INTEGER NOT NULL DEFAULT 0,
 bytes_sent INTEGER NOT NULL DEFAULT 0, connections INTEGER NOT NULL DEFAULT 0, admin_ended INTEGER NOT NULL DEFAULT 0,
 started_at INTEGER NOT NULL, last_report_at INTEGER NOT NULL, file_id TEXT NOT NULL, file_revision INTEGER NOT NULL CHECK(file_revision>0),
 source_version TEXT NOT NULL, library_id TEXT NOT NULL, config_revision INTEGER NOT NULL CHECK(config_revision>=0), timeline_mode TEXT NOT NULL CHECK(timeline_mode='full_file'),
 tracks_version INTEGER, play_session_id TEXT NOT NULL, generation INTEGER NOT NULL CHECK(generation>=0),
 last_report_at_ms INTEGER NOT NULL DEFAULT 0, last_event_sequence INTEGER, PRIMARY KEY(user_id,device_id)
);
-- v5 dropped `playback_{sessions,logs,metrics}_media`. v3 added them because
-- `Store::delete_playback_for_media` deleted by `media_id` while holding the playback
-- write lock; that helper is now test-only (`src/test_support.rs`) and **no production
-- path deletes playback rows by `media_id`** — media rows are retired, never deleted,
-- and production deletes go by `file_id`/`user_id`/`id`. They were an index maintained
-- on every playback write, serving nothing. See docs/audits/2026-10-26-library-schema-audit.md.
CREATE TABLE playback_logs (
 id TEXT PRIMARY KEY NOT NULL, user_id TEXT NOT NULL, media_id TEXT NOT NULL, season INTEGER, episode INTEGER,
 client TEXT, device_name TEXT, device_id TEXT NOT NULL, play_method TEXT NOT NULL DEFAULT 'local',
 started_at INTEGER NOT NULL, ended_at INTEGER NOT NULL, watched_ms INTEGER NOT NULL DEFAULT 0,
 start_position_ms INTEGER NOT NULL DEFAULT 0, end_position_ms INTEGER NOT NULL DEFAULT 0, duration_ms INTEGER,
 completed INTEGER NOT NULL DEFAULT 0, admin_ended INTEGER NOT NULL DEFAULT 0,
 play_session_id TEXT NOT NULL, generation INTEGER NOT NULL, file_id TEXT NOT NULL, file_revision INTEGER NOT NULL, source_version TEXT NOT NULL, timeline_mode TEXT NOT NULL,
 UNIQUE(user_id,device_id,play_session_id,generation)
);
CREATE INDEX logs_user_started ON playback_logs(user_id,started_at DESC,id DESC);
CREATE INDEX logs_started ON playback_logs(started_at DESC,id DESC);
CREATE INDEX logs_ended ON playback_logs(ended_at,started_at,id);
CREATE TABLE playback_metrics (
 id TEXT PRIMARY KEY NOT NULL, user_id TEXT NOT NULL, media_id TEXT NOT NULL, tier INTEGER NOT NULL DEFAULT 0,
 engine TEXT, ttff_ms INTEGER, rebuffer_ms INTEGER NOT NULL DEFAULT 0, rebuffer_count INTEGER NOT NULL DEFAULT 0,
 seek_count INTEGER NOT NULL DEFAULT 0, dropped_frames INTEGER, total_frames INTEGER, watched_ms INTEGER NOT NULL DEFAULT 0, recorded_at INTEGER NOT NULL
);
CREATE INDEX metrics_recorded ON playback_metrics(recorded_at,id);

CREATE TABLE media_operations (
 operation_id TEXT PRIMARY KEY NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('merge','reassign')),
 state TEXT NOT NULL CHECK(state IN ('planned','frozen','draining','applying','checking','succeeded','failed')), survivor_id TEXT NOT NULL, loser_id TEXT, plan_json TEXT NOT NULL CHECK(json_valid(plan_json)),
 phase_cursor TEXT, error TEXT, created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL
);
CREATE TABLE library_operations (
 operation_id TEXT PRIMARY KEY NOT NULL, library_id TEXT NOT NULL, kind TEXT NOT NULL CHECK(kind IN ('roots','locale','default_locale','integrity','scrape')),
 state TEXT NOT NULL CHECK(state IN ('planned','frozen','draining','applying','checking','succeeded','failed')), expected_revision INTEGER NOT NULL, plan_json TEXT NOT NULL CHECK(json_valid(plan_json)),
 cursor_json TEXT, error TEXT, created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL
);
CREATE INDEX library_operation_dispatch ON library_operations(state,updated_at_ms,operation_id);
-- Browse paging (library_query/items/page.rs) and item detail
-- (library_query/details/mod.rs) resolve the latest integrity operation of one
-- Library. Without this index the plan dispatches by state and sorts the matches.
CREATE INDEX operations_library_kind ON library_operations(library_id,kind,updated_at_ms DESC);
CREATE TABLE library_query_count_rebuild (
 operation_id TEXT NOT NULL REFERENCES library_operations(operation_id) ON DELETE CASCADE,
 surface TEXT NOT NULL, dimension TEXT NOT NULL, value TEXT NOT NULL,
 member_count INTEGER NOT NULL CHECK(member_count>=0),
 PRIMARY KEY(operation_id,surface,dimension,value)
);
CREATE TABLE library_removals (
 operation_id TEXT PRIMARY KEY NOT NULL, library_id TEXT NOT NULL UNIQUE, state TEXT NOT NULL,
 config_revision INTEGER NOT NULL, snapshot_cursor TEXT NOT NULL DEFAULT '', snapshot_done INTEGER NOT NULL DEFAULT 0, total_files INTEGER NOT NULL DEFAULT 0,
 removed_ledger_count INTEGER NOT NULL DEFAULT 0, error TEXT, created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL
);
CREATE TABLE library_removal_roots (operation_id TEXT NOT NULL REFERENCES library_removals(operation_id),path TEXT NOT NULL,PRIMARY KEY(operation_id,path));
CREATE TABLE library_removal_files (
 operation_id TEXT NOT NULL REFERENCES library_removals(operation_id), file_id TEXT NOT NULL,
 root_id TEXT NOT NULL, path_key TEXT NOT NULL, display_path TEXT NOT NULL, media_id TEXT NOT NULL,
 file_revision INTEGER NOT NULL, source_version TEXT NOT NULL, units_json TEXT NOT NULL CHECK(json_valid(units_json)),
 PRIMARY KEY(operation_id,file_id)
);
CREATE INDEX playback_session_file ON playback_sessions(file_id,user_id,device_id);
CREATE TABLE pending_file_deletions (path TEXT PRIMARY KEY NOT NULL,reason TEXT NOT NULL,attempts INTEGER NOT NULL DEFAULT 0,next_retry_at INTEGER NOT NULL DEFAULT 0,last_error TEXT,file_id TEXT,file_size INTEGER,file_inode INTEGER);
-- Store::list_due_file_deletions reads the retry window; the caller orders by
-- next_retry_at so the index supplies both the window and the order.
CREATE INDEX pending_deletions_due ON pending_file_deletions(next_retry_at);
CREATE TABLE pending_root_reassignments (kind TEXT NOT NULL, root_key TEXT NOT NULL, cursor_key TEXT NOT NULL, unidentified_cursor_key TEXT NOT NULL DEFAULT '', unidentified_cursor_id TEXT NOT NULL DEFAULT '', dispatch_order INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(kind, root_key));
CREATE INDEX root_reassignment_dispatch ON pending_root_reassignments(dispatch_order,kind,root_key);
CREATE TABLE root_reassignment_state (
 kind TEXT NOT NULL, root_key TEXT NOT NULL, operation_id TEXT NOT NULL UNIQUE,
 library_id TEXT NOT NULL REFERENCES libraries(library_id) ON DELETE CASCADE, config_revision INTEGER NOT NULL, state TEXT NOT NULL,
 revision INTEGER NOT NULL, error TEXT, updated_at_ms INTEGER NOT NULL,
 PRIMARY KEY(kind,root_key)
);
CREATE INDEX root_reassignment_library ON root_reassignment_state(library_id,root_key);

CREATE TABLE pending_file_renames (
 rename_id TEXT PRIMARY KEY NOT NULL,
 file_id TEXT NOT NULL,
 from_path TEXT NOT NULL,
 to_path TEXT NOT NULL,
 expected_revision INTEGER NOT NULL,
 expected_source_version TEXT NOT NULL,
 from_library_id TEXT NOT NULL,
 from_root_id TEXT NOT NULL,
 to_library_id TEXT NOT NULL,
 to_root_id TEXT NOT NULL,
 sidecars_json TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(sidecars_json)),
 phase TEXT NOT NULL CHECK(phase IN ('planned','disk_applied','library_published','references_applied','failed')),
 last_completed_phase TEXT CHECK(last_completed_phase IN ('planned','disk_applied','library_published','references_applied')),
 source_size INTEGER NOT NULL,
 source_device INTEGER NOT NULL,
 source_inode INTEGER NOT NULL,
 source_mtime_ns INTEGER NOT NULL,
 dest_size INTEGER,
 dest_device INTEGER,
 dest_inode INTEGER,
 dest_mtime_ns INTEGER,
 from_config_revision INTEGER NOT NULL,
 to_config_revision INTEGER NOT NULL,
 attempts INTEGER NOT NULL DEFAULT 0,
 next_retry_at INTEGER NOT NULL DEFAULT 0,
 last_error TEXT,
 created_at_ms INTEGER NOT NULL,
 updated_at_ms INTEGER NOT NULL,
 CHECK((dest_device IS NULL) = (dest_inode IS NULL))
);
CREATE INDEX pending_file_renames_due ON pending_file_renames(next_retry_at, rename_id);
CREATE TABLE scrape_sidecars (file_id TEXT NOT NULL REFERENCES library_files(file_id) ON DELETE CASCADE,kind TEXT NOT NULL,path TEXT NOT NULL,PRIMARY KEY(file_id,kind));
CREATE TABLE unidentified (unidentified_id TEXT NOT NULL,library_id TEXT NOT NULL,root_id TEXT NOT NULL,path TEXT NOT NULL,path_key TEXT NOT NULL,confidence TEXT NOT NULL,revision INTEGER NOT NULL DEFAULT 1,PRIMARY KEY(library_id,path_key),FOREIGN KEY(root_id,library_id) REFERENCES library_roots(root_id,library_id));
CREATE UNIQUE INDEX unidentified_identity ON unidentified(unidentified_id);
CREATE INDEX unidentified_path_seek ON unidentified(path_key,unidentified_id);
CREATE INDEX unidentified_root ON unidentified(root_id,library_id);
CREATE TABLE scan_operations (
 id TEXT PRIMARY KEY NOT NULL,library_id TEXT NOT NULL,scope_signature TEXT NOT NULL,status TEXT NOT NULL,generation INTEGER NOT NULL,
 created_at_ms INTEGER NOT NULL,report_json TEXT NOT NULL DEFAULT '{}',targets_json TEXT NOT NULL DEFAULT '[]',
 scope_kind TEXT NOT NULL DEFAULT 'full' CHECK(scope_kind IN ('full','targeted')),dirty_rev INTEGER NOT NULL DEFAULT 0,consumed_rev INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX scan_library_status ON scan_operations(library_id,status);
CREATE TABLE scan_seen (operation_id TEXT NOT NULL,path TEXT NOT NULL,path_key TEXT NOT NULL,mtime_ms INTEGER NOT NULL DEFAULT 0,file_size INTEGER NOT NULL DEFAULT 0,content_digest TEXT,PRIMARY KEY(operation_id,path_key));
CREATE TABLE probe_jobs (
 id TEXT PRIMARY KEY NOT NULL,kind TEXT NOT NULL,media_id TEXT NOT NULL,season INTEGER,scope_key TEXT NOT NULL,status TEXT NOT NULL,
 total INTEGER NOT NULL,completed INTEGER NOT NULL DEFAULT 0,succeeded INTEGER NOT NULL DEFAULT 0,failed INTEGER NOT NULL DEFAULT 0,partial INTEGER NOT NULL DEFAULT 0,error TEXT,
 created_at_ms INTEGER NOT NULL,started_at_ms INTEGER,finished_at_ms INTEGER,analysis_policy_key TEXT,sampling_mode TEXT
);
CREATE UNIQUE INDEX probe_active_scope ON probe_jobs(scope_key) WHERE status IN ('queued','running');
CREATE INDEX probe_media_created ON probe_jobs(media_id,created_at_ms DESC);
CREATE INDEX probe_terminal ON probe_jobs(finished_at_ms,id) WHERE status IN ('succeeded','failed','cancelled');
CREATE INDEX probe_scope_terminal ON probe_jobs(scope_key,finished_at_ms DESC,id DESC) WHERE status IN ('succeeded','failed','cancelled');
CREATE TABLE probe_job_units (
 job_id TEXT NOT NULL REFERENCES probe_jobs(id) ON DELETE CASCADE,file_id TEXT NOT NULL,kind TEXT NOT NULL,
 force_fingerprint INTEGER NOT NULL DEFAULT 0,reuse_fingerprint_cache INTEGER NOT NULL DEFAULT 0,overwrite_markers INTEGER NOT NULL DEFAULT 0,
 status TEXT NOT NULL,error TEXT,reuse_media_info_cache INTEGER NOT NULL DEFAULT 0,stage TEXT NOT NULL DEFAULT 'metadata',
 sampling_plan_json TEXT,detection_outcome_json TEXT,PRIMARY KEY(job_id,file_id)
);
CREATE UNIQUE INDEX probe_unit_active ON probe_job_units(file_id) WHERE status IN ('queued','running');
CREATE INDEX probe_unit_dispatch ON probe_job_units(stage,status,job_id) WHERE status='queued';
CREATE INDEX probe_unit_status ON probe_job_units(job_id,status);
CREATE TABLE file_probe_obligations (file_id TEXT PRIMARY KEY NOT NULL REFERENCES library_files(file_id) ON DELETE CASCADE,created_at_ms INTEGER NOT NULL,reason TEXT NOT NULL);
CREATE INDEX probe_obligation_dispatch ON file_probe_obligations(created_at_ms,file_id);
CREATE TABLE probe_stage_state (file_id TEXT NOT NULL,context_key TEXT NOT NULL,stage TEXT NOT NULL,status TEXT NOT NULL,failure_count INTEGER NOT NULL DEFAULT 0,next_retry_at_ms INTEGER,error_kind TEXT,error TEXT,active_job_id TEXT,dispatched_until_ms INTEGER,updated_at_ms INTEGER NOT NULL,PRIMARY KEY(file_id,context_key,stage));
CREATE INDEX probe_stage_active_job ON probe_stage_state(active_job_id) WHERE active_job_id IS NOT NULL;
CREATE INDEX probe_retry ON probe_stage_state(next_retry_at_ms,file_id,context_key,stage) WHERE status='failed' AND next_retry_at_ms IS NOT NULL AND active_job_id IS NULL;
CREATE TABLE probe_season_comparison_history (media_id TEXT NOT NULL,season INTEGER NOT NULL,comparison_digest TEXT NOT NULL,published_at_ms INTEGER NOT NULL,PRIMARY KEY(media_id,season));
CREATE TABLE chapter_frame_tasks (id TEXT PRIMARY KEY NOT NULL,file_id TEXT NOT NULL,source_version TEXT NOT NULL,targets_json TEXT NOT NULL,targets_hash TEXT NOT NULL,status TEXT NOT NULL,error TEXT,created_at_ms INTEGER NOT NULL,started_at_ms INTEGER,finished_at_ms INTEGER,UNIQUE(file_id,source_version,targets_hash));
CREATE INDEX chapter_dispatch ON chapter_frame_tasks(status,created_at_ms,id);
CREATE TABLE fingerprint_cache (file_id TEXT PRIMARY KEY NOT NULL,cache_key TEXT NOT NULL,algorithm_version INTEGER NOT NULL,sample_duration_secs INTEGER NOT NULL,media_duration_ms INTEGER,intro_json TEXT NOT NULL,outro_json TEXT,captured_at_ms INTEGER NOT NULL);
CREATE TABLE media_markers (media_id TEXT NOT NULL,season INTEGER NOT NULL,episode INTEGER NOT NULL,intro_start_ms INTEGER,intro_end_ms INTEGER,outro_start_ms INTEGER,outro_end_ms INTEGER,source TEXT NOT NULL,locked INTEGER NOT NULL DEFAULT 0,updated_at INTEGER NOT NULL,PRIMARY KEY(media_id,season,episode));
CREATE TABLE fingerprint_samples (sample_id TEXT PRIMARY KEY NOT NULL,file_id TEXT NOT NULL,source_version TEXT NOT NULL,capture_profile_key TEXT NOT NULL,kind TEXT NOT NULL,window_start_ms INTEGER NOT NULL,window_end_ms INTEGER NOT NULL,pcm_duration_ms INTEGER,fingerprint_json TEXT NOT NULL,captured_job_id TEXT NOT NULL,captured_at_ms INTEGER NOT NULL,metrics_json TEXT NOT NULL,UNIQUE(file_id,source_version,capture_profile_key,kind,window_start_ms,window_end_ms));
CREATE INDEX sample_lookup ON fingerprint_samples(file_id,kind,source_version,capture_profile_key);
CREATE INDEX sample_job ON fingerprint_samples(captured_job_id);
CREATE TABLE fingerprint_season_models (model_id TEXT PRIMARY KEY NOT NULL,media_id TEXT NOT NULL,season INTEGER NOT NULL,kind TEXT NOT NULL,model_version INTEGER NOT NULL,membership_key TEXT NOT NULL,policy_key TEXT NOT NULL,model_json TEXT NOT NULL,created_at_ms INTEGER NOT NULL);
CREATE INDEX season_model_lookup ON fingerprint_season_models(media_id,season,kind);
CREATE TABLE fingerprint_model_members (model_id TEXT NOT NULL REFERENCES fingerprint_season_models(model_id) ON DELETE CASCADE,sample_id TEXT NOT NULL,file_id TEXT NOT NULL,source_version TEXT NOT NULL,PRIMARY KEY(model_id,sample_id));
CREATE INDEX model_file ON fingerprint_model_members(file_id);
CREATE TABLE fingerprint_attempts (attempt_id TEXT PRIMARY KEY NOT NULL,job_id TEXT NOT NULL,file_id TEXT NOT NULL,kind TEXT NOT NULL,window_start_ms INTEGER NOT NULL,window_end_ms INTEGER NOT NULL,phase TEXT NOT NULL,started_at_ms INTEGER NOT NULL,finished_at_ms INTEGER,status TEXT NOT NULL,error_kind TEXT,metrics_json TEXT NOT NULL);
CREATE INDEX attempt_job_file ON fingerprint_attempts(job_id,file_id);

-- File cleanup and reassignment must seek one file instead of scanning all history.
CREATE INDEX attempts_file ON fingerprint_attempts(file_id);
CREATE INDEX session_ids_live_file ON playback_session_ids(file_id) WHERE state <> 'closed';
-- Terminal predicates match the owning state machines; live/retryable work stays outside.
CREATE INDEX media_operations_terminal_age ON media_operations(updated_at_ms,operation_id)
 WHERE state = 'succeeded';
CREATE INDEX library_operations_terminal_age ON library_operations(updated_at_ms,operation_id)
 WHERE state = 'succeeded' OR (state = 'failed' AND kind NOT IN ('roots','locale','integrity','default_locale'));
CREATE INDEX scan_operations_terminal_age ON scan_operations(created_at_ms,id)
 WHERE status IN ('completed','partial','cancelled','failed');
