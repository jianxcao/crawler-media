use std::path::PathBuf;

use domain::{Media, MediaId, MediaKind};
use library::{
    WatchJob, WatchKind, plan_watch_intake_candidates,
    scan_watch_intake_controlled_with_destinations,
};

use crate::management::ApiState;
use crate::scrape_store::ScrapeStoreExt;
struct IntakeSnapshot {
    intake: PathBuf,
    movie_library: store::Library,
    movie_rev: i64,
    tv_library: Option<store::Library>,
    tv_rev: Option<i64>,
    nfo: bool,
}

fn load_intake_snapshot(store: &crate::Store) -> Result<Option<IntakeSnapshot>, String> {
    let Some(path) = store.watch_intake().map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    if path.is_empty() {
        return Ok(None);
    }
    let movie_library = store
        .default_library(MediaKind::Movie)
        .map_err(|e| e.to_string())?
        .ok_or("Movie Library 不存在")?;
    let movie_rev = store
        .library_config_revision(&movie_library.id)
        .map_err(|e| e.to_string())?;
    let tv_library = store
        .default_library(MediaKind::Tv)
        .map_err(|e| e.to_string())?;
    let tv_rev = tv_library
        .as_ref()
        .map(|lib| store.library_config_revision(&lib.id))
        .transpose()
        .map_err(|e| e.to_string())?;
    let nfo = store
        .get_scrape_config()
        .map_err(|e| e.to_string())?
        .effective
        .mirror_nfo;
    Ok(Some(IntakeSnapshot {
        intake: PathBuf::from(path),
        movie_library,
        movie_rev,
        tv_library,
        tv_rev,
        nfo,
    }))
}

