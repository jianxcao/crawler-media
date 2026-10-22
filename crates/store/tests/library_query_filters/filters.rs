use super::*;

#[test]
fn filters_restrict_items_and_total_agrees() {
    let (_tmp, store, library, _scope) = seeded();
    let library_id = library.id.parse().unwrap();

    let all = store
        .library_item_page(&query(library_id, BrowseFilter::default(), BrowseOrder::Title, false, offset(0, 50)))
        .unwrap();
    assert_eq!(all.total, Some(3));
    assert_eq!(all.items.len(), 3);

    // rating_gte over NULL-rated items returns nothing (Charlie has no adopted rating).
    let rated = store
        .library_item_page(&query(
            library_id,
            BrowseFilter { min_rating: Some(9.0), ..BrowseFilter::default() },
            BrowseOrder::Title,
            false,
            offset(0, 50),
        ))
        .unwrap();
    assert_eq!(rated.total, Some(1));
    assert_eq!(rated.items.iter().map(|item| item.title.as_str()).collect::<Vec<_>>(), ["Alpha"]);

    // rating_gte=9 with a 9.5 cut still finds Alpha; 9.6 finds nobody.
    let strict = store
        .library_item_page(&query(
            library_id,
            BrowseFilter { min_rating: Some(9.6), ..BrowseFilter::default() },
            BrowseOrder::Title,
            false,
            offset(0, 50),
        ))
        .unwrap();
    assert_eq!(strict.total, Some(0));
    assert!(strict.items.is_empty());

    // Every dimension restricts, and the page count always matches the total.
    let cases: Vec<(&str, BrowseFilter, Vec<&str>)> = vec![
        (
            "genre",
            BrowseFilter { genres: vec!["28".into()], ..BrowseFilter::default() },
            vec!["Alpha"],
        ),
        (
            "resolution",
            BrowseFilter { resolutions: vec!["720p".into()], ..BrowseFilter::default() },
            vec!["Charlie"],
        ),
        (
            "decade",
            BrowseFilter { decades: vec!["2020s".into()], ..BrowseFilter::default() },
            vec!["Alpha"],
        ),
        (
            "earlier",
            BrowseFilter { decades: vec!["earlier".into()], ..BrowseFilter::default() },
            vec!["Charlie"],
        ),
        (
            "2010s",
            BrowseFilter { decades: vec!["2010s".into()], ..BrowseFilter::default() },
            vec!["Bravo"],
        ),
        (
            "empty decade bucket",
            BrowseFilter { decades: vec!["1990s".into()], ..BrowseFilter::default() },
            vec![],
        ),
        (
            "runtime",
            BrowseFilter { runtimes: vec!["gt120".into()], ..BrowseFilter::default() },
            vec!["Alpha"],
        ),
        (
            "language",
            BrowseFilter { languages: vec!["JA".into()], ..BrowseFilter::default() },
            vec!["Bravo"],
        ),
        (
            "hdr",
            BrowseFilter { hdr: Some(true), ..BrowseFilter::default() },
            vec!["Alpha"],
        ),
        (
            "missing",
            BrowseFilter { stock: vec!["missing".into()], ..BrowseFilter::default() },
            vec!["Charlie"],
        ),
        (
            "unscraped",
            BrowseFilter { stock: vec!["unscraped".into()], ..BrowseFilter::default() },
            vec!["Charlie"],
        ),
        (
            "confirmed",
            BrowseFilter { identity: Some("confirmed".into()), ..BrowseFilter::default() },
            vec!["Alpha", "Bravo"],
        ),
        (
            "provisional",
            BrowseFilter { identity: Some("provisional".into()), ..BrowseFilter::default() },
            vec!["Charlie"],
        ),
    ];
    for (name, filter, expected) in cases {
        let page = store
            .library_item_page(&query(library_id, filter, BrowseOrder::Title, false, offset(0, 50)))
            .unwrap();
        let titles = page.items.iter().map(|item| item.title.as_str()).collect::<Vec<_>>();
        assert_eq!(titles, expected, "dimension {name} returned {titles:?}");
        assert_eq!(page.total, Some(expected.len() as u64), "dimension {name} total");
    }

    // A filter that matches nothing still agrees between page and total.
    let none = store
        .library_item_page(&query(
            library_id,
            BrowseFilter { genres: vec!["999".into()], ..BrowseFilter::default() },
            BrowseOrder::Title,
            false,
            offset(0, 50),
        ))
        .unwrap();
    assert_eq!(none.total, Some(0));
    assert!(none.items.is_empty());
}

