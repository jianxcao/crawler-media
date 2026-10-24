use super::*;

#[test]
fn locale_change_keeps_existing_nfo_languages() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let library = store
        .create_library(MediaKind::Tv, "TV", &["/tv"], "everyone", true, &[])
        .unwrap();
    let lib_id: domain::LibraryId = library.id.parse().unwrap();
    // A scoped metadata input recorded in the old display language.
    {
        let conn = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
        let root_id = root_id_of(&store, &library.id, Path::new("/tv"));
        let media_id = MediaId::new();
        store
            .insert_media(&Media {
                id: media_id,
                kind: MediaKind::Tv,
                title: "Show".into(),
                year: Some(2020),
                original_title: None,
                aliases: Vec::new(),
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        conn.execute(
            "INSERT INTO library_metadata_inputs
                (library_id, media_id, entity_scope, season, episode, locale, input_key,
                 root_id, path_key, parse_state, source_version, fields_json,
                 present_fields_json, provenance_json, revision)
             VALUES (?1, ?2, 'work', -1, -1, 'zh-CN', 'tvshow.nfo', ?3, '/tv/tvshow.nfo',
                     'valid', 'v1', '{}', '[]', '{}', 1)",
            rusqlite::params![library.id, media_id.to_string(), root_id],
        )
        .unwrap();
    }

    let expected_revision = store.library_config_revision(&library.id).unwrap();
    let operation_id = uuid::Uuid::new_v4().to_string();
    store
        .begin_library_reconfiguration(
            &LibraryReconfiguration {
                operation_id: operation_id.clone(),
                library_id: lib_id,
                expected_revision,
                root_paths: None,
                display_locale: Some("en-US".into()),
            },
            3_000,
        )
        .unwrap();
    let status = store
        .advance_library_operation(&operation_id, 3_100, 100)
        .unwrap();
    assert_eq!(status.state, "succeeded");

    let locale: String = {
        let conn = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
        conn.query_row(
            "SELECT display_locale FROM libraries WHERE library_id = ?1",
            rusqlite::params![library.id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(locale, "en-US");
    let input_locale: String = {
        let conn = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
        conn.query_row(
            "SELECT locale FROM library_metadata_inputs WHERE library_id = ?1",
            rusqlite::params![library.id],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert_eq!(
        input_locale, "zh-CN",
        "switching the display locale must not re-label existing NFO observations"
    );
    assert_eq!(store.library_lifecycle_state(&library.id).unwrap(), "ready");
}

/// R06: a locale switch rebuilds every card in keyset batches; the operation only
/// succeeds once each Media's display title follows the new locale.
#[test]
fn locale_change_rebuilds_cards_in_batches_before_success() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let library = store
        .create_library(MediaKind::Movie, "Movies", &["/m"], "everyone", true, &[])
        .unwrap();
    let lib_id: domain::LibraryId = library.id.parse().unwrap();
    let root = root_id_of(&store, &library.id, Path::new("/m"));
    let ids = [
        "00000000-0000-0000-0000-0000000000e1",
        "00000000-0000-0000-0000-0000000000e2",
        "00000000-0000-0000-0000-0000000000e3",
    ];
    let movies: Vec<Media> = ids
        .iter()
        .map(|id| media(&store, id, MediaKind::Movie))
        .collect();
    {
        let conn = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
        for id in ids {
            for (locale, title) in [("zh-CN", "中文"), ("en-US", "English")] {
                conn.execute(
                    "INSERT INTO media_metadata
                        (media_id, locale, title, original_title, year, overview, release_date,
                         rating, runtime_minutes, language, genres_json, countries_json,
                         poster_url, backdrop_url, details_json, field_origins_json, revision)
                     VALUES (?1, ?2, ?3, NULL, 2024, NULL, NULL, NULL, NULL, NULL, '[]', '[]',
                             NULL, NULL, '{}', '{}', 1)",
                    rusqlite::params![id, locale, title],
                )
                .unwrap();
            }
        }
    }
    for (index, movie) in movies.iter().enumerate() {
        publish_one(&store, lib_id, movie, &root, &format!("/m/{index}.mkv"));
    }
    let titles = || -> Vec<String> {
        let conn = rusqlite::Connection::open(tmp.path().join("library.db")).unwrap();
        let mut stmt = conn
            .prepare("SELECT title FROM library_items WHERE library_id = ?1 ORDER BY media_id")
            .unwrap();
        stmt.query_map(rusqlite::params![library.id], |r| r.get(0))
            .unwrap()
            .collect::<Result<Vec<String>, _>>()
            .unwrap()
    };
    assert_eq!(titles(), vec!["中文"; 3], "cards start in the old locale");

    let operation_id = uuid::Uuid::new_v4().to_string();
    store
        .begin_library_reconfiguration(
            &LibraryReconfiguration {
                operation_id: operation_id.clone(),
                library_id: lib_id,
                expected_revision: store.library_config_revision(&library.id).unwrap(),
                root_paths: None,
                display_locale: Some("en-US".into()),
            },
            3_000,
        )
        .unwrap();
    let first = store
        .advance_library_operation(&operation_id, 3_100, 2)
        .unwrap();
    assert_ne!(
        first.state, "succeeded",
        "one batch of 2 cannot cover 3 Media"
    );
    drop(store);
    let store = Store::open(tmp.path()).unwrap();
    let mut state = first.state;
    for step in 0..5 {
        if state == "succeeded" {
            break;
        }
        state = store
            .advance_library_operation(&operation_id, 3_200 + step, 2)
            .unwrap()
            .state;
    }
    assert_eq!(state, "succeeded");
    assert_eq!(titles(), vec!["English"; 3]);
    assert_eq!(store.library_lifecycle_state(&library.id).unwrap(), "ready");
}
