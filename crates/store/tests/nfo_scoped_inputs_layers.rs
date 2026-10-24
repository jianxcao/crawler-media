use std::path::{Path, PathBuf};

use domain::{
    Confidence, EntityScope, FieldValue, FilePresence, FilePublication, FileVersion, LibraryFile,
    LibraryId, MediaId, MediaKind, QualitySource, UnitKey,
};
use store::Store;
use store::library_write::{FilePublication as StorePublication, WriteScope};

fn library_with_root(store: &Store, dir: &Path, name: &str, kind: MediaKind) -> (String, String) {
    std::fs::create_dir_all(dir).unwrap();
    let library = store.create_library(kind, name, &[dir.to_str().unwrap()], "everyone", true, &[]).unwrap();
    let root_id = store.list_library_roots().unwrap().into_iter()
        .find(|root| root.library_id.as_deref() == Some(&library.id)).unwrap().id;
    (library.id, root_id)
}

fn insert_media(store: &Store, kind: MediaKind, title: &str) -> MediaId {
    let media = domain::Media {
        id: MediaId::new(), kind, title: title.into(), year: None, original_title: None,
        aliases: vec![], tmdb_id: None, douban_id: None, tvdb_id: None, bangumi_id: None, anilist_id: None,
    };
    store.insert_media(&media).unwrap();
    media.id
}

fn video_file(library_id: &str, root_id: &str, media_id: MediaId, kind: MediaKind, path: PathBuf) -> LibraryFile {
    LibraryFile {
        file_id: domain::LedgerId::new(), library_id: library_id.parse::<LibraryId>().unwrap(),
        root_id: root_id.to_string(), media_id, kind, path,
        version: FileVersion { revision: 1, source_version: "v1".into() },
        presence: FilePresence::Present, publication: FilePublication::Published,
        size_bytes: Some(5), stat_known: true, created_ns: None, modified_ns: None,
        source_digest: None, source_path: None, transfer_mode: None, resolution: None,
        codec: None, hdr: None, quality_source: QualitySource::Release, confidence: Confidence::High,
        filter_score: None, release_quality: None, duration_ms: None, tracks_version: 0,
        poster_path: None, backdrop_path: None, still_path: None,
    }
}

fn overview_of(store: &Store, library: &str, media: MediaId, locale: &str) -> Option<String> {
    store.library_item_detail(library.parse().unwrap(), media, locale).unwrap().unwrap()
        .metadata.overview.value().cloned()
}

fn publish(store: &Store, scope: &WriteScope, file: &LibraryFile, units: &[UnitKey], locale: &str) -> Vec<store::metadata::NfoObservation> {
    let observations = store.observe_nfo_inputs(&file.path, file.kind, units, locale, &file.version, scope.config_revision);
    store.publish_files(scope, &[StorePublication { file, units, expected_version: None, nfo_observation: Some(&observations), probe_required: false }], scope.config_revision).unwrap();
    observations
}

#[test]
fn unnumbered_tv_file_does_not_create_subscription_coverage() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (library, root) = library_with_root(&store, &tmp.path().join("tv"), "TV", MediaKind::Tv);
    let media = insert_media(&store, MediaKind::Tv, "Unnumbered");
    let file = video_file(
        &library,
        &root,
        media,
        MediaKind::Tv,
        tmp.path().join("tv/Unnumbered.mkv"),
    );
    let scope = WriteScope {
        library_id: library.parse().unwrap(),
        config_revision: 0,
    };
    publish(&store, &scope, &file, &[], "zh-CN");
    let subscribe = domain::Subscribe {
        id: domain::SubscribeId::new(),
        user_id: domain::UserId::new(),
        media_id: media,
        coverage: domain::Coverage::Tv {
            season: 1,
            episode_from: 1,
            episode_to: Some(2),
        },
        fetch_mode: domain::FetchMode::Search,
        filter_id: domain::FilterId::new(),
        wash_cut: false,
        wash_cut_filter_id: None,
        keep_old_versions: false,
        full_season_pack: false,
        downloader_id: None,
        library_id: Some(scope.library_id),
        tracking_state: "active".into(),
        follow_future: false,
        search_interval_secs: 1800,
    };

    let facts = store
        .load_library_subscribe_facts(&subscribe, MediaKind::Tv)
        .unwrap();
    assert!(facts.entries().next().is_none());
    assert!(store.get_file_units(file.file_id).unwrap().is_empty());
    let item = store
        .get_library_item(scope.library_id, media)
        .unwrap()
        .unwrap();
    assert_eq!((item.file_count, item.distinct_episode_count), (1, 0));
}

