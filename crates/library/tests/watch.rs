use std::fs;

use domain::{Confidence, Media, MediaId, MediaKind};
use library::{WatchJob, WatchKind, scan_watch};

fn movie() -> Media {
    Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "The Matrix".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: Some("603".into()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}

#[test]
fn intake_transfers_high_confidence_file_into_library() {
    let tmp = tempfile::tempdir().unwrap();
    let intake = tmp.path().join("intake");
    let library = tmp.path().join("library");
    fs::create_dir_all(&intake).unwrap();
    fs::create_dir_all(&library).unwrap();
    let src = intake.join("The.Matrix.1999.2160p.BluRay.mkv");
    fs::write(&src, b"video").unwrap();
    let job = WatchJob {
        kind: WatchKind::Intake,
        scrape: true,
        path: intake.clone(),
        library_root: library.clone(),
        tv_library_root: None,
    };
    let outcome = scan_watch(&job, &movie()).unwrap();
    assert_eq!(outcome.transferred.len(), 1);
    assert!(outcome.unidentified.is_empty());
    assert!(library.join("The.Matrix.1999.2160p.BluRay.mkv").is_file());
}

#[test]
fn intake_does_not_overwrite_an_existing_library_file() {
    let tmp = tempfile::tempdir().unwrap();
    let intake = tmp.path().join("intake");
    let library = tmp.path().join("library");
    fs::create_dir_all(&intake).unwrap();
    fs::create_dir_all(&library).unwrap();
    let dest = library.join("The.Matrix.1999.2160p.BluRay.mkv");
    fs::write(&dest, b"old-library").unwrap();
    let src = intake.join("The.Matrix.1999.2160p.BluRay.mkv");
    fs::write(&src, b"new-drop").unwrap();
    let job = WatchJob {
        kind: WatchKind::Intake,
        scrape: false,
        path: intake,
        library_root: library,
        tv_library_root: None,
    };
    let outcome = scan_watch(&job, &movie()).unwrap();
    assert!(outcome.transferred.is_empty());
    assert_eq!(outcome.errors.len(), 1);
    assert!(outcome.errors[0].error.contains("already exists"));
    assert_eq!(outcome.errors[0].path, src);
    assert_eq!(fs::read(&dest).unwrap(), b"old-library");
}

#[test]
fn intake_batch_partial_success_does_not_discard_successful_transfers() {
    let tmp = tempfile::tempdir().unwrap();
    let intake = tmp.path().join("intake");
    let library = tmp.path().join("library");
    fs::create_dir_all(&intake).unwrap();
    fs::create_dir_all(&library).unwrap();

    // A: 正常文件
    let src_a = intake.join("The.Matrix.1999.2160p.BluRay.mkv");
    fs::write(&src_a, b"matrix-bytes").unwrap();

    // B: 冲突文件（library 中已有不同内容）
    let dest_b = library.join("Inception.2010.1080p.BluRay.mkv");
    fs::write(&dest_b, b"inception-existing").unwrap();
    let src_b = intake.join("Inception.2010.1080p.BluRay.mkv");
    fs::write(&src_b, b"inception-new").unwrap();

    // C: 正常文件
    let src_c = intake.join("Interstellar.2014.2160p.BluRay.mkv");
    fs::write(&src_c, b"interstellar-bytes").unwrap();

    let job = WatchJob {
        kind: WatchKind::Intake,
        scrape: false,
        path: intake,
        library_root: library.clone(),
        tv_library_root: None,
    };
    let outcome = scan_watch(&job, &movie()).unwrap();

    // 冲突项 B 记录在 errors 中，旧字节未被覆盖
    assert_eq!(outcome.errors.len(), 1);
    assert_eq!(outcome.errors[0].path, src_b);
    assert!(outcome.errors[0].error.contains("already exists"));
    assert_eq!(fs::read(&dest_b).unwrap(), b"inception-existing");

    // 正常项 A 和 C 成功转移
    let transferred_paths: Vec<_> = outcome.transferred.iter().map(|t| &t.path).collect();
    let dest_a = library.join("The.Matrix.1999.2160p.BluRay.mkv");
    let dest_c = library.join("Interstellar.2014.2160p.BluRay.mkv");
    assert!(transferred_paths.contains(&&dest_a));
    assert!(transferred_paths.contains(&&dest_c));
    assert_eq!(fs::read(&dest_a).unwrap(), b"matrix-bytes");
    assert_eq!(fs::read(&dest_c).unwrap(), b"interstellar-bytes");
}

#[test]
fn in_place_watch_keeps_unidentified_video_without_placeholder_nfo() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lib");
    fs::create_dir_all(&dir).unwrap();
    let video = dir.join("The.Matrix.1999.2160p.mkv");
    fs::write(&video, b"stay").unwrap();
    let job = WatchJob {
        kind: WatchKind::InPlace,
        scrape: true,
        path: dir.clone(),
        library_root: dir.clone(),
        tv_library_root: None,
    };
    scan_watch(&job, &movie()).unwrap();
    assert_eq!(fs::read(&video).unwrap(), b"stay");
    assert!(!dir.join("The.Matrix.1999.2160p.nfo").exists());
}

