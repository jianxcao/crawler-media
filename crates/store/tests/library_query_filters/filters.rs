use super::*;

#[test]
fn filters_restrict_items_and_total_agrees() {
    let (_tmp, store, library, _scope) = seeded();
    let library_id = library.id.parse().unwrap();

    let all = store
        .library_item_page(&query(
            library_id,
            BrowseFilter::default(),
            BrowseOrder::Title,
            false,
            offset(0, 50),
        ))
        .unwrap();
    assert_eq!(all.total, Some(3));
    assert_eq!(all.items.len(), 3);

    // rating_gte over NULL-rated items returns nothing (Charlie has no adopted rating).
    let rated = store
        .library_item_page(&query(
            library_id,
            BrowseFilter {
                min_rating: Some(9.0),
                ..BrowseFilter::default()
            },
            BrowseOrder::Title,
            false,
            offset(0, 50),
        ))
        .unwrap();
    assert_eq!(rated.total, Some(1));
    assert_eq!(
        rated
            .items
            .iter()
            .map(|item| item.title.as_str())
            .collect::<Vec<_>>(),
        ["Alpha"]
    );

    // rating_gte=9 with a 9.5 cut still finds Alpha; 9.6 finds nobody.
    let strict = store
        .library_item_page(&query(
            library_id,
            BrowseFilter {
                min_rating: Some(9.6),
                ..BrowseFilter::default()
            },
            BrowseOrder::Title,
            false,
            offset(0, 50),
        ))
        .unwrap();
    assert_eq!(strict.total, Some(0));
    assert!(strict.items.is_empty());

    // Every dimension restricts, and the page count always matches the total.
    let cases = classification_filter_cases()
        .into_iter()
        .chain(date_runtime_filter_cases());
    for (name, filter, expected) in cases {
        let page = store
            .library_item_page(&query(
                library_id,
                filter,
                BrowseOrder::Title,
                false,
                offset(0, 50),
            ))
            .unwrap();
        let titles = page
            .items
            .iter()
            .map(|item| item.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(titles, expected, "dimension {name} returned {titles:?}");
        assert_eq!(
            page.total,
            Some(expected.len() as u64),
            "dimension {name} total"
        );
    }

    // A filter that matches nothing still agrees between page and total.
    let none = store
        .library_item_page(&query(
            library_id,
            BrowseFilter {
                genres: vec!["999".into()],
                ..BrowseFilter::default()
            },
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
        BrowseFilter {
            decades: vec!["2030s".into()],
            ..BrowseFilter::default()
        },
        BrowseFilter {
            runtimes: vec!["lte0".into()],
            ..BrowseFilter::default()
        },
        BrowseFilter {
            stock: vec!["gone".into()],
            ..BrowseFilter::default()
        },
        BrowseFilter {
            watch: Some("binged".into()),
            ..BrowseFilter::default()
        },
        BrowseFilter {
            identity: Some("maybe".into()),
            ..BrowseFilter::default()
        },
        BrowseFilter {
            min_rating: Some(11.0),
            ..BrowseFilter::default()
        },
    ] {
        let error = store
            .library_item_page(&query(
                library_id,
                filter.clone(),
                BrowseOrder::Title,
                false,
                offset(0, 10),
            ))
            .expect_err("filter must be rejected");
        assert!(
            matches!(error, store::StoreError::Invalid(_)),
            "got {error:?}"
        );
    }
}

#[test]
fn cursor_pages_round_trip_without_gaps() {
    let (_tmp, store, library, _scope) = seeded();
    let library_id = library.id.parse().unwrap();

    let offset_order = store
        .library_item_page(&query(
            library_id,
            BrowseFilter::default(),
            BrowseOrder::Title,
            false,
            offset(0, 50),
        ))
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
                BrowsePage::Cursor {
                    after: after.clone(),
                    limit: 2,
                },
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
                BrowsePage::Cursor {
                    after: Some(cursor.to_string()),
                    limit: 2,
                },
            ))
            .expect_err("cursor must be rejected");
        assert!(
            matches!(error, store::StoreError::Invalid(_)),
            "got {error:?}"
        );
    }
}

