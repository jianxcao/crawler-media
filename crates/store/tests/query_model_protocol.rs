mod query_model_support;
use domain::{ProtocolItemQuery, ProtocolItemType, ProtocolParent, UserId};
use query_model_support::setup_count;

fn query(library: domain::LibraryId) -> ProtocolItemQuery {
    ProtocolItemQuery {
        user_id: UserId::new(),
        parent: ProtocolParent::Library(library),
        series_id: None,
        season: None,
        person_ids: vec![],
        studio_ids: vec![],
        search_term: None,
        genres: vec!["Drama".into()],
        genre_ids: vec![],
        filters: vec![],
        is_favorite: None,
        is_played: None,
        include_types: vec![ProtocolItemType::Movie],
        recursive: true,
        is_folder: None,
        sort: vec![],
        offset: 0,
        limit: 10,
    }
}

#[test]
fn effective_locale_genre_does_not_read_each_candidate_metadata() {
    let (tmp, store, library, ids) = setup_count(400);
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    // Metadata payload size must not affect genre-only SQL work.
    db.execute("INSERT INTO media_metadata(media_id,locale,title,details_json,revision) SELECT media_id,'en-US',title,?1,1 FROM media WHERE true ON CONFLICT(media_id,locale) DO UPDATE SET details_json=excluded.details_json", [serde_json::json!({"padding":"x".repeat(8192)}).to_string()]).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM media_metadata WHERE length(details_json)>8192",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        400
    );
    let (page, stats) = store
        .protocol_item_page_with_stats(&query(library))
        .unwrap();
    assert_eq!(page.total, 2);
    assert_eq!(page.items.len(), 2);
    assert!(page.items.iter().all(|item| {
        ids[..2]
            .iter()
            .any(|id| item.item == domain::TypedItemId::Work(id.parse().unwrap()))
    }));
    assert!(stats.vm_steps > 0);
}

#[test]
fn scoped_person_aliases_preserve_name_or_tmdb_matching() {
    let (tmp, store, library, ids) = setup_count(5);
    publish_hidden_person(&tmp, &store);
    for (n, name, tmdb, director) in [
        (0, "Jane", "10", false),
        (1, "Jane", "20", false),
        (2, "Other", "10", false),
        (3, "Jane", "", true),
        (4, "Outside", "99", false),
    ] {
        let fields = if director {
            serde_json::json!({"directors":{"state":"value","value":[name]}})
        } else {
            serde_json::json!({"cast":{"state":"value","value":[{"name":name,"tmdb_id":tmdb}]}})
        };
        store
            .set_metadata_overrides(
                ids[n].parse().unwrap(),
                domain::EntityScope::Work,
                "zh-CN",
                &serde_json::from_value(fields).unwrap(),
                0,
            )
            .unwrap();
    }
    let mut request = query(library);
    request.genres.clear();
    request.person_ids =
        vec![uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"tmdb:10:Actor").to_string()];
    let page = store.protocol_item_page(&request).unwrap();
    assert_eq!(page.total, 3);
    assert!(page.items.iter().all(|item| {
        ids[..3]
            .iter()
            .any(|id| item.item == domain::TypedItemId::Work(id.parse().unwrap()))
    }));
    request.person_ids = vec!["10".into()];
    assert_eq!(store.protocol_item_page(&request).unwrap(), page);
    request.person_ids =
        vec![uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"name:jane:Director").to_string()];
    let page = store.protocol_item_page(&request).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(
        page.items[0].item,
        domain::TypedItemId::Work(ids[3].parse().unwrap())
    );
    request.parent = ProtocolParent::Root;
    request.person_ids =
        vec![uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"tmdb:hidden-id:Actor").to_string()];
    assert_eq!(
        store.protocol_item_page(&request).unwrap().total,
        0,
        "unauthorized identity cannot expand a visible name alias"
    );
    request.parent = ProtocolParent::Library(library);
    request.series_id = Some(ids[0].parse().unwrap());
    request.person_ids = vec!["99".into()];
    assert_eq!(
        store.protocol_item_page(&request).unwrap().total,
        0,
        "an identity outside Scope cannot seed aliases"
    );
}

