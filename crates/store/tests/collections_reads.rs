//! Proposed public seam: collection_items_page(user,id,libraries,limit,cursor)
//! -> {items:[{media_id,library_id,..}],total,next_cursor}.
#[path = "support/task10_fixture.rs"]
mod fixture;
use domain::UnitKey;
use fixture::Fixture;
use store::stats::StatsScope;
#[test]
fn collection_items_keyset_keeps_visible_total_and_member_order() {
    let f = Fixture::new();
    let a = f.library("a");
    let b = f.library("b");
    let collection = f.store.create_collection(f.user, "Ordered").unwrap();
    let mut expected = vec![];
    for n in 1..=7 {
        let id = f.media(n);
        f.publish(
            if n == 1 { b } else { a },
            id,
            &format!("{n}.mkv"),
            &[UnitKey::WHOLE],
        );
        f.store
            .add_collection_item(&collection, &id.to_string())
            .unwrap();
        if n != 1 {
            expected.push(id);
        }
    }
    // Duplicate physical ownership must not duplicate a logical member.
    f.publish(b, expected[0], "duplicate.mkv", &[UnitKey::WHOLE]);
    let mut cursor = None;
    let mut actual = vec![];
    loop {
        let page = f
            .store
            .collection_items_page(f.user, &collection, &[a], 2, cursor.as_deref())
            .unwrap();
        assert_eq!(page.total, 6);
        assert!(page.items.len() <= 2);
        assert!(page.items.iter().all(|row| row.library_id == a));
        actual.extend(page.items.into_iter().map(|r| r.media.id));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(actual, expected);
    let first = f
        .store
        .collection_items_page(f.user, &collection, &[a], 2, None)
        .unwrap();
    let other = f.store.create_collection(f.user, "Other").unwrap();
    assert!(
        f.store
            .collection_items_page(f.user, &other, &[a], 2, first.next_cursor.as_deref())
            .is_err(),
        "a cursor is scoped to its collection"
    );
    assert!(
        f.store
            .collection_items_page(domain::UserId::new(), &collection, &[a], 2, None)
            .is_err()
    );
    assert!(
        f.store
            .collection_items_page(f.user, &collection, &[a], 2, Some("broken"))
            .is_err()
    );
}
#[test]
fn collection_page_statement_count_is_constant_as_members_grow() {
    let f = Fixture::new();
    let lib = f.library("a");
    let collection = f.store.create_collection(f.user, "Scale").unwrap();
    for n in 1..=5 {
        let id = f.media(n);
        f.publish(lib, id, &format!("{n}.mkv"), &[UnitKey::WHOLE]);
        f.store
            .add_collection_item(&collection, &id.to_string())
            .unwrap();
    }
    let measure = |total| {
        f.store.with_stats(StatsScope::Library, || {
            let page = f
                .store
                .collection_items_page(f.user, &collection, &[lib], 5, None)
                .unwrap();
            assert_eq!(page.total, total);
            assert_eq!(page.items.len(), 5);
        })
    };
    let (_, small) = measure(5);
    for n in 6..=50 {
        let id = f.media(n);
        f.publish(lib, id, &format!("{n}.mkv"), &[UnitKey::WHOLE]);
        f.store
            .add_collection_item(&collection, &id.to_string())
            .unwrap();
    }
    let (_, large) = measure(50);
    assert!(small.statements > 0 && large.statements > 0);
    assert_eq!(small.statements, large.statements, "{small:?} -> {large:?}");
    // Exact visible total may inspect all candidates. No false O(page) VM claim.
    assert!(large.hydrated_rows <= 5, "{large:?}");
}

#[test]
fn collection_page_keyset_uses_index_order_without_large_scan_or_sort() {
    let f = Fixture::new();
    let lib = f.library("a");
    let collection = f.store.create_collection(f.user, "Plan").unwrap();
    for n in 1..=12 {
        let id = f.media(n);
        f.publish(lib, id, &format!("{n}.mkv"), &[UnitKey::WHOLE]);
        f.store
            .add_collection_item(&collection, &id.to_string())
            .unwrap();
    }
    let first = f
        .store
        .collection_items_page(f.user, &collection, &[lib], 2, None)
        .unwrap();
    for cursor in [None, first.next_cursor.as_deref()] {
        let plans = f
            .store
            .collection_items_page_query_plans(f.user, &collection, &[lib], 2, cursor)
            .unwrap();
        assert!(
            !plans.iter().any(|p| p.contains("TEMP B-TREE")),
            "{plans:?}"
        );
        assert!(
            !plans
                .iter()
                .any(|p| p.starts_with("SCAN ci") || p.starts_with("SCAN f ") || p == "SCAN f"),
            "{plans:?}"
        );
    }
}

#[test]
fn collection_reads_reject_removed_owners_even_with_a_stale_scope() {
    let f = Fixture::new();
    let a = f.library("a");
    let b = f.library("b");
    let collection = f.store.create_collection(f.user, "Owners").unwrap();
    let only_a = f.media(1);
    let shared = f.media(2);
    f.publish(a, only_a, "only.mkv", &[UnitKey::WHOLE]);
    f.publish(a, shared, "shared.mkv", &[UnitKey::WHOLE]);
    f.publish(b, shared, "shared.mkv", &[UnitKey::WHOLE]);
    for media in [only_a, shared] {
        f.store
            .add_collection_item(&collection, &media.to_string())
            .unwrap();
    }
    assert_eq!(
        f.store
            .collection_items_page(f.user, &collection, &[a, b], 5, None)
            .unwrap()
            .total,
        2
    );
    f.store.begin_library_removal(&a.to_string()).unwrap();
    let page = f
        .store
        .collection_items_page(f.user, &collection, &[a, b], 5, None)
        .unwrap();
    assert_eq!(
        page.total, 1,
        "hidden owners must be filtered before total and page"
    );
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].media.id, shared);
    assert_eq!(page.items[0].library_id, b);
    assert!(
        page.items[0]
            .coverages
            .iter()
            .all(|c| c.file.library_id == b)
    );
    let all = f
        .store
        .collection_items_all(f.user, &collection, &[a, b])
        .unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].library_id, b);
    let summaries = f.store.collection_summaries(f.user, &[a, b]).unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(summaries[0].items.len(), 1);
    assert_eq!(summaries[0].items[0].library_id, b);
}