#[test]
fn in_place_watch_scrapes_nested_season_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lib");
    let nested = dir.join("The Long Watch/Season 01");
    fs::create_dir_all(&nested).unwrap();
    let video = nested.join("The.Long.Watch.S01E01.1080p.mkv");
    fs::write(&video, b"stay").unwrap();
    let job = WatchJob {
        kind: WatchKind::InPlace,
        scrape: true,
        path: dir.clone(),
        library_root: dir.clone(),
        tv_library_root: None,
    };
    let outcome = scan_watch(&job, &movie()).unwrap();
    assert_eq!(fs::read(&video).unwrap(), b"stay");
    assert!(!nested.join("The.Long.Watch.S01E01.1080p.nfo").exists());
    assert_eq!(outcome.transferred, vec![video]);
}

#[test]
fn low_confidence_intake_is_unidentified() {
    let tmp = tempfile::tempdir().unwrap();
    let intake = tmp.path().join("intake");
    let library = tmp.path().join("library");
    fs::create_dir_all(&intake).unwrap();
    fs::create_dir_all(&library).unwrap();
    let src = intake.join("foo.mkv");
    fs::write(&src, b"??").unwrap();
    let job = WatchJob {
        kind: WatchKind::Intake,
        scrape: true,
        path: intake,
        library_root: library.clone(),
        tv_library_root: None,
    };
    let outcome = scan_watch(&job, &movie()).unwrap();
    assert_eq!(outcome.unidentified.len(), 1);
    assert_eq!(outcome.unidentified[0].confidence, Confidence::Low);
    assert!(library.read_dir().unwrap().next().is_none());
}

#[test]
fn in_place_watch_supports_avi_ts_and_other_video_formats() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lib");
    fs::create_dir_all(&dir).unwrap();
    let avi = dir.join("Movie.1999.1080p.avi");
    let ts = dir.join("Movie.1999.1080p.ts");
    fs::write(&avi, b"avi-video").unwrap();
    fs::write(&ts, b"ts-video").unwrap();
    let job = WatchJob {
        kind: WatchKind::InPlace,
        scrape: false,
        path: dir.clone(),
        library_root: dir.clone(),
        tv_library_root: None,
    };
    let outcome = scan_watch(&job, &movie()).unwrap();
    assert!(outcome.transferred.iter().any(|t| t.path == avi));
    assert!(outcome.transferred.iter().any(|t| t.path == ts));
}

#[test]
fn in_place_watch_resolves_low_confidence_movie_via_nfo_or_parent_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("lib/The Matrix (1999)");
    fs::create_dir_all(&dir).unwrap();
    // 只有通用名字 movie.mkv，自身 release::parse 是 Low
    let movie_file = dir.join("movie.mkv");
    let nfo_file = dir.join("movie.nfo");
    fs::write(&movie_file, b"video-data").unwrap();
    fs::write(
        &nfo_file,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<movie>
    <title>The Matrix</title>
    <year>1999</year>
</movie>"#,
    )
    .unwrap();
    let job = WatchJob {
        kind: WatchKind::InPlace,
        scrape: false,
        path: dir.clone(),
        library_root: dir.clone(),
        tv_library_root: None,
    };
    let outcome = scan_watch(&job, &movie()).unwrap();
    assert_eq!(outcome.transferred, vec![movie_file]);
}