#[test]
fn locale_memberships_filter_each_library_before_root_deduplication() {
    use store::test_support::{FixtureFile, publish_fixture_file};
    let (tmp, store, zh, ids) = setup_count(1);
    let root = tmp.path().join("english");
    std::fs::create_dir_all(&root).unwrap();
    let en = store
        .create_library(
            domain::MediaKind::Movie,
            "English",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let en: domain::LibraryId = en.id.parse().unwrap();
    // Configure the empty Library before publishing its first card.
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    db.execute(
        "UPDATE libraries SET display_locale='en-US' WHERE library_id=?1",
        [en.to_string()],
    )
    .unwrap();
    let media = ids[0].parse().unwrap();
    for (locale, genre, revision) in [("en-US", "Comedy", 1), ("zh-CN", "Drama", 0)] {
        let fields=serde_json::from_value(serde_json::json!({"genres":{"state":"value","value":[{"source":"fixture","name":genre,"filter_key":genre}]}})).unwrap();
        store
            .set_metadata_overrides(media, domain::EntityScope::Work, locale, &fields, revision)
            .unwrap();
    }
    let file = root.join("Film.mkv");
    std::fs::write(&file, b"payload").unwrap();
    // The NFO chooses fallback locale independently from the adopted selection.
    std::fs::write(
        root.join("Film.nfo"),
        "<movie><title>Film</title><genre>NFO only</genre></movie>",
    )
    .unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media,
            path: file.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(store.protocol_item_page(&query(zh)).unwrap().total, 1);
    assert_eq!(store.protocol_item_page(&query(en)).unwrap().total, 0);
    let mut request = query(en);
    request.genres = vec!["Comedy".into()];
    assert_eq!(store.protocol_item_page(&request).unwrap().total, 1);
    request.genres = vec!["NFO only".into()];
    assert_eq!(store.protocol_item_page(&request).unwrap().total, 1);
    request.parent = ProtocolParent::Root;
    request.genres.clear();
    let representative = store.protocol_item_page(&request).unwrap().items[0].library_id;
    for (genre, owner) in [("Drama", zh), ("Comedy", en)] {
        request.genres = vec![genre.into()];
        let page = store.protocol_item_page(&request).unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].item, domain::TypedItemId::Work(media));
        assert_eq!(page.items[0].library_id, owner);
    }
    assert!([zh, en].iter().any(|owner| *owner != representative));
    request.genres = vec!["Drama".into(), "Comedy".into()];
    assert_eq!(store.protocol_item_page(&request).unwrap().total, 1);
}

fn publish_hidden_person(tmp: &tempfile::TempDir, store: &store::Store) {
    use store::test_support::{FixtureFile, publish_fixture_file};
    let root = tmp.path().join("hidden");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            domain::MediaKind::Movie,
            "Hidden",
            &[root.to_str().unwrap()],
            "selected",
            false,
            &[],
        )
        .unwrap();
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "Hidden".into(),
        year: None,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
        aliases: vec![],
    };
    store.insert_media(&media).unwrap();
    let file = root.join("Hidden.mkv");
    std::fs::write(&file, b"payload").unwrap();
    publish_fixture_file(
        store,
        &FixtureFile {
            media_id: media.id,
            path: file.display().to_string(),
            ..Default::default()
        },
    )
    .unwrap();
    let fields=serde_json::from_value(serde_json::json!({"cast":{"state":"value","value":[{"name":"Jane","tmdb_id":"hidden-id"}]}})).unwrap();
    store
        .set_metadata_overrides(media.id, domain::EntityScope::Work, "zh-CN", &fields, 0)
        .unwrap();
}

