//! Public entity reads are exact, visible, and independent of unrelated Library growth.
use domain::{LibraryId, Media, MediaId, MediaKind, UnitKey, User, UserId, UserRole};
use store::test_support::{FixtureFile, publish_fixture_file, publish_fixture_file_units};
use store::{ProtocolEntityKind, ProtocolEntityQuery, Store, stats::StatsScope};

struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    user: UserId,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir_in("/tmp/crawler-storage-fixes").unwrap();
        let store = Store::open(dir.path().join("data")).unwrap();
        let user = UserId::new();
        store
            .insert_user(&User {
                id: user,
                login: "entity-reader".into(),
                enabled: true,
                role: UserRole::Admin,
            })
            .unwrap();
        Self { dir, store, user }
    }
    fn library(&self, name: &str, kind: MediaKind) -> LibraryId {
        let root = self.dir.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        self.store
            .create_library(kind, name, &[root.to_str().unwrap()], "everyone", true, &[])
            .unwrap()
            .id
            .parse()
            .unwrap()
    }
    fn media(&self, kind: MediaKind, title: &str) -> MediaId {
        let id = MediaId::new();
        self.store
            .insert_media(&Media {
                id,
                kind,
                title: title.into(),
                year: None,
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        id
    }
    fn publish(
        &self,
        library: LibraryId,
        media: MediaId,
        genre: &str,
        studio: &str,
        episodes: &[UnitKey],
    ) {
        let config = self
            .store
            .get_library(&library.to_string())
            .unwrap()
            .unwrap();
        let root = &config.root_paths[0];
        let file = root.join(format!("{media}.mkv"));
        std::fs::write(&file, b"movie").unwrap();
        std::fs::write(file.with_extension("nfo"),format!("<movie><title>{media}</title><genre>{genre}</genre><studio>{studio}</studio><actor><name>Jane Roe</name><role>Lead</role><thumb>https://example.invalid/jane.jpg</thumb></actor></movie>")).unwrap();
        let fixture = FixtureFile {
            media_id: media,
            path: file.display().to_string(),
            ..Default::default()
        };
        if episodes.is_empty() {
            publish_fixture_file(&self.store, &fixture).unwrap();
        } else {
            publish_fixture_file_units(&self.store, &fixture, episodes).unwrap();
        }
    }
    fn query(&self, kind: ProtocolEntityKind) -> ProtocolEntityQuery {
        ProtocolEntityQuery {
            user_id: self.user,
            library_id: None,
            kind,
            search: None,
            offset: 0,
            limit: 50,
        }
    }
}
fn wire(name: &str) -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS, name.as_bytes())
        .simple()
        .to_string()
}

#[test]
fn entity_counts_and_representative_artwork_respect_library_visibility() {
    let f = Fixture::new();
    let a = f.library("first", MediaKind::Movie);
    let b = f.library("second", MediaKind::Movie);
    let shared = f.media(MediaKind::Movie, "Shared");
    let private = f.media(MediaKind::Movie, "Private");
    f.publish(a, shared, "Science Fiction", "Mixed Case", &[]);
    f.publish(b, shared, "Science Fiction", "Mixed Case", &[]);
    f.publish(b, private, "Secret", "Secret Studio", &[]);
    let genres = f
        .store
        .protocol_entities(&f.query(ProtocolEntityKind::Genre))
        .unwrap();
    let genre = genres
        .items
        .iter()
        .find(|e| e.name == "Science Fiction")
        .unwrap();
    assert_eq!(genre.wire_id, wire("Science Fiction"));
    assert_eq!(genre.child_count, 1);
    let studios = f
        .store
        .protocol_entities(&f.query(ProtocolEntityKind::Studio))
        .unwrap();
    assert_eq!(
        studios
            .items
            .iter()
            .find(|e| e.name == "Mixed Case")
            .unwrap()
            .child_count,
        1
    );
    f.store.begin_library_removal(&b.to_string()).unwrap();
    let genres = f
        .store
        .protocol_entities(&f.query(ProtocolEntityKind::Genre))
        .unwrap();
    assert_eq!(genres.items.len(), 1, "{:?}", genres.items);
    assert_eq!(genres.items[0].child_count, 1);
    let representative = f
        .store
        .protocol_entity_representative(
            Some(f.user),
            ProtocolEntityKind::Genre,
            &wire("Science Fiction"),
        )
        .unwrap()
        .unwrap();
    assert_eq!(representative.library_id, a);
    assert_eq!(representative.item.media_id(), shared);
    assert!(
        f.store
            .protocol_entity_representative(Some(f.user), ProtocolEntityKind::Genre, "Secret")
            .unwrap()
            .is_none()
    );
}

