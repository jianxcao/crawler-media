use std::collections::BTreeMap;

use domain::{
    Coverage, EntityScope, FetchMode, FilterId, LedgerId, LibraryId, Media, MediaId, MediaKind,
    MetadataAdoption, MetadataPreferences, Subscribe, SubscribeId, UnitKey, UserId,
};
use store::Store;
use store::library_write::{FilePublication, WriteScope};
use store::playback::MediaMergeRequest;

#[path = "merge_support/mod.rs"]
mod helpers;

struct Fixture {
    _temp: tempfile::TempDir,
    store: Store,
    library: LibraryId,
    root: String,
    survivor: Media,
    loser: MediaId,
    user: UserId,
    file: LedgerId,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let store = Store::open(temp.path()).unwrap();
        let survivor = helpers::create_media(
            &store,
            &MediaId::new().to_string(),
            "Survivor",
            MediaKind::Tv,
        );
        let loser =
            helpers::create_media(&store, &MediaId::new().to_string(), "Loser", MediaKind::Tv);
        let (library, root) = helpers::create_library(&store, "TV", MediaKind::Tv, "/tv");
        let mut fixture = Self {
            _temp: temp,
            store,
            library: library.id.parse().unwrap(),
            root,
            survivor,
            loser: loser.id,
            user: UserId::new(),
            file: LedgerId::new(),
        };
        fixture.file = fixture.publish("episode.mkv");
        fixture
            .store
            .set_playback_state(
                fixture.user,
                fixture.survivor.id,
                12_345,
                None,
                Some(true),
                Some(1),
                Some(1),
            )
            .unwrap();
        let operation = fixture
            .store
            .begin_media_merge(
                &MediaMergeRequest {
                    request_id: MediaId::new().to_string(),
                    survivor: fixture.survivor.id,
                    loser: fixture.loser,
                    survivor_revision: 0,
                    loser_revision: 0,
                    refresh_targets: BTreeMap::from([(
                        "tmdb".into(),
                        fixture.survivor.id.to_string(),
                    )]),
                },
                1,
            )
            .unwrap();
        assert_eq!(
            fixture
                .store
                .advance_media_merge(&operation, 2)
                .unwrap()
                .state,
            "succeeded"
        );
        fixture
    }

    fn publish(&self, path: &str) -> LedgerId {
        let id = LedgerId::new();
        let file = helpers::make_file(
            &id.to_string(),
            &self.library.to_string(),
            &self.root,
            &self.survivor.id.to_string(),
            path,
            MediaKind::Tv,
            1_000,
        );
        self.store
            .publish_files(
                &WriteScope {
                    library_id: self.library,
                    config_revision: 0,
                },
                &[FilePublication {
                    file: &file,
                    units: &[UnitKey::new(1, 1)],
                    expected_version: None,
                    nfo_observation: None,
                    probe_required: false,
                }],
                1_000,
            )
            .unwrap();
        id
    }

    fn delete(&self, id: LedgerId) {
        assert_eq!(self.store.delete_files(self.library, &[id]).unwrap(), 1);
    }

    fn assert_playback(&self, retained: bool) {
        assert_eq!(
            self.store
                .playback_marks(self.user, self.survivor.id)
                .unwrap(),
            retained.then_some((false, true)),
            "historical merge redirects must not retain favorites"
        );
        assert_eq!(
            self.store
                .playback_unit_progress(self.user, self.survivor.id, Some(1), Some(1))
                .unwrap(),
            retained.then_some(12_345),
            "historical merge redirects must not retain playback progress"
        );
    }
}

