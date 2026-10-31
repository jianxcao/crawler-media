//! Gate for the 2026-10-26 library.db schema audit
//! (`docs/audits/2026-10-26-library-schema-audit.md`).
//!
//! `library_query_plans.rs` pins the six indexes v3 *added*. This file pins the
//! unread term index v3 left behind, and the read shapes that decide whether
//! neighbouring protocol indexes earn their keep. Task 12.2 introduced a direct
//! Person name/image lookup, so its new name-leading index must earn its place
//! with a seek that also serves the image preference order.

use domain::MediaKind;
use rusqlite::Connection;
use store::{
    Store,
    library_query::{BrowseFilter, FacetRequest, FacetTier},
};

#[path = "query_work/support.rs"]
#[allow(dead_code)]
mod support;

fn open_library_db() -> (tempfile::TempDir, Connection) {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data");
    drop(Store::open(&data).unwrap());
    let conn = Connection::open(data.join("library.db")).unwrap();
    (tmp, conn)
}

fn plan_text(conn: &Connection, sql: &str) -> String {
    let mut stmt = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let args: Vec<String> = (0..stmt.parameter_count())
        .map(|_| "x".to_string())
        .collect();
    let rows: Vec<String> = stmt
        .query_map(rusqlite::params_from_iter(args.iter()), |r| r.get(3))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    rows.join(" | ")
}

fn index_exists(conn: &Connection, name: &str) -> bool {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type = 'index' AND name = ?1)",
        [name],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        == 1
}

/// The unused value-first term index stays dropped; Person names now have a
/// direct reader in `protocol/entities.rs::protocol_person` (Task 12.2).
#[test]
fn unread_protocol_term_index_stays_dropped() {
    let (_tmp, conn) = open_library_db();
    assert!(
        !index_exists(&conn, "protocol_terms_value"),
        "protocol_terms_value must not come back without a reader"
    );
    for name in [
        "protocol_terms_media",
        "protocol_people_wire",
        "protocol_people_tmdb",
        "protocol_people_name",
    ] {
        assert!(index_exists(&conn, name), "{name} must exist");
    }
}

/// The production Person-by-name lookup must seek only that person's rows and
/// prefer an available image without sorting all people or all memberships.
#[test]
fn person_name_image_lookup_uses_name_leading_index_without_sorting() {
    let (_tmp, conn) = open_library_db();
    let text = plan_text(
        &conn,
        "SELECT p.wire_id,p.display_name,p.role,p.character,NULLIF(p.tmdb_id,''),p.image_url \
         FROM library_protocol_people p \
         JOIN library_items i ON i.library_id=p.library_id AND i.media_id=p.media_id \
         WHERE p.name_key=?1 AND p.library_id IN (SELECT value FROM json_each(?2)) \
           AND i.available_file_count>0 ORDER BY p.image_url DESC LIMIT 1",
    );
    assert!(
        text.contains("SEARCH p USING INDEX protocol_people_name (name_key=?)"),
        "Person lookup must seek the name-leading image index: {text}"
    );
    assert!(
        !text.contains("SCAN p") && !text.contains("TEMP B-TREE"),
        "Person lookup must not scan unrelated names or sort memberships: {text}"
    );
}

/// `library_query/protocol/membership.rs:17` — the term predicate the protocol
/// item query runs per scoped card. It must stay a primary-key seek after the
/// value-index delete.
#[test]
fn protocol_term_predicate_seeks_the_primary_key() {
    let (_tmp, conn) = open_library_db();
    let text = plan_text(
        &conn,
        "SELECT 1 FROM library_protocol_terms t \
         WHERE t.library_id = 'lib' AND t.media_id = 'm' \
           AND t.term_kind = 'genre' AND t.term_value IN ('drama')",
    );
    println!("protocol term predicate: {text}");
    assert!(
        text.contains("SEARCH t USING PRIMARY KEY"),
        "the term predicate must seek the primary key: {text}"
    );
    assert!(!text.contains("SCAN t"), "must not scan: {text}");
}

/// `query_projection/browse.rs::read_terms` for the protocol table — the reader
/// that keeps `protocol_terms_media` alive. It has no `term_kind`/`term_value`
/// equality, which is why `protocol_terms_value` could not serve it.
#[test]
fn protocol_term_projection_uses_media_index() {
    let (_tmp, conn) = open_library_db();
    let text = plan_text(
        &conn,
        "SELECT term_kind, term_value FROM library_protocol_terms \
         WHERE library_id = 'lib' AND media_id = 'm'",
    );
    println!("protocol term projection: {text}");
    assert!(
        text.contains("protocol_terms_media"),
        "the projection read must use protocol_terms_media: {text}"
    );
}

/// Why `protocol_people_wire` / `protocol_people_tmdb` survive even though the
/// current person predicate does not reach them: they are the only cheap way to
/// resolve a person filter. See the audit's ranked list — the fix is the SQL
/// shape, not another index.
#[test]
fn person_identity_lookup_stays_an_index_seek() {
    let (_tmp, conn) = open_library_db();
    for sql in [
        "SELECT media_id FROM library_protocol_people WHERE wire_id IN ('Actor:1')",
        "SELECT media_id FROM library_protocol_people WHERE tmdb_id IN ('1')",
    ] {
        let text = plan_text(&conn, sql);
        println!("{sql}: {text}");
        assert!(
            text.contains("SEARCH library_protocol_people USING COVERING INDEX"),
            "a person-identity lookup must stay a covering seek: {text}"
        );
    }
}

