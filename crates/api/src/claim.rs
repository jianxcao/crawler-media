use std::path::{Path, PathBuf};
use std::str::FromStr;

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use domain::{
    Confidence, FilePresence, FilePublication, FileVersion, LedgerId, LibraryFile, LibraryId, Media,
    MediaId, MediaKind, QualitySource, Release, UnitKey,
};
use serde::Deserialize;
use serde_json::{Value, json};
use store::StoreError;
use store::library_write::{FilePublication as PublicationRequest, WriteScope};

use crate::directory::transfer_plan;
use crate::management::{ApiError, ApiState};
use crate::scrape_store::ScrapeStoreExt;

#[derive(Deserialize)]
pub struct ClaimBody {
    path: String,
    title: String,
    kind: String,
    year: Option<u16>,
    tmdb_id: Option<String>,
}

/// Facts a claim observed on disk, plus the coverage the claim asserts.
///
/// The publication writer takes these instead of a retired per-unit ledger row
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
    /// Facts parsed out of the file name; coverage widens to every episode the
    /// release spans (a single episode stays one unit).
    pub(crate) fn from_release(release: &Release) -> Self {
        let units: Vec<UnitKey> = release
            .covered_episodes()
            .into_iter()
            .map(|(season, episode)| UnitKey::new(season as i32, episode as i32))
            .collect();
        Self {
            resolution: release.resolution.clone(),
            codec: release.codec.clone(),
            hdr: release.hdr.clone(),
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
            units: if units.is_empty() {
                vec![UnitKey::WHOLE]
            } else {
                units
            },
        }
    }

    /// An administrator claim identified the media but not the episode: one
    /// whole-work unit and no parsed quality facts.
    pub(crate) fn unidentified() -> Self {
        Self {
            resolution: None,
            codec: None,
            hdr: None,
            quality_source: QualitySource::Release,
            confidence: Confidence::High,
            filter_score: None,
            units: vec![UnitKey::WHOLE],
        }
    }
}

/// Resolve the owning Library, the existing identity at the destination and the
/// NFO context observed before publication, then commit one published file.
///
/// Mirrors the canonical publication path: the root comes from the destination
/// path, an existing row at that path keeps its file identity, and the NFO
/// sidecars observed here are recorded with the same transaction as the row.
pub(crate) fn publish_claimed_file(
    store: &store::Store,
    media: &Media,
    dest: &Path,
    facts: &ClaimedFacts,
) -> Result<LibraryFile, StoreError> {
    let owner = store
        .library_for_path_strict(dest, media.kind)?
        .ok_or_else(|| {
            StoreError::Invalid(format!("path is outside every Library root: {}", dest.display()))
        })?;
    let library_id = owner.id.parse::<LibraryId>()?;
    let root_id = owning_root_id(store, &owner.id, dest)?;
    let path = dest.display().to_string();
    let (file_id, version) = match store.file_coverage_by_path(&path)? {
        Some(row) => {
            let file_id = row.file.file_id;
            let version = store
                .get_file_version(file_id)?
                .ok_or_else(|| StoreError::Missing(format!("file {}", file_id)))?;
            (file_id, version)
        }
        None => {
            let file_id = LedgerId::new();
            let version = FileVersion {
                revision: 1,
                source_version: format!("{}-{}", store::unix_now(), file_id),
            };
            (file_id, version)
        }
    };
    let config_revision = store
        .library_config_revision(&owner.id)
        .unwrap_or(0);
    let locale = store::library_query::library_display_locale(store, library_id)
        .ok()
        .flatten()
        .unwrap_or_default();
    let observations = store::metadata::observe_nfo_inputs(
        dest,
        media.kind,
        &facts.units,
        &locale,
        &version,
        config_revision,
    );
    let artwork = store::files::DiscoveredArtwork::discover(dest);
    let file = LibraryFile {
        file_id,
        library_id,
        root_id,
        media_id: media.id,
        kind: media.kind,
        path: dest.to_path_buf(),
        version: version.clone(),
        presence: FilePresence::Present,
        publication: FilePublication::Published,
        size_bytes: None,
        stat_known: false,
        created_ns: None,
        modified_ns: None,
        source_digest: None,
        source_path: None,
        transfer_mode: None,
        resolution: facts.resolution.clone(),
        codec: facts.codec.clone(),
        hdr: facts.hdr.clone(),
        quality_source: facts.quality_source,
        confidence: facts.confidence,
        filter_score: facts.filter_score,
        release_quality: None,
        duration_ms: None,
        tracks_version: 0,
        poster_path: artwork.poster,
        backdrop_path: artwork.backdrop,
        still_path: artwork.still,
    };
    store.publish_files(
        &WriteScope {
            library_id,
            config_revision: 0,
        },
        &[PublicationRequest {
            file: &file,
            units: &facts.units,
            expected_version: Some(&version),
            nfo_observation: Some(&observations),
            probe_required: false,
        }],
        store::unix_now() * 1000,
    )?;
    tracing::info!(file_id = %file_id, path = %path, "Published claimed file");
    Ok(file)
}

