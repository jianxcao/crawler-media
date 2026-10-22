//! 分页/过滤/展开的等价性与工作量：重元数据只为选中的页构建。

use super::*;

#[tokio::test]
async fn jellyfin_limit_one_builds_one_heavy_snapshot() {
    let fixture = tv_fixture(200, 10);
    let app = router(fixture.state.clone());
    let listing = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&Limit=1&SortBy=SortName",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(items(&listing).len(), 1);
    assert_eq!(listing["TotalRecordCount"], 200);
    assert_eq!(items(&listing)[0]["Name"], "Show199", "缺省 SortName 倒序");
    assert_eq!(
        fixture.files.subtitles(),
        1,
        "limit=1 只应构建一条重元数据（2000 条候选全部 enrich 是旧行为）"
    );
    // stat 只覆盖本页条目自己的行（1 部剧 × 10 集 = 10），不是全部 2000 条候选。
    assert!(
        fixture.files.metadata() <= 10,
        "stat 必须限于本页条目自己的行，实际 {}",
        fixture.files.metadata()
    );

    fixture.files.reset();
    let page = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&Limit=20&SortBy=SortName",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(items(&page).len(), 20);
    assert_eq!(page["TotalRecordCount"], 200);
    assert_eq!(fixture.files.subtitles(), 20, "重元数据条数 = 本页条数");
}

/// 页外的重事实损坏（非过滤字段）不得影响当前页。
#[tokio::test]
async fn corrupted_heavy_facts_outside_the_page_do_not_fail_the_page() {
    let fixture = tv_fixture(4, 2);
    let app = router(fixture.state.clone());
    let out_of_page = {
        let store = fixture.state.store();
        let store = store.lock();
        let rows: Vec<_> = store
            .list_file_coverage()
            .unwrap()
            .into_iter()
            .filter(|row| row.file.path.to_string_lossy().contains("Show000"))
            .collect();
        assert!(!rows.is_empty(), "Show000 rows");
        let conn = rusqlite::Connection::open(fixture.dir.path().join("data/library.db")).unwrap();
        for row in &rows {
            conn.execute(
                "INSERT INTO file_details
                     (file_id, source_revision, source_version, tracks_version,
                      audio_json, subtitle_json, chapters_json)
                 SELECT file_id, file_revision, source_version, 0, '[]', '[]', x'5b5d'
                 FROM library_files WHERE file_id = ?1",
                [row.file.file_id.to_string()],
            )
            .unwrap();
        }
        rows[0].file.file_id
    };
    assert!(!out_of_page.to_string().is_empty());
    let listing = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&Limit=1&SortBy=SortName",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(listing["TotalRecordCount"], 4);
    assert_eq!(items(&listing)[0]["Name"], "Show003");
    let (status, body) = get(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series&SortBy=SortName&SortOrder=Ascending&Limit=1",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "选中损坏行必须失败：{body}");
}

/// SCOPE-6-01：库作用域 + `SeriesId` 就是「列这部剧的分集」，与 `IncludeItemTypes`
/// 无关，并且仍然受库归属约束。

#[tokio::test]
async fn filter_before_paging_keeps_names_order_and_count() {
    // genre/studio 是库内合成的 NFO 事实，sidecar 必须在发布前就位。
    let fixture = tv_fixture_seeded(6, 2, |show_dir| {
        let name = show_dir.file_name().unwrap().to_str().unwrap();
        let genre = if name.ends_with('0') || name.ends_with('2') {
            "Drama"
        } else {
            "Comedy"
        };
        std::fs::write(
            show_dir.join("tvshow.nfo"),
            format!("<tvshow><genre>{genre}</genre><studio>Studio X</studio></tvshow>"),
        )
        .unwrap();
    });
    let app = router(fixture.state.clone());
    let drama = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series&Genres=Drama&Limit=1&SortBy=SortName",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(drama["TotalRecordCount"], 2, "{drama}");
    assert_eq!(items(&drama).len(), 1);

    let studio = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series&StudioIds=Studio%20X&SortBy=SortName",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(studio["TotalRecordCount"], 6, "{studio}");

    // StartIndex 越界与 ties：两次请求结果一致（稳定 tie-break）。
    let first = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series&SortBy=SortName&StartIndex=5",
            fixture.library_id
        ),
    )
    .await;
    let again = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series&SortBy=SortName&StartIndex=5",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(first, again, "同一 fixture 的次序必须稳定");
    assert_eq!(items(&first).len(), 1);
    assert_eq!(first["TotalRecordCount"], 6);
}