pub fn run(state: &ApiState) -> Result<(), String> {
    let Some(IntakeSnapshot {
        intake,
        movie_library,
        movie_rev,
        tv_library,
        tv_rev,
        nfo,
    }) = load_intake_snapshot(state.store.as_ref())?
    else {
        return Ok(());
    };
    if !intake.is_dir() {
        return Ok(());
    }
    let library_root = movie_library
        .root_paths
        .first()
        .cloned()
        .ok_or("Movie Library 没有根目录")?;
    let tv_library_root = tv_library
        .as_ref()
        .and_then(|library| library.root_paths.first().cloned());
    let probe = Media {
        id: MediaId::new(),
        kind: MediaKind::Movie,
        title: "intake".into(),
        year: None,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let job = WatchJob {
        kind: WatchKind::Intake,
        path: intake,
        library_root,
        tv_library_root,
        scrape: nfo,
    };
    let planned_renames =
        preplan_intake_renames(state, &job, &probe, &movie_library.id, tv_library.as_ref())?;
    let movie_lease = state.acquire_library_work(&movie_library.id)?;
    let tv_lease = tv_library
        .as_ref()
        .map(|library| state.acquire_library_work(&library.id))
        .transpose()?;
    let renamed_destinations = {
        let store = state.store.as_ref();
        check_intake_libraries(&store, &movie_lease, tv_lease.as_ref())?;
        verify_leased_intake_state(
            &store,
            &movie_library.id,
            movie_rev,
            tv_library.as_ref().map(|l| l.id.as_str()),
            tv_rev,
            planned_renames,
        )?
    };
    let outcome = scan_watch_intake_controlled_with_destinations(
        &job,
        &probe,
        movie_lease.control(),
        tv_lease.as_ref().map(|lease| lease.control()),
        &renamed_destinations,
    )
    .map_err(|e| e.to_string())?;
    let transferred = outcome.transferred;
    {
        let store = state.store.as_ref();
        check_intake_libraries(&store, &movie_lease, tv_lease.as_ref())?;
        for parked in outcome.unidentified {
            store
                .insert_unidentified(parked.path.display().to_string(), parked.confidence)
                .map_err(|e| e.to_string())?;
        }
    }
    let plan = {
        let store = state.store.as_ref();
        check_intake_libraries(&store, &movie_lease, tv_lease.as_ref())?;
        crate::watch_ledger::plan_paths(&store, transferred.clone())?
    };
    let observed = plan.observe()?;
    let inserted = {
        let store = state.store.as_ref();
        check_intake_libraries(&store, &movie_lease, tv_lease.as_ref())?;
        observed.record(&store)?
    };
    if !inserted.is_empty() {
        crate::plugins::events::wake(state);
    }

    finish_intake_metadata(
        state,
        &transferred,
        inserted,
        &movie_lease,
        tv_lease.as_ref(),
        nfo,
    )?;
    if !outcome.errors.is_empty() {
        let err_msgs: Vec<String> = outcome
            .errors
            .iter()
            .map(|e| format!("{}: {}", e.path.display(), e.error))
            .collect();
        return Err(format!(
            "watch intake completed with {} error(s): {}",
            outcome.errors.len(),
            err_msgs.join("; ")
        ));
    }
    Ok(())
}

fn preplan_intake_renames(
    state: &ApiState,
    job: &WatchJob,
    probe: &Media,
    movie_library_id: &str,
    tv_library: Option<&store::Library>,
) -> Result<Vec<(PathBuf, PathBuf, MediaKind)>, String> {
    let candidates = plan_watch_intake_candidates(job, probe).map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(candidates.len());
    for cand in candidates {
        let (target_lib_str, media_kind) = if cand.is_tv {
            let Some(tv) = tv_library else {
                continue;
            };
            (tv.id.as_str(), MediaKind::Tv)
        } else {
            (movie_library_id, MediaKind::Movie)
        };
        let Ok(library_id) = target_lib_str.parse::<domain::LibraryId>() else {
            continue;
        };
        let dest = crate::plugins::transfer_rename::run_transfer_rename(
            state,
            &crate::plugins::transfer_rename::TransferRenameRequest {
                workflow_id: format!("watch:{}", library_id),
                library_id,
                library_root: cand.target_root,
                media_id: MediaId::new(),
                media_kind,
                subscribe_id: None,
                source_path: cand.source_path.clone(),
                proposed_relative_path: cand.proposed_relative_path,
            },
        );
        out.push((cand.source_path, dest, media_kind));
    }
    Ok(out)
}

fn resolve_existing_ancestor_path(path: &std::path::Path) -> PathBuf {
    for anc in path.ancestors() {
        if anc.as_os_str().is_empty() {
            break;
        }
        if let Ok(canon) = std::fs::canonicalize(anc) {
            return match path.strip_prefix(anc) {
                Ok(rem) if !rem.as_os_str().is_empty() => canon.join(rem),
                _ => canon,
            };
        }
    }
    path.to_path_buf()
}

fn verify_leased_intake_state(
    store: &crate::Store,
    movie_id: &str,
    movie_rev: i64,
    tv_id: Option<&str>,
    tv_rev: Option<i64>,
    planned: Vec<(PathBuf, PathBuf, MediaKind)>,
) -> Result<std::collections::HashMap<PathBuf, PathBuf>, String> {
    let cur_movie = store
        .default_library(MediaKind::Movie)
        .map_err(|e| e.to_string())?;
    if cur_movie.as_ref().map(|l| l.id.as_str()) != Some(movie_id)
        || store
            .library_config_revision(movie_id)
            .map_err(|e| e.to_string())?
            != movie_rev
    {
        return Err("Watch intake 默认 Movie Library 配置已变更".into());
    }
    let cur_tv = store
        .default_library(MediaKind::Tv)
        .map_err(|e| e.to_string())?;
    if cur_tv.as_ref().map(|l| l.id.as_str()) != tv_id {
        return Err("Watch intake 默认 TV Library 配置已变更".into());
    }
    if let (Some(tid), Some(expected_rev)) = (tv_id, tv_rev)
        && store
            .library_config_revision(tid)
            .map_err(|e| e.to_string())?
            != expected_rev
    {
        return Err("Watch intake 目标 TV Library 配置已变更".into());
    }
    let mut verified = std::collections::HashMap::with_capacity(planned.len());
    for (src, dest, kind) in planned {
        let expected_owner = match kind {
            MediaKind::Tv => tv_id.ok_or("TV Library 未配置")?,
            _ => movie_id,
        };
        let resolved = resolve_existing_ancestor_path(&dest);
        let owner = store
            .library_root_owning_key(&resolved, kind)
            .map_err(|e| e.to_string())?;
        if owner.as_ref().map(|(lib_id, _)| lib_id.as_str()) != Some(expected_owner) {
            return Err(format!(
                "Watch intake 目标路径越出所属 Library 根目录: {}",
                dest.display()
            ));
        }
        verified.insert(src, dest);
    }
    Ok(verified)
}

fn finish_intake_metadata(
    state: &ApiState,
    transferred: &[library::TransferredFile],
    inserted: Vec<PathBuf>,
    movie_lease: &crate::library_lifecycle::LibraryWorkLease,
    tv_lease: Option<&crate::library_lifecycle::LibraryWorkLease>,
    nfo: bool,
) -> Result<(), String> {
    // 为没有 TMDB ID 的影视条目自动匹配 TMDB 元数据与海报
    let unresolved_media = {
        let store = state.store.as_ref();
        let mut map = std::collections::HashMap::new();
        for dest in transferred {
            if let Ok(Some(row)) = store.file_coverage_by_path(&dest.path.display().to_string())
                && let Ok(Some(m)) = store.get_media(row.file.media_id)
                && m.tmdb_id.is_none()
                && m.kind != domain::MediaKind::Video
            {
                map.entry(m.id).or_insert((m, dest.path.clone()));
            }
        }
        map
    };
    for (_, (media, dest)) in unresolved_media {
        check_intake_libraries(&state.store.as_ref(), movie_lease, tv_lease)?;
        let resolved = crate::auto_resolve::resolve_scanned_media(state, &media, &dest);
        let active_media = if let Some(canonical) = resolved {
            crate::auto_resolve::rebind_scan_rows(state, media.id, canonical.id, &inserted)?;
            canonical
        } else {
            media
        };
        if nfo && active_media.tmdb_id.is_some() {
            let _ = crate::poster_fetch::attach_poster(state, &active_media, &dest);
            let _ = crate::poster_fetch::attach_backdrop(state, &active_media, &dest);
            crate::auto_resolve::write_resolved_nfo(state, &active_media, &dest);
        }
    }

    crate::http::library::enqueue_probes_for_paths(state, inserted);

    // 自动补封面：目标媒体库目录下缺 poster.jpg 的文件从 TMDB 拉海报 + 背板
    let targets: Vec<(domain::MediaId, PathBuf)> = {
        let store = state.store.as_ref();
        transferred
            .iter()
            .filter_map(|t| {
                let row = store
                    .file_coverage_by_path(&t.path.display().to_string())
                    .ok()
                    .flatten()?;
                Some((row.file.media_id, t.path.clone()))
            })
            .collect()
    };
    for (media_id, row_path) in targets {
        if row_path
            .parent()
            .is_none_or(|dir| dir.join("poster.jpg").is_file())
        {
            continue;
        }
        check_intake_libraries(&state.store.as_ref(), movie_lease, tv_lease)?;
        let Some(real) = state.store.as_ref().get_media(media_id).ok().flatten() else {
            continue;
        };
        let _ = crate::poster_fetch::attach_poster(state, &real, &row_path);
        let _ = crate::poster_fetch::attach_backdrop(state, &real, &row_path);
    }
    Ok(())
}

fn check_intake_libraries(
    store: &crate::Store,
    movie: &crate::library_lifecycle::LibraryWorkLease,
    tv: Option<&crate::library_lifecycle::LibraryWorkLease>,
) -> Result<(), String> {
    for lease in std::iter::once(movie).chain(tv) {
        lease.control().check().map_err(|e| e.to_string())?;
        if !store
            .library_accepts_work(lease.library_id())
            .map_err(|e| e.to_string())?
        {
            return Err("Watch intake 的目标 Library 正在删除或已不存在".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use downloader::MemoryDownloader;
    use indexer::{FetchRequest, Fetcher, IndexerError, RegistrySnapshot};

    use super::*;

    struct EmptyFetcher;

    impl Fetcher for EmptyFetcher {
        fn fetch(&self, _request: &FetchRequest) -> Result<String, IndexerError> {
            Err(IndexerError::Fetch("unexpected request".into()))
        }
    }

    #[test]
    fn transferred_media_is_queued_for_probe_without_waiting_for_detail_view() {
        let tmp = tempfile::tempdir().unwrap();
        let intake = tmp.path().join("intake");
        std::fs::create_dir_all(&intake).unwrap();
        std::fs::write(intake.join("The.Matrix.1999.2160p.BluRay.mkv"), b"media").unwrap();
        let store = crate::Store::open(tmp.path().join("data")).unwrap();
        store.set_watch_intake(intake.to_str().unwrap()).unwrap();
        let state = ApiState::new(
            store,
            "test-token".into(),
            RegistrySnapshot::builtin().unwrap(),
            Arc::new(EmptyFetcher),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .unwrap();

        run(&state).unwrap();

        let rows = state.store.as_ref().list_file_coverage().unwrap();
        assert_eq!(rows.len(), 1);
        assert!(
            state.probe.is_queued(&rows[0].file.file_id.to_string()),
            "new files should start media probing during intake"
        );
    }

    #[test]
    fn newly_added_tv_episode_queues_metadata_without_voiceprint_when_voiceprint_is_off() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::Store::open(tmp.path().join("data")).unwrap();
        let state = ApiState::new(
            store,
            "test-token".into(),
            RegistrySnapshot::builtin().unwrap(),
            Arc::new(EmptyFetcher),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .unwrap();
        let tv_library = state
            .store
            .as_ref()
            .default_library(MediaKind::Tv)
            .unwrap()
            .unwrap();
        assert!(!tv_library.detect_intros);
        assert!(!tv_library.enable_fingerprint);
        let root = tv_library.root_paths.first().unwrap();
        let episode_path = root.join("Pantheon").join("Pantheon.S01E01.mkv");
        std::fs::create_dir_all(episode_path.parent().unwrap()).unwrap();
        std::fs::write(&episode_path, b"media fixture").unwrap();

        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Tv,
            title: "万神殿".into(),
            year: Some(2022),
            original_title: Some("Pantheon".into()),
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        let media = state.store.as_ref().ensure_media(media).unwrap();
        let fixture = store::test_support::FixtureFile {
            media_id: media.id,
            path: episode_path.display().to_string(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        };
        store::test_support::publish_fixture_file(&state.store.as_ref(), &fixture).unwrap();

        assert_eq!(
            crate::http::library::enqueue_probes_for_paths(
                &state,
                [std::path::PathBuf::from(&fixture.path)]
            ),
            1
        );
        let queued = state.probe.take_queued_for_test().unwrap();
        assert_eq!(queued.kind, MediaKind::Tv);
        assert!(
            !queued.force_fingerprint,
            "metadata ingestion should not override the library voiceprint toggle"
        );
    }

    #[test]
    fn newly_added_tv_episode_runs_voiceprint_when_chapter_detection_is_off() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::Store::open(tmp.path().join("data")).unwrap();
        let state = ApiState::new(
            store,
            "test-token".into(),
            RegistrySnapshot::builtin().unwrap(),
            Arc::new(EmptyFetcher),
            Arc::new(MemoryDownloader::new(tmp.path().join("stage"))),
        )
        .unwrap();
        let tv_library = state
            .store
            .as_ref()
            .default_library(MediaKind::Tv)
            .unwrap()
            .unwrap();
        state
            .store
            .as_ref()
            .set_library_intro_settings(&tv_library.id, true, true)
            .unwrap();
        let episode_path = tv_library.root_paths[0]
            .join("Pantheon")
            .join("Pantheon.S01E01.mkv");
        std::fs::create_dir_all(episode_path.parent().unwrap()).unwrap();
        std::fs::write(&episode_path, b"media fixture").unwrap();

        let media = state
            .store
            .as_ref()
            .ensure_media(Media {
                id: MediaId::new(),
                kind: MediaKind::Tv,
                title: "万神殿".into(),
                year: Some(2022),
                original_title: Some("Pantheon".into()),
                aliases: Vec::new(),
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .unwrap();
        let fixture = store::test_support::FixtureFile {
            media_id: media.id,
            path: episode_path.display().to_string(),
            season: Some(1),
            episode: Some(1),
            ..Default::default()
        };
        store::test_support::publish_fixture_file(&state.store.as_ref(), &fixture).unwrap();

        assert_eq!(
            crate::http::library::enqueue_probes_for_paths(
                &state,
                [std::path::PathBuf::from(&fixture.path)]
            ),
            1
        );
        let queued = state.probe.take_queued_for_test().unwrap();
        assert!(queued.force_fingerprint);
    }

    #[cfg(unix)]
    #[test]
    fn leased_intake_revalidation_rejects_symlink_nested_root_and_revision_drift() {
        let tmp = tempfile::tempdir().unwrap();
        let store = crate::Store::open(tmp.path().join("data")).unwrap();
        let movie = store.default_library(MediaKind::Movie).unwrap().unwrap();
        let root = movie.root_paths[0].clone();
        let tv = store.default_library(MediaKind::Tv).unwrap();
        let tv_id = tv.as_ref().map(|l| l.id.as_str());
        let tv_rev = tv
            .as_ref()
            .map(|l| store.library_config_revision(&l.id).unwrap());

        let src = tmp.path().join("in.mkv");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        let sym_dest = root.join("escape/Movie (2024)/Movie (2024).mkv");
        assert!(
            verify_leased_intake_state(
                &store,
                &movie.id,
                0,
                tv_id,
                tv_rev,
                vec![(src.clone(), sym_dest, MediaKind::Movie)],
            )
            .is_err()
        );

        let nested_dir = root.join("nested_lib");
        std::fs::create_dir_all(&nested_dir).unwrap();
        store
            .create_library(
                MediaKind::Movie,
                "Nested",
                &[nested_dir.to_str().unwrap()],
                "everyone",
                false,
                &[],
            )
            .unwrap();
        let nested_dest = nested_dir.join("Movie (2024).mkv");
        assert!(
            verify_leased_intake_state(
                &store,
                &movie.id,
                0,
                tv_id,
                tv_rev,
                vec![(src.clone(), nested_dest, MediaKind::Movie)],
            )
            .is_err()
        );

        let valid_dest = root.join("Safe (2024)/Safe (2024).mkv");
        assert!(
            verify_leased_intake_state(
                &store,
                &movie.id,
                99,
                tv_id,
                tv_rev,
                vec![(src, valid_dest, MediaKind::Movie)],
            )
            .is_err()
        );
    }
}