#[test]
fn entity_lookup_work_is_bounded_by_result_page() {
    let f = Fixture::new();
    let a = f.library("first", MediaKind::Movie);
    let b = f.library("second", MediaKind::Movie);
    let target = f.media(MediaKind::Movie, "Target");
    f.publish(a, target, "Drama", "Studio", &[]);
    f.publish(b, target, "Drama", "Studio", &[]);
    let measure = || {
        f.store.with_stats(StatsScope::Library, || {
            let genres = f
                .store
                .protocol_entities(&f.query(ProtocolEntityKind::Genre))
                .unwrap();
            let person = f
                .store
                .protocol_person(Some(f.user), "Jane Roe")
                .unwrap()
                .unwrap();
            assert_eq!(person.name, "Jane Roe");
            let representative = f
                .store
                .protocol_entity_representative(Some(f.user), ProtocolEntityKind::Genre, "Drama")
                .unwrap()
                .unwrap();
            assert_eq!(representative.item.media_id(), target);
            genres
        })
    };
    let (before, small) = measure();
    assert_eq!(before.items[0].child_count, 1);
    for index in 0..120 {
        let media = f.media(MediaKind::Movie, &format!("Unrelated{index}"));
        f.publish(a, media, "Comedy", "Another", &[]);
        f.publish(b, media, "Comedy", "Another", &[]);
    }
    let (after, large) = measure();
    assert_eq!(
        after
            .items
            .iter()
            .find(|e| e.name == "Comedy")
            .unwrap()
            .child_count,
        120
    );
    assert!(
        large.vm_steps <= small.vm_steps + 1800,
        "entity dimensions, not memberships, bound the work: {small:?} -> {large:?}"
    );
    assert!(
        large.fullscan_steps <= small.fullscan_steps + 30,
        "{small:?} -> {large:?}"
    );
    assert_eq!(large.hydrated_rows, 0);
}

#[test]
fn protocol_counts_deduplicate_visible_work_and_episode_identities() {
    let f = Fixture::new();
    let a = f.library("movies-a", MediaKind::Movie);
    let b = f.library("movies-b", MediaKind::Movie);
    let tv = f.library("series", MediaKind::Tv);
    let movie = f.media(MediaKind::Movie, "Shared");
    f.publish(a, movie, "Drama", "Studio", &[]);
    f.publish(b, movie, "Drama", "Studio", &[]);
    let series = f.media(MediaKind::Tv, "Series");
    f.publish(
        tv,
        series,
        "Drama",
        "Studio",
        &[UnitKey::new(1, 1), UnitKey::new(1, 2), UnitKey::new(2, 1)],
    );
    let counts = f.store.protocol_counts(f.user).unwrap();
    assert_eq!(
        (counts.movies, counts.series, counts.episodes, counts.items),
        (1, 1, 3, 2)
    );
    f.store.begin_library_removal(&tv.to_string()).unwrap();
    let counts = f.store.protocol_counts(f.user).unwrap();
    assert_eq!(
        (counts.movies, counts.series, counts.episodes, counts.items),
        (1, 0, 0, 1)
    );
}

#[test]
fn seasons_are_series_scoped_and_preserve_play_state() {
    let f = Fixture::new();
    let tv = f.library("series", MediaKind::Tv);
    let series = f.media(MediaKind::Tv, "Target");
    f.publish(
        tv,
        series,
        "Drama",
        "Studio",
        &[UnitKey::new(1, 1), UnitKey::new(1, 2), UnitKey::new(2, 1)],
    );
    f.store
        .upsert_unit(
            f.user,
            series,
            1,
            -1,
            0,
            Some(true),
            None,
            None,
            None,
            None,
            false,
            10,
        )
        .unwrap();
    let measure = || {
        f.store.with_stats(StatsScope::Library, || {
            f.store.protocol_seasons(f.user, series).unwrap()
        })
    };
    let (rows, small) = measure();
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[0].season, rows[0].total, rows[0].played), (1, 2, 2));
    assert_eq!(rows[1].played, 0);
    for index in 0..80 {
        let media = f.media(MediaKind::Tv, &format!("Other{index}"));
        f.publish(
            tv,
            media,
            "Comedy",
            "Other",
            &[UnitKey::new(1, 1), UnitKey::new(1, 2)],
        );
    }
    let (after, large) = measure();
    assert_eq!(after, rows);
    assert!(
        large.vm_steps <= small.vm_steps + 1200,
        "pinned seasons must seek: {small:?} -> {large:?}"
    );
    assert!(
        large.fullscan_steps <= small.fullscan_steps + 10,
        "{small:?} -> {large:?}"
    );
}