/// 分集展开 + folder 归属：folder 作用域只列该根下的分集。
#[tokio::test]
async fn episode_expansion_stays_inside_the_folder() {
    let dir = tempfile::tempdir().unwrap();
    let files = Arc::new(CountingFiles::default());
    let state = new_state(dir.path(), files);
    let root_a = dir.path().join("folder-a");
    let root_b = dir.path().join("folder-b");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();
    let show_media = {
        let store = state.store();
        let store = store.lock();
        store
            .create_library(
                MediaKind::Tv,
                "Folders",
                &[root_a.to_str().unwrap(), root_b.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media_id = new_media(&store, "Folder show");
        insert_episode(&store, &root_a, media_id, 1, 1);
        insert_episode(&store, &root_b, media_id, 1, 2);
        media_id
    };
    let app = router(state);
    let listing = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Episode",
            folder_id(&root_b)
        ),
    )
    .await;
    assert_eq!(listing["TotalRecordCount"], 1, "{listing}");
    assert_eq!(items(&listing)[0]["Id"], unit_wire_id(show_media, 1, 2));

    let whole = get_json(&app, "/Items?Recursive=true&IncludeItemTypes=Episode").await;
    assert_eq!(whole["TotalRecordCount"], 2);
    assert!(items(&whole).iter().any(|item| item["Id"] == unit_wire_id(show_media, 1, 1)));

    // 剧 parent（不给 IncludeItemTypes）就是「列这部剧的分集」：一行一条，不折叠成剧。
    let by_show = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true",
            show_media.to_string().replace('-', "")
        ),
    )
    .await;
    assert_eq!(by_show["TotalRecordCount"], 2, "{by_show}");
    assert!(items(&by_show).iter().all(|item| item["Type"] == "Episode"));
}

/// 认不出的 parent 一律空集合（绝不退化成整库），未知 Media 同样如此。
#[tokio::test]
async fn unknown_parent_returns_an_empty_page() {
    let fixture = tv_fixture(2, 1);
    let app = router(fixture.state.clone());
    let unknown = get_json(&app, "/Items?ParentId=does-not-exist&Recursive=true").await;
    assert_eq!(unknown["TotalRecordCount"], 0, "{unknown}");
    assert!(items(&unknown).is_empty(), "{unknown}");
    let movie_shaped = get_json(&app, &format!("/Items?ParentId={}", MediaId::new())).await;
    assert_eq!(movie_shaped["TotalRecordCount"], 0, "{movie_shaped}");
    assert!(items(&movie_shaped).is_empty(), "{movie_shaped}");
    let unknown_folder = get_json(
        &app,
        &format!(
            "/Items?ParentId={}",
            folder_id(Path::new("/nonexistent-root"))
        ),
    )
    .await;
    assert_eq!(unknown_folder["TotalRecordCount"], 0, "{unknown_folder}");
    assert!(items(&unknown_folder).is_empty(), "{unknown_folder}");
}