#[test]
fn intake_ignores_non_video_files_like_subtitles() {
    let tmp = tempfile::tempdir().unwrap();
    let intake = tmp.path().join("intake");
    let library = tmp.path().join("library");
    fs::create_dir_all(&intake).unwrap();
    fs::create_dir_all(&library).unwrap();
    let srt = intake.join("The.Matrix.1999.srt");
    fs::write(&srt, b"1\n00:00:01,000 --> 00:00:02,000\nHello").unwrap();
    let job = WatchJob {
        kind: WatchKind::Intake,
        scrape: false,
        path: intake,
        library_root: library,
        tv_library_root: None,
    };
    let outcome = scan_watch(&job, &movie()).unwrap();
    // 字幕不作为视频被转移或标记为未识别视频
    assert!(outcome.transferred.is_empty());
    assert!(outcome.unidentified.is_empty());
}

#[test]
fn watch_kind_is_two_jobs_not_one_switch() {
    assert_ne!(WatchKind::Intake, WatchKind::InPlace);
}

#[test]
fn high_confidence_episode_keeps_filename_title_and_inherits_ancestor_year() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp
        .path()
        .join("children/动灵守护者 (2022)/Spirit.Rangers.S01");
    fs::create_dir_all(&dir).unwrap();
    let episode = dir.join("Spirit.Rangers.S01E01.Thunder.Mountain.1080p.NF.WEB-DL.strm");
    fs::write(&episode, b"https://cdn.example/ep.mkv").unwrap();
    let job = WatchJob {
        kind: WatchKind::InPlace,
        scrape: false,
        path: tmp.path().to_path_buf(),
        library_root: tmp.path().to_path_buf(),
        tv_library_root: None,
    };
    let outcome = scan_watch(&job, &movie()).unwrap();
    let found = outcome
        .transferred
        .iter()
        .find(|file| file.path == episode)
        .unwrap();
    let parsed = found.identified_release.as_ref().unwrap();
    assert_eq!(parsed.title, "Spirit Rangers");
    assert_eq!(parsed.year, Some(2022));
    assert_eq!(parsed.season, Some(1));
    assert_eq!(parsed.episode, Some(1));
}

#[test]
fn intake_identifies_tvshow_nfo_when_triggered_with_movie_media() {
    let tmp = tempfile::tempdir().unwrap();
    let intake = tmp.path().join("intake");
    let show_dir = intake.join("My Show");
    let movies_lib = tmp.path().join("movies");
    let tv_lib = tmp.path().join("tv");
    fs::create_dir_all(&show_dir).unwrap();
    fs::create_dir_all(&movies_lib).unwrap();
    fs::create_dir_all(&tv_lib).unwrap();

    fs::write(
        show_dir.join("tvshow.nfo"),
        "<tvshow><title>Anchor Show</title><year>2021</year><tmdbid>999</tmdbid></tvshow>",
    )
    .unwrap();
    let ep = show_dir.join("01.mkv");
    fs::write(&ep, b"ep-data").unwrap();

    let job = WatchJob {
        kind: WatchKind::Intake,
        scrape: false,
        path: intake,
        library_root: movies_lib.clone(),
        tv_library_root: Some(tv_lib.clone()),
    };
    let outcome = scan_watch(&job, &movie()).unwrap();
    assert_eq!(outcome.transferred.len(), 1);
    let nfo_id = outcome.transferred[0]
        .nfo_identity
        .as_ref()
        .expect("should read tvshow.nfo even if WatchJob media param is Movie");
    assert_eq!(nfo_id.tmdb_id.as_deref(), Some("999"));
    assert!(tv_lib.join("My Show/01.mkv").is_file());
}

