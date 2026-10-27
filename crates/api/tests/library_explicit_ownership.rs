//! A published file's owner is its recorded `library_files.library_id`.
//!
//! A root can be renamed or removed and leave a published row behind whose path
//! no longer lies under any root. Such a file must keep the Library that recorded
//! it: it may not fall into whichever Library happens to match the path, and it
//! may never inherit the default Library's ACL.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use api::{ApiState, Store};
use domain::{
    Confidence, LedgerId, LibraryFile, LibraryId, Media, MediaId, MediaKind, QualitySource, User,
    UserId, UserRole,
};
use downloader::MemoryDownloader;
use indexer::{FetchRequest, Fetcher, IndexerError, ProfileSet};
use media_server::provider::MediaServerProvider;
use store::test_support::{FixtureFile, publish_fixture_file};

struct NoFetch;

impl Fetcher for NoFetch {
    fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
        Err(IndexerError::Fetch("offline fixture".into()))
    }
}

fn make_state(root: &Path) -> ApiState {
    ApiState::new(
        Store::open(root.join("data")).unwrap(),
        "management-secret".into(),
        ProfileSet::load(None).unwrap(),
        Arc::new(NoFetch),
        Arc::new(MemoryDownloader::new(root.join("stage"))),
        root.join("library"),
    )
    .unwrap()
}

fn movie(title: &str) -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: title.into(),
        year: Some(2024),
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

fn member(store: &Store, login: &str) -> User {
    let user = User {
        id: UserId::new(),
        login: login.into(),
        enabled: true,
        role: UserRole::Member,
    };
    store.insert_user(&user).unwrap();
    user
}

fn publish(store: &Store, media: &Media, path: &Path) -> LibraryFile {
    publish_fixture_file(
        store,
        &FixtureFile {
            file_id: Some(LedgerId::new()),
            media_id: media.id,
            path: path.display().to_string(),
            season: None,
            episode: None,
            resolution: Some("1080p".into()),
            codec: Some("h264".into()),
            hdr: None,
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: None,
            ..Default::default()
        },
    )
    .unwrap()
}

fn default_movie_library(store: &Store) -> String {
    store
        .default_library(MediaKind::Movie)
        .unwrap()
        .expect("seeded default movie Library")
        .id
}

/// The row now points outside every root while still recording `library_id`:
/// the state a root rename, a root removal or a rebuild can leave behind.
fn orphan(store: &Store, file: &LibraryFile, outside: &Path) -> LibraryFile {
    assert!(
        store::test_support::force_file_path(
            store,
            &file.path.display().to_string(),
            &outside.display().to_string(),
        )
        .unwrap()
    );
    let mut moved = file.clone();
    moved.path = outside.to_path_buf();
    moved
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orphaned_published_file_keeps_its_recorded_library() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let store = state.store();
    let (member_user, outsider, library, media, file) = {
        let guard = store.lock();
        let member_user = member(&guard, "member");
        let outsider = member(&guard, "outsider");
        let root = tmp.path().join("library-a");
        std::fs::create_dir_all(&root).unwrap();
        // Library A is restricted to `member_user`; the seeded default movie
        // Library B is visible to everyone.
        let library = guard
            .create_library(
                MediaKind::Movie,
                "A",
                &[root.to_str().unwrap()],
                "selected",
                true,
                &[member_user.id],
            )
            .unwrap();
        let media = movie("Secret");
        guard.insert_media(&media).unwrap();
        let file = publish(&guard, &media, &root.join("secret.mkv"));
        assert_eq!(file.library_id, library.id.parse::<LibraryId>().unwrap());
        let outside = tmp.path().join("moved-away").join("secret.mkv");
        let file = orphan(&guard, &file, &outside);
        (member_user.id, outsider.id, library, media, file)
    };
    assert_ne!(
        library.id,
        default_movie_library(&store.lock()),
        "the restricted Library must not be the default one"
    );
    assert_eq!(file.library_id.to_string(), library.id);

    let provider = api::media_server_provider::ApiServerProvider::new(state);

    let visible_to_member = provider.list_visible_items(member_user, None).await.unwrap();
    let member_item = visible_to_member
        .iter()
        .find(|item| item.media.id == media.id)
        .expect("the owner's member must still see the published file");
    assert_eq!(
        member_item.parent_id, library.id,
        "an orphaned published file must stay in the Library that recorded it"
    );

    let visible_to_outsider = provider.list_visible_items(outsider, None).await.unwrap();
    assert!(
        visible_to_outsider
            .iter()
            .all(|item| item.media.id != media.id),
        "a file whose owner is Library A must not reach a user who can only see the default Library"
    );
}

#[test]
fn delivery_targets_files_by_library_id() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("library-a");
    std::fs::create_dir_all(&root).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "A",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let media = movie("Twice");
    store.insert_media(&media).unwrap();

    // The same Media has a file in A and a file in the default Library B.
    let in_a = publish(&store, &media, &root.join("twice.mkv"));
    let default_root = PathBuf::from(
        store
            .default_library(MediaKind::Movie)
            .unwrap()
            .unwrap()
            .root_paths
            .first()
            .unwrap(),
    );
    std::fs::create_dir_all(&default_root).unwrap();
    let in_b = publish(&store, &media, &default_root.join("twice.mkv"));
    assert_ne!(in_a.library_id, in_b.library_id);
    // A's file is orphaned: its path no longer lies under any root.
    let orphaned = orphan(&store, &in_a, &tmp.path().join("elsewhere").join("twice.mkv"));

    // Delivery resolves its sources from the target Library's recorded files.
    let a_sources = store
        .file_coverage_in_library(
            library.id.parse::<LibraryId>().unwrap(),
            Some(media.id),
            store::FileReadScope::AllRecorded,
        )
        .unwrap();
    assert_eq!(
        a_sources
            .iter()
            .map(|coverage| coverage.file.file_id)
            .collect::<Vec<_>>(),
        vec![orphaned.file_id],
        "delivering for Library A must use A's recorded file, even when its path left A's roots"
    );

    let b_sources = store
        .file_coverage_in_library(
            in_b.library_id,
            Some(media.id),
            store::FileReadScope::AllRecorded,
        )
        .unwrap();
    assert_eq!(
        b_sources
            .iter()
            .map(|coverage| coverage.file.file_id)
            .collect::<Vec<_>>(),
        vec![in_b.file_id],
        "Library B must not receive Library A's file as a delivery source"
    );
}

#[test]
fn new_file_outside_roots_is_unclassified() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path().join("data")).unwrap();
    let root = tmp.path().join("library-a");
    std::fs::create_dir_all(&root).unwrap();
    let library = store
        .create_library(
            MediaKind::Movie,
            "A",
            &[root.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let libraries = store.list_libraries().unwrap();

    let outside = tmp.path().join("outside").join("new.mkv");
    assert!(
        store::classify_new_file(&libraries, &outside, MediaKind::Movie).is_none(),
        "a new file outside every root has no Library; it must not fall back to the default one"
    );

    let inside = root.join("new.mkv");
    assert_eq!(
        store::classify_new_file(&libraries, &inside, MediaKind::Movie).map(|owner| owner.id.clone()),
        Some(library.id),
        "a new file under a single root classifies to that Library"
    );
}
