//! Library layout v3 (one-shot DDL optimization): every index the DDL keeps or
//! adds must be reachable from a real statement, and the statement must reach it.
//!
//! Budget: a statement that merely *works* proves nothing. Each case below is an
//! `EXPLAIN QUERY PLAN` assertion in the idiom of `cleanup_index_work.rs`: the
//! plan must name the index (or the primary key) and must not fall back to a
//! full table scan or a `TEMP B-TREE` sort on the paths we claim to have fixed.
//!
//! The SQL in each case is the production shape, copied from the site named in
//! the comment above it, with bind parameters rendered as literals.

use rusqlite::Connection;
use store::{Store, StoreError};

fn open_library_db() -> (tempfile::TempDir, Connection) {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    drop(Store::open(&data).unwrap());
    let conn = Connection::open(data.join("library.db")).unwrap();
    (tmp, conn)
}

fn plan(conn: &Connection, sql: &str) -> Vec<String> {
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let n = stmt.parameter_count();
    let args: Vec<String> = (0..n).map(|_| "x".to_string()).collect();
    stmt.query_map(rusqlite::params_from_iter(args.iter()), |r| {
        r.get::<_, String>(3)
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

/// One-line rendering of a plan, for assertion messages.
fn plan_text(conn: &Connection, sql: &str) -> String {
    plan(conn, sql).join(" | ")
}

fn assert_no_scan(text: &str, table: &str) {
    assert!(
        !text.contains(&format!("SCAN {table}")),
        "{table} must be searched, not scanned: {text}"
    );
}

/// Browse paging (`library_query/items/page.rs:361`) and item detail
/// (`library_query/details/mod.rs:55`) both run this correlated subquery once
/// per page/detail read.
const INTEGRITY_SUBQUERY: &str = "SELECT l.integrity_state, \
     (SELECT operation_id FROM library_operations \
      WHERE library_id = l.library_id AND kind = 'integrity' \
        AND state IN ('failed','succeeded') \
      ORDER BY updated_at_ms DESC LIMIT 1) \
     FROM libraries l WHERE l.library_id = 'lib'";

#[test]
fn integrity_lookup_uses_library_index() {
    let (_tmp, conn) = open_library_db();
    let text = plan_text(&conn, INTEGRITY_SUBQUERY);
    println!("integrity plan: {text}");
    assert!(
        text.contains("operations_library_kind"),
        "the integrity lookup must search operations_library_kind: {text}"
    );
    assert_no_scan(&text, "library_operations");
    assert!(
        !text.contains("TEMP B-TREE"),
        "the index must also deliver the updated_at_ms order, without a sort: {text}"
    );
}

// v5 removed `delete_playback_for_media_uses_media_indexes` together with the three
// `playback_{sessions,logs,metrics}_media` indexes it pinned.
//
// This is the one place in this file where a plan assertion was deleted rather than
// added, so it is worth stating why it is not a smuggled regression: the *query* it
// protected no longer exists in production. `Store::delete_playback_for_media` moved to
// `src/test_support.rs` and deletes by `media_id` only for fixtures; production deletes
// playback rows by `file_id` (`library_write/reassign.rs`), by `user_id`/`device_id`
// (`playback/{event,sessions}.rs`) or by `id` (`playback/{logs,retention}.rs`), and media
// rows are retired, never deleted. Keeping the indexes to keep this assertion green would
// have meant paying three index writes per playback row for a test helper — the exact
// write amplification the v4/v5 rounds removed elsewhere. The replacement guard is the
// layout check: if one of those indexes comes back, `LIBRARY_INDEXES` and the DDL disagree
// and `Store::open` refuses the file (see
// `tests/schema_version_guard.rs::index_layout_is_strict_in_both_directions`).

#[test]
fn due_pending_deletions_use_index() {
    let (_tmp, conn) = open_library_db();
    // `Store::list_due_file_deletions` (pending_file_deletions.rs:71-85).
    let sql = "SELECT path, file_id, file_size, file_inode FROM pending_file_deletions \
               WHERE next_retry_at <= 1761393600000 AND attempts < 20 \
               ORDER BY next_retry_at";
    let text = plan_text(&conn, sql);
    println!("due deletions plan: {text}");
    assert!(
        text.contains("pending_deletions_due"),
        "due deletions must search pending_deletions_due: {text}"
    );
    assert_no_scan(&text, "pending_file_deletions");
    assert!(
        !text.contains("TEMP B-TREE"),
        "the index must deliver next_retry_at order, without a sort: {text}"
    );
}

#[test]
fn metadata_inputs_media_lookup_uses_index() {
    let (_tmp, conn) = open_library_db();
    // The shape SQLite runs for `FOREIGN KEY(media_id) REFERENCES media(media_id)`
    // when a media row is deleted: the PK of media is (media_id) but the PK of
    // library_metadata_inputs starts with library_id, so without its own index
    // the child table is scanned.
    let sql = "SELECT 1 FROM library_metadata_inputs WHERE media_id = 'm'";
    let text = plan_text(&conn, sql);
    println!("metadata inputs FK child plan: {text}");
    assert!(
        text.contains("metadata_inputs_media"),
        "the media_id FK child lookup must use metadata_inputs_media: {text}"
    );
    assert_no_scan(&text, "library_metadata_inputs");
}

/// `work_index_for_sort` (`library_query/protocol/page.rs:136-146`) forces
/// `items_protocol_default` for the default work order and
/// `items_protocol_default_explicit` when the caller asks for
/// `DateCreated DESC, Name DESC`. The two share their first three columns but
/// serve *different* `ORDER BY`s; this case pins that down in both directions,
/// so a future "they look redundant" cleanup has to argue with the planner.
#[test]
fn protocol_default_and_explicit_indexes_each_avoid_a_sort() {
    let (_tmp, conn) = open_library_db();
    // Default work order: added_at_sort DESC, protocol_title_key DESC, then the
    // convergence tie-breaks (library_id is fixed by the single-library scope).
    let default_order = "ORDER BY i.added_at_sort DESC, i.protocol_title_key DESC, \
                         i.library_id ASC, i.media_id ASC LIMIT 50";
    let default_sql = format!(
        "SELECT i.library_id, i.media_id FROM library_items i WHERE i.library_id = 'lib' {default_order}"
    );
    let default_text = plan_text(&conn, &default_sql);
    println!("default work order: {default_text}");
    assert!(
        default_text.contains("items_protocol_default"),
        "the planner must pick items_protocol_default for the default work order: {default_text}"
    );
    assert!(
        !default_text.contains("TEMP B-TREE"),
        "the default work order must not sort: {default_text}"
    );

    // Explicit DateCreated DESC + Name DESC adds `title ASC` before the tie-breaks.
    let explicit_order = "ORDER BY i.added_at_sort DESC, i.protocol_title_key DESC, \
                          i.title ASC, i.library_id ASC, i.media_id ASC LIMIT 50";
    let explicit_sql = format!(
        "SELECT i.library_id, i.media_id FROM library_items i \
         INDEXED BY items_protocol_default_explicit WHERE i.library_id = 'lib' {explicit_order}"
    );
    let explicit_text = plan_text(&conn, &explicit_sql);
    println!("explicit name order: {explicit_text}");
    assert!(
        explicit_text.contains("items_protocol_default_explicit"),
        "the explicit name order must use items_protocol_default_explicit: {explicit_text}"
    );
    assert!(
        !explicit_text.contains("TEMP B-TREE"),
        "the explicit name order must not sort: {explicit_text}"
    );

    // Counter-check: the "redundant" pairing is not interchangeable. Serving the
    // default order from the explicit index costs a sort, which is exactly what
    // items_protocol_default exists to avoid.
    let crossed_sql = format!(
        "SELECT i.library_id, i.media_id FROM library_items i \
         INDEXED BY items_protocol_default_explicit WHERE i.library_id = 'lib' {default_order}"
    );
    let crossed_text = plan_text(&conn, &crossed_sql);
    println!("default order through the explicit index: {crossed_text}");
    assert!(
        crossed_text.contains("TEMP B-TREE"),
        "if this stops sorting, one of the two protocol indexes became redundant: {crossed_text}"
    );
}

/// P5: narrow composite-PK tables are stored as the primary-key b-tree instead of
/// "table b-tree + one autoindex per table". `playback_units` is deliberately
/// *not* in this list: its row carries the resume pack (`resume_file_id`,
/// `resume_source_version`, `audio_track`, `subtitle_track`) so the row is wider
/// than the narrow-row budget this conversion needs to pay off.
const WITHOUT_ROWID_TABLES: &[&str] = &[
    "library_file_units",
    "library_item_terms",
    "library_protocol_people",
    "library_protocol_terms",
    "user_library_items",
];

#[test]
fn p5_narrow_tables_are_without_rowid() {
    let (_tmp, conn) = open_library_db();
    let mut wrong = Vec::new();
    for table in WITHOUT_ROWID_TABLES {
        let wr: i64 = conn
            .query_row(
                "SELECT wr FROM pragma_table_list WHERE schema = 'main' AND name = ?1",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        if wr != 1 {
            wrong.push(*table);
        }
    }
    let playback_units_wr: i64 = conn
        .query_row(
            "SELECT wr FROM pragma_table_list WHERE schema = 'main' AND name = 'playback_units'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(playback_units_wr, 0, "playback_units keeps its rowid");
    assert!(wrong.is_empty(), "not WITHOUT ROWID: {wrong:#?}");
}

#[test]
fn p5_primary_key_reads_use_the_table_btree() {
    let (_tmp, conn) = open_library_db();
    // Real reads: user_library_items by full PK (file_reads.rs summaries),
    // library_protocol_people by PK prefix (query_projection/people.rs:49),
    // library_file_units by its UNIQUE cover (scoped coverage reads).
    let reads = [
        (
            "user_library_items",
            "SELECT * FROM user_library_items \
             WHERE user_id = 'u' AND library_id = 'lib' AND media_id = 'm'",
        ),
        (
            "library_protocol_people",
            "SELECT role, name_key, tmdb_id, wire_id FROM library_protocol_people \
             WHERE library_id = 'lib' AND media_id = 'm'",
        ),
    ];
    for (table, sql) in reads {
        let text = plan_text(&conn, sql);
        println!("{table} pk read: {text}");
        assert!(
            text.contains(&format!("SEARCH {table} USING PRIMARY KEY")),
            "{table} must read through its own primary-key b-tree: {text}"
        );
        assert_no_scan(&text, table);
    }

    // Counter-check on the table we did not convert: the same shape needs the
    // implicit autoindex plus a table lookup, which is the extra b-tree P5 removes.
    let skipped = plan_text(
        &conn,
        "SELECT * FROM playback_units \
         WHERE user_id = 'u' AND media_id = 'm' AND season = 1 AND episode = 1",
    );
    println!("playback_units pk read: {skipped}");
    assert!(
        skipped.contains("sqlite_autoindex_playback_units"),
        "playback_units is expected to keep the autoindex shape: {skipped}"
    );
}

#[test]
fn p5_primary_key_order_scan_is_clustered() {
    let (_tmp, conn) = open_library_db();
    // Clustered storage: the row order *is* the PK order, so an ordered walk of a
    // converted table never sorts. This is the property the conversion buys.
    // `library_item_terms`' key became `(library_id, media_id, term_kind,
    // term_value)` in the v4 round so that a per-card read is a key prefix; the
    // clustered walk follows the new order.
    let text = plan_text(
        &conn,
        "SELECT * FROM library_item_terms ORDER BY library_id, media_id, term_kind, term_value",
    );
    println!("clustered walk: {text}");
    assert!(
        !text.contains("TEMP B-TREE"),
        "a WITHOUT ROWID table is already in PK order: {text}"
    );
}

/// Review Focus 5: an existing `library.db` at the previous layout must be
/// An unknown file identity is refused before any other file is touched.
#[test]
fn a_different_library_header_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    {
        let conn = Connection::open(data.join("library.db")).unwrap();
        conn.pragma_update(None, "application_id", 1129139249_i64)
            .unwrap();
        conn.pragma_update(
            None,
            "user_version",
            store::schema::layout::LIBRARY_USER_VERSION + 1,
        )
        .unwrap();
    }
    let error = Store::open(&data).expect_err("an unknown identity must be refused");
    let StoreError::LayoutMismatch(message) = &error else {
        panic!("expected LayoutMismatch, got {error:?}");
    };
    let expected = format!(
        "user_version {}",
        store::schema::layout::LIBRARY_USER_VERSION
    );
    for required in [
        "library".to_string(),
        expected,
        "separate empty directory".to_string(),
    ] {
        assert!(message.contains(&required), "missing {required}: {message}");
    }
}

/// `PRAGMA auto_vacuum = INCREMENTAL` is a file-header property that can only be
/// set before the first table exists, so it belongs to the create path, not to a
/// later `VACUUM`. 2 is `INCREMENTAL`.
#[test]
fn fresh_store_uses_incremental_auto_vacuum() {
    let (_tmp, conn) = open_library_db();
    let mode: i64 = conn
        .pragma_query_value(None, "auto_vacuum", |r| r.get(0))
        .unwrap();
    assert_eq!(
        mode, 2,
        "expected INCREMENTAL auto_vacuum on a fresh library.db"
    );
}

/// Playback source selection (`playback/source.rs::source_rows`, the only read on
/// the playback start path). The user-scoped variant adds `f.library_id IN (…)`.
///
/// That library predicate must never become the way into `library_files`: driving
/// through `files_presence (library_id, presence_state)` visits **every present file
/// in the Library** to play one item, which measured 56.4 ms p50 at 61k files against
/// 0.029 ms for the same query without a user scope. The join order is therefore
/// pinned (`CROSS JOIN`) to the `media_id` seek in `coverage_media`, and the library
/// scope narrows a `file_id` seek instead of leading it.
const PLAYBACK_SOURCES: &str = "SELECT f.file_id, MIN(u.season), MIN(u.episode), \
     COALESCE(d.tracks_version, f.tracks_version) \
     FROM library_file_units u \
     CROSS JOIN library_files f ON u.file_id = f.file_id \
     LEFT JOIN file_details d ON d.file_id = f.file_id \
     WHERE f.presence_state = 'present' AND f.publication_state = 'published' \
       AND u.media_id = 'media' AND f.library_id IN ('library') \
     GROUP BY f.file_id ORDER BY f.file_revision DESC, f.file_id ASC";

#[test]
fn playback_source_selection_seeks_units_by_media_not_the_whole_library() {
    let (_tmp, conn) = open_library_db();
    let text = plan_text(&conn, PLAYBACK_SOURCES);
    println!("playback sources plan: {text}");
    assert!(
        text.starts_with("SEARCH u USING COVERING INDEX coverage_media (media_id=?)"),
        "the media_id seek must drive playback source selection: {text}"
    );
    assert!(
        text.contains(
            "SEARCH f USING INDEX files_presence (library_id=? AND presence_state=? AND file_id=?)"
        ),
        "the Library scope must narrow a file_id seek, not lead the scan: {text}"
    );
    assert_no_scan(&text, "library_files");
    assert_no_scan(&text, "library_file_units");
}

/// The person filter (`library_query/protocol/membership.rs`) used to carry
/// `wire_id IN (…) OR tmdb_id IN (…)` *inside* the per-card subquery. That `OR`
/// blocked both person indexes, so the planner answered
/// `SEARCH a USING PRIMARY KEY (library_id=?)`: one walk of the Library's whole
/// people table per card (0.78 s at 500 cards, 14.6 s at 2 000, ≈2.4 h at 20 000 on a
/// 200k-person Library). Task 25 hoisted the identity resolution into a materialised
/// `person_wanted` set.
///
/// This pins the shape that makes it cheap: one materialisation, a `MULTI-INDEX OR`
/// that seeks `protocol_people_wire` **and** `protocol_people_tmdb` (so both indexes
/// still earn their keep), and no per-card primary-key walk of the people table.
#[test]
fn person_filter_resolves_identities_once_and_seeks_both_person_indexes() {
    let (_tmp, conn) = open_library_db();
    // The production statement, from `protocol/{mod,membership,predicates,sql}.rs`,
    // with the request literals substituted (`?` scope library, one person id).
    let sql = "WITH scoped AS (SELECT i.library_id, i.media_id, i.kind, i.title, \
                   i.protocol_title_key, i.added_at_sort, i.pref_locale, i.search_text \
                 FROM (SELECT li.*, COALESCE(NULLIF(TRIM(l.display_locale), ''), 'en') AS pref_locale \
                         FROM library_items li LEFT JOIN libraries l ON l.library_id = li.library_id) i \
                 WHERE i.library_id IN ('lib')), \
               person_wanted AS MATERIALIZED (SELECT DISTINCT a.role AS role, trim(a.name_key) AS name_key, \
                   CASE WHEN a.role='Actor' AND a.tmdb_id<>'' THEN a.tmdb_id ELSE '' END AS tmdb_id \
                 FROM library_protocol_people a \
                 WHERE (a.wire_id IN ('person') OR a.tmdb_id IN ('person')) \
                   AND EXISTS (SELECT 1 FROM scoped seed \
                                WHERE seed.library_id=a.library_id AND seed.media_id=a.media_id)), \
               work_rows AS (SELECT s.library_id, s.media_id, s.kind, s.title, -1 AS season, -1 AS episode \
                 FROM scoped s WHERE s.kind = 'movie' AND EXISTS (\
                   SELECT 1 FROM library_protocol_people p \
                    WHERE p.library_id=s.library_id AND p.media_id=s.media_id \
                      AND EXISTS (SELECT 1 FROM person_wanted w WHERE w.role=p.role \
                                   AND (w.name_key=p.name_key OR (w.tmdb_id<>'' AND w.tmdb_id=p.tmdb_id))))), \
               rows AS (SELECT * FROM work_rows) \
             SELECT count(*) FROM rows";
    let text = plan_text(&conn, sql);
    println!("person filter plan: {text}");
    assert!(
        text.contains("MATERIALIZE person_wanted"),
        "the identity set must be materialised once: {text}"
    );
    assert!(
        text.contains("protocol_people_wire"),
        "the wire-id identity must be found through protocol_people_wire: {text}"
    );
    assert!(
        text.contains("protocol_people_tmdb"),
        "the tmdb-id identity must be found through protocol_people_tmdb: {text}"
    );
    assert!(
        !text.contains("SEARCH a USING PRIMARY KEY (library_id=?)"),
        "the people table must not be walked by primary-key prefix per card: {text}"
    );
    assert!(
        text.contains("MULTI-INDEX OR") || text.contains("SEARCH a USING COVERING INDEX"),
        "the disjunction must be answered by index seeks, not a scan: {text}"
    );
}
