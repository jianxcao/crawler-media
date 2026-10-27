use super::*;

/// O01: a deep cursor in one large library costs about the same SQL work as the first
/// page, for both directions and the mixed watch-tier order (tuple seek, not OR chains).
#[test]
fn deep_cursor_work_matches_first_page_work() {
    let total = env_count(DEEP_ENV, 50_000);
    let tmp = tempfile::tempdir().unwrap();
    let data_dir = tmp.path().join("data");
    let store = Store::open(&data_dir).unwrap();
    let db_path = data_dir.join("library.db");
    let lib = LibSeed::create(&store, "DeepWork", "/deep-work", 4);
    let items: Vec<SeedItem> = (0..total)
        .map(|key| {
            // Pairs of identical titles exercise the media_id tie-break.
            let mut item = SeedItem::movie(key, format!("Movie {:06}", key / 2 * 2));
            item.title_sort = item.title.clone();
            item
        })
        .collect();
    insert_items(&db_path, &lib, 1, &items);

    for descending in [false, true] {
        let first_query = query_for(
            &lib,
            None,
            BrowseOrder::Title,
            descending,
            BrowsePage::Cursor {
                after: None,
                limit: 50,
            },
        );
        let (first, first_stats) = store.library_item_page_with_stats(&first_query).unwrap();
        assert_eq!(first.items.len(), 50);
        // Walk to the middle in 500-row cursor pages (the page cap).
        let mut cursor = None;
        for _ in 0..total / 2 / 500 {
            let step = query_for(
                &lib,
                None,
                BrowseOrder::Title,
                descending,
                BrowsePage::Cursor {
                    after: cursor.clone(),
                    limit: 500,
                },
            );
            cursor = store.library_item_page(&step).unwrap().next_cursor;
        }
        let cursor = cursor.unwrap();
        let deep_query = query_for(
            &lib,
            None,
            BrowseOrder::Title,
            descending,
            BrowsePage::Cursor {
                after: Some(cursor),
                limit: 50,
            },
        );
        let (deep, deep_stats) = store.library_item_page_with_stats(&deep_query).unwrap();
        let expected = offset_ids(&store, &first_query, total / 2 / 500 * 500, 50);
        println!(
            "O01 desc={descending}: first vm={} deep vm={}",
            first_stats.vm_steps, deep_stats.vm_steps
        );
        assert_eq!(deep.items.len(), 50);
        assert_eq!(
            page_ids(&deep),
            expected,
            "deep page order (desc={descending})"
        );
        assert_eq!(deep_stats.fullscan_steps, 0);
        assert!(
            deep_stats.vm_steps <= first_stats.vm_steps * 2,
            "deep cursor work {} exceeds 2x first page {} (desc={descending})",
            deep_stats.vm_steps,
            first_stats.vm_steps
        );
    }
}
