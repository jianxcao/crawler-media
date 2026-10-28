use api::router;
use domain::MediaId;
use store::Store;

use super::common::{add_ledger, favorite, get, make_state, seed_library, seed_media, seed_member};

#[tokio::test]
async fn collection_gallery_reports_real_favorite_state() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let store = state.store();
    let library = seed_library(&store.lock(), tmp.path(), "collections");
    let (member, token) = seed_member(&store.lock(), "alice");
    let (favorited, plain, collection) = {
        let guard = store.lock();
        let favorited = seed_media(&guard, "Favorited", Some(2020));
        let plain = seed_media(&guard, "Plain", Some(2021));
        add_ledger(&guard, favorited, &library, "Favorited");
        add_ledger(&guard, plain, &library, "Plain");
        let collection = guard.create_collection(member, "混装").unwrap();
        guard
            .add_collection_item(&collection, &favorited.to_string())
            .unwrap();
        guard
            .add_collection_item(&collection, &plain.to_string())
            .unwrap();
        (favorited, plain, collection)
    };
    favorite(&store.lock(), member, favorited, true);

    let data = get(
        &app,
        &format!("/api/v1/collections/{collection}/gallery"),
        &token,
    )
    .await;
    let groups = data.as_array().unwrap();
    assert_eq!(groups.len(), 2, "{data}");
    let by_id = |id: MediaId| {
        groups
            .iter()
            .find(|group| group["media_item_id"] == id.to_string())
            .unwrap_or_else(|| panic!("missing {id}: {data}"))
            .clone()
    };
    assert_eq!(
        by_id(favorited)["is_favorite"],
        true,
        "favorited item must report true: {data}"
    );
    assert_eq!(
        by_id(plain)["is_favorite"],
        false,
        "unfavorited item must not be hardcoded true: {data}"
    );
}