/// The deepest root of `library_id` whose path contains `dest`.
///
/// Lexical containment: `store::library_paths::resolved_path` is crate-private, so
/// the API cannot normalize symlinks the way the legacy writer did. The two agree
/// whenever the roots and the destination are already in the same normalized form.
fn owning_root_id(store: &store::Store, library_id: &str, dest: &Path) -> Result<String, StoreError> {
    let mut best: Option<(usize, String)> = None;
    for root in store.list_library_roots()? {
        if root.library_id.as_deref() != Some(library_id) {
            continue;
        }
        if !dest.starts_with(&root.path) {
            continue;
        }
        let depth = root.path.components().count();
        if best
            .as_ref()
            .is_none_or(|(best_depth, _)| *best_depth < depth)
        {
            best = Some((depth, root.id.clone()));
        }
    }
    best.map(|(_, id)| id).ok_or_else(|| {
        StoreError::Invalid(format!(
            "no root of library {library_id} contains {}",
            dest.display()
        ))
    })
}

struct ClaimPlan {
    media: Media,
    release: Release,
    dest: PathBuf,
    requested: library::TransferMode,
    mirror_nfo: bool,
    scrape: bool,
}

fn plan_claim(
    state: &ApiState,
    body: &ClaimBody,
    kind: MediaKind,
    src: &Path,
) -> Result<ClaimPlan, ApiError> {
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
    let (root, mode, scrape) = {
        let store = state.store.lock();
        if !store
            .list_unidentified()?
            .iter()
            .any(|(path, _)| path == &body.path)
        {
            return Err(ApiError::missing(
                "unidentified.missing",
                "Unidentified not found".into(),
            ));
        }
        transfer_plan(&store, kind)?
    };
    let mirror_nfo = {
        let store = state.store.lock();
        let cfg = store
            .get_scrape_config()
            .map_err(|e| ApiError::invalid("unidentified.invalid", e.to_string()))?;
        scrape && cfg.effective.mirror_nfo
    };
    let naming = {
        let store = state.store.lock();
        store
            .naming_pattern(kind)
            .map_err(|e| ApiError::invalid("unidentified.invalid", e.to_string()))?
    };
    let dest = library::render_path(&root, &naming, &media, &release, src)
        .map_err(|err| ApiError::invalid("unidentified.invalid", err.to_string()))?;
    // 没进台账不等于可以覆盖：未扫描、识别失败，或上次提交失败留下的视频
    // 都还在磁盘上。源和目标是同一文件（原地认领）才允许继续。
    if dest.exists() && dest != src {
        return Err(ApiError::with(
            StatusCode::CONFLICT,
            "unidentified.destination_exists",
            "媒体库中已存在相同目标文件，禁止覆盖已有文件".to_string(),
        ));
    }
    let requested = library::resolve_mode(src, dest.parent().unwrap_or(Path::new(".")), mode);
    Ok(ClaimPlan {
        media,
        release,
        dest,
        requested,
        mirror_nfo,
        scrape,
    })
}

/// Move 先复制、库记录成功后再删源。中途失败时源还在，同一请求可以重试。
/// 返回 Move 需要清理的目标副本。
fn stage_claim_file(
    state: &ApiState,
    plan: &ClaimPlan,
    src: &Path,
) -> Result<Option<PathBuf>, ApiError> {
    let staged = if plan.requested == library::TransferMode::Move {
        library::TransferMode::Copy
    } else {
        plan.requested
    };
    library::transfer_file(src, &plan.dest, staged)
        .map_err(|err| ApiError::invalid("unidentified.invalid", err.to_string()))?;
    let rollback_dest =
        (plan.requested == library::TransferMode::Move && plan.dest != src).then(|| plan.dest.clone());
    let _ = library::scrape_beside(&plan.dest, &plan.media, plan.mirror_nfo, None);
    if plan.scrape {
        crate::poster_fetch::attach_poster(state, &plan.media, &plan.dest);
    }
    Ok(rollback_dest)
}

/// Publish the staged file and retire the Unidentified record.
fn commit_claim(
    state: &ApiState,
    unidentified: &str,
    media: &Media,
    plan: &ClaimPlan,
) -> Result<LibraryFile, ApiError> {
    let facts = ClaimedFacts::from_release(&plan.release);
    let store = state.store.lock();
    let file = publish_claimed_file(&store, media, &plan.dest, &facts)?;
    if let Err(error) = store.delete_unidentified(unidentified) {
        tracing::warn!(path = %unidentified, %error, "认领已入账，但未识别记录清理失败");
    }
    Ok(file)
}