#[test]
fn cursor_seek_reflects_new_items_after_the_last_key() {
    let (_tmp, store, library, scope) = seeded();
    let mut q = query(
        library.id.parse().unwrap(),
        BrowseFilter::default(),
        BrowseOrder::Title,
        false,
        BrowsePage::Cursor {
            after: None,
            limit: 1,
        },
    );
    let first = store.library_item_page(&q).unwrap();
    assert_eq!(first.items[0].title, "Alpha");
    let root = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|root| root.library_id == library.id)
        .unwrap();
    let entries = [
        (
            "00000000-0000-0000-0000-0000000000d4",
            "Aardvark",
            "00000000-0000-0000-0001-0000000000d4",
            "Aardvark.mkv",
        ),
        (
            "00000000-0000-0000-0000-0000000000e5",
            "Baker",
            "00000000-0000-0000-0001-0000000000e5",
            "Baker.mkv",
        ),
    ];
    let mut files = Vec::new();
    for (media_id, title, file_id, path) in entries {
        let media = create_media(&store, media_id, title, MediaKind::Movie, None, None);
        files.push((
            make_file(
                &library,
                &root.id,
                media.id,
                FileShape {
                    file_id,
                    path,
                    resolution: None,
                    hdr: None,
                    presence: FilePresence::Present,
                },
            ),
            vec![UnitKey::WHOLE],
        ));
    }
    publish(&store, &scope, files);
    q.page = BrowsePage::Cursor {
        after: first.next_cursor,
        limit: 10,
    };
    let titles: Vec<_> = store
        .library_item_page(&q)
        .unwrap()
        .items
        .into_iter()
        .map(|item| item.title)
        .collect();
    assert_eq!(titles, ["Baker", "Bravo", "Charlie"]);
}

#[test]
fn cursor_is_bound_to_query_scope_and_valid_key_types() {
    let (_tmp, store, library, _scope) = seeded();
    let library_id = library.id.parse().unwrap();
    let (other, _) = create_library(&store, "Other", MediaKind::Movie, "/other");
    let mut base = query(
        library_id,
        BrowseFilter::default(),
        BrowseOrder::Title,
        false,
        BrowsePage::Cursor {
            after: None,
            limit: 1,
        },
    );
    let cursor = store.library_item_page(&base).unwrap().next_cursor.unwrap();
    base.page = BrowsePage::Cursor {
        after: Some(cursor.clone()),
        limit: 1,
    };
    assert_eq!(
        store.library_item_page(&base).unwrap().items[0].title,
        "Bravo"
    );

    let mut different = Vec::new();
    let mut changed = base.clone();
    changed.library_id = other.id.parse().unwrap();
    different.push(changed);
    let mut changed = base.clone();
    changed.user_id = Some("00000000-0000-0000-0000-000000000001".parse().unwrap());
    different.push(changed);
    let mut changed = base.clone();
    changed.filter.require_image = true;
    different.push(changed);
    let mut changed = base.clone();
    changed.order = BrowseOrder::Rating;
    different.push(changed);
    let mut changed = base.clone();
    changed.descending = true;
    different.push(changed);
    for changed in different {
        assert!(matches!(
            store.library_item_page(&changed),
            Err(store::StoreError::Invalid(_))
        ));
    }

    let mut damaged: serde_json::Value = serde_json::from_str(&cursor).unwrap();
    damaged["keys"][0] = serde_json::json!(42);
    let mut missing_version: serde_json::Value = serde_json::from_str(&cursor).unwrap();
    missing_version.as_object_mut().unwrap().remove("version");
    let mut missing_scope: serde_json::Value = serde_json::from_str(&cursor).unwrap();
    missing_scope.as_object_mut().unwrap().remove("scope");
    let mut invalid_id: serde_json::Value = serde_json::from_str(&cursor).unwrap();
    invalid_id["media_id"] = serde_json::json!("invalid");
    for raw in [
        damaged.to_string(),
        missing_version.to_string(),
        missing_scope.to_string(),
        invalid_id.to_string(),
        "{\"keys\":[\"alpha\"],\"media_id\":\"x\"}".into(),
    ] {
        let mut changed = base.clone();
        changed.page = BrowsePage::Cursor {
            after: Some(raw),
            limit: 1,
        };
        assert!(matches!(
            store.library_item_page(&changed),
            Err(store::StoreError::Invalid(_))
        ));
    }
}

#[test]
fn item_index_cursor_uses_the_same_scope_as_pages() {
    let (_tmp, store, library, _scope) = seeded();
    let mut q = query(
        library.id.parse().unwrap(),
        BrowseFilter::default(),
        BrowseOrder::Title,
        false,
        BrowsePage::Cursor {
            after: None,
            limit: 1,
        },
    );
    let buckets = store.library_item_index(&q).unwrap();
    let cursor = buckets
        .iter()
        .find(|bucket| bucket.initial == "B")
        .unwrap()
        .cursor
        .clone();
    q.page = BrowsePage::Cursor {
        after: cursor,
        limit: 1,
    };
    assert_eq!(store.library_item_page(&q).unwrap().items[0].title, "Bravo");
}