#[test]
fn unit_classification_inherits_work_while_search_reads_episode() {
    use store::test_support::{FixtureFile, publish_fixture_file};
    let tmp = tempfile::tempdir().unwrap();
    let store = store::Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("tv");
    std::fs::create_dir_all(&root).unwrap();
    let lib = store
        .create_library(
            domain::MediaKind::Tv,
            "TV",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Tv,
        title: "Work title".into(),
        year: None,
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
        aliases: vec![],
    };
    store.insert_media(&media).unwrap();
    std::fs::write(root.join("tvshow.nfo"), "<tvshow><title>Work title</title><genre>Work genre</genre><studio>Work studio</studio><actor><name>Work actor</name></actor></tvshow>").unwrap();
    std::fs::write(root.join("E01.mkv"), b"payload").unwrap();
    std::fs::write(root.join("E01.nfo"), "<episodedetails><season>1</season><episode>1</episode><title>Episode needle</title><genre>Episode genre</genre><studio>Episode studio</studio><actor><name>Episode actor</name></actor></episodedetails>").unwrap();
    publish_fixture_file(
        &store,
        &FixtureFile {
            media_id: media.id,
            path: root.join("E01.mkv").display().to_string(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    let mut request = query(lib.id.parse().unwrap());
    request.include_types = vec![ProtocolItemType::Episode];
    request.genres = vec!["Work genre".into()];
    request.studio_ids = vec!["Work studio".into()];
    request.person_ids =
        vec![uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"name:work actor:Actor").to_string()];
    let expected = domain::TypedItemId::Unit {
        media: media.id,
        key: domain::UnitKey::new(1, 1),
    };
    let page = store.protocol_item_page(&request).unwrap();
    assert_eq!(page.total, 1);
    assert_eq!(page.items[0].item, expected);
    request.search_term = Some("Episode needle".into());
    assert_eq!(store.protocol_item_page(&request).unwrap(), page);
    request.search_term = Some("Work title".into());
    assert_eq!(store.protocol_item_page(&request).unwrap().total, 0);
    request.search_term = None;
    for dimension in 0..3 {
        let mut episode = request.clone();
        match dimension {
            0 => episode.genres = vec!["Episode genre".into()],
            1 => episode.studio_ids = vec!["Episode studio".into()],
            _ => {
                episode.person_ids = vec![
                    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, b"name:episode actor:Actor")
                        .to_string(),
                ]
            }
        }
        assert_eq!(
            store.protocol_item_page(&episode).unwrap().total,
            0,
            "episode-only dimension {dimension}"
        );
    }
}

/// Task 25's cost contract: identities are resolved once, so the work of a filtered page
/// is bounded by the Sloped cards' own people rows — never by the Library's.
///
/// Fixture: 40 cards carry the requested actor; 400 filler cards carry ten strangers
/// each, so the Library's people table is ~100× the matched cards' own rows. The
/// pre-fix shape walked the whole people table *per card*, so its work grew with that
/// 100×; the fixed shape's per-card probe is over the resolved identity set (here one
/// identity), so the same page stays a small constant per card.
///
/// The pinned number is this fixture's measured work with headroom, in the idiom of
/// `perf_baseline`: a regression that reintroduces the per-card library scan cannot fit
/// under it (the pre-fix shape measured ~100× this on the same data).
#[test]
fn person_filter_work_is_bounded_by_scoped_people_not_library_people() {
    let (tmp, store, library, ids) = setup_count(440);
    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    // The matched cards go through the real write path so their wire ids are canonical.
    for id in ids.iter().take(40) {
        let fields = serde_json::json!({
            "cast":{"state":"value","value":[{"name":"Jane","tmdb_id":"10"}]}
        });
        store
            .set_metadata_overrides(
                id.parse().unwrap(),
                domain::EntityScope::Work,
                "zh-CN",
                &serde_json::from_value(fields).unwrap(),
                0,
            )
            .unwrap();
    }
    // Filler people: this test is about the read path, so they are inserted directly.
    for (n, id) in ids.iter().enumerate().skip(40) {
        for k in 0..10 {
            db.execute(
                "INSERT INTO library_protocol_people(library_id,media_id,role,name_key,tmdb_id,wire_id,display_name) \
                 VALUES(?1,?2,'Actor',?3,?4,?5,?6)",
                rusqlite::params![
                    library.to_string(),
                    id,
                    format!("stranger {n} {k}"),
                    format!("{}", 90_000 + n * 10 + k),
                    format!("Actor:s{}", 90_000 + n * 10 + k),
                    format!("Stranger {n} {k}"),
                ],
            )
            .unwrap();
        }
    }
    let people: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM library_protocol_people WHERE library_id=?1",
            [library.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(people, 40 + 400 * 10, "fixture shape");

    let mut request = query(library);
    request.genres.clear();
    request.person_ids = vec!["10".into()];
    request.limit = 20;
    let (page, work) = store.protocol_item_page_with_stats(&request).unwrap();
    println!(
        "person filter over {} people: total={} {:?}",
        people, page.total, work
    );
    assert_eq!(page.total, 40, "every matched card, and only those");
    // Pin: 440 cards × their own people rows + one resolved identity. A per-card walk of
    // the 4,040-row people table would be ~100× this.
    assert!(
        work.vm_steps <= 400_000,
        "person filter work grew past the resolved-identity bound: {work:?}"
    );
}
