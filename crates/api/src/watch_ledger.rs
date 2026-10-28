//! Scan publication: prepare DB context, observe files without Store, then commit.

use std::path::PathBuf;

use domain::{Confidence, LedgerId, Media, MediaId, MediaKind, QualitySource, Release, UnitKey};
use store::ScannedLedger;
use store::metadata::nfo_input::{
    NfoInputObserver, NfoInputReconcileObserved, NfoInputReconcilePlan,
};

use crate::Store;

#[cfg(test)]
mod tests;

pub(crate) struct ScanLedgerPlan {
    files: Vec<PlannedFile>,
    existing: Vec<ExistingFile>,
    observer: NfoInputObserver,
}

struct PlannedFile {
    row: ScannedLedger,
    kind: MediaKind,
    context: store::ScanWriteContext,
    locale: String,
}

struct ExistingFile {
    file: domain::LibraryFile,
    reconcile: Option<NfoInputReconcilePlan>,
    context: store::ScanWriteContext,
}

pub(crate) struct ObservedScanFiles {
    rows: Vec<ScannedLedger>,
    contexts: Vec<store::ScanWriteContext>,
    existing: Vec<(
        domain::LibraryFile,
        store::ScanWriteContext,
        Option<NfoInputReconcileObserved>,
    )>,
}

/// DB-only planning. The caller releases Store and its publication gate before observe.
pub(crate) fn plan_paths(
    store: &Store,
    paths: impl IntoIterator<Item = library::TransferredFile>,
) -> Result<ScanLedgerPlan, String> {
    let mut plan = ScanLedgerPlan::new(store);
    for file in paths {
        if plan.reuse(store, &file.path, true)? {
            continue;
        }
        let parsed = file.identified_release.unwrap_or_else(|| {
            release::parse(
                file.path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or_default(),
            )
        });
        let mut incoming = media_from_release(&parsed);
        if let Some(identity) = file.nfo_identity {
            incoming.kind = identity.kind;
            incoming.title = identity.title.unwrap_or(incoming.title);
            incoming.year = identity.year;
            incoming.original_title = identity.original_title;
            incoming.tmdb_id = identity.tmdb_id;
            incoming.douban_id = identity.douban_id;
            incoming.tvdb_id = identity.tvdb_id;
            incoming.bangumi_id = identity.bangumi_id;
            incoming.anilist_id = identity.anilist_id;
        }
        let media = store
            .resolve_scanned_media(incoming)
            .map_err(|e| e.to_string())?;
        plan.files
            .push(planned_file(store, file.path, media, Some(parsed))?);
    }
    Ok(plan)
}

