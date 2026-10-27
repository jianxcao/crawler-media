use super::*;

fn set_cast(store: &Store, library: LibraryId, people: usize) {
    let cast = (0..people)
        .map(|index| serde_json::json!({"name":format!("Actor {index}")}))
        .collect::<Vec<_>>();
    let field = serde_json::json!({"state":"value","value":cast}).to_string();
    let db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    db.execute(
        "UPDATE library_metadata_inputs SET fields_json=json_set(fields_json,'$.cast',json(?1)), present_fields_json='[\"genres\",\"year\",\"cast\"]' WHERE library_id=?2",
        rusqlite::params![field, library.to_string()],
    )
    .unwrap();
    drop(db);
    repair(store, library, &format!("cast-{people}"));
}

fn measure_cast(store: &Store, library: LibraryId, size: usize, people: usize) {
    let db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    let rows: i64 = db
        .query_row(
            "SELECT count(*) FROM library_protocol_people WHERE library_id=?1 AND role='Actor'",
            [library.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, (size * people) as i64);
    drop(db);
    let mut query = support::protocol();
    query.parent = ProtocolParent::Library(library);
    query.limit = 100;
    measure(&format!("actors_{people}_protocol"), size as u64, || {
        let (page, work) = store.protocol_item_page_with_stats(&query).unwrap();
        (page.total, work)
    });
    if people > 0 {
        query.person_ids =
            vec![uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"name:actor 0:Actor").to_string()];
        measure(
            &format!("actors_{people}_person_filter"),
            size as u64,
            || {
                let (page, work) = store.protocol_item_page_with_stats(&query).unwrap();
                (page.total, work)
            },
        );
    }
    db_sizes(store, &format!("actors_{people}"));
}

pub(super) fn run(size: usize) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library = fixed_library(&store, MediaKind::Movie, "axis-actors");
    support::seed_into(
        &store,
        MediaKind::Movie,
        0,
        size,
        library,
        "/query-work/movie",
    );
    measure_cast(&store, library, size, 0);
    for people in [20, 100] {
        let started = Instant::now();
        set_cast(&store, library, people);
        println!(
            "actors_{people}_repair_ms={:.3}",
            started.elapsed().as_secs_f64() * 1_000.
        );
        measure_cast(&store, library, size, people);
    }
}
