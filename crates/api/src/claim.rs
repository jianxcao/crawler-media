use std::path::{Path, PathBuf};
use std::str::FromStr;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use domain::{Confidence, LibraryFile, Media, MediaId, MediaKind, QualitySource, Release, UnitKey};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::directory::transfer_plan;
use crate::management::{ApiError, ApiState};
use crate::scrape_store::ScrapeStoreExt;

mod bind;
mod publication;
#[cfg(test)]
pub(crate) use publication::publish_claimed_file;

/// The only claim body: the unidentified path plus one explicit action.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaimBody {
    path: String,
    action: ClaimAction,
}

/// Bind to an existing Media in place, or identify a new title and Transfer.
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ClaimAction {
    BindExisting {
        media_id: String,
    },
    IdentifyAndTransfer {
        title: String,
        kind: String,
        year: Option<u16>,
        tmdb_id: Option<String>,
    },
}

/// The identify-and-transfer use case's inputs.
pub(crate) struct IdentifyClaim {
    path: String,
    title: String,
    kind: String,
    year: Option<u16>,
    tmdb_id: Option<String>,
}

/// Facts a claim observed on disk, plus the coverage the claim asserts.
///
/// The publication writer takes these instead of a per-unit ledger row
/// so the claim path and the scan/import path feed the same file shape into
/// `publish_files`.
pub(crate) struct ClaimedFacts {
    resolution: Option<String>,
    codec: Option<String>,
    hdr: Option<String>,
    quality_source: QualitySource,
    confidence: Confidence,
    filter_score: Option<i32>,
    units: Vec<UnitKey>,
}

impl ClaimedFacts {
    /// Facts parsed out of the file name. Coverage spans every episode the
    /// release names; only a movie/video defaults to the whole-work unit. An
    /// unknown TV episode carries no coverage instead of a synthesized whole.
    pub(crate) fn from_release(kind: MediaKind, release: &Release) -> Self {
        let units: Vec<UnitKey> = release
            .covered_episodes()
            .into_iter()
            .map(|(season, episode)| UnitKey::new(season as i32, episode as i32))
            .collect();
        let units = match kind {
            MediaKind::Tv => units,
            _ => vec![UnitKey::WHOLE],
        };
        Self {
            resolution: release.resolution.clone(),
            codec: release.codec.clone(),
            hdr: release.hdr.clone(),
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
            units,
        }
    }

    /// Facts for an administrator claim onto an existing Media: the file name
    /// at `path` is parsed so a named multi-episode file keeps its coverage.
    pub(crate) fn for_path(kind: MediaKind, path: &Path) -> Self {
        let parsed = release::parse(
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default(),
        );
        Self::from_release(kind, &parsed)
    }

    /// Coverage this claim asserts.
    #[cfg(test)]
    pub(crate) fn units(&self) -> &[UnitKey] {
        &self.units
    }
}

struct ClaimPlan {
    source: store::UnidentifiedRow,
    source_version: String,
    media: Media,
    release: Release,
    dest: PathBuf,
    requested: library::TransferMode,
    mirror_nfo: bool,
    scrape: bool,
}

fn plan_claim(
    state: &ApiState,
    body: &IdentifyClaim,
    kind: MediaKind,
    src: &Path,
) -> Result<ClaimPlan, axum::response::Response> {
    use axum::response::IntoResponse;
    let (source, root, mode, scrape, mirror_nfo, naming) = admitted_store(state, |store| {
        let source = store
            .get_unidentified(&body.path)
            .map_err(store_response)?
            .ok_or_else(|| {
                ApiError::missing("unidentified.missing", "Unidentified not found".into())
                    .into_response()
            })?;
        let (root, mode, scrape) = transfer_plan(store, kind).map_err(store_response)?;
        let mirror_nfo = scrape
            && store
                .get_scrape_config()
                .map_err(store_response)?
                .effective
                .mirror_nfo;
        let naming = store.naming_pattern(kind).map_err(store_response)?;
        Ok((source, root, mode, scrape, mirror_nfo, naming))
    })?;
    let source_version = library::observe_source(src)
        .map_err(|error| {
            ApiError::invalid("unidentified.source", error.to_string()).into_response()
        })?
        .source_version;
    let media = Media {
        id: MediaId::new(),
        kind,
        title: body.title.trim().to_string(),
        year: body.year,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: body.tmdb_id.clone().filter(|id| !id.is_empty()),
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    };
    let parsed = release::parse(
        src.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default(),
    );
    let release = Release {
        title: media.title.clone(),
        year: media.year.or(parsed.year),
        confidence: Confidence::High,
        ..parsed
    };
    let dest = library::render_path(&root, &naming, &media, &release, src).map_err(|err| {
        ApiError::invalid("unidentified.invalid", err.to_string()).into_response()
    })?;
    // 没进台账不等于可以覆盖：未扫描、识别失败，或上次提交失败留下的视频
    // 都还在磁盘上。源和目标是同一文件（原地认领）才允许继续。
    if dest.exists() && dest != src {
        return Err(ApiError::with(
            StatusCode::CONFLICT,
            "unidentified.destination_exists",
            "媒体库中已存在相同目标文件，禁止覆盖已有文件".to_string(),
        )
        .into_response());
    }
    let requested = library::resolve_mode(src, dest.parent().unwrap_or(Path::new(".")), mode);
    Ok(ClaimPlan {
        source,
        source_version,
        media,
        release,
        dest,
        requested,
        mirror_nfo,
        scrape,
    })
}