/// R06: an NFO without its own language stays bound to the locale it was imported in.
/// Rescans after a display-locale switch (same bytes or edited) keep that binding; only
/// a sidecar seen for the first time adopts the current display locale.
#[test]
fn imported_nfo_keeps_its_locale_across_display_locale_switch() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (library, root) = library_with_root(&store, &tmp.path().join("m"), "M", MediaKind::Movie);
    let media = insert_media(&store, MediaKind::Movie, "M");
    let file = video_file(
        &library,
        &root,
        media,
        MediaKind::Movie,
        tmp.path().join("m/M.mkv"),
    );
    std::fs::write(&file.path, b"bytes").unwrap();
    let nfo = file.path.with_extension("nfo");
    std::fs::write(&nfo, "<movie><plot>Plot zh</plot></movie>").unwrap();
    let scope = WriteScope {
        library_id: library.parse().unwrap(),
        config_revision: 0,
    };
    publish(&store, &scope, &file, &[UnitKey::WHOLE], "zh-CN");
    let db = tmp.path().join("library.db");
    let locales = || -> Vec<String> {
        let conn = rusqlite::Connection::open(&db).unwrap();
        let mut stmt = conn
            .prepare("SELECT locale FROM library_metadata_inputs WHERE library_id = ?1")
            .unwrap();
        stmt.query_map([&library], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<String>, _>>()
            .unwrap()
    };
    assert_eq!(locales(), vec!["zh-CN"], "the initial import is real");
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute(
            "UPDATE libraries SET display_locale = 'en-US' WHERE library_id = ?1",
            [&library],
        )
        .unwrap();

    store.reconcile_nfo_inputs(&file.path).unwrap();
    assert_eq!(
        locales(),
        vec!["zh-CN"],
        "same-bytes rescan keeps the import locale"
    );
    std::fs::write(&nfo, "<movie><plot>Plot zh edited</plot></movie>").unwrap();
    store.reconcile_nfo_inputs(&file.path).unwrap();
    assert_eq!(locales(), vec!["zh-CN"], "an edit keeps the import locale");
    drop(store);
    let store = Store::open(tmp.path()).unwrap();
    store.reconcile_nfo_inputs(&file.path).unwrap();
    assert_eq!(locales(), vec!["zh-CN"], "restart keeps the import locale");
    assert_eq!(
        overview_of(&store, &library, media, "zh-CN").as_deref(),
        Some("Plot zh edited")
    );

    std::fs::remove_file(&nfo).unwrap();
    store.reconcile_nfo_inputs(&file.path).unwrap();
    assert!(locales().is_empty(), "a removed sidecar releases its input");
    std::fs::write(&nfo, "<movie><plot>Plot en</plot></movie>").unwrap();
    store.reconcile_nfo_inputs(&file.path).unwrap();
    assert_eq!(
        locales(),
        vec!["en-US"],
        "a new sidecar adopts the current locale"
    );
}

/// C07: carving a nested Library out of a parent moves the sidecar inputs under the new
/// root together with the files (keeping their imported locale), while a work-level
/// tvshow.nfo shared with files that stay in the parent keeps its parent ownership.
#[test]
fn nested_library_carries_scoped_inputs_and_keeps_shared_parent_nfo() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let shows = tmp.path().join("shows");
    let (parent, root_id) = library_with_root(&store, &shows, "Shows", MediaKind::Tv);
    let media = insert_media(&store, MediaKind::Tv, "Show");
    let show = shows.join("Show");
    let season1 = show.join("Season 01");
    let season2 = show.join("Season 02");
    std::fs::create_dir_all(&season1).unwrap();
    std::fs::create_dir_all(&season2).unwrap();
    std::fs::write(
        show.join("tvshow.nfo"),
        "<tvshow><title>Work</title></tvshow>",
    )
    .unwrap();
    std::fs::write(
        season1.join("season.nfo"),
        "<season><title>Season one</title></season>",
    )
    .unwrap();
    let e01 = season1.join("Show.S01E01.mkv");
    std::fs::write(&e01, b"e01").unwrap();
    std::fs::write(
        season1.join("Show.S01E01.nfo"),
        "<episodedetails><season>1</season><episode>1</episode><title>One</title></episodedetails>",
    )
    .unwrap();
    let e21 = season2.join("Show.S02E01.mkv");
    std::fs::write(&e21, b"e21").unwrap();
    let scope = WriteScope {
        library_id: parent.parse().unwrap(),
        config_revision: 0,
    };
    let first = video_file(&parent, &root_id, media, MediaKind::Tv, e01);
    publish(&store, &scope, &first, &[UnitKey::new(1, 1)], "en-US");
    let second = video_file(&parent, &root_id, media, MediaKind::Tv, e21);
    publish(&store, &scope, &second, &[UnitKey::new(2, 1)], "en-US");

    let nested = store
        .create_library(
            MediaKind::Tv,
            "Season one",
            &[season1.to_str().unwrap()],
            "everyone",
            true,
            &[],
        )
        .unwrap();

    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    let inputs: Vec<(String, String, String)> = db
        .prepare(
            "SELECT library_id, entity_scope, locale FROM library_metadata_inputs
             ORDER BY entity_scope",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let owner = |scope: &str| -> Vec<(String, String)> {
        inputs
            .iter()
            .filter(|(_, s, _)| s == scope)
            .map(|(l, _, locale)| (l.clone(), locale.clone()))
            .collect()
    };
    assert_eq!(
        owner("episode"),
        vec![(nested.id.clone(), "en-US".to_string())]
    );
    assert_eq!(
        owner("season"),
        vec![(nested.id.clone(), "en-US".to_string())]
    );
    // The shared tvshow.nfo still describes the files that stay in the parent, so the
    // parent keeps it; the nested Library gets its own copy (same locale) because its
    // files are described by it too (A6).
    let mut work = owner("work");
    work.sort();
    let mut expected = vec![
        (parent.clone(), "en-US".to_string()),
        (nested.id.clone(), "en-US".to_string()),
    ];
    expected.sort();
    assert_eq!(work, expected);
    let title = |library: &str| -> String {
        db.query_row(
            "SELECT title FROM library_items WHERE library_id = ?1",
            [library],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        title(&nested.id),
        title(&parent),
        "nested card is composed from the same work facts as the parent"
    );
}