#[test]
fn invalid_filter_values_are_rejected() {
    let (_tmp, store, library, _scope) = seeded();
    let library_id = library.id.parse().unwrap();
    for filter in [
        BrowseFilter { decades: vec!["2030s".into()], ..BrowseFilter::default() },
        BrowseFilter { runtimes: vec!["lte0".into()], ..BrowseFilter::default() },
        BrowseFilter { stock: vec!["gone".into()], ..BrowseFilter::default() },
        BrowseFilter { watch: Some("binged".into()), ..BrowseFilter::default() },
        BrowseFilter { identity: Some("maybe".into()), ..BrowseFilter::default() },
        BrowseFilter { min_rating: Some(11.0), ..BrowseFilter::default() },
    ] {
        let error = store
            .library_item_page(&query(library_id, filter.clone(), BrowseOrder::Title, false, offset(0, 10)))
            .expect_err("filter must be rejected");
        assert!(matches!(error, store::StoreError::Invalid(_)), "got {error:?}");
    }
}

#[test]
fn cursor_pages_round_trip_without_gaps() {
    let (_tmp, store, library, _scope) = seeded();
    let library_id = library.id.parse().unwrap();

    let offset_order = store
        .library_item_page(&query(library_id, BrowseFilter::default(), BrowseOrder::Title, false, offset(0, 50)))
        .unwrap()
        .items
        .iter()
        .map(|item| item.title.clone())
        .collect::<Vec<_>>();
    assert_eq!(offset_order, ["Alpha", "Bravo", "Charlie"]);

    let mut seen = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = store
            .library_item_page(&query(
                library_id,
                BrowseFilter::default(),
                BrowseOrder::Title,
                false,
                BrowsePage::Cursor { after: after.clone(), limit: 2 },
            ))
            .unwrap();
        for item in &page.items {
            seen.push(item.title.clone());
        }
        match page.next_cursor {
            Some(cursor) => after = Some(cursor),
            None => break,
        }
        assert!(seen.len() <= 3, "cursor walk did not terminate");
    }
    assert_eq!(seen, offset_order);

    // The same walk in the other direction follows the requested direction exactly.
    let mut descending = Vec::new();
    let mut after = None;
    loop {
        let page = store
            .library_item_page(&query(
                library_id,
                BrowseFilter::default(),
                BrowseOrder::Title,
                true,
                BrowsePage::Cursor { after, limit: 2 },
            ))
            .unwrap();
        descending.extend(page.items.iter().map(|item| item.title.clone()));
        match page.next_cursor {
            Some(cursor) => after = Some(cursor),
            None => break,
        }
    }
    assert_eq!(descending, ["Charlie", "Bravo", "Alpha"]);

    // A cursor that does not parse, or a well-formed one with the wrong shape, is an error.
    for cursor in ["not json", "{\"keys\":[],\"media_id\":\"nope\"}", "{}"] {
        let error = store
            .library_item_page(&query(
                library_id,
                BrowseFilter::default(),
                BrowseOrder::Title,
                false,
                BrowsePage::Cursor { after: Some(cursor.to_string()), limit: 2 },
            ))
            .expect_err("cursor must be rejected");
        assert!(matches!(error, store::StoreError::Invalid(_)), "got {error:?}");
    }
}

#[test]
fn unwatched_includes_media_without_a_user_row() {
    let (_tmp, store, library, _scope) = seeded();
    let library_id = library.id.parse().unwrap();

    let cases = [
        ("unwatched", 3usize),
        ("unseen", 3),
        ("played", 0),
        ("watching", 0),
        ("favorite", 0),
        ("seen", 0),
    ];
    for (watch, expected) in cases {
        let mut request = query(
            library_id,
            BrowseFilter { watch: Some(watch.into()), ..BrowseFilter::default() },
            BrowseOrder::Title,
            false,
            offset(0, 50),
        );
        // Signed-in viewers with no `user_library_items` row behave exactly like signed-out
        // ones: the LEFT JOIN yields NULL and every watch predicate reads it as "not seen".
        request.user_id = Some(UserId::new());
        let page = store.library_item_page(&request).unwrap();
        assert_eq!(page.total, Some(expected as u64), "watch={watch}");
    }
}