#[test]
fn merged_media_last_file_removal_clears_business_facts_but_keeps_redirect_recovery() {
    let fixture = Fixture::new();
    fixture
        .store
        .adopt_metadata(&metadata_adoption(&fixture), 3)
        .unwrap();
    assert!(
        fixture
            .store
            .get_effective_metadata(fixture.survivor.id, "zh-CN")
            .unwrap()
            .is_some()
    );
    fixture.delete(fixture.file);
    fixture.assert_playback(false);
    assert!(
        fixture
            .store
            .get_effective_metadata(fixture.survivor.id, "zh-CN")
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .store
            .get_media_by_alias_kind(
                "tmdb",
                &fixture.survivor.id.to_string(),
                Some(MediaKind::Tv)
            )
            .unwrap()
            .is_none(),
        "a reimport must not recover purged identity from its former aliases"
    );
    assert_eq!(
        fixture.store.retired_media_survivors().unwrap(),
        vec![(fixture.loser, fixture.survivor.id)],
        "Jobs recovery still needs the historical identity redirect"
    );
    let mut incoming = fixture.survivor.clone();
    incoming.id = MediaId::new();
    let reimported = fixture.store.ensure_media(incoming.clone()).unwrap();
    assert_eq!(reimported.id, incoming.id);
    assert!(
        fixture
            .store
            .playback_marks(fixture.user, reimported.id)
            .unwrap()
            .is_none()
    );
}

fn metadata_adoption(fixture: &Fixture) -> MetadataAdoption {
    let preferences = MetadataPreferences {
        language_priority: vec!["zh-CN".into()],
        cert_country_priority: vec!["CN".into()],
    };
    let normalized = media::normalize_metadata(
        "tmdb",
        EntityScope::Work,
        r#"{"name":"Survivor","overview":"Adopted overview"}"#,
        &preferences,
    )
    .unwrap();
    MetadataAdoption {
        media_id: fixture.survivor.id,
        scope: EntityScope::Work,
        locale: "zh-CN".into(),
        source: "tmdb".into(),
        provider_entity_id: fixture.survivor.id.to_string(),
        provider_work_id: fixture.survivor.id.to_string(),
        fetched_at: 3,
        normalized,
        expected_revision: fixture.store.media_revision(fixture.survivor.id).unwrap(),
        preferences,
    }
}

#[test]
fn last_file_cleanup_rejects_metadata_fetched_before_the_orphan_purge() {
    let fixture = Fixture::new();
    let fetched = metadata_adoption(&fixture);
    fixture.delete(fixture.file);
    assert!(
        fixture.store.adopt_metadata(&fetched, 4).is_err(),
        "a stale Catalog response must not recreate purged facts on the merge identity"
    );
    assert!(
        fixture
            .store
            .get_effective_metadata(fixture.survivor.id, "zh-CN")
            .unwrap()
            .is_none()
    );
}

#[test]
fn orphan_merge_identity_rejects_adoption_even_with_the_current_revision() {
    let fixture = Fixture::new();
    fixture.delete(fixture.file);
    let fetched = metadata_adoption(&fixture);
    assert!(
        fixture.store.adopt_metadata(&fetched, 4).is_err(),
        "reading a fresh revision must not revive facts on an identity-only merge target"
    );
    assert!(
        fixture
            .store
            .get_effective_metadata(fixture.survivor.id, "zh-CN")
            .unwrap()
            .is_none()
    );
}

#[test]
fn orphan_merge_identity_rejects_late_scraped_display() {
    let fixture = Fixture::new();
    fixture.delete(fixture.file);
    assert!(
        fixture
            .store
            .record_scraped_display(
                fixture.survivor.id,
                "zh-CN",
                store::metadata::ScrapedDisplay {
                    rating: Some(8.5),
                    ..Default::default()
                },
            )
            .is_err(),
        "a late Scrape must not recreate display facts on the retained merge identity"
    );
    assert!(
        fixture
            .store
            .get_effective_metadata(fixture.survivor.id, "zh-CN")
            .unwrap()
            .is_none()
    );
}

#[test]
fn orphan_merge_identity_rejects_aliases_from_a_stale_media_snapshot() {
    let fixture = Fixture::new();
    fixture.delete(fixture.file);
    assert!(
        fixture.store.update_media(&fixture.survivor).is_err(),
        "a stale Media snapshot must not reopen the identity through its old aliases"
    );
    assert!(
        fixture
            .store
            .media_refresh_alias(fixture.survivor.id, "tmdb")
            .unwrap()
            .is_none()
    );
}

