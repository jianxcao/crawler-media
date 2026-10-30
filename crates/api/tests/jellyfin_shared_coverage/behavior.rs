use super::Fixture;
use domain::{LedgerId, MediaId, PhysicalSourceId, UnitKey, User, UserId, UserRole};
use media_server::provider::MediaServerProvider;
use store::playback::PlaybackMutation;
use store::test_support::{FixtureFile, publish_fixture_file_units};

fn add_variant(fixture: &Fixture) -> LedgerId {
    let handle = fixture.state.store();
    let store = handle.as_ref();
    let current = store
        .file_coverage_by_id(fixture.source.file_id)
        .unwrap()
        .unwrap();
    let path = current.file.path.with_file_name("variant.mkv");
    std::fs::write(&path, b"second version").unwrap();
    let file_id = "00000000-0000-4000-8000-000000000001".parse().unwrap();
    publish_fixture_file_units(
        store,
        &FixtureFile {
            media_id: fixture.media,
            path: path.display().to_string(),
            file_id: Some(file_id),
            ..Default::default()
        },
        &current.units,
    )
    .unwrap();
    file_id
}

fn mark(fixture: &Fixture, season: i32, episode: i32, played: bool, clock: i64) {
    fixture
        .state
        .store()
        .mutate_playback(
            fixture.media,
            &PlaybackMutation::marks(fixture.user, season, episode, Some(played), None),
            clock,
        )
        .unwrap();
}

pub fn marks_and_versions(runtime: &tokio::runtime::Runtime) {
    let fixture = Fixture::new(4);
    let variant = add_variant(&fixture);
    mark(&fixture, -1, -1, true, 2000);
    mark(&fixture, 1, -1, false, 3000);
    mark(&fixture, 1, 1, true, 3000);
    let provider = fixture.provider();
    let (next, resume) = runtime.block_on(async {
        (
            provider
                .next_up_items(fixture.user, None, 0, 1)
                .await
                .unwrap(),
            provider
                .resumable_items_page(fixture.user, None, 0, 1)
                .await
                .unwrap(),
        )
    });
    assert_eq!(next.total, 1);
    assert_eq!(next.items[0].unit, Some(UnitKey::new(1, 2)));
    assert_eq!(resume.total, 1);
    assert_eq!(resume.items[0].unit, Some(UnitKey::new(1, 2)));
    assert_eq!(resume.items[0].file.file_id, variant);
    assert!(resume.items[0].metadata.is_favorite);
    mark(&fixture, 1, 2, true, 4000);
    let resume = runtime
        .block_on(provider.resumable_items_page(fixture.user, None, 0, 1))
        .unwrap();
    assert_eq!(resume.total, 0);
    assert!(resume.items.is_empty());
}

fn resume_position(store: &store::Store, user: UserId, media: MediaId, source: PhysicalSourceId) {
    store
        .upsert_unit_for_source(
            user,
            media,
            UnitKey::new(1, 2),
            source,
            20_000,
            Some(false),
            Some(true),
            Some(100_000),
            None,
            None,
            false,
            1000,
        )
        .unwrap();
}

fn member_with_hidden_resume(fixture: &Fixture) -> UserId {
    let handle = fixture.state.store();
    let store = handle.as_ref();
    let user = UserId::new();
    store
        .insert_user(&User {
            id: user,
            login: "member".into(),
            enabled: true,
            role: UserRole::Member,
        })
        .unwrap();
    resume_position(store, user, fixture.media, fixture.source.clone());
    let root = fixture._dir.path().join("hidden");
    std::fs::create_dir_all(&root).unwrap();
    store
        .create_library(
            domain::MediaKind::Tv,
            "Hidden",
            &[root.to_str().unwrap()],
            "members",
            true,
            &[],
        )
        .unwrap();
    let mut media = store.get_media(fixture.media).unwrap().unwrap();
    media.id = MediaId::new();
    media.title = "Hidden".into();
    store.insert_media(&media).unwrap();
    let path = root.join("hidden.mkv");
    std::fs::write(&path, b"hidden").unwrap();
    let file = publish_fixture_file_units(
        store,
        &FixtureFile {
            media_id: media.id,
            path: path.display().to_string(),
            ..Default::default()
        },
        &[UnitKey::new(1, 1), UnitKey::new(1, 2)],
    )
    .unwrap();
    resume_position(
        store,
        user,
        media.id,
        PhysicalSourceId {
            file_id: file.file_id,
            revision: file.version.revision,
        },
    );
    user
}

pub fn acl_and_user_state(runtime: &tokio::runtime::Runtime) {
    let fixture = Fixture::new(4);
    let member = member_with_hidden_resume(&fixture);
    // Another User's whole assertion must not hide the member's candidates.
    mark(&fixture, -1, -1, true, 10_000);
    let provider = fixture.provider();
    let (next, resume) = runtime.block_on(async {
        (
            provider.next_up_items(member, None, 0, 1).await.unwrap(),
            provider
                .resumable_items_page(member, None, 0, 1)
                .await
                .unwrap(),
        )
    });
    for page in [&next, &resume] {
        assert_eq!(page.total, 1);
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].media.id, fixture.media);
        assert!(!page.items[0].played);
    }
    assert!(resume.items[0].metadata.is_favorite);
}
