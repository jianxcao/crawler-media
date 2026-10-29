//! Hydration must never decode or walk metadata outside a selected file's scopes/locales.
use domain::{
    EntityScope, FieldValue, LibraryFile, Media, MediaId, MediaKind, MetadataFields, UnitKey,
};
use rusqlite::{Connection, params};
use store::{Store, stats::StatsScope};

struct Fixture {
    dir: tempfile::TempDir,
    store: Store,
    file: LibraryFile,
}
impl Fixture {
    fn new() -> Self {
        Self::with_units(&[UnitKey::new(1, 1)])
    }
    fn with_units(units: &[UnitKey]) -> Self {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let store = Store::open(dir.path()).unwrap();
        let root = dir.path().join("tv");
        std::fs::create_dir_all(&root).unwrap();
        store
            .create_library(
                MediaKind::Tv,
                "TV",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "Series".into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        store.insert_media(&media).unwrap();
        let path = root.join("Show.S01E01.mkv");
        std::fs::write(&path, b"local fixture").unwrap();
        std::fs::write(path.with_extension("nfo"),"<episodedetails><season>1</season><episode>1</episode><title>Selected episode</title></episodedetails>").unwrap();
        let file = store::test_support::publish_fixture_file_units(
            &store,
            &store::test_support::FixtureFile {
                media_id: media.id,
                path: path.display().to_string(),
                ..Default::default()
            },
            units,
        )
        .unwrap();
        for scope in [
            EntityScope::Work,
            EntityScope::Episode {
                season: 1,
                episode: 1,
            },
        ] {
            store
                .adopt_metadata(
                    &domain::MetadataAdoption {
                        media_id: media.id,
                        scope,
                        locale: "zh-CN".into(),
                        source: "tmdb".into(),
                        provider_entity_id: "1".into(),
                        provider_work_id: "1".into(),
                        fetched_at: 1,
                        normalized: domain::NormalizedMetadata {
                            fields: MetadataFields {
                                title: FieldValue::Value("Provider title".into()),
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
        }
        store
            .set_metadata_overrides(
                media.id,
                EntityScope::Episode {
                    season: 1,
                    episode: 1,
                },
                "zh-CN",
                &MetadataFields::default(),
                0,
            )
            .unwrap();
        Self { dir, store, file }
    }
    /// Deliberately corrupt otherwise-valid stored JSON in fixture setup. The public
    /// reader must isolate it when neither its scope nor its locale was selected.
    fn add_metadata(&self, episode: i32, locale: &str, malformed: bool) {
        let db = Connection::open(self.dir.path().join("library.db")).unwrap();
        let fields = if malformed { r#"{"title":37}"# } else { "{}" };
        db.execute("INSERT INTO media_metadata_sources
            SELECT media_id,'episode',1,?1,source,?2,provider_entity_id,provider_work_id,?3,
              present_fields_json,preferences_json,selection_origins_json,fetched_at,source_revision
            FROM media_metadata_sources WHERE media_id=?4 AND entity_scope='episode' AND season=1 AND episode=1 AND locale='zh-CN'",
            params![episode,locale,fields,self.file.media_id.to_string()]).unwrap();
        db.execute("INSERT INTO media_metadata_overrides
            SELECT media_id,'episode',1,?1,?2,?3,revision FROM media_metadata_overrides
            WHERE media_id=?4 AND entity_scope='episode' AND season=1 AND episode=1 AND locale='zh-CN'",
            params![episode,locale,fields,self.file.media_id.to_string()]).unwrap();
        db.execute("INSERT INTO library_metadata_inputs
            SELECT library_id,media_id,'episode',1,?1,?2,input_key,root_id,path_key,parse_state,source_version,?3,
              present_fields_json,provenance_json,export_id,export_digest,export_fields_json,revision
            FROM library_metadata_inputs WHERE library_id=?4 AND media_id=?5
              AND entity_scope='episode' AND season=1 AND episode=1 AND locale='zh-CN'",
            params![episode,locale,fields,self.file.library_id.to_string(),self.file.media_id.to_string()]).unwrap();
        db.execute(
            "INSERT INTO media_markers VALUES (?1,1,?2,10,20,NULL,NULL,'manual',0,1)",
            params![self.file.media_id.to_string(), episode],
        )
        .unwrap();
    }
    fn selected_fields(&self) -> MetadataFields {
        self.store
            .protocol_page_metadata(
                self.file.library_id,
                self.file.media_id,
                EntityScope::Episode {
                    season: 1,
                    episode: 1,
                },
                None,
            )
            .unwrap()
    }
}

#[test]
fn page_hydration_ignores_malformed_metadata_in_off_page_scopes() {
    let fixture = Fixture::new();
    let expected = fixture.selected_fields();
    fixture.add_metadata(99, "zh-CN", true);
    let hydrated = fixture
        .store
        .page_hydration(&[fixture.file.file_id])
        .unwrap();
    assert_eq!(
        hydrated[&fixture.file.file_id].fields[&EntityScope::Episode {
            season: 1,
            episode: 1
        }],
        expected
    );
    assert!(
        !hydrated[&fixture.file.file_id]
            .markers
            .contains_key(&(1, 99))
    );
}

#[test]
fn page_hydration_ignores_malformed_metadata_in_unselected_locale() {
    let fixture = Fixture::new();
    let expected = fixture.selected_fields();
    fixture.add_metadata(1, "en-US", true);
    let hydrated = fixture
        .store
        .page_hydration(&[fixture.file.file_id])
        .unwrap();
    assert_eq!(
        hydrated[&fixture.file.file_id].fields[&EntityScope::Episode {
            season: 1,
            episode: 1
        }],
        expected
    );
}

#[test]
fn page_hydration_work_does_not_grow_with_unselected_series_episodes() {
    let fixture = Fixture::new();
    let (before, before_work) = fixture.store.with_stats(StatsScope::Library, || {
        fixture
            .store
            .page_hydration(&[fixture.file.file_id])
            .unwrap()
    });
    for episode in 2..202 {
        fixture.add_metadata(episode, "zh-CN", false);
    }
    let (after, after_work) = fixture.store.with_stats(StatsScope::Library, || {
        fixture
            .store
            .page_hydration(&[fixture.file.file_id])
            .unwrap()
    });
    assert_eq!(
        after[&fixture.file.file_id].fields,
        before[&fixture.file.file_id].fields
    );
    assert_eq!(
        after_work.sql_rows, before_work.sql_rows,
        "{before_work:?} -> {after_work:?}"
    );
    assert!(
        after_work.vm_steps <= before_work.vm_steps + 200,
        "{before_work:?} -> {after_work:?}"
    );
    assert!(after[&fixture.file.file_id].markers.is_empty());
}

#[test]
fn page_hydration_pack_keeps_each_covered_episode_marker() {
    let fixture = Fixture::with_units(&[UnitKey::new(1, 1), UnitKey::new(1, 2)]);
    fixture.add_metadata(1, "en-US", false);
    fixture.add_metadata(2, "zh-CN", false);
    fixture.add_metadata(99, "zh-CN", true);
    let hydrated = fixture
        .store
        .page_hydration(&[fixture.file.file_id])
        .unwrap();
    let facts = &hydrated[&fixture.file.file_id];
    assert!(facts.fields.contains_key(&EntityScope::Episode {
        season: 1,
        episode: 1
    }));
    assert!(facts.fields.contains_key(&EntityScope::Episode {
        season: 1,
        episode: 2
    }));
    assert_eq!(facts.markers[&(1, 1)].intro_start_ms, Some(10));
    assert_eq!(facts.markers[&(1, 2)].intro_start_ms, Some(10));
    assert!(!facts.markers.contains_key(&(1, 99)));
}