#[test]
fn person_prefers_visible_image_source_and_preserves_display_and_character() {
    let f = Fixture::new();
    let a = f.library("no-image", MediaKind::Movie);
    let b = f.library("image", MediaKind::Movie);
    let (a, b) = if a.to_string() < b.to_string() {
        (a, b)
    } else {
        (b, a)
    };
    let first = f.media(MediaKind::Movie, "First");
    let second = f.media(MediaKind::Movie, "Second");
    for (library, media) in [(a, first), (b, second)] {
        let config = f.store.get_library(&library.to_string()).unwrap().unwrap();
        let path = config.root_paths[0].join(format!("{media}.mkv"));
        std::fs::write(&path, b"movie").unwrap();
        publish_fixture_file(
            &f.store,
            &FixtureFile {
                media_id: media,
                path: path.display().to_string(),
                ..Default::default()
            },
        )
        .unwrap();
    }
    // Adopted fields replace the existing NFO image without changing person identity.
    let update = |media: MediaId, thumb: Option<&str>| {
        f.store
            .adopt_metadata(
                &domain::MetadataAdoption {
                    media_id: media,
                    scope: domain::EntityScope::Work,
                    locale: "zh-CN".into(),
                    source: "tmdb".into(),
                    provider_entity_id: media.to_string(),
                    provider_work_id: media.to_string(),
                    fetched_at: 1,
                    normalized: domain::NormalizedMetadata {
                        fields: domain::MetadataFields {
                            cast: domain::FieldValue::Value(vec![domain::MetadataCast {
                                name: "Jane Roe".into(),
                                role: Some("Lead".into()),
                                tmdb_id: None,
                                thumb: thumb.map(str::to_string),
                                order: None,
                            }]),
                            ..Default::default()
                        },
                        field_origins: Default::default(),
                    },
                    expected_revision: Some(0),
                    preferences: domain::MetadataPreferences {
                        language_priority: vec!["zh-CN".into()],
                        cert_country_priority: vec![],
                    },
                },
                1,
            )
            .unwrap();
    };
    update(first, None);
    update(second, Some("https://example.invalid/jane.jpg"));
    let person = f
        .store
        .protocol_person(Some(f.user), "Jane Roe")
        .unwrap()
        .unwrap();
    assert_eq!(person.name, "Jane Roe");
    assert_eq!(person.role.as_deref(), Some("Lead"));
    assert_eq!(
        person.image_url.as_deref(),
        Some("https://example.invalid/jane.jpg")
    );
    let by_id = f
        .store
        .protocol_person(Some(f.user), &person.wire_id)
        .unwrap()
        .unwrap();
    assert_eq!(by_id, person);
    f.store.begin_library_removal(&b.to_string()).unwrap();
    let person = f
        .store
        .protocol_person(Some(f.user), "Jane Roe")
        .unwrap()
        .unwrap();
    assert!(
        person.image_url.is_none(),
        "hidden image source cannot supply a profile"
    );
}

