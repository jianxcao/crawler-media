#[test]
fn fresh_fingerprint_replaces_stale_unlocked_intro_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    let media_id = domain::MediaId::new();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "万神殿".into(),
            year: None,
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: tmp.path().join("Pantheon.S01E01.mkv").display().to_string(),
        season: Some(1),
        episode: Some(1),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&row).unwrap();
    store
        .put_media_marker(&crate::store::StoredMediaMarker {
            media_id,
            season: 1,
            episode: 1,
            intro_start_ms: Some(0),
            intro_end_ms: Some(104_000),
            outro_start_ms: None,
            outro_end_ms: None,
            source: "theintrodb".into(),
            locked: false,
            updated_at: 0,
        })
        .unwrap();

    persist_marker(
        &store,
        &[row.clone()],
        crate::store::StoredMediaMarker {
            media_id,
            season: 1,
            episode: 1,
            intro_start_ms: Some(0),
            intro_end_ms: Some(180_000),
            outro_start_ms: Some(1_000_000),
            outro_end_ms: Some(1_100_000),
            source: "fingerprint".into(),
            locked: false,
            updated_at: 0,
        },
        false,
    )
    .unwrap();

    let marker = store
        .get_media_marker(media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(marker.intro_end_ms, Some(180_000));
    assert_eq!(marker.outro_start_ms, Some(1_000_000));
    assert_eq!(marker.source, "fingerprint");
}

fn fingerprint_marker(media_id: domain::MediaId) -> crate::store::StoredMediaMarker {
    crate::store::StoredMediaMarker {
        media_id,
        season: 1,
        episode: 1,
        intro_start_ms: Some(0),
        intro_end_ms: Some(90_000),
        outro_start_ms: Some(1_000_000),
        outro_end_ms: Some(1_060_000),
        source: "fingerprint".into(),
        locked: false,
        updated_at: 0,
    }
}

fn tv_episode(tmp: &tempfile::TempDir) -> (Store, domain::MediaId, LedgerRow) {
    let store = Store::open(tmp.path()).unwrap();
    let media_id = domain::MediaId::new();
    store
        .insert_media(&domain::Media {
            id: media_id,
            kind: MediaKind::Tv,
            title: "喜剧之王".into(),
            year: Some(2026),
            original_title: None,
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        })
        .unwrap();
    let video = tmp.path().join("episode.strm");
    std::fs::write(&video, "https://cdn.example.test/episode.mkv\n").unwrap();
    let row = LedgerRow {
        id: LedgerId::new(),
        media_id,
        path: video.display().to_string(),
        season: Some(1),
        episode: Some(1),
        resolution: None,
        codec: None,
        hdr: None,
        quality_source: QualitySource::Release,
        confidence: Confidence::High,
        filter_score: None,
    };
    store.insert_ledger(&row).unwrap();
    (store, media_id, row)
}

/// 假 ffmpeg：把参数记到固定文件并写出一张空图。路径写死在脚本里，
/// 后台抓帧线程不读环境变量，并行测试也不会互相清掉。
fn recording_ffmpeg(dir: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let ffmpeg = bin.join("ffmpeg");
    let args = dir.join("ffmpeg-args");
    std::fs::write(
        &ffmpeg,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> '{}'\nprintf '\\n---\\n' >> '{}'\nout=\nwhile [ $# -gt 0 ]; do out=\"$1\"; shift; done\n: > \"$out\"\nexit 0\n",
            args.display(),
            args.display(),
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&ffmpeg, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    (ffmpeg, args)
}

#[test]
fn fingerprint_marker_extracts_scene_frames_when_library_enables_them() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, media_id, row) = tv_episode(&tmp);
    let root = tmp.path().to_str().unwrap().to_string();
    let library = store
        .create_library(MediaKind::Tv, "国语剧", &[&root], "everyone", true, &[])
        .unwrap();
    store
        .set_library_switch_settings(&library.id, None, None, Some(true), None, None)
        .unwrap();

    let (ffmpeg, args) = recording_ffmpeg(tmp.path());
    crate::http::library_chapters::set_scene_frame_ffmpeg_for_test(leak_path(ffmpeg));
    let updates =
        persist_marker(&store, &[row.clone()], fingerprint_marker(media_id), false).unwrap();
    crate::http::library_chapters::trigger_scene_frames_for_chapter_updates(&updates, &store);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while std::time::Instant::now() < deadline && !args.is_file() {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let recorded = std::fs::read_to_string(&args).unwrap_or_default();
    assert!(
        recorded.contains("-frames:v"),
        "开启生成章节后，声纹写入片头片尾必须后台抓场景图: {recorded}"
    );
}

#[test]
fn fingerprint_marker_skips_scene_frames_when_library_disables_them() {
    let tmp = tempfile::tempdir().unwrap();
    let (store, media_id, row) = tv_episode(&tmp);
    let root = tmp.path().to_str().unwrap().to_string();
    let library = store
        .create_library(MediaKind::Tv, "国语剧", &[&root], "everyone", true, &[])
        .unwrap();
    store
        .set_library_switch_settings(&library.id, None, None, Some(false), None, None)
        .unwrap();

    let (ffmpeg, args) = recording_ffmpeg(tmp.path());
    crate::http::library_chapters::set_scene_frame_ffmpeg_for_test(leak_path(ffmpeg));
    let updates =
        persist_marker(&store, &[row.clone()], fingerprint_marker(media_id), false).unwrap();
    crate::http::library_chapters::trigger_scene_frames_for_chapter_updates(&updates, &store);
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        !args.is_file(),
        "关闭生成章节后，声纹写入片头片尾不得抓场景图"
    );
    let marker = store
        .get_media_marker(media_id, Some(1), Some(1))
        .unwrap()
        .unwrap();
    assert_eq!(marker.source, "fingerprint");
}

fn leak_path(path: std::path::PathBuf) -> &'static str {
    Box::leak(path.display().to_string().into_boxed_str())
}