/// F10: a root registered through a symlink alias must persist the sidecar under the
/// same canonical key as the video `path_key`. Display spelling is for IO only — the
/// first full reduce must not treat a real input as an orphan.
#[test]
fn aliased_root_persists_canonical_nfo_input_and_detail() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    let alias = tmp.path().join("alias");
    std::fs::create_dir_all(&real).unwrap();
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let (library, root_id) = library_with_root(&store, &alias, "Aliased", MediaKind::Movie);
    let media = insert_media(&store, MediaKind::Movie, "Aliased");
    let video = alias.join("A.mkv");
    std::fs::write(&video, b"bytes").unwrap();
    std::fs::write(
        alias.join("A.nfo"),
        "<movie><plot>Alias plot</plot></movie>",
    )
    .unwrap();
    let file = video_file(&library, &root_id, media, MediaKind::Movie, video.clone());
    let scope = WriteScope {
        library_id: library.parse().unwrap(),
        config_revision: 0,
    };
    let observations = publish(&store, &scope, &file, &[UnitKey::WHOLE], "zh-CN");
    let valid: Vec<_> = observations
        .iter()
        .filter(|observation| observation.state == store::metadata::NfoParseState::Valid)
        .collect();
    assert_eq!(valid.len(), 1, "{observations:#?}");

    let db = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
    let (input_key, path_key, video_key): (String, String, String) = db
        .query_row(
            "SELECT i.input_key, i.path_key, f.path_key
             FROM library_metadata_inputs i
             JOIN library_files f ON f.library_id = i.library_id AND f.media_id = i.media_id
             WHERE i.parse_state = 'valid'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("aliased publish must persist the valid NFO input");
    let real_prefix = std::fs::canonicalize(tmp.path()).unwrap();
    assert!(
        input_key.starts_with(real_prefix.to_str().unwrap()),
        "input_key must be canonical, got {input_key}"
    );
    assert!(
        !input_key.contains("/alias/"),
        "input_key must not keep the alias spelling: {input_key}"
    );
    assert!(
        path_key.starts_with(real_prefix.to_str().unwrap()),
        "video path_key on the input row must be canonical, got {path_key} vs {video_key}"
    );
    assert_eq!(
        overview_of(&store, &library, media, "zh-CN").as_deref(),
        Some("Alias plot")
    );

    std::fs::write(
        alias.join("A.nfo"),
        "<movie><plot>Alias plot edited</plot></movie>",
    )
    .unwrap();
    assert!(store.reconcile_nfo_inputs(&video).unwrap());
    assert_eq!(
        overview_of(&store, &library, media, "zh-CN").as_deref(),
        Some("Alias plot edited")
    );
    let persisted: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM library_metadata_inputs WHERE parse_state = 'valid'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(persisted, 1, "an edit keeps one canonical input");

    std::fs::remove_file(alias.join("A.nfo")).unwrap();
    assert!(
        store.reconcile_nfo_inputs(&video).unwrap(),
        "deleting through the alias spelling releases the input"
    );
    assert_eq!(overview_of(&store, &library, media, "zh-CN"), None);
    let persisted: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM library_metadata_inputs",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(persisted, 0);

    std::fs::write(
        real.join("A.nfo"),
        "<movie><plot>Real plot</plot></movie>",
    )
    .unwrap();
    let real_video = real.join("A.mkv");
    assert!(
        store.reconcile_nfo_inputs(&real_video).unwrap(),
        "the real spelling of the same file must record the new sidecar"
    );
    assert_eq!(
        overview_of(&store, &library, media, "zh-CN").as_deref(),
        Some("Real plot")
    );
    assert!(
        !store.reconcile_nfo_inputs(&video).unwrap(),
        "rescanning the alias spelling of an unchanged sidecar is the same input"
    );
    assert_eq!(
        overview_of(&store, &library, media, "zh-CN").as_deref(),
        Some("Real plot")
    );
}