#[test]
fn overlapping_nfo_and_adopted_people_have_one_consistent_projection() {
    let f = Fixture::new();
    let library = f.library("people", MediaKind::Movie);
    let media = f.media(MediaKind::Movie, "Shared person");
    f.publish(library, media, "Drama", "Studio", &[]);
    f.store
        .adopt_metadata(
            &domain::MetadataAdoption {
                media_id: media,
                scope: domain::EntityScope::Work,
                locale: "zh-CN".into(),
                source: "tmdb".into(),
                provider_entity_id: "p".into(),
                provider_work_id: "p".into(),
                fetched_at: 1,
                normalized: domain::NormalizedMetadata {
                    fields: domain::MetadataFields {
                        cast: domain::FieldValue::Value(vec![domain::MetadataCast {
                            name: "Jane Roe".into(),
                            role: Some("Different adopted character".into()),
                            tmdb_id: None,
                            thumb: Some("https://example.invalid/different.jpg".into()),
                            order: None,
                        }]),
                        ..Default::default()
                    },
                    field_origins: Default::default(),
                },
                expected_revision: Some(0),
                preferences: domain::MetadataPreferences {
                    language_priority: vec!["zh-CN".into()],
                    cert_country_priority: vec![],
                },
            },
            1,
        )
        .unwrap();
    let person = f
        .store
        .protocol_person(Some(f.user), "Jane Roe")
        .unwrap()
        .unwrap();
    assert_eq!(person.role.as_deref(), Some("Lead"));
    assert_eq!(
        person.image_url.as_deref(),
        Some("https://example.invalid/jane.jpg")
    );
    let op = "consistent-people";
    f.store
        .begin_library_integrity(library, false, op, 1)
        .unwrap();
    f.store.advance_library_operation(op, 2, 100).unwrap();
    let report = f.store.library_integrity_report(op).unwrap();
    assert!(!report.has_issues(), "{report:?}");
}

#[test]
fn integrity_repairs_entity_memberships_and_aggregated_overlap_counts() {
    let f = Fixture::new();
    let a = f.library("first", MediaKind::Movie);
    let b = f.library("second", MediaKind::Movie);
    let shared = f.media(MediaKind::Movie, "Shared");
    f.publish(a, shared, "Drama", "Studio", &[]);
    f.publish(b, shared, "Drama", "Studio", &[]);
    let db = rusqlite::Connection::open(f.dir.path().join("data/library.db")).unwrap();
    db.execute(
        "UPDATE protocol_entity_overlap_counts SET member_count=99",
        [],
    )
    .unwrap();
    drop(db);
    let op = "entity-overlap-repair";
    f.store.begin_library_integrity(a, true, op, 1).unwrap();
    f.store.advance_library_operation(op, 2, 100).unwrap();
    let entities = f
        .store
        .protocol_entities(&f.query(ProtocolEntityKind::Genre))
        .unwrap();
    assert_eq!(entities.items.len(), 1);
    assert_eq!(entities.items[0].child_count, 1);
    let person = f
        .store
        .protocol_person(Some(f.user), "Jane Roe")
        .unwrap()
        .unwrap();
    assert_eq!(person.role.as_deref(), Some("Lead"));
}

#[test]
fn entity_edits_and_completed_library_removal_settle_exact_counts() {
    let f = Fixture::new();
    let a = f.library("first", MediaKind::Movie);
    let b = f.library("second", MediaKind::Movie);
    let shared = f.media(MediaKind::Movie, "Shared");
    f.publish(a, shared, "Drama", "Studio", &[]);
    f.publish(b, shared, "Drama", "Studio", &[]);
    f.publish(a, shared, "Comedy", "New Studio", &[]);
    let entities = f
        .store
        .protocol_entities(&f.query(ProtocolEntityKind::Genre))
        .unwrap();
    assert_eq!(
        entities
            .items
            .iter()
            .find(|row| row.name == "Drama")
            .unwrap()
            .child_count,
        1
    );
    assert_eq!(
        entities
            .items
            .iter()
            .find(|row| row.name == "Comedy")
            .unwrap()
            .child_count,
        1
    );
    let removal = f.store.begin_library_removal(&b.to_string()).unwrap();
    f.store
        .set_removal_state(&removal.operation_id, "cleaning", None)
        .unwrap();
    f.store
        .clean_library_removal(&removal.operation_id)
        .unwrap();
    let entities = f
        .store
        .protocol_entities(&f.query(ProtocolEntityKind::Genre))
        .unwrap();
    assert_eq!(entities.items.len(), 1);
    assert_eq!(entities.items[0].name, "Comedy");
    assert_eq!(entities.items[0].child_count, 1);
    let studios = f
        .store
        .protocol_entities(&f.query(ProtocolEntityKind::Studio))
        .unwrap();
    assert_eq!(studios.items.len(), 1);
    assert_eq!(studios.items[0].name, "New Studio");
}