#[tokio::test]
async fn favorites_sort_matches_existing_keys() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let store = state.store();
    let library = seed_library(&store.lock(), tmp.path(), "favorites");
    let (member, token) = seed_member(&store.lock(), "bob");
    let (alpha_old, alpha_new, beta, gamma) = {
        let guard = store.lock();
        let alpha_old = seed_media(&guard, "Alpha", Some(2019));
        let alpha_new = seed_media(&guard, "Alpha", Some(2024));
        let beta = seed_media(&guard, "Beta", Some(2021));
        let gamma = seed_media(&guard, "Gamma", Some(2022));
        for (id, name) in [
            (alpha_old, "alpha-old"),
            (alpha_new, "alpha-new"),
            (beta, "beta"),
            (gamma, "gamma"),
        ] {
            add_ledger(&guard, id, &library, name);
            favorite(&guard, member, id, true);
        }
        (alpha_old, alpha_new, beta, gamma)
    };

    let by_title = get(
        &app,
        "/api/v1/playback/favorites?sort=title&order=asc&limit=10",
        &token,
    )
    .await;
    let titles: Vec<&str> = by_title["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        vec!["Alpha", "Alpha", "Beta", "Gamma"],
        "{by_title}"
    );

    let alpha_ids = by_title["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["title"] == "Alpha")
        .map(|item| item["media_item_id"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    let mut expected = vec![alpha_new.to_string(), alpha_old.to_string()];
    expected.sort();
    assert_eq!(
        alpha_ids, expected,
        "same-key tie-break must stay id-ascending"
    );

    let by_year = get(
        &app,
        "/api/v1/playback/favorites?sort=year&order=asc&limit=10",
        &token,
    )
    .await;
    let years: Vec<u64> = by_year["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["year"].as_u64().unwrap())
        .collect();
    assert_eq!(years, vec![2019, 2021, 2022, 2024], "{by_year}");
    assert_eq!(by_year["total"], 4);
    let _ = (beta, gamma);
}

#[tokio::test]
async fn newest_favorite_leads_without_reordering_played_marks() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let store = state.store();
    let library = seed_library(&store.lock(), tmp.path(), "recent-favorites");
    let (member, token) = seed_member(&store.lock(), "dana");
    let (older, newer) = {
        let guard = store.lock();
        let older = seed_media(&guard, "Older favorite", Some(2020));
        let newer = seed_media(&guard, "Newer favorite", Some(2021));
        add_ledger(&guard, older, &library, "older");
        add_ledger(&guard, newer, &library, "newer");
        guard
            .upsert_unit(
                member,
                older,
                -1,
                -1,
                0,
                Some(true),
                Some(true),
                None,
                None,
                None,
                false,
                10,
            )
            .unwrap();
        guard
            .upsert_unit(
                member,
                newer,
                -1,
                -1,
                0,
                Some(true),
                Some(false),
                None,
                None,
                None,
                false,
                50,
            )
            .unwrap();
        guard
            .mutate_playback(
                newer,
                &store::playback::PlaybackMutation::marks(member, -1, -1, None, Some(true)),
                80_000,
            )
            .unwrap();
        (older, newer)
    };

    let favorites = get(&app, "/api/v1/playback/favorites?limit=10", &token).await;
    let ids: Vec<&str> = favorites["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["media_item_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids[0],
        newer.to_string(),
        "new favourite must lead: {favorites}"
    );
    let gallery = get(&app, "/api/v1/playback/favorites/gallery?limit=10", &token).await;
    assert_eq!(gallery[0]["media_item_id"], newer.to_string(), "{gallery}");
    let played = store.lock().user_active_media_ids(member).unwrap();
    assert_eq!(played[0].0, newer);
    assert_eq!(
        played[0].1, 50,
        "favourite activity must not reorder played marks"
    );
    let _ = older;
}

#[tokio::test]
async fn playing_an_old_favorite_does_not_move_it_ahead_of_a_newer_one() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let store = state.store();
    let library = seed_library(&store.lock(), tmp.path(), "stable-favorites");
    let (member, token) = seed_member(&store.lock(), "erin");
    let (historical, recent) = {
        let guard = store.lock();
        let historical = seed_media(&guard, "Historical favorite", Some(2018));
        let recent = seed_media(&guard, "Recent favorite", Some(2024));
        add_ledger(&guard, historical, &library, "historical");
        add_ledger(&guard, recent, &library, "recent");
        guard
            .upsert_unit(
                member,
                historical,
                -1,
                -1,
                0,
                Some(false),
                Some(true),
                None,
                None,
                None,
                false,
                10,
            )
            .unwrap();
        guard
            .mutate_playback(
                recent,
                &store::playback::PlaybackMutation::marks(member, -1, -1, None, Some(true)),
                50_000,
            )
            .unwrap();
        (historical, recent)
    };
    {
        let guard = store.lock();
        guard
            .upsert_unit(
                member, historical, -1, -1, 90_000, None, None, None, None, None, false, 200,
            )
            .unwrap();
    }
    {
        let conn = rusqlite::Connection::open(tmp.path().join("data/library.db")).unwrap();
        conn.execute(
            "UPDATE playback_units SET favorite_at = 0
             WHERE user_id = ?1 AND media_id = ?2",
            rusqlite::params![member.to_string(), historical.to_string()],
        )
        .unwrap();
    }

    let favorites = get(&app, "/api/v1/playback/favorites?limit=10", &token).await;
    let ids: Vec<&str> = favorites["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["media_item_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids[0],
        recent.to_string(),
        "playback must not promote an old favourite: {favorites}"
    );
    let gallery = get(&app, "/api/v1/playback/favorites/gallery?limit=10", &token).await;
    assert_eq!(gallery[0]["media_item_id"], recent.to_string(), "{gallery}");

    drop(store);
    drop(state);
    let reopened = Store::open(tmp.path().join("data")).unwrap();
    let rows = reopened.favorite_rows(member).unwrap();
    assert_eq!(
        rows[0].0, recent,
        "reopening must not capture playback as favourite time: {rows:?}"
    );
    assert_eq!(
        reopened
            .unit_state(member, historical, -1, -1)
            .unwrap()
            .unwrap()
            .favorite_at,
        0,
        "an unknown historical favourite time must stay unknown"
    );
}

#[tokio::test]
async fn favorite_coverage_spans_every_visible_library_holding_the_media() {
    let tmp = tempfile::tempdir().unwrap();
    let state = make_state(tmp.path());
    let app = router(state.clone());
    let store = state.store();
    let first = seed_library(&store.lock(), tmp.path(), "first");
    let second = seed_library(&store.lock(), tmp.path(), "second");
    let (member, token) = seed_member(&store.lock(), "carol");
    let media = {
        let guard = store.lock();
        let media = seed_media(&guard, "Twice", Some(2020));
        add_ledger(&guard, media, &first, "twice-a");
        add_ledger(&guard, media, &second, "twice-b");
        media
    };
    favorite(&store.lock(), member, media, true);
    let data = get(&app, "/api/v1/playback/favorites", &token).await;
    assert_eq!(data["total"], 1, "{data}");
    assert_eq!(data["items"][0]["file_count"], 2, "{data}");
    let gallery = get(&app, "/api/v1/playback/favorites/gallery", &token).await;
    assert_eq!(gallery.as_array().map(Vec::len), Some(1), "{gallery}");
}