struct StagedClaim {
    rollback_dest: Option<PathBuf>,
    destination_version: String,
}

fn check_claim_source(plan: &ClaimPlan, src: &Path) -> Result<(), ApiError> {
    if library::current_source_version(src) != plan.source_version {
        tracing::warn!(path=%src.display(),"Claim input source changed during Transfer");
        return Err(ApiError::with(
            StatusCode::CONFLICT,
            "unidentified.source_changed",
            "Unidentified source changed during Transfer".into(),
        ));
    }
    Ok(())
}

/// Move stages a Copy; the original is removed only after publication and source validation.
fn stage_claim_file(
    state: &ApiState,
    plan: &ClaimPlan,
    src: &Path,
) -> Result<StagedClaim, ApiError> {
    check_claim_source(plan, src)?;
    let staged = if plan.requested == library::TransferMode::Move {
        library::TransferMode::Copy
    } else {
        plan.requested
    };
    library::transfer_file(src, &plan.dest, staged)
        .map_err(|error| ApiError::invalid("unidentified.invalid", error.to_string()))?;
    let receipt = StagedClaim {
        rollback_dest: (plan.dest != src).then(|| plan.dest.clone()),
        destination_version: library::current_source_version(&plan.dest),
    };
    let _ = library::scrape_beside(&plan.dest, &plan.media, plan.mirror_nfo, None);
    if plan.scrape {
        crate::poster_fetch::attach_poster(state, &plan.media, &plan.dest);
    }
    if let Err(error) = check_claim_source(plan, src) {
        discard_rollback(&receipt);
        return Err(error);
    }
    Ok(receipt)
}

/// The original parked identity is consumed by the same transaction as publication.
fn commit_claim(
    state: &ApiState,
    media: &Media,
    plan: &ClaimPlan,
    src: &Path,
    staged: &StagedClaim,
) -> Result<LibraryFile, axum::response::Response> {
    use axum::response::IntoResponse;
    check_claim_source(plan, src).map_err(IntoResponse::into_response)?;
    let facts = ClaimedFacts::from_release(plan.media.kind, &plan.release);
    publication::publish_http_unidentified(
        state,
        media,
        &plan.dest,
        &facts,
        &plan.source,
        Some(&staged.destination_version),
    )
}

fn discard_rollback(staged: &StagedClaim) {
    if let Some(path) = &staged.rollback_dest
        && library::current_source_version(path) == staged.destination_version
        && let Err(error) = std::fs::remove_file(path)
    {
        tracing::warn!(path=%path.display(),%error,"Claim staged Copy cleanup failed");
    }
}

fn remove_moved_source(state: &ApiState, plan: &ClaimPlan, src: &Path) {
    if plan.requested != library::TransferMode::Move || plan.dest == src {
        return;
    }
    if check_claim_source(plan, src).is_err() {
        if let Err(error) = state
            .store
            .lock()
            .insert_unidentified(src.display().to_string(), Confidence::Low)
        {
            tracing::error!(path=%src.display(),%error,"Changed Claim input retained but could not be parked");
        }
        return;
    }
    if let Err(error) = std::fs::remove_file(src) {
        tracing::warn!(path=%src.display(),%error,"Claim published but Move input cleanup failed");
    }
}