#[test]
fn title_only_reimport_skips_the_identity_only_merge_target() {
    let fixture = Fixture::new();
    fixture.delete(fixture.file);
    let mut incoming = fixture.survivor.clone();
    incoming.id = MediaId::new();
    incoming.tmdb_id = None;
    let resolved = fixture
        .store
        .resolve_scanned_media(incoming.clone())
        .unwrap();
    assert_eq!(resolved.id, incoming.id);
    assert!(
        fixture
            .store
            .playback_marks(fixture.user, resolved.id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn ordinary_aliasless_media_can_adopt_metadata_before_its_first_file() {
    let fixture = Fixture::new();
    let mut media = fixture.survivor.clone();
    media.id = MediaId::new();
    media.tmdb_id = None;
    fixture.store.insert_media(&media).unwrap();
    let mut adoption = metadata_adoption(&fixture);
    adoption.media_id = media.id;
    adoption.expected_revision = fixture.store.media_revision(media.id).unwrap();
    fixture.store.adopt_metadata(&adoption, 4).unwrap();
    fixture
        .store
        .record_scraped_display(
            media.id,
            "zh-CN",
            store::metadata::ScrapedDisplay {
                rating: Some(8.5),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        fixture
            .store
            .get_effective_metadata(media.id, "zh-CN")
            .unwrap()
            .unwrap()
            .rating,
        Some(8.5)
    );
}

#[test]
fn a_new_collection_reference_allows_an_aliasless_merge_target_to_adopt_metadata() {
    let fixture = Fixture::new();
    fixture.delete(fixture.file);
    let collection = fixture
        .store
        .create_collection(fixture.user, "Keep")
        .unwrap();
    fixture
        .store
        .add_collection_item(&collection, &fixture.survivor.id.to_string())
        .unwrap();
    fixture
        .store
        .adopt_metadata(&metadata_adoption(&fixture), 4)
        .unwrap();
    fixture
        .store
        .record_scraped_display(
            fixture.survivor.id,
            "zh-CN",
            store::metadata::ScrapedDisplay {
                rating: Some(8.5),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        fixture
            .store
            .get_effective_metadata(fixture.survivor.id, "zh-CN")
            .unwrap()
            .unwrap()
            .rating,
        Some(8.5)
    );
    fixture.assert_playback(false);
}

#[test]
fn another_file_retains_merged_media_playback_until_its_removal() {
    let fixture = Fixture::new();
    let other = fixture.publish("second-version.mkv");
    fixture.delete(fixture.file);
    fixture.assert_playback(true);
    fixture.delete(other);
    fixture.assert_playback(false);
}

#[test]
fn collection_retains_merged_media_playback_until_its_last_membership_is_removed() {
    let fixture = Fixture::new();
    let collection = fixture
        .store
        .create_collection(fixture.user, "Keep")
        .unwrap();
    fixture
        .store
        .add_collection_item(&collection, &fixture.survivor.id.to_string())
        .unwrap();
    fixture.delete(fixture.file);
    fixture.assert_playback(true);
    fixture
        .store
        .remove_collection_item(&collection, &fixture.survivor.id.to_string())
        .unwrap();
    fixture.assert_playback(false);
}

#[test]
fn subscribe_retains_merged_media_playback_until_its_last_release() {
    let fixture = Fixture::new();
    let subscribe = Subscribe {
        id: SubscribeId::new(),
        user_id: fixture.user,
        media_id: fixture.survivor.id,
        coverage: Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: Some(1),
        },
        fetch_mode: FetchMode::Search,
        filter_id: FilterId::new(),
        wash_cut: false,
        wash_cut_filter_id: None,
        keep_old_versions: false,
        full_season_pack: false,
        downloader_id: None,
        library_id: Some(fixture.library),
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1_800,
    };
    fixture.store.insert_subscribe(&subscribe).unwrap();
    fixture.delete(fixture.file);
    fixture.assert_playback(true);
    assert!(fixture.store.delete_subscribe(subscribe.id).unwrap());
    fixture.assert_playback(false);
}

#[test]
fn library_removal_clears_merged_media_playback_after_its_last_file() {
    let fixture = Fixture::new();
    helpers::create_library(&fixture.store, "Other TV", MediaKind::Tv, "/other");
    let operation = fixture
        .store
        .begin_library_removal(&fixture.library.to_string())
        .unwrap();
    fixture
        .store
        .set_removal_state(&operation.operation_id, "cleaning", None)
        .unwrap();
    assert_eq!(
        fixture
            .store
            .clean_library_removal(&operation.operation_id)
            .unwrap()
            .state,
        "succeeded"
    );
    fixture.assert_playback(false);
}