#[test]
fn intake_skips_nfo_write_when_media_has_no_tmdb_id() {
    let tmp = tempfile::tempdir().unwrap();
    let intake = tmp.path().join("intake");
    let library = tmp.path().join("library");
    fs::create_dir_all(&intake).unwrap();
    fs::create_dir_all(&library).unwrap();
    let src = intake.join("The.Matrix.1999.2160p.BluRay.mkv");
    fs::write(&src, b"video").unwrap();
    let mut local_media = movie();
    local_media.tmdb_id = None;
    let job = WatchJob {
        kind: WatchKind::Intake,
        scrape: true,
        path: intake,
        library_root: library.clone(),
        tv_library_root: None,
    };
    let outcome = scan_watch(&job, &local_media).unwrap();
    assert_eq!(outcome.transferred.len(), 1);
    assert!(
        !library.join("The.Matrix.1999.2160p.BluRay.nfo").exists(),
        "scrape without tmdb_id must not fabricate an NFO beside transferred file"
    );
}

#[test]
fn intake_ignores_renamed_destination_escaping_target_root() {
    let tmp = tempfile::tempdir().unwrap();
    let intake = tmp.path().join("intake");
    let library = tmp.path().join("library");
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&intake).unwrap();
    fs::create_dir_all(&library).unwrap();
    fs::create_dir_all(&outside).unwrap();

    let src = intake.join("The.Matrix.1999.2160p.BluRay.mkv");
    fs::write(&src, b"matrix-video").unwrap();

    let escaped = outside.join("The.Matrix.1999.mkv");
    let mut renamed = std::collections::HashMap::new();
    renamed.insert(src.clone(), escaped.clone());

    let job = WatchJob {
        kind: WatchKind::Intake,
        scrape: false,
        path: intake,
        library_root: library.clone(),
        tv_library_root: None,
    };
    let outcome = library::scan_watch_intake_controlled_with_destinations(
        &job,
        &movie(),
        &library::WorkControl::default(),
        None,
        &renamed,
    )
    .unwrap();

    assert!(
        !escaped.exists(),
        "renamed destination outside target_root must never be written"
    );
    let fallback_dest = library.join("The.Matrix.1999.2160p.BluRay.mkv");
    assert!(fallback_dest.is_file());
    assert_eq!(outcome.transferred.len(), 1);
    assert_eq!(outcome.transferred[0].path, fallback_dest);
}

#[test]
fn intake_ignores_parent_dir_and_root_equal_destinations() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();

    let intake_1 = tmp.path().join("intake_1");
    let lib_1 = tmp.path().join("lib_1");
    fs::create_dir_all(&intake_1).unwrap();
    fs::create_dir_all(&lib_1).unwrap();
    let src_1 = intake_1.join("The.Matrix.1999.2160p.BluRay.mkv");
    fs::write(&src_1, b"video-1").unwrap();
    let mut renamed_1 = std::collections::HashMap::new();
    renamed_1.insert(src_1, lib_1.join("../outside/The.Matrix.1999.mkv"));
    let out_1 = library::scan_watch_intake_controlled_with_destinations(
        &WatchJob {
            kind: WatchKind::Intake,
            scrape: false,
            path: intake_1,
            library_root: lib_1.clone(),
            tv_library_root: None,
        },
        &movie(),
        &library::WorkControl::default(),
        None,
        &renamed_1,
    )
    .unwrap();
    assert!(!outside.join("The.Matrix.1999.mkv").exists());
    assert_eq!(out_1.errors.len(), 0);
    assert_eq!(
        out_1.transferred[0].path,
        lib_1.join("The.Matrix.1999.2160p.BluRay.mkv")
    );

    let intake_2 = tmp.path().join("intake_2");
    let lib_2 = tmp.path().join("lib_2");
    fs::create_dir_all(&intake_2).unwrap();
    fs::create_dir_all(&lib_2).unwrap();
    let src_2 = intake_2.join("The.Matrix.1999.2160p.BluRay.mkv");
    fs::write(&src_2, b"video-2").unwrap();
    let mut renamed_2 = std::collections::HashMap::new();
    renamed_2.insert(src_2, lib_2.clone());
    let out_2 = library::scan_watch_intake_controlled_with_destinations(
        &WatchJob {
            kind: WatchKind::Intake,
            scrape: false,
            path: intake_2,
            library_root: lib_2.clone(),
            tv_library_root: None,
        },
        &movie(),
        &library::WorkControl::default(),
        None,
        &renamed_2,
    )
    .unwrap();
    assert_eq!(out_2.errors.len(), 0);
    assert_eq!(
        out_2.transferred[0].path,
        lib_2.join("The.Matrix.1999.2160p.BluRay.mkv")
    );
}