#[test]
fn integrity_excludes_shared_media_owners_but_allows_independent_libraries() {
    let f = Fixture::new();
    let a = f.library("first", MediaKind::Movie);
    let b = f.library("second", MediaKind::Movie);
    let c = f.library("independent", MediaKind::Movie);
    for index in 0..2 {
        let shared = f.media(MediaKind::Movie, &format!("Shared {index}"));
        f.publish(a, shared, "Drama", "Studio", &[]);
        f.publish(b, shared, "Drama", "Studio", &[]);
    }
    let independent = f.media(MediaKind::Movie, "Independent");
    f.publish(c, independent, "Comedy", "Other", &[]);
    let db = rusqlite::Connection::open(f.dir.path().join("data/library.db")).unwrap();
    db.execute(
        "DELETE FROM library_protocol_entities WHERE library_id=?1",
        [b.to_string()],
    )
    .unwrap();
    drop(db);
    f.store
        .begin_library_integrity(a, true, "shared-first", 1)
        .unwrap();
    let staged = f
        .store
        .advance_library_operation("shared-first", 2, 1)
        .unwrap();
    assert_ne!(
        staged.state, "succeeded",
        "one shared Media remains outside staged batch"
    );
    f.store
        .begin_library_integrity(c, true, "independent", 3)
        .unwrap();
    let blocked = f.store.begin_library_integrity(b, true, "shared-second", 4);
    assert!(
        matches!(blocked, Err(store::StoreError::Protected(_))),
        "shared owner must not change membership during staged aggregate repair: {blocked:?}"
    );
    f.store
        .advance_library_operation("shared-first", 5, 100)
        .unwrap();
    f.store
        .advance_library_operation("independent", 6, 100)
        .unwrap();
    f.store
        .begin_library_integrity(b, true, "shared-second", 7)
        .unwrap();
    f.store
        .advance_library_operation("shared-second", 8, 100)
        .unwrap();
    let genres = f
        .store
        .protocol_entities(&f.query(ProtocolEntityKind::Genre))
        .unwrap();
    assert_eq!(
        genres
            .items
            .iter()
            .find(|item| item.name == "Drama")
            .unwrap()
            .child_count,
        2
    );
    // The same durable exclusion applies in the inverse acquisition order.
    f.store
        .begin_library_integrity(b, true, "reverse-second", 9)
        .unwrap();
    assert!(matches!(
        f.store
            .begin_library_integrity(a, true, "reverse-first", 10),
        Err(store::StoreError::Protected(_))
    ));
    f.store
        .advance_library_operation("reverse-second", 11, 100)
        .unwrap();
}

#[test]
fn staged_integrity_blocks_shared_locale_rebuild_but_allows_independent_rebuild() {
    let f = Fixture::new();
    let a = f.library("integrity-owner", MediaKind::Movie);
    let b = f.library("shared-locale", MediaKind::Movie);
    let c = f.library("independent-locale", MediaKind::Movie);
    for index in 0..2 {
        let media = f.media(MediaKind::Movie, &format!("Shared locale {index}"));
        f.publish(a, media, "Drama", "Studio", &[]);
        f.publish(b, media, "Drama", "Studio", &[]);
    }
    let independent = f.media(MediaKind::Movie, "Independent locale");
    f.publish(c, independent, "Comedy", "Other", &[]);
    f.store
        .begin_library_integrity(a, true, "staged-before-locale", 1)
        .unwrap();
    let staged = f
        .store
        .advance_library_operation("staged-before-locale", 2, 1)
        .unwrap();
    assert_ne!(staged.state, "succeeded");
    let reconfigure = |library: LibraryId, operation: &str| {
        let expected_revision = f
            .store
            .library_config_revision(&library.to_string())
            .unwrap();
        f.store.begin_library_reconfiguration(
            &store::LibraryReconfiguration {
                operation_id: operation.into(),
                library_id: library,
                expected_revision,
                root_paths: None,
                display_locale: Some("en-US".into()),
            },
            3,
        )
    };
    reconfigure(c, "independent-locale").unwrap();
    let independent_done = f
        .store
        .advance_library_operation("independent-locale", 4, 100)
        .unwrap();
    assert_eq!(independent_done.state, "succeeded");
    let shared_rebuild = reconfigure(b, "shared-locale")
        .and_then(|_| f.store.advance_library_operation("shared-locale", 5, 100));
    assert!(
        matches!(shared_rebuild, Err(store::StoreError::Protected(_))),
        "a shared locale rebuild must not settle ownership during staged integrity: {shared_rebuild:?}"
    );
    f.store
        .advance_library_operation("staged-before-locale", 6, 100)
        .unwrap();
}