#[test]
fn collection_milliseconds_and_ties_page_exactly_and_reject_text_cursor() {
    let f = Fixture::new();
    let lib = f.library("milliseconds");
    let collection = f.store.create_collection(f.user, "Milliseconds").unwrap();
    let db = rusqlite::Connection::open(f.dir.path().join("data/library.db")).unwrap();
    let created: rusqlite::types::Value = db
        .query_row(
            "SELECT created_at FROM collections WHERE id=?1",
            [&collection],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        matches!(created, rusqlite::types::Value::Integer(n) if n>1_700_000_000_000),
        "{created:?}"
    );
    let mut expected = Vec::new();
    for (n, ms) in [(4, 999), (3, 2), (2, 2), (1, 1)] {
        let id = f.media(n);
        f.publish(lib, id, &format!("{n}.mkv"), &[UnitKey::WHOLE]);
        f.store
            .add_collection_item(&collection, &id.to_string())
            .unwrap();
        let added: rusqlite::types::Value = db
            .query_row(
                "SELECT added_at FROM collection_items WHERE collection_id=?1 AND media_id=?2",
                rusqlite::params![collection, id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            matches!(added, rusqlite::types::Value::Integer(n) if n>1_700_000_000_000),
            "{added:?}"
        );
        db.execute("UPDATE collection_items SET sort_order=0,added_at=?3 WHERE collection_id=?1 AND media_id=?2",rusqlite::params![collection,id.to_string(),1_800_000_000_000_i64+ms]).unwrap();
        expected.push(id);
    }
    expected.reverse();
    let first = f
        .store
        .collection_items_page(f.user, &collection, &[lib], 1, None)
        .unwrap();
    let cursor = first.next_cursor.unwrap();
    let mut old: serde_json::Value = serde_json::from_str(&cursor).unwrap();
    old["added"] = serde_json::json!("2027-01-15T08:00:00.001Z");
    assert!(
        f.store
            .collection_items_page(f.user, &collection, &[lib], 1, Some(&old.to_string()))
            .is_err()
    );
    assert!(
        f.store
            .collection_items_page(f.user, &collection, &[], 1, Some(&cursor))
            .is_err()
    );
    let mut actual = vec![];
    let mut cursor = None;
    loop {
        let page = f
            .store
            .collection_items_page(f.user, &collection, &[lib], 1, cursor.as_deref())
            .unwrap();
        actual.extend(page.items.into_iter().map(|r| r.media.id));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(actual, expected);
    assert_eq!(
        f.store.collection_item_ids(&collection).unwrap(),
        expected.iter().map(ToString::to_string).collect::<Vec<_>>()
    );
}
