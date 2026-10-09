use api::fs_watcher::StrmGraceTracker;
use notify_debouncer_mini::DebouncedEvent;
use std::sync::Arc;

struct EmptyFetcher;

impl indexer::Fetcher for EmptyFetcher {
    fn fetch(&self, _request: &indexer::FetchRequest) -> Result<String, indexer::IndexerError> {
        Err(indexer::IndexerError::Fetch("unexpected request".into()))
    }
}

#[tokio::test]
async fn test_directory_deletion_removes_nested_strm_and_cleans_up_media() {
    let temp = tempfile::tempdir().unwrap();
    let library_root = temp.path().join("media/movie/china");
    let movie_dir = library_root.join("“骗骗”喜欢你 (2024)");
    std::fs::create_dir_all(&movie_dir).unwrap();

    let strm_file = movie_dir.join("“骗骗”喜欢你 (2024) - 2160p.strm");
    std::fs::write(&strm_file, "https://example.com/video.mkv").unwrap();

    let store = api::Store::open(temp.path().join("data")).unwrap();
    store
        .set_library_root(domain::MediaKind::Movie, library_root.to_str().unwrap())
        .unwrap();

    let state = api::management::ApiState::new(
        store,
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(EmptyFetcher),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();

    // 1. 入账并关联媒体
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Movie,
        title: "“骗骗”喜欢你".into(),
        year: Some(2024),
        original_title: None,
        tmdb_id: Some("12345".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    state.store().lock().insert_media(&media).unwrap();

    let row = domain::LedgerRow {
        id: domain::LedgerId::new(),
        media_id: media.id,
        path: strm_file.display().to_string(),
        season: None,
        episode: None,
        resolution: Some("2160p".into()),
        codec: None,
        hdr: None,
        quality_source: domain::QualitySource::Release,
        confidence: domain::Confidence::High,
        filter_score: None,
    };
    state.store().lock().insert_ledger(&row).unwrap();

    // 校验入账成功
    let rows = state.store().lock().list_ledger().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].path, strm_file.display().to_string());

    // 2. 模拟用户直接删除了整部剧/电影目录（包含 strm）
    std::fs::remove_dir_all(&movie_dir).unwrap();

    // 3. 文件系统监控捕获到 movie_dir 的删除事件
    let tracker = Arc::new(StrmGraceTracker::new());
    let events = vec![DebouncedEvent {
        path: movie_dir.clone(),
        kind: notify_debouncer_mini::DebouncedEventKind::Any,
    }];

    // 调用事件处理
    api::fs_watcher::handle_fs_events(&state, &tracker, events);

    // 4. 验证 ledger 中的该文件记录已被自动清除
    let remaining_rows = state.store().lock().list_ledger().unwrap();
    assert!(
        remaining_rows.is_empty(),
        "整目录删除后，该目录下的 strm 台账记录应被自动清除，但实际仍存在: {:?}",
        remaining_rows
    );

    // 5. 验证 media 记录以及关联的播放/标记也被妥善清理（没有剩余 ledger 的媒体）
    let remaining_media = state.store().lock().get_media(media.id).unwrap();
    assert!(
        remaining_media.is_none(),
        "台账全清后，无关联文件的孤立媒体记录也应被清理"
    );

    // 6. 模拟从回收站还原该目录（重新创建目录与 strm 文件）
    std::fs::create_dir_all(&movie_dir).unwrap();
    std::fs::write(&strm_file, "https://example.com/video.mkv").unwrap();

    // 监控捕获到 movie_dir 的还原事件
    let restore_events = vec![DebouncedEvent {
        path: movie_dir.clone(),
        kind: notify_debouncer_mini::DebouncedEventKind::Any,
    }];
    api::fs_watcher::handle_fs_events(&state, &tracker, restore_events);

    // 等待异步 spawn_blocking 的扫描完成
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // 7. 验证还原后重新入账
    let restored_rows = state.store().lock().list_ledger().unwrap();
    assert_eq!(
        restored_rows.len(),
        1,
        "目录还原后应重新扫描入账，但未找到记录"
    );
    assert_eq!(restored_rows[0].path, strm_file.display().to_string());
}

fn episode_row(media_id: domain::MediaId, path: &std::path::Path, episode: u32) -> domain::LedgerRow {
    domain::LedgerRow {
        id: domain::LedgerId::new(),
        media_id,
        path: path.display().to_string(),
        season: Some(1),
        episode: Some(episode),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: domain::QualitySource::Release,
        confidence: domain::Confidence::High,
        filter_score: None,
    }
}

fn fs_event(path: std::path::PathBuf) -> DebouncedEvent {
    DebouncedEvent {
        path,
        kind: notify_debouncer_mini::DebouncedEventKind::Any,
    }
}