fn discard_rollback(rollback_dest: &Option<PathBuf>) {
    if let Some(path) = rollback_dest {
        let _ = std::fs::remove_file(path);
    }
}

pub async fn claim_unidentified(
    State(state): State<ApiState>,
    Json(body): Json<ClaimBody>,
) -> Result<Json<Value>, ApiError> {
    if body.title.trim().is_empty() {
        return Err(ApiError::invalid(
            "unidentified.invalid",
            "title is required".into(),
        ));
    }
    let kind = MediaKind::from_str(&body.kind).map_err(|_| {
        ApiError::invalid("unidentified.invalid", "kind must be movie or tv".into())
    })?;
    let src = PathBuf::from(&body.path);
    if !src.is_file() {
        return Err(ApiError::invalid(
            "unidentified.invalid",
            "Unidentified file is missing".into(),
        ));
    }
    let plan = plan_claim(&state, &body, kind, &src)?;
    let rollback_dest = stage_claim_file(&state, &plan, &src)?;
    let media = match state.store.lock().ensure_media(plan.media.clone()) {
        Ok(media) => media,
        Err(error) => {
            discard_rollback(&rollback_dest);
            return Err(error.into());
        }
    };
    let file = match commit_claim(&state, &body.path, &media, &plan) {
        Ok(file) => file,
        Err(error) => {
            discard_rollback(&rollback_dest);
            return Err(error);
        }
    };
    if plan.requested == library::TransferMode::Move && plan.dest != src {
        if let Err(error) = std::fs::remove_file(&src) {
            tracing::warn!(src = %src.display(), %error, "认领已入账，但 Move 源文件清理失败");
        }
    }
    crate::http::library::enqueue_probes_for_files(&state, std::slice::from_ref(&file));
    Ok(Json(json!({
        "path": file.path.display().to_string(),
        "media_id": media.id.to_string(),
        "title": media.title,
    })))
}

pub async fn claim_unidentified_v1(
    state: State<ApiState>,
    body: Json<ClaimBody>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    match claim_unidentified(state, body).await {
        Ok(Json(data)) => crate::http::ok(data).into_response(),
        Err(error) => error.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn fixture(tmp: &Path, roots: &[&Path]) -> (store::Store, Media, store::Library) {
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
        (store, media, library)
    }

    /// Roots nest: the deepest root of the owning Library owns the claimed path.
    #[test]
    fn claim_uses_the_deepest_root_of_the_owning_library() {
        let tmp = tempfile::tempdir().unwrap();
        let outer = tmp.path().join("media");
        let inner = outer.join("Anime");
        let (store, media, library) = fixture(tmp.path(), &[&outer, &inner]);
        let inner_root = store
            .list_library_roots()
            .unwrap()
            .into_iter()
            .find(|root| root.path == inner)
            .unwrap();
        let dest = inner.join("The Matrix.1999.mkv");
        std::fs::write(&dest, b"matrix").unwrap();

        let file =
            publish_claimed_file(&store, &media, &dest, &ClaimedFacts::unidentified()).unwrap();
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
        std::fs::write(dest.with_extension("nfo"), "<movie><plot>Plot A</plot></movie>").unwrap();
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
        assert_eq!(store.get_file_units(file.file_id).unwrap(), vec![UnitKey::new(1, 2)]);
        let locale = store::library_query::library_display_locale(&store, library_id)
            .unwrap()
            .unwrap_or_default();
        let detail = store
            .library_item_detail(library_id, media.id, &locale)
            .unwrap()
            .unwrap();
        assert_eq!(detail.metadata.overview.value().map(String::as_str), Some("Plot A"));
    }

    /// Known deviation: the strict Library lookup resolves `dest`, the root
    /// selection here stays lexical, so a destination reached through a symlink
    /// is refused where the legacy writer (`Store::root_for_path`, which
    /// resolved both sides) accepted it. Observation: this call never invokes
    /// `resolved_path`, so the two agree only while `dest` and the roots are
    /// already in the same normalized form.
    #[cfg(unix)]
    #[test]
    fn claim_refuses_a_symlinked_destination_the_legacy_writer_accepted() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("media");
        let (store, media, _library) = fixture(tmp.path(), &[&root]);
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        let dest = link.join("The Matrix.1999.mkv");
        std::fs::write(&dest, b"matrix").unwrap();
        assert!(
            store.library_for_path_strict(&dest, MediaKind::Movie).unwrap().is_some(),
            "the strict owner lookup resolves the symlink and does find the Library"
        );

        match publish_claimed_file(&store, &media, &dest, &ClaimedFacts::unidentified()) {
            Err(StoreError::Invalid(message)) => {
                assert!(message.contains("contains"), "{message}")
            }
            other => panic!("expected the lexical root selection to refuse, got {other:?}"),
        }
    }
}