fn identify_and_transfer(
    State(state): State<ApiState>,
    Json(body): Json<IdentifyClaim>,
) -> Result<Json<Value>, axum::response::Response> {
    use axum::response::IntoResponse;
    if body.title.trim().is_empty() {
        return Err(
            ApiError::invalid("unidentified.invalid", "title is required".into()).into_response(),
        );
    }
    let kind = MediaKind::from_str(&body.kind).map_err(|_| {
        ApiError::invalid("unidentified.invalid", "kind must be movie or tv".into()).into_response()
    })?;
    let src = PathBuf::from(&body.path);
    if !src.is_file() {
        return Err(ApiError::invalid(
            "unidentified.invalid",
            "Unidentified file is missing".into(),
        )
        .into_response());
    }
    admitted_store(&state, |_| Ok(()))?;
    let plan = plan_claim(&state, &body, kind, &src)?;
    admitted_store(&state, |_| Ok(()))?;
    let staged = stage_claim_file(&state, &plan, &src).map_err(IntoResponse::into_response)?;
    let media = match admitted_store(&state, |store| {
        store
            .ensure_media(plan.media.clone())
            .map_err(store_response)
    }) {
        Ok(media) => media,
        Err(error) => {
            discard_rollback(&staged);
            return Err(error);
        }
    };
    let file = match commit_claim(&state, &media, &plan, &src, &staged) {
        Ok(file) => file,
        Err(error) => {
            discard_rollback(&staged);
            return Err(error);
        }
    };
    remove_moved_source(&state, &plan, &src);
    crate::http::library::enqueue_probes_for_files(&state, std::slice::from_ref(&file));
    Ok(Json(json!({
        "path": file.path.display().to_string(),
        "media_id": media.id.to_string(),
        "title": media.title,
    })))
}

/// POST /unidentified/claim — the single claim entry; dispatches on `action`.
/// Any other body shape is a 400.
pub async fn claim_unidentified(
    state: State<ApiState>,
    Json(raw): Json<Value>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let body: ClaimBody = match serde_json::from_value(raw) {
        Ok(body) => body,
        Err(error) => {
            return ApiError::invalid("unidentified.invalid", format!("claim body: {error}"))
                .into_response();
        }
    };
    let blocking = state.0.clone();
    crate::http::subscriptions::guarded_blocking(&blocking, move || claim_blocking(state, body))
        .await
}