/// 最新列表：缺省只排不筛（已看完沉底），显式 `IsPlayed` 才严格筛。
#[tokio::test]
async fn latest_items_keeps_played_last_by_default_and_filters_when_asked() {
    let fixture = tv_fixture(3, 1);
    let app = router(fixture.state.clone());
    let user = {
        let store = fixture.state.store();
        let store = store.lock();
        store.admin_user_id().unwrap()
    };
    let first_show = {
        let store = fixture.state.store();
        let store = store.lock();
        let row = store
            .list_file_coverage()
            .unwrap()
            .into_iter()
            .find(|row| row.file.path.to_string_lossy().contains("Show000"))
            .expect("Show000 row");
        store.upsert_unit(user, row.file.media_id, -1, -1, 0, Some(true), None, None, None, None, false, 10).unwrap();
        row.file.media_id
    };
    let latest = get_json(&app, "/Users/me/Items/Latest").await;
    let listed = latest.as_array().expect("latest array");
    assert_eq!(listed.len(), 3);
    let ids: Vec<&str> = listed.iter().map(|item| item["Id"].as_str().unwrap()).collect();
    let unique = {
        let mut copy = ids.clone();
        copy.sort_unstable();
        copy.dedup();
        copy.len()
    };
    assert_eq!(unique, ids.len(), "最新列表不得重复：{ids:?}");
    assert_eq!(
        listed.last().unwrap()["Id"],
        work_wire_id(first_show),
        "缺省：已看完沉底"
    );
    let played = get_json(&app, "/Users/me/Items/Latest?IsPlayed=true").await;
    assert_eq!(played.as_array().map(Vec::len), Some(1), "{played}");
}

/// NextUp：同一 Media 跨库的未看分集聚合成一条。

#[tokio::test]
async fn person_filter_selects_before_paging() {
    let fixture = tv_fixture(4, 1);
    let (person_id, matched) = {
        let store = fixture.state.store();
        let store = store.lock();
        let row = store
            .list_file_coverage()
            .unwrap()
            .into_iter()
            .find(|row| row.file.path.to_string_lossy().contains("Show000"))
            .expect("Show000 row");
        let dir = row.file.path.parent().unwrap();
        std::fs::write(
            dir.join("tvshow.nfo"),
            "<tvshow><actor><name>Alice</name><role>Lead</role></actor></tvshow>",
        )
        .unwrap();
        // R24: PersonIds reads observed local facts, never the live sidecar at query time.
        store.reconcile_nfo_inputs(&row.file.path).unwrap();
        let key = format!("name:{}:{}", "alice", "Actor");
        (
            uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, key.as_bytes()).to_string(),
            "Show000",
        )
    };
    let app = router(fixture.state.clone());
    // SortName 倒序下 Show000 排在最后：先选页再筛 person 会选中 Show003。
    let listing = get_json(
        &app,
        &format!(
            "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series&PersonIds={person_id}&Limit=1&SortBy=SortName",
            fixture.library_id
        ),
    )
    .await;
    assert_eq!(listing["TotalRecordCount"], 1, "{listing}");
    assert_eq!(items(&listing)[0]["Name"], matched, "{listing}");
}

