use super::*;

fn genre(name: &str) -> serde_json::Value {
    serde_json::json!({"source":"fixture","name":name,"filter_key":name})
}

fn nfo(name: &str) -> String {
    serde_json::json!({
        "genres":{"state":"value","value":[genre(name)]},
        "year":{"state":"value","value":2024}
    })
    .to_string()
}

fn adopted(name: &str) -> (String, String) {
    let value = genre(name);
    (
        serde_json::json!([value.clone()]).to_string(),
        serde_json::json!({
            "fields":{"genres":{"state":"value","value":[value]}},
            "padding":"x".repeat(8192)
        })
        .to_string(),
    )
}

fn add_conflicting_metadata(store: &Store, library: LibraryId, size: usize) {
    let mut db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    let tx = db.transaction().unwrap();
    tx.execute(
        "UPDATE library_metadata_inputs SET fields_json=?1 WHERE library_id=?2 AND locale='zh-CN'",
        rusqlite::params![nfo("Common"), library.to_string()],
    )
    .unwrap();
    tx.execute(
        "UPDATE library_metadata_inputs SET fields_json=?1 WHERE library_id=?2 AND locale='zh-CN' AND rowid%100=0",
        rusqlite::params![nfo("Rare"), library.to_string()],
    )
    .unwrap();
    for (locale, genre_name) in [("en-US", "NfoEnglish"), ("ja", "NfoJapanese")] {
        tx.execute(
            "INSERT INTO library_metadata_inputs(library_id,media_id,entity_scope,season,episode,locale,input_key,root_id,path_key,parse_state,source_version,fields_json,present_fields_json,revision)
             SELECT library_id,media_id,entity_scope,season,episode,?1,input_key||?2,root_id,path_key||?2,parse_state,source_version,?3,present_fields_json,revision
             FROM library_metadata_inputs WHERE library_id=?4 AND locale='zh-CN'",
            rusqlite::params![locale,format!(".{locale}"),nfo(genre_name),library.to_string()],
        )
        .unwrap();
    }
    for (locale, genre_name) in [
        ("zh-CN", "AdoptedChinese"),
        ("en-US", "AdoptedEnglish"),
        ("ja", "AdoptedJapanese"),
    ] {
        let (genres_json, details_json) = adopted(genre_name);
        assert!(details_json.len() > 8192);
        tx.execute(
            "INSERT INTO media_metadata(media_id,locale,title,year,genres_json,details_json,revision)
             SELECT media_id,?1,title,2024,?2,?3,1 FROM media WHERE kind='movie'",
            rusqlite::params![locale, genres_json, details_json],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let nfo_count: i64 = db
        .query_row(
            "SELECT count(*) FROM library_metadata_inputs WHERE library_id=?1",
            [library.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    let adopted_count: i64 = db
        .query_row("SELECT count(*) FROM media_metadata", [], |r| r.get(0))
        .unwrap();
    let rare_count: i64 = db
        .query_row(
            "SELECT count(*) FROM library_metadata_inputs WHERE library_id=?1 AND locale='zh-CN' AND fields_json LIKE '%Rare%'",
            [library.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        (nfo_count, adopted_count, rare_count),
        ((size * 3) as i64, (size * 3) as i64, (size / 100) as i64)
    );
    println!(
        "metadata_raw size={size} adopted_8k_rows={adopted_count} nfo_locale_rows={nfo_count} zh_rare_rows={rare_count}"
    );
}

fn browse_request(library: LibraryId, genres: Vec<String>) -> LibraryQuery {
    LibraryQuery {
        library_id: library,
        user_id: None,
        filter: BrowseFilter {
            genres,
            ..Default::default()
        },
        order: BrowseOrder::Title,
        descending: false,
        page: BrowsePage::Offset {
            start: 0,
            limit: 100,
        },
        include_total: true,
    }
}

fn measure_cursor(store: &Store, library: LibraryId, size: usize) {
    let mut request = browse_request(library, vec![]);
    request.page = BrowsePage::Cursor {
        after: None,
        limit: 100,
    };
    request.include_total = false;
    let pages = size / 200;
    for _ in 0..pages {
        let page = store.library_item_page(&request).unwrap();
        request.page = BrowsePage::Cursor {
            after: page.next_cursor,
            limit: 100,
        };
    }
    request.include_total = true;
    measure("web_cursor_deep", size as u64, || {
        let (page, work) = store.library_item_page_with_stats(&request).unwrap();
        assert_eq!(page.items.len(), size.saturating_sub(pages * 100).min(100));
        (page.total.unwrap(), work)
    });
}

pub(super) fn run(size: usize) {
    assert_eq!(size % 100, 0, "1% fixture needs a multiple of 100");
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library = fixed_library(&store, MediaKind::Movie, "axis-metadata");
    support::seed_into(
        &store,
        MediaKind::Movie,
        0,
        size,
        library,
        "/query-work/movie",
    );
    let started = Instant::now();
    add_conflicting_metadata(&store, library, size);
    repair(&store, library, "metadata");
    println!(
        "metadata_repair_ms={:.3}",
        started.elapsed().as_secs_f64() * 1_000.
    );
    db_sizes(&store, "metadata_8k_three_locale");
    let rare = browse_request(library, vec!["Rare".into()]);
    measure("genre_one_percent_work", (size / 100) as u64, || {
        let (page, work) = store.library_item_page_with_stats(&rare).unwrap();
        (page.total.unwrap(), work)
    });
    let facets = FacetRequest {
        library_id: library,
        user_id: None,
        filter: rare.filter.clone(),
        tier: FacetTier::All,
    };
    measure("genre_one_percent_facets", (size / 100) as u64, || {
        let (page, work) = store.library_facets_with_stats(&facets).unwrap();
        (page.total, work)
    });
    let mut protocol = support::protocol();
    protocol.parent = ProtocolParent::Library(library);
    protocol.limit = 100;
    protocol.sort.clear();
    measure("protocol_empty_sort_default", size as u64, || {
        let (page, work) = store.protocol_item_page_with_stats(&protocol).unwrap();
        (page.total, work)
    });
    protocol.sort = vec![
        ProtocolSort {
            key: ProtocolSortKey::DateCreated,
            descending: true,
        },
        ProtocolSort {
            key: ProtocolSortKey::Name,
            descending: true,
        },
    ];
    measure("protocol_api_default_explicit", size as u64, || {
        let (page, work) = store.protocol_item_page_with_stats(&protocol).unwrap();
        (page.total, work)
    });
    measure_cursor(&store, library, size);
}