#[test]
fn cursor_accepts_equivalent_filter_order_and_case() {
    let (_tmp, store, library, _scope) = seeded();
    let mut q = query(
        library.id.parse().unwrap(),
        BrowseFilter {
            languages: vec!["EN".into(), "ja".into()],
            ..BrowseFilter::default()
        },
        BrowseOrder::Title,
        false,
        BrowsePage::Cursor {
            after: None,
            limit: 1,
        },
    );
    let cursor = store.library_item_page(&q).unwrap().next_cursor.unwrap();
    q.filter.languages = vec!["JA".into(), "en".into(), "EN".into()];
    q.page = BrowsePage::Cursor {
        after: Some(cursor),
        limit: 1,
    };
    assert!(store.library_item_page(&q).is_ok());
}

#[test]
fn unwatched_includes_media_without_a_user_row() {
    let (_tmp, store, library, _scope) = seeded();
    let library_id = library.id.parse().unwrap();

    let cases = [
        ("unwatched", 3usize),
        ("played", 0),
        ("watching", 0),
        ("favorite", 0),
        ("seen", 0),
    ];
    for (watch, expected) in cases {
        let mut request = query(
            library_id,
            BrowseFilter {
                watch: Some(watch.into()),
                ..BrowseFilter::default()
            },
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

#[test]
fn old_watch_values_are_rejected_instead_of_normalized() {
    let (_tmp, store, library, _scope) = seeded();
    for watch in ["watched", "unplayed", "unseen"] {
        let request = query(
            library.id.parse().unwrap(),
            BrowseFilter {
                watch: Some(watch.into()),
                ..BrowseFilter::default()
            },
            BrowseOrder::Title,
            false,
            offset(0, 50),
        );
        assert!(
            matches!(
                store.library_item_page(&request),
                Err(store::StoreError::Invalid(_))
            ),
            "watch={watch}"
        );
    }
}

fn classification_filter_cases() -> Vec<(&'static str, BrowseFilter, Vec<&'static str>)> {
    vec![
        (
            "genre",
            BrowseFilter {
                genres: vec!["28".into()],
                ..BrowseFilter::default()
            },
            vec!["Alpha"],
        ),
        (
            "resolution",
            BrowseFilter {
                resolutions: vec!["720p".into()],
                ..BrowseFilter::default()
            },
            vec!["Charlie"],
        ),
        (
            "language",
            BrowseFilter {
                languages: vec!["JA".into()],
                ..BrowseFilter::default()
            },
            vec!["Bravo"],
        ),
        (
            "hdr",
            BrowseFilter {
                hdr: Some(true),
                ..BrowseFilter::default()
            },
            vec!["Alpha"],
        ),
        (
            "missing",
            BrowseFilter {
                stock: vec!["missing".into()],
                ..BrowseFilter::default()
            },
            vec!["Charlie"],
        ),
        (
            "unscraped",
            BrowseFilter {
                stock: vec!["unscraped".into()],
                ..BrowseFilter::default()
            },
            vec!["Charlie"],
        ),
        (
            "confirmed",
            BrowseFilter {
                identity: Some("confirmed".into()),
                ..BrowseFilter::default()
            },
            vec!["Alpha", "Bravo"],
        ),
        (
            "provisional",
            BrowseFilter {
                identity: Some("provisional".into()),
                ..BrowseFilter::default()
            },
            vec!["Charlie"],
        ),
    ]
}

fn date_runtime_filter_cases() -> Vec<(&'static str, BrowseFilter, Vec<&'static str>)> {
    vec![
        (
            "decade",
            BrowseFilter {
                decades: vec!["2020s".into()],
                ..BrowseFilter::default()
            },
            vec!["Alpha"],
        ),
        (
            "earlier",
            BrowseFilter {
                decades: vec!["earlier".into()],
                ..BrowseFilter::default()
            },
            vec!["Charlie"],
        ),
        (
            "2010s",
            BrowseFilter {
                decades: vec!["2010s".into()],
                ..BrowseFilter::default()
            },
            vec!["Bravo"],
        ),
        (
            "empty decade bucket",
            BrowseFilter {
                decades: vec!["1990s".into()],
                ..BrowseFilter::default()
            },
            vec![],
        ),
        (
            "runtime",
            BrowseFilter {
                runtimes: vec!["gt120".into()],
                ..BrowseFilter::default()
            },
            vec!["Alpha"],
        ),
    ]
}