#[cfg(unix)]
#[test]
fn intake_ignores_symlink_escape_destination_under_target_root() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tmp.path().join("outside");
    let intake = tmp.path().join("intake_3");
    let lib = tmp.path().join("lib_3");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(&intake).unwrap();
    fs::create_dir_all(&lib).unwrap();
    std::os::unix::fs::symlink(&outside, lib.join("sym")).unwrap();
    let src = intake.join("The.Matrix.1999.2160p.BluRay.mkv");
    fs::write(&src, b"video-3").unwrap();
    let mut renamed = std::collections::HashMap::new();
    renamed.insert(src, lib.join("sym/Sym.Escape.mkv"));
    let out = library::scan_watch_intake_controlled_with_destinations(
        &WatchJob {
            kind: WatchKind::Intake,
            scrape: false,
            path: intake,
            library_root: lib.clone(),
            tv_library_root: None,
        },
        &movie(),
        &library::WorkControl::default(),
        None,
        &renamed,
    )
    .unwrap();
    assert!(!outside.join("Sym.Escape.mkv").exists());
    assert_eq!(out.errors.len(), 0);
    assert_eq!(
        out.transferred[0].path,
        lib.join("The.Matrix.1999.2160p.BluRay.mkv")
    );
}

#[cfg(unix)]
#[test]
fn intake_preserves_valid_rename_when_target_root_not_yet_created_and_blocks_fallback_symlink_escape()
 {
    let tmp = tempfile::tempdir().unwrap();
    let intake_uncreated = tmp.path().join("intake_uncreated");
    let uncreated_lib = tmp.path().join("nested_not_created_yet/movies");
    fs::create_dir_all(&intake_uncreated).unwrap();
    let src_1 = intake_uncreated.join("The.Matrix.1999.2160p.BluRay.mkv");
    fs::write(&src_1, b"video-uncreated").unwrap();
    let renamed_dest = uncreated_lib.join("The Matrix (1999)/The Matrix (1999).mkv");
    let mut renamed = std::collections::HashMap::new();
    renamed.insert(src_1, renamed_dest.clone());

    let out_1 = library::scan_watch_intake_controlled_with_destinations(
        &WatchJob {
            kind: WatchKind::Intake,
            scrape: false,
            path: intake_uncreated,
            library_root: uncreated_lib,
            tv_library_root: None,
        },
        &movie(),
        &library::WorkControl::default(),
        None,
        &renamed,
    )
    .unwrap();
    assert_eq!(out_1.errors.len(), 0);
    assert_eq!(out_1.transferred.len(), 1);
    assert_eq!(out_1.transferred[0].path, renamed_dest);
    assert!(renamed_dest.exists());

    // Fallback rel_path (`sub/The.Matrix...`) must NOT traverse a symlink `lib/sub -> outside`.
    let outside = tmp.path().join("outside_fallback");
    let intake_sub = tmp.path().join("intake_sub");
    let lib_sub = tmp.path().join("lib_sub");
    fs::create_dir_all(&outside).unwrap();
    fs::create_dir_all(intake_sub.join("sub")).unwrap();
    fs::create_dir_all(&lib_sub).unwrap();
    std::os::unix::fs::symlink(&outside, lib_sub.join("sub")).unwrap();
    let src_2 = intake_sub.join("sub/The.Matrix.1999.2160p.BluRay.mkv");
    fs::write(&src_2, b"video-sym-fallback").unwrap();

    let out_2 = library::scan_watch_intake_controlled_with_destinations(
        &WatchJob {
            kind: WatchKind::Intake,
            scrape: false,
            path: intake_sub,
            library_root: lib_sub,
            tv_library_root: None,
        },
        &movie(),
        &library::WorkControl::default(),
        None,
        &std::collections::HashMap::new(),
    )
    .unwrap();
    assert!(!outside.join("The.Matrix.1999.2160p.BluRay.mkv").exists());
    assert!(out_2.transferred.is_empty());
    assert_eq!(out_2.errors.len(), 1);
}