/// The 13 `library_items` indexes are the largest write-amplification block on
/// the publication path, so each one has to name the sort that needs it
/// (`library_query/items/page.rs:72-80`). This pins the eight browse sorts that
/// v3 never covered; the four protocol indexes are pinned by
/// `library_query_plans.rs`.
#[test]
fn every_browse_sort_has_its_own_index() {
    let (_tmp, conn) = open_library_db();
    let sorts = [
        ("i.title_sort", "items_title"),
        ("i.added_at_sort", "items_added"),
        ("i.release_date_sort", "items_release"),
        ("i.rating_sort", "items_rating"),
        ("i.runtime_sort", "items_runtime"),
        ("i.size_sort", "items_size"),
        ("i.resolution_rank", "items_resolution"),
        ("i.stable_shuffle_key", "items_shuffle"),
    ];
    for (expr, index) in sorts {
        for direction in ["ASC", "DESC"] {
            let sql = format!(
                "SELECT i.media_id FROM library_items i WHERE i.library_id = 'lib' \
                 ORDER BY {expr} {direction}, i.media_id {direction} LIMIT 50 OFFSET 100"
            );
            let text = plan_text(&conn, &sql);
            println!("{expr} {direction}: {text}");
            assert!(
                text.contains(index),
                "ORDER BY {expr} {direction} must use {index}: {text}"
            );
            assert!(
                !text.contains("TEMP B-TREE"),
                "ORDER BY {expr} {direction} must not sort: {text}"
            );
        }
    }
}

/// R1 (`library_item_terms` primary-key reorder + `terms_media` delete) is only
/// allowed to shrink the file, never to slow a read. Every facet dimension runs
/// through the real builder, so this measures the *whole* request work rather
/// than one hand-written statement: the pinned numbers are the pre-reorder
/// baseline, i.e. the change must be byte-cheaper for the same plans.
#[test]
fn facet_dimensions_keep_their_work_after_the_terms_pk_reorder() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let lib = support::seed(&store, MediaKind::Movie, 400);
    let cases = facet_dimension_cases();
    // Pre-reorder baseline, measured on this fixture before the key changed.
    // `statements` / `sort_operations` / `fullscan_steps` are deterministic; the
    // budget is an upper bound so a future improvement does not have to edit it.
    let budget: [(&str, u64, u64, u64); 11] = [
        ("none", 5, 0, 0),
        ("genre", 6, 0, 797),
        ("country", 6, 1, 399),
        ("decade", 6, 0, 0),
        ("runtime", 6, 1, 399),
        ("language", 6, 1, 399),
        ("resolution", 6, 1, 399),
        ("hdr", 6, 0, 0),
        ("rating", 5, 1, 399),
        ("stock", 6, 1, 399),
        ("watch", 5, 1, 1995),
    ];
    let (_, lease) = store.with_stats(store::stats::StatsScope::Library, || {
        store.with_read(store::Db::Library, |_| Ok(())).unwrap()
    });
    // The historical budgets already included BEGIN/ROLLBACK. Add only the
    // newly measured validation overhead, keeping their query budgets intact.
    let validation_statements = lease.statements - 2;
    for ((name, filter), (want, statements, sorts, fullscan)) in cases.into_iter().zip(budget) {
        let statements = statements + validation_statements;
        assert_eq!(name, want);
        let request = FacetRequest {
            library_id: lib,
            user_id: None,
            filter,
            tier: FacetTier::All,
        };
        let (_, work) = store.library_facets_with_stats(&request).unwrap();
        println!(
            "{name:11} statements={:3} sort={:3} fullscan={:6} vm={:8}",
            work.statements, work.sort_operations, work.fullscan_steps, work.vm_steps
        );
        assert!(
            work.statements <= statements
                && work.sort_operations <= sorts
                && work.fullscan_steps <= fullscan,
            "{name}: the terms-key reorder must not cost work: got {work:?}, \
             budget statements<={statements} sort<={sorts} fullscan<={fullscan}"
        );
    }
}

fn facet_dimension_cases() -> Vec<(&'static str, BrowseFilter)> {
    vec![
        ("none", BrowseFilter::default()),
        (
            "genre",
            BrowseFilter {
                genres: vec!["Drama".into()],
                ..Default::default()
            },
        ),
        (
            "country",
            BrowseFilter {
                countries: vec!["US".into()],
                ..Default::default()
            },
        ),
        (
            "decade",
            BrowseFilter {
                decades: vec!["2020s".into()],
                ..Default::default()
            },
        ),
        (
            "runtime",
            BrowseFilter {
                runtimes: vec!["60to90".into()],
                ..Default::default()
            },
        ),
        (
            "language",
            BrowseFilter {
                languages: vec!["zh".into()],
                ..Default::default()
            },
        ),
        (
            "resolution",
            BrowseFilter {
                resolutions: vec!["1080p".into()],
                ..Default::default()
            },
        ),
        (
            "hdr",
            BrowseFilter {
                hdr: Some(false),
                ..Default::default()
            },
        ),
        (
            "rating",
            BrowseFilter {
                min_rating: Some(5.0),
                ..Default::default()
            },
        ),
        (
            "stock",
            BrowseFilter {
                stock: vec!["missing".into()],
                ..Default::default()
            },
        ),
        (
            "watch",
            BrowseFilter {
                watch: Some("unwatched".into()),
                ..Default::default()
            },
        ),
    ]
}