#[tokio::test]
async fn deleting_one_episode_keeps_sibling_ledger_rows() {
    let temp = tempfile::tempdir().unwrap();
    let library_root = temp.path().join("media/tv/china");
    let season_dir = library_root.join("喜剧之王 (2026)/Season 1");
    std::fs::create_dir_all(&season_dir).unwrap();
    let episode_one = season_dir.join("喜剧之王 - S01E01 - 第 1 集.strm");
    let episode_two = season_dir.join("喜剧之王 - S01E02 - 第 2 集.strm");
    std::fs::write(&episode_one, "https://example.com/e01.mkv").unwrap();
    std::fs::write(&episode_two, "https://example.com/e02.mkv").unwrap();

    let store = api::Store::open(temp.path().join("data")).unwrap();
    store
        .set_library_root(domain::MediaKind::Tv, library_root.to_str().unwrap())
        .unwrap();
    let state = api::management::ApiState::new(
        store,
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(EmptyFetcher),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Tv,
        title: "喜剧之王".into(),
        year: Some(2026),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    state.store().lock().insert_media(&media).unwrap();
    let first = episode_row(media.id, &episode_one, 1);
    let second = episode_row(media.id, &episode_two, 2);
    let first_id = first.id;
    state.store().lock().insert_ledger(&first).unwrap();
    state.store().lock().insert_ledger(&second).unwrap();

    std::fs::remove_file(&episode_two).unwrap();
    // macOS 的 notify 会在单集删除后上报父目录，且事件处理时 path.exists()
    // 可能为 false，即使目录随后仍在。只把季目录临时挪走，E01 留在原路径。
    let parked_season = season_dir.with_file_name("Season 1.parked");
    let episode_one_parked = parked_season.join(episode_one.file_name().unwrap());
    std::fs::rename(&season_dir, &parked_season).unwrap();
    std::fs::create_dir_all(episode_one.parent().unwrap()).unwrap();
    std::fs::rename(&episode_one_parked, &episode_one).unwrap();
    let tracker = Arc::new(StrmGraceTracker::new());
    api::fs_watcher::handle_fs_events(
        &state,
        &tracker,
        vec![fs_event(episode_two), fs_event(season_dir.clone())],
    );
    if parked_season.exists() {
        let _ = std::fs::remove_dir_all(&parked_season);
    }

    let rows = state.store().lock().list_ledger().unwrap();
    assert_eq!(rows.len(), 1, "只应删除已不存在的 E02，实际: {rows:?}");
    assert_eq!(rows[0].id, first_id, "仍在磁盘上的 E01 不能换 ledger_id");
    assert_eq!(rows[0].path, episode_one.display().to_string());
    assert!(state.store().lock().get_media(media.id).unwrap().is_some());
}

#[tokio::test]
async fn deleting_the_show_directory_removes_every_episode_ledger_row() {
    let temp = tempfile::tempdir().unwrap();
    let library_root = temp.path().join("media/tv/china");
    let show_dir = library_root.join("喜剧之王 (2026)");
    let season_dir = show_dir.join("Season 1");
    std::fs::create_dir_all(&season_dir).unwrap();
    let episode_one = season_dir.join("喜剧之王 - S01E01 - 第 1 集.strm");
    let episode_two = season_dir.join("喜剧之王 - S01E02 - 第 2 集.strm");
    std::fs::write(&episode_one, "https://example.com/e01.mkv").unwrap();
    std::fs::write(&episode_two, "https://example.com/e02.mkv").unwrap();

    let store = api::Store::open(temp.path().join("data")).unwrap();
    store
        .set_library_root(domain::MediaKind::Tv, library_root.to_str().unwrap())
        .unwrap();
    let state = api::management::ApiState::new(
        store,
        "test-token".into(),
        indexer::ProfileSet::load(None).unwrap(),
        Arc::new(EmptyFetcher),
        Arc::new(downloader::MemoryDownloader::new(temp.path().join("stage"))),
        temp.path().join("library"),
    )
    .unwrap();
    let media = domain::Media {
        id: domain::MediaId::new(),
        kind: domain::MediaKind::Tv,
        title: "喜剧之王".into(),
        year: Some(2026),
        original_title: None,
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    state.store().lock().insert_media(&media).unwrap();
    state
        .store()
        .lock()
        .insert_ledger(&episode_row(media.id, &episode_one, 1))
        .unwrap();
    state
        .store()
        .lock()
        .insert_ledger(&episode_row(media.id, &episode_two, 2))
        .unwrap();

    std::fs::remove_dir_all(&show_dir).unwrap();
    let tracker = Arc::new(StrmGraceTracker::new());
    api::fs_watcher::handle_fs_events(&state, &tracker, vec![fs_event(show_dir)]);

    assert!(
        state.store().lock().list_ledger().unwrap().is_empty(),
        "整目录删除后两集台账都应消失"
    );
    assert!(state.store().lock().get_media(media.id).unwrap().is_none());
}