/// 同名 ties 必须稳定：分页不得重复或漏条目。
#[tokio::test]
async fn name_ties_keep_a_stable_order() {
    let dir = tempfile::tempdir().unwrap();
    let files = Arc::new(CountingFiles::default());
    let state = new_state(dir.path(), files);
    let root = dir.path().join("ties");
    std::fs::create_dir_all(&root).unwrap();
    let library_id = {
        let store = state.store();
        let store = store.lock();
        let library = store
            .create_library(
                MediaKind::Tv,
                "Ties",
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap();
        let media_ids = [
            "00000000-0000-0000-0000-000000000003",
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000002",
        ];
        let mut expected = Vec::new();
        for (index, id) in media_ids.into_iter().enumerate() {
            let media_id = MediaId::from_uuid(uuid::Uuid::parse_str(id).unwrap());
            let media = Media {
                id: media_id,
                kind: MediaKind::Tv,
                title: "Same title".into(),
                year: Some(2024),
                original_title: None,
                aliases: Vec::new(),
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            };
            store.insert_media(&media).unwrap();
            let ledger_id = LedgerId::from_uuid(uuid::Uuid::parse_str(&format!("00000000-0000-0000-0000-00000000000{}", 3 - index)).unwrap());
            let show_dir = root.join(format!("same-{index}"));
            std::fs::create_dir_all(&show_dir).unwrap();
            let path = show_dir.join("01x01.mkv");
            std::fs::write(&path, b"episode").unwrap();
            store::test_support::publish_fixture_file(
                &store,
                &store::test_support::FixtureFile {
                    media_id,
                    path: path.display().to_string(),
                    season: Some(1),
                    episode: Some(1),
                    quality_source: QualitySource::Probe,
                    confidence: Confidence::High,
                    file_id: Some(ledger_id),
                    ..Default::default()
                },
            )
            .unwrap();
            expected.push(work_wire_id(media_id));
        }
        expected.sort();
        (library.id, expected)
    };
    let app = router(state);
    let (library_id, expected) = library_id;
    let first = get_json(
        &app,
        &format!(
            "/Items?ParentId={library_id}&Recursive=true&IncludeItemTypes=Series&SortBy=SortName&Limit=2"
        ),
    )
    .await;
    let rest = get_json(
        &app,
        &format!(
            "/Items?ParentId={library_id}&Recursive=true&IncludeItemTypes=Series&SortBy=SortName&StartIndex=2&Limit=2"
        ),
    )
    .await;
    let ids: Vec<String> = items(&first)
        .iter()
        .chain(items(&rest).iter())
        .map(|item| item["Id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        ids, expected,
        "同名次序必须是 ledger id 升序；去掉 tie-break 或先排序再断言都会放过错误实现"
    );
}

/// 一部剧的已采纳元数据（Task 7 采纳面）：简介/工作室/类型三个字段。
fn adopt_show_metadata(state: &ApiState, media: MediaId) {
    let fields = domain::MetadataFields {
        overview: domain::FieldValue::Value("adopted plot".into()),
        studios: domain::FieldValue::Value(vec!["Studio X".into()]),
        genres: domain::FieldValue::Value(vec![domain::MetadataGenre {
            source: "tmdb".into(),
            source_id: None,
            name: "Drama".into(),
            filter_key: "drama".into(),
        }]),
        ..Default::default()
    };
    let adoption = domain::MetadataAdoption {
        media_id: media,
        scope: domain::EntityScope::Work,
        locale: "zh-CN".into(),
        source: "tmdb".into(),
        provider_entity_id: "100".into(),
        provider_work_id: "100".into(),
        fetched_at: 1,
        normalized: domain::NormalizedMetadata {
            fields,
            field_origins: std::collections::BTreeMap::new(),
        },
        expected_revision: None,
        preferences: domain::MetadataPreferences {
            language_priority: vec!["zh-CN".into()],
            cert_country_priority: Vec::new(),
        },
    };
    let store = state.store();
    let store = store.lock();
    store.adopt_metadata(&adoption, 1).unwrap();
}

/// 页内元数据取库内快照（Task 8 观测 + Task 7 采纳），现场 NFO 不参与渲染。
///
/// 观测输入只在 `publish_fixture_file` 时采集，所以 `publish_fixture_file` 之后写下的 sidecar 只可能
/// 被「现场读 NFO」看到；坏 sidecar 也不得改变同一页的响应。
#[tokio::test]
async fn page_metadata_uses_composed_snapshot_and_ignores_live_nfo() {
    let fixture = tv_fixture(1, 1);
    let media = media_for_title(&fixture.state, "Show000");
    adopt_show_metadata(&fixture.state, media);
    let sidecar = fixture.roots[0].join("Show000").join("tvshow.nfo");
    std::fs::write(
        &sidecar,
        b"<tvshow><plot>live plot</plot><studio>Live Studio</studio><genre>Comedy</genre></tvshow>",
    )
    .unwrap();
    let app = router(fixture.state.clone());
    let uri = format!(
        "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series&SortBy=SortName",
        fixture.library_id
    );
    let listing = get_json(&app, &uri).await;
    assert_eq!(items(&listing).len(), 1, "{listing}");
    assert_eq!(
        items(&listing)[0]["Overview"],
        "adopted plot",
        "页内简介必须用库内合成值：{listing}"
    );
    assert_eq!(
        items(&listing)[0]["Studios"][0]["Name"],
        "Studio X",
        "页内工作室必须用库内合成值：{listing}"
    );
    assert_eq!(
        items(&listing)[0]["Genres"][0],
        "Drama",
        "页内类型必须用库内合成值：{listing}"
    );

    std::fs::write(&sidecar, b"<tvshow><plot>broken").unwrap();
    let after = get_json(&app, &uri).await;
    assert_eq!(after, listing, "坏 NFO 不得改变页内元数据");
}