pub(crate) fn plan_video_paths(
    store: &Store,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Result<ScanLedgerPlan, String> {
    let mut plan = ScanLedgerPlan::new(store);
    for path in paths {
        if plan.reuse(store, &path, false)? {
            continue;
        }
        let title = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        let media = store
            .ensure_media(Media {
                id: MediaId::new(),
                kind: MediaKind::Video,
                title,
                year: None,
                original_title: None,
                aliases: vec![],
                tmdb_id: None,
                douban_id: None,
                tvdb_id: None,
                bangumi_id: None,
                anilist_id: None,
            })
            .map_err(|e| e.to_string())?;
        plan.files.push(planned_file(store, path, media, None)?);
    }
    Ok(plan)
}

fn planned_file(
    store: &Store,
    path: PathBuf,
    media: Media,
    parsed: Option<Release>,
) -> Result<PlannedFile, String> {
    let units = match (&parsed, media.kind) {
        (Some(parsed), MediaKind::Tv) if parsed.abnormal_episode_span().is_some() => {
            let (from, to) = parsed.abnormal_episode_span().unwrap_or_default();
            tracing::warn!(
                path = %path.display(),
                from,
                to,
                max = Release::MAX_PLAUSIBLE_FILE_EPISODES,
                "abnormal episode mapping: file parsed as covering {} episodes; keeping only the first",
                to - from + 1
            );
            vec![UnitKey::new(parsed.season.unwrap_or(1) as i32, from as i32)]
        }
        (Some(parsed), MediaKind::Tv) => parsed
            .covered_episodes()
            .into_iter()
            .map(|(season, episode)| UnitKey::new(season as i32, episode as i32))
            .collect(),
        _ => vec![UnitKey::WHOLE],
    };
    let context = store
        .scan_write_context(&path, media.kind)
        .map_err(|e| e.to_string())?;
    let locale = store::library_query::library_display_locale(store, context.library_id)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();
    Ok(PlannedFile {
        kind: media.kind,
        context,
        locale,
        row: ScannedLedger {
            file_id: LedgerId::new(),
            media_id: media.id,
            path: path.display().to_string(),
            units,
            resolution: parsed
                .as_ref()
                .and_then(|release| release.resolution.clone()),
            codec: parsed.as_ref().and_then(|release| release.codec.clone()),
            hdr: parsed.as_ref().and_then(|release| release.hdr.clone()),
            quality_source: QualitySource::Release,
            confidence: parsed
                .as_ref()
                .map_or(Confidence::High, |release| release.confidence),
            filter_score: None,
            source_version: String::new(),
            source_digest: None,
            size_bytes: None,
            created_ns: None,
            modified_ns: None,
            artwork: Default::default(),
            observations: vec![],
        },
    })
}

impl ScanLedgerPlan {
    fn new(store: &Store) -> Self {
        Self {
            files: vec![],
            existing: vec![],
            observer: store.nfo_input_observer(),
        }
    }

    fn reuse(&mut self, store: &Store, path: &std::path::Path, nfo: bool) -> Result<bool, String> {
        let Some(row) = store
            .file_coverage_by_path(&path.to_string_lossy())
            .map_err(|e| e.to_string())?
        else {
            return Ok(false);
        };
        let reconcile = if nfo {
            store
                .plan_nfo_input_reconcile(path)
                .map_err(|e| e.to_string())?
        } else {
            None
        };
        let context = store
            .scan_write_context(path, row.file.kind)
            .map_err(|e| e.to_string())?;
        self.existing.push(ExistingFile {
            file: row.file,
            reconcile,
            context,
        });
        Ok(true)
    }

    /// Cold source, artwork and NFO reads. This value owns no Store or work lease.
    pub(crate) fn observe(self) -> Result<ObservedScanFiles, String> {
        let mut rows = Vec::with_capacity(self.files.len());
        let mut contexts = Vec::with_capacity(self.files.len());
        for PlannedFile {
            mut row,
            kind,
            context,
            locale,
        } in self.files
        {
            let path = std::path::Path::new(&row.path);
            let source = library::observe_source(path).map_err(|e| e.to_string())?;
            row.source_version = source.source_version;
            row.source_digest = source.content_digest;
            row.size_bytes = source.size_bytes.and_then(|size| i64::try_from(size).ok());
            row.created_ns = source.created_ns;
            row.modified_ns = source.modified_ns;
            row.artwork = store::files::DiscoveredArtwork::discover(path);
            if kind != MediaKind::Video {
                row.observations = self.observer.observe(
                    path,
                    kind,
                    &row.units,
                    &locale,
                    &domain::FileVersion {
                        revision: 1,
                        source_version: row.source_version.clone(),
                    },
                    context.config_revision,
                );
            }
            if library::current_source_version(path) != row.source_version {
                return Err(format!(
                    "source changed during scan observation: {}",
                    path.display()
                ));
            }
            rows.push(row);
            contexts.push(context);
        }
        let mut existing = Vec::with_capacity(self.existing.len());
        for prepared in self.existing {
            let mut observed = prepared.reconcile.map(NfoInputReconcilePlan::observe);
            if library::current_source_version(&prepared.file.path)
                != prepared.file.version.source_version
            {
                tracing::warn!(path = %prepared.file.path.display(), "scan source changed; defer NFO inputs until source reconciliation");
                observed = None;
            }
            existing.push((prepared.file, prepared.context, observed));
        }
        Ok(ObservedScanFiles {
            rows,
            contexts,
            existing,
        })
    }
}

impl ObservedScanFiles {
    /// DB-only commit. The caller revalidates the scan scope in its publication gate.
    pub(crate) fn record(self, store: &Store) -> Result<Vec<PathBuf>, String> {
        let mut inserted = Vec::new();
        for (file, context, observed) in self.existing {
            let observations = observed.map(NfoInputReconcileObserved::into_observations);
            if store
                .refresh_scanned_file(&file, &context, observations.as_deref())
                .map_err(|e| e.to_string())?
            {
                inserted.push(file.path);
            }
        }
        let now_ms = crate::job_loop::unix_now() * 1000;
        for (batch, contexts) in self
            .rows
            .chunks(store::MAX_SCAN_BATCH)
            .zip(self.contexts.chunks(store::MAX_SCAN_BATCH))
        {
            store
                .insert_scanned_files_in_contexts(batch, contexts, now_ms)
                .map_err(|e| e.to_string())?;
        }
        inserted.extend(self.rows.into_iter().map(|row| PathBuf::from(row.path)));
        Ok(inserted)
    }
}

fn media_from_release(parsed: &Release) -> Media {
    Media {
        id: MediaId::new(),
        kind: if parsed.season.is_some() {
            MediaKind::Tv
        } else {
            MediaKind::Movie
        },
        title: parsed.title.clone(),
        year: parsed.year,
        original_title: None,
        aliases: Vec::new(),
        tmdb_id: None,
        douban_id: None,
        tvdb_id: None,
        bangumi_id: None,
        anilist_id: None,
    }
}