fn claim_blocking(state: State<ApiState>, body: ClaimBody) -> axum::response::Response {
    use axum::response::IntoResponse;
    if let Err(response) = admitted_store(&state.0, |_| Ok(())) {
        return response;
    }
    match body.action {
        ClaimAction::BindExisting { media_id } => {
            bind::bind_existing(&state.0, &media_id, &body.path)
        }
        ClaimAction::IdentifyAndTransfer {
            title,
            kind,
            year,
            tmdb_id,
        } => {
            let claim = IdentifyClaim {
                path: body.path,
                title,
                kind,
                year,
                tmdb_id,
            };
            match identify_and_transfer(state, Json(claim)) {
                Ok(Json(data)) => crate::http::ok(data).into_response(),
                Err(error) => error.into_response(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::{FilePresence, FilePublication, LibraryId};
    use parking_lot::Mutex;

    fn media() -> Media {
        Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: "The Matrix".into(),
            year: Some(1999),
            original_title: None,
            aliases: Vec::new(),
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        }
    }

    /// A store with one claimed media and a Library registered over `roots`.
    fn fixture(tmp: &Path, roots: &[&Path]) -> (Mutex<store::Store>, Media, store::Library) {
        let store = store::Store::open(tmp).unwrap();
        let media = media();
        store.insert_media(&media).unwrap();
        let paths: Vec<&str> = roots.iter().map(|root| root.to_str().unwrap()).collect();
        for root in roots {
            std::fs::create_dir_all(root).unwrap();
        }
        let library = store
            .create_library(MediaKind::Movie, "Movies", &paths, "everyone", true, &[])
            .unwrap();
        (Mutex::new(store), media, library)
    }

    /// Roots nest: the deepest root of the owning Library owns the claimed path.
    #[test]
    fn claim_uses_the_deepest_root_of_the_owning_library() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path().join("media");
        let inner = outer.join("Anime");
        let (store, media, library) = fixture(tmp.path(), &[&outer, &inner]);
        let inner_root = store
            .lock()
            .list_library_roots()
            .unwrap()
            .into_iter()
            .find(|root| root.path == inner)
            .unwrap();
        let dest = inner.join("The Matrix.1999.mkv");
        std::fs::write(&dest, b"matrix").unwrap();

        let file = publish_claimed_file(
            &store,
            &media,
            &dest,
            &ClaimedFacts::for_path(media.kind, &dest),
        )
        .unwrap();
        assert_eq!(file.root_id, inner_root.id);
        assert_eq!(file.library_id.to_string(), library.id);
    }

    /// The claimed verdict, its coverage and the NFO sidecar beside it all reach
    /// the published row.
    #[test]
    fn claim_publishes_facts_units_and_the_nfo_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("media");
        let (store, media, library) = fixture(tmp.path(), &[&root]);
        let dest = root.join("The Matrix.1999.mkv");
        std::fs::write(&dest, b"matrix").unwrap();
        std::fs::write(
            dest.with_extension("nfo"),
            "<movie><plot>Plot A</plot></movie>",
        )
        .unwrap();
        let facts = ClaimedFacts {
            resolution: Some("1080p".into()),
            codec: Some("H264".into()),
            hdr: Some("HDR10".into()),
            quality_source: QualitySource::Probe,
            confidence: Confidence::High,
            filter_score: Some(88),
            units: vec![UnitKey::new(1, 2)],
        };

        let file = publish_claimed_file(&store, &media, &dest, &facts).unwrap();
        let library_id: LibraryId = library.id.parse().unwrap();
        let files = store
            .lock()
            .library_file_page(library_id, media.id, None, 100)
            .unwrap()
            .files;
        assert_eq!(files.len(), 1);
        let published = &files[0];
        assert_eq!(published.file_id, file.file_id);
        assert_eq!(published.root_id, file.root_id);
        assert_eq!(published.resolution.as_deref(), Some("1080p"));
        assert_eq!(published.codec.as_deref(), Some("H264"));
        assert_eq!(published.hdr.as_deref(), Some("HDR10"));
        assert_eq!(published.quality_source, QualitySource::Probe);
        assert_eq!(published.confidence, Confidence::High);
        assert_eq!(published.filter_score, Some(88));
        assert_eq!(published.presence, FilePresence::Present);
        assert_eq!(published.publication, FilePublication::Published);
        assert_eq!(
            store.lock().get_file_units(file.file_id).unwrap(),
            vec![UnitKey::new(1, 2)]
        );
        let locale = store::library_query::library_display_locale(&store.lock(), library_id)
            .unwrap()
            .unwrap_or_default();
        let detail = store
            .lock()
            .library_item_detail(library_id, media.id, &locale)
            .unwrap()
            .unwrap();
        assert_eq!(
            detail.metadata.overview.value().map(String::as_str),
            Some("Plot A")
        );
    }

    /// Ownership and publication use the canonical locator of a symlinked path.
    #[cfg(unix)]
    #[test]
    fn claim_accepts_a_symlinked_destination_inside_the_owning_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("media");
        let (store, media, library) = fixture(tmp.path(), &[&root]);
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        let dest = link.join("The Matrix.1999.mkv");
        std::fs::write(&dest, b"matrix").unwrap();
        assert!(
            store
                .lock()
                .library_for_path_strict(&dest, MediaKind::Movie)
                .unwrap()
                .is_some(),
            "the strict owner lookup resolves the symlink and does find the Library"
        );

        let file = publish_claimed_file(
            &store,
            &media,
            &dest,
            &ClaimedFacts::for_path(media.kind, &dest),
        )
        .unwrap();
        assert_eq!(file.library_id.to_string(), library.id);
        assert_eq!(file.path, std::fs::canonicalize(dest).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn claim_through_a_registered_root_alias_keeps_one_file_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let physical = tmp.path().join("physical");
        let alias = tmp.path().join("alias");
        std::fs::create_dir_all(&physical).unwrap();
        std::os::unix::fs::symlink(&physical, &alias).unwrap();
        let (store, media, _) = fixture(tmp.path(), &[&alias]);
        let dest = alias.join("The Matrix.1999.mkv");
        std::fs::write(&dest, b"matrix").unwrap();
        let facts = ClaimedFacts::for_path(media.kind, &dest);
        let first = publish_claimed_file(&store, &media, &dest, &facts).unwrap();
        let physical_path = std::fs::canonicalize(&dest).unwrap();
        let second = publish_claimed_file(&store, &media, &physical_path, &facts).unwrap();
        assert_eq!(first.file_id, second.file_id);
        assert_eq!(
            store
                .lock()
                .file_coverage_by_path(dest.to_str().unwrap())
                .unwrap()
                .unwrap()
                .file
                .file_id,
            first.file_id
        );
    }

    #[cfg(unix)]
    #[test]
    fn claim_rejects_a_symlink_escaping_the_owning_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("media");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        let (store, media, _) = fixture(tmp.path(), &[&root]);
        std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();
        let dest = root.join("escape/The Matrix.1999.mkv");
        std::fs::write(&dest, b"matrix").unwrap();
        assert!(
            publish_claimed_file(
                &store,
                &media,
                &dest,
                &ClaimedFacts::for_path(media.kind, &dest)
            )
            .is_err()
        );
        assert!(
            store
                .lock()
                .file_coverage_by_path(dest.to_str().unwrap())
                .unwrap()
                .is_none()
        );
    }

    /// Administrator claims parse the real file name: a named multi-episode TV
    /// file keeps every episode, an unknown TV
    /// episode asserts no coverage, and only movie/video default to whole.
    #[test]
    fn claim_coverage_follows_the_file_name_and_media_kind() {
        let units = |kind, name: &str| {
            ClaimedFacts::for_path(kind, Path::new(name))
                .units()
                .to_vec()
        };
        assert_eq!(
            units(MediaKind::Tv, "Show.S01E01-E02.1080p.mkv"),
            vec![UnitKey::new(1, 1), UnitKey::new(1, 2)]
        );
        assert_eq!(
            units(MediaKind::Tv, "Show.S00E00.mkv"),
            vec![UnitKey::new(0, 0)]
        );
        assert!(units(MediaKind::Tv, "Show.Behind.the.Scenes.mkv").is_empty());
        assert_eq!(
            units(MediaKind::Movie, "The Matrix.1999.mkv"),
            vec![UnitKey::WHOLE]
        );
    }

    /// Claim publication uses the owning Library's current configuration revision.
    #[test]
    fn claim_publishes_under_the_current_config_revision() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("media");
        let (store, media, library) = fixture(tmp.path(), &[&root]);
        let dest = root.join("The Matrix.1999.mkv");
        std::fs::write(&dest, b"matrix").unwrap();
        {
            let store = store.lock();
            store
                .begin_library_reconfiguration(
                    &store::library_operations::LibraryReconfiguration {
                        operation_id: "claim-current-config".into(),
                        library_id: library.id.parse().unwrap(),
                        expected_revision: 0,
                        root_paths: None,
                        display_locale: Some("ja-JP".into()),
                    },
                    1_000,
                )
                .unwrap();
            for _ in 0..10 {
                if store
                    .advance_library_operation("claim-current-config", 1_100, 100)
                    .unwrap()
                    .state
                    == "succeeded"
                {
                    break;
                }
            }
            assert_eq!(store.library_config_revision(&library.id).unwrap(), 1);
            assert_eq!(store.library_lifecycle_state(&library.id).unwrap(), "ready");
        }
        let file = publish_claimed_file(
            &store,
            &media,
            &dest,
            &ClaimedFacts::for_path(media.kind, &dest),
        )
        .unwrap();
        assert_eq!(
            store.lock().get_file_units(file.file_id).unwrap(),
            vec![UnitKey::WHOLE]
        );
    }
}

fn admitted_store<T>(
    state: &ApiState,
    work: impl FnOnce(&store::Store) -> Result<T, axum::response::Response>,
) -> Result<T, axum::response::Response> {
    crate::http::store_wait::with_bounded_store(state.store(), |store| {
        state.require_writable()?;
        state.require_write_admitted()?;
        work(store)
    })
    .ok_or_else(crate::http::store_wait::busy_response)?
}

fn store_response(error: store::StoreError) -> axum::response::Response {
    use axum::response::IntoResponse;
    tracing::error!(%error, "Claim Store access failed");
    ApiError::from(error).into_response()
}
