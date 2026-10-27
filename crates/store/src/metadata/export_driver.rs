//! Driver for generated NFO sidecars: capture external edits, render effective
//! metadata, replace atomically, register, and recover after a crash.
//!
//! Why the file is a *projection* of effective metadata: a generated sidecar is
//! also a metadata **input** (Task 8), so whatever it holds is read back with
//! sidecar authority and would otherwise outvote the provider forever. The
//! driver therefore (1) never lets an edit to the file vanish silently — it is
//! captured as an override before the rewrite, and (2) re-renders the whole file
//! from effective metadata afterwards, so the file can never lock a field the
//! library no longer believes.

use std::collections::BTreeMap;
use std::path::Path;

use domain::{
    EntityScope, FieldValue, FileVersion, LibraryId, MetadataCast, MetadataFields, MetadataGenre,
};
use rusqlite::{OptionalExtension, params};

use super::export::NfoExportRequest;
use crate::{Store, StoreError};

/// What the driver did to the file on this pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NfoExportAction {
    /// The file already held the rendered body: only the registration was written.
    Confirmed,
    /// The file was atomically replaced with the rendered body.
    Replaced,
    /// Crash recovery found the replacement already done and only registered it.
    Registered,
}

/// Observable result of one export pass.
#[derive(Clone, Debug, PartialEq)]
pub struct NfoExportOutcome {
    pub action: NfoExportAction,
    pub export_id: String,
    pub output_digest: String,
}

impl Store {
    /// Build the export request for the ledgered video file at `video`.
    ///
    /// The Library comes from the ledger row itself, so every caller mirrors the
    /// file into the Library that owns it. `Ok(None)` means the path has no
    /// ledger row (nothing to mirror yet). The request's `locale` stays empty:
    /// every lookup resolves the Library display locale, exactly as the sidecar
    /// observation does.
    pub fn nfo_export_target(
        &self,
        video: &Path,
        sidecar: &Path,
        scope: EntityScope,
        operation_id: &str,
        export_id: &str,
    ) -> Result<Option<NfoExportRequest>, StoreError> {
        let Some(mut request) =
            self.nfo_export_target_unobserved(video, sidecar, scope, operation_id, export_id)?
        else {
            return Ok(None);
        };
        request.expected_digest = library::nfo::nfo_digest(sidecar)?;
        Ok(Some(request))
    }

    /// [`Self::nfo_export_target`] without reading the sidecar: `expected_digest`
    /// stays `None` and the caller fills it from an off-lock observation (S01).
    pub fn nfo_export_target_unobserved(
        &self,
        video: &Path,
        sidecar: &Path,
        scope: EntityScope,
        operation_id: &str,
        export_id: &str,
    ) -> Result<Option<NfoExportRequest>, StoreError> {
        let key = crate::path_key::canonical_recorded_key(&self.library, &video.to_string_lossy());
        let row: Option<(String, String, String, String, i64, String)> = self
            .library
            .query_row(
                "SELECT media_id, library_id, root_id, file_id, file_revision, source_version
                 FROM library_files WHERE path_key = norm_path_key(?1) LIMIT 1",
                params![key],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                    ))
                },
            )
            .optional()?;
        let Some((media_id, library_id, root_id, file_id, revision, source_version)) = row else {
            return Ok(None);
        };
        let library: LibraryId = library_id.parse()?;
        Ok(Some(NfoExportRequest {
            operation_id: operation_id.to_string(),
            export_id: export_id.to_string(),
            library_id: library,
            media_id: media_id.parse()?,
            root_id,
            file_id: file_id.parse()?,
            expected_version: FileVersion {
                revision,
                source_version,
            },
            config_revision: self.library_config_revision(&library_id)?,
            scope,
            locale: String::new(),
            path: sidecar.to_path_buf(),
            expected_digest: None,
            body: String::new(),
            output_digest: String::new(),
            fields: MetadataFields::default(),
            field_origins: BTreeMap::new(),
            local_edits: MetadataFields::default(),
        }))
    }

    /// R18: this Library's local edits of a generated sidecar, observed off-lock.
    ///
    /// The edits are the sidecar's readable fields that differ from the base
    /// effective fields of the last export (`export_fields_json`), compared after a
    /// render/parse round trip so serialisation noise is not mistaken for an edit.
    /// `Ok(None)` means there is no baseline (never exported here, or the sidecar is
    /// gone/unreadable): a hand-written NFO keeps its own sidecar authority.
    /// Nothing is written: the edits travel in the export request and are rendered
    /// back into this sidecar only, never promoted to global Media overrides.
    pub fn nfo_local_edits_observed(
        &self,
        request: &NfoExportRequest,
        observation: &NfoSidecarObservation,
    ) -> Result<Option<MetadataFields>, StoreError> {
        let Some(anchor) = self.nfo_export_anchor(request)? else {
            return Ok(None);
        };
        let Some(baseline) = anchor.fields else {
            return Ok(None);
        };
        if observation.digest.is_none() {
            return Ok(None);
        }
        let Some(parsed) = observation.parsed.as_ref() else {
            tracing::warn!(
                path = %request.path.display(),
                "generated NFO is no longer parseable; no local edits are carried"
            );
            return Ok(None);
        };
        Ok(Some(local_edits(parsed, &baseline, request.scope)))
    }

    /// Locked phase 1 of an export: render effective metadata and persist the intent.
    ///
    /// `begin_nfo_export` validates file/source/config/Media binding and refuses a
    /// stale request; no file is touched.
    pub fn plan_nfo_export(
        &self,
        request: &NfoExportRequest,
        now_ms: i64,
    ) -> Result<NfoExportRequest, StoreError> {
        let locale = self.nfo_export_locale(request)?;
        let effective = self.effective_metadata_fields(
            request.library_id,
            request.media_id,
            request.scope,
            &locale,
        )?;
        let mut rendered = effective.fields.clone();
        overlay_fields(&mut rendered, &request.local_edits);
        let body = library::nfo::render_nfo(request.scope, &rendered);
        let output_digest = library::nfo::nfo_body_digest(&body);
        let plan = NfoExportRequest {
            locale,
            body,
            output_digest,
            fields: effective.fields,
            field_origins: effective.field_origins,
            ..request.clone()
        };
        self.begin_nfo_export(&plan, now_ms)?;
        Ok(plan)
    }

    /// Locked phase 2 of registering a written sidecar: the video's input keys.
    pub fn plan_nfo_export_registration(
        &self,
        plan: &NfoExportRequest,
    ) -> Result<Option<super::nfo_input::NfoInputReconcilePlan>, StoreError> {
        let video: Option<String> = self
            .library
            .query_row(
                "SELECT path_key FROM library_files WHERE file_id = ?1",
                params![plan.file_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        match video {
            Some(video) => self.plan_nfo_input_reconcile(Path::new(&video)),
            None => Ok(None),
        }
    }

    /// Locked final phase: revalidate, register what was written, then go terminal.
    ///
    /// The file was written off-lock, so everything the write relied on is checked
    /// again here (CAS): an operation superseded by a newer export is left alone
    /// (the newer one owns the file); a file/config/Media binding that moved while
    /// the lock was released still registers the bytes that *are* on disk — they
    /// are ours, and an unregistered body would later look like an external edit —
    /// but the operation is failed instead of succeeded.
    pub fn finish_planned_nfo_export(
        &self,
        plan: &NfoExportRequest,
        action: NfoExportAction,
        observed_inputs: Option<super::nfo_input::NfoInputReconcileObserved>,
        now_ms: i64,
    ) -> Result<NfoExportOutcome, StoreError> {
        if !self.nfo_export_pending(&plan.operation_id)? {
            return Err(StoreError::Conflict(format!(
                "nfo export {} was superseded while its sidecar was written",
                plan.export_id
            )));
        }
        // Register (input + export tag) before the operation turns terminal: a crash in
        // between leaves a pending operation that recovery re-registers, never a
        // succeeded export whose sidecar is not an input.
        if let Some(observed) = observed_inputs {
            self.apply_nfo_input_reconcile(observed)?;
        }
        self.tag_nfo_export(plan)?;
        if let Err(error) = self.nfo_export_binding_current(plan) {
            self.fail_nfo_export(&plan.operation_id, &error.to_string(), now_ms)?;
            return Err(error);
        }
        self.complete_nfo_export(&plan.operation_id, &plan.output_digest, now_ms)?;
        tracing::info!(
            path = %plan.path.display(),
            media = %plan.media_id,
            ?action,
            export_id = %plan.export_id,
            "已导出 NFO sidecar"
        );
        Ok(NfoExportOutcome {
            action,
            export_id: plan.export_id.clone(),
            output_digest: plan.output_digest.clone(),
        })
    }

    /// Recover one export after a restart.
    ///
    /// The file decides which step is missing: it already holds the intended
    /// body (register only), it still holds what the intent saw (retry the
    /// replacement), or a third party changed it — then the edit is escalated
    /// into user overrides first and the body is re-derived from the effective
    /// metadata, so re-rendering cannot silently discard it.
    pub fn resume_nfo_export(
        &self,
        request: &NfoExportRequest,
        now_ms: i64,
    ) -> Result<NfoExportOutcome, StoreError> {
        resume_nfo_export_phased(|| self, request, now_ms)
    }

    /// Library display locale, matching the locale the sidecar input is recorded
    /// under; empty means "instance default".
    fn nfo_export_locale(&self, request: &NfoExportRequest) -> Result<String, StoreError> {
        Ok(
            crate::library_query::library_display_locale(self, request.library_id)?
                .unwrap_or_default(),
        )
    }

    /// The `export_digest` recorded when this library last wrote the sidecar.
    fn nfo_export_anchor(
        &self,
        request: &NfoExportRequest,
    ) -> Result<Option<NfoExportAnchor>, StoreError> {
        let (season, episode) = request.scope.season_episode();
        let key =
            crate::path_key::canonical_recorded_key(&self.library, &request.path.to_string_lossy());
        let row: Option<Option<String>> = self
            .library
            .query_row(
                "SELECT export_fields_json FROM library_metadata_inputs
                 WHERE library_id = ?1 AND media_id = ?2 AND entity_scope = ?3
                   AND season = ?4 AND episode = ?5 AND locale = ?6 AND input_key = ?7",
                params![
                    request.library_id.to_string(),
                    request.media_id.to_string(),
                    request.scope.as_str(),
                    season,
                    episode,
                    self.nfo_export_locale(request)?,
                    key
                ],
                |row| row.get(0),
            )
            .optional()?;
        let Some(fields_json) = row else {
            return Ok(None);
        };
        let fields = match fields_json {
            Some(json) => Some(serde_json::from_str::<MetadataFields>(&json)?),
            None => None,
        };
        Ok(Some(NfoExportAnchor { fields }))
    }

    pub(crate) fn tag_nfo_export(&self, request: &NfoExportRequest) -> Result<bool, StoreError> {
        let (season, episode) = request.scope.season_episode();
        let fields_json = serde_json::to_string(&request.fields)?;
        let key =
            crate::path_key::canonical_recorded_key(&self.library, &request.path.to_string_lossy());
        let affected = self.library.execute(
            "UPDATE library_metadata_inputs
             SET export_id = ?1, export_digest = ?2, export_fields_json = ?3
             WHERE library_id = ?4 AND media_id = ?5 AND entity_scope = ?6
               AND season = ?7 AND episode = ?8 AND locale = ?9 AND input_key = ?10",
            params![
                request.export_id,
                request.output_digest,
                fields_json,
                request.library_id.to_string(),
                request.media_id.to_string(),
                request.scope.as_str(),
                season,
                episode,
                self.nfo_export_locale(request)?,
                key
            ],
        )?;
        if affected == 0 {
            tracing::debug!(
                path = %request.path.display(),
                "sidecar 输入行不存在，导出标记未落库"
            );
        }
        Ok(affected > 0)
    }
}

/// A sidecar as read from disk without the Store: digest plus parsed fields.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NfoSidecarObservation {
    /// `None` when there is no file.
    pub digest: Option<String>,
    /// Parsed fields when the file is a readable NFO for the requested scope.
    pub parsed: Option<MetadataFields>,
}

/// Cold IO + parse of one sidecar. Never touches the Store (S01).
pub fn observe_nfo_sidecar(
    path: &Path,
    scope: EntityScope,
) -> Result<NfoSidecarObservation, StoreError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(NfoSidecarObservation::default());
        }
        Err(error) => return Err(error.into()),
    };

    let Ok(body) = String::from_utf8(bytes) else {
        return Ok(NfoSidecarObservation {
            digest: library::nfo::nfo_digest(path)?,
            parsed: None,
        });
    };
    let parsed = match library::nfo::parse_nfo_fields(&body, scope) {
        library::nfo::NfoReadOutcome::Valid(parsed) => Some(parsed.fields),
        _ => None,
    };
    Ok(NfoSidecarObservation {
        digest: Some(library::nfo::nfo_body_digest(&body)),
        parsed,
    })
}

/// Off-lock phase of an export: replace the sidecar unless it already holds the
/// planned body. Refuses a file that changed since the snapshot or is unreadable.
pub fn write_planned_nfo(plan: &NfoExportRequest) -> Result<NfoExportAction, StoreError> {
    let observation = observe_nfo_sidecar(&plan.path, plan.scope)?;
    if observation.digest.as_deref() == Some(plan.output_digest.as_str()) {
        return Ok(NfoExportAction::Confirmed);
    }
    ensure_sidecar_replaceable(plan, &observation)?;
    library::nfo::replace_nfo_atomically(&plan.path, &plan.body)?;
    Ok(NfoExportAction::Replaced)
}

/// Register a sidecar written off-lock: plan inputs (locked), observe (unlocked),
/// revalidate + register + complete (locked).
fn finish_nfo_export_phased<L, G>(
    lock: &L,
    plan: &NfoExportRequest,
    action: NfoExportAction,
    now_ms: i64,
) -> Result<NfoExportOutcome, StoreError>
where
    L: Fn() -> G,
    G: std::ops::Deref<Target = Store>,
{
    let inputs = lock().plan_nfo_export_registration(plan)?;
    let observed = inputs.map(|inputs| inputs.observe());
    lock().finish_planned_nfo_export(plan, action, observed, now_ms)
}

/// Full export with the Store held only for database phases.
///
/// `lock` hands out the Store for one phase at a time — `|| store` for a bare
/// Store, `|| mutex.lock()` for the shared one — and every disk read/write runs
/// between two calls, never under the guard (S01).
pub fn export_nfo_sidecar_phased<L, G>(
    lock: L,
    request: &NfoExportRequest,
    now_ms: i64,
) -> Result<NfoExportOutcome, StoreError>
where
    L: Fn() -> G,
    G: std::ops::Deref<Target = Store>,
{
    let plan = lock().plan_nfo_export(request, now_ms)?;
    let action = write_planned_nfo(&plan)?;
    finish_nfo_export_phased(&lock, &plan, action, now_ms)
}

/// Recover one export after a restart with the Store held only for database phases.
///
/// The file decides which step is missing: it already holds the intended body
/// (register only), it still holds what the intent saw (retry the replacement),
/// or a third party changed it — then the edit is escalated into user overrides
/// first and the body is re-derived from the effective metadata, so re-rendering
/// cannot silently discard it.
pub fn resume_nfo_export_phased<L, G>(
    lock: L,
    request: &NfoExportRequest,
    now_ms: i64,
) -> Result<NfoExportOutcome, StoreError>
where
    L: Fn() -> G,
    G: std::ops::Deref<Target = Store>,
{
    let observation = observe_nfo_sidecar(&request.path, request.scope)?;
    if observation.digest.as_deref() == Some(request.output_digest.as_str()) {
        tracing::info!(
            path = %request.path.display(),
            export_id = %request.export_id,
            "崩溃恢复：sidecar 已替换，仅补登记"
        );
        return finish_nfo_export_phased(&lock, request, NfoExportAction::Registered, now_ms);
    }
    if !request.body.is_empty() && observation.digest == request.expected_digest {
        ensure_sidecar_replaceable(request, &observation)?;
        library::nfo::replace_nfo_atomically(&request.path, &request.body)?;
        tracing::info!(
            path = %request.path.display(),
            export_id = %request.export_id,
            "崩溃恢复：sidecar 未替换，重放原子替换"
        );
        return finish_nfo_export_phased(&lock, request, NfoExportAction::Replaced, now_ms);
    }
    // Neither digest matches: the sidecar changed while this export was
    // pending. Escalate that edit into user overrides before re-rendering.
    let edits = lock().nfo_local_edits_observed(request, &observation)?;
    if let Some(local_edits) = edits {
        tracing::warn!(
            path = %request.path.display(),
            media = %request.media_id,
            export_id = %request.export_id,
            "崩溃恢复：sidecar 在导出期间被外部修改，保留为本库局部编辑后重新导出"
        );
        let fresh = NfoExportRequest {
            expected_digest: observation.digest,
            local_edits,
            ..request.clone()
        };
        return export_nfo_sidecar_phased(lock, &fresh, now_ms);
    }
    tracing::warn!(
        path = %request.path.display(),
        media = %request.media_id,
        export_id = %request.export_id,
        "崩溃恢复：sidecar 与意图不符且没有可比较的导出基线，保留原文件等待下次重试"
    );
    Err(StoreError::Invalid(format!(
        "nfo export {} refused to overwrite externally changed {}",
        request.export_id,
        request.path.display()
    )))
}

/// The last export this library registered for the sidecar.
struct NfoExportAnchor {
    /// Fields of the last export: the three-way baseline an edit is measured against.
    fields: Option<MetadataFields>,
}

/// Fields the sidecar changed relative to the last exported baseline, or `None`.
///
/// A field the sidecar does not carry at all (`Missing`) is ambiguous — the
/// renderer deliberately omits artwork and list provenance — so a removal is not
/// escalated as an override; only a value we can actually read back is.
fn nfo_field_diff(parsed: &MetadataFields, effective: &MetadataFields) -> Option<MetadataFields> {
    let mut diff = MetadataFields::default();
    let mut changed = false;
    changed |= diff_scalar(&parsed.title, &effective.title, &mut diff.title);
    changed |= diff_scalar(
        &parsed.original_title,
        &effective.original_title,
        &mut diff.original_title,
    );
    changed |= diff_scalar(&parsed.overview, &effective.overview, &mut diff.overview);
    changed |= diff_scalar(&parsed.tagline, &effective.tagline, &mut diff.tagline);
    changed |= diff_scalar(
        &parsed.release_date,
        &effective.release_date,
        &mut diff.release_date,
    );
    changed |= diff_scalar(&parsed.end_date, &effective.end_date, &mut diff.end_date);
    changed |= diff_scalar(
        &parsed.content_rating,
        &effective.content_rating,
        &mut diff.content_rating,
    );
    changed |= diff_scalar(&parsed.language, &effective.language, &mut diff.language);
    changed |= diff_scalar(&parsed.status, &effective.status, &mut diff.status);
    changed |= diff_scalar(&parsed.air_date, &effective.air_date, &mut diff.air_date);
    changed |= diff_scalar(&parsed.year, &effective.year, &mut diff.year);
    changed |= diff_scalar(&parsed.rating, &effective.rating, &mut diff.rating);
    changed |= diff_scalar(
        &parsed.vote_count,
        &effective.vote_count,
        &mut diff.vote_count,
    );
    changed |= diff_scalar(
        &parsed.runtime_minutes,
        &effective.runtime_minutes,
        &mut diff.runtime_minutes,
    );
    changed |= diff_scalar(&parsed.countries, &effective.countries, &mut diff.countries);
    changed |= diff_scalar(&parsed.studios, &effective.studios, &mut diff.studios);
    changed |= diff_scalar(&parsed.directors, &effective.directors, &mut diff.directors);
    changed |= diff_scalar(&parsed.creators, &effective.creators, &mut diff.creators);
    changed |= diff_genres(&parsed.genres, &effective.genres, &mut diff.genres);
    changed |= diff_cast(&parsed.cast, &effective.cast, &mut diff.cast);
    changed.then_some(diff)
}

/// A scalar/list field differs when the sidecar carries a readable value that is
/// not what effective metadata says. Provenance is not part of the comparison.
fn diff_scalar<T: Clone + PartialEq>(
    parsed: &FieldValue<T>,
    effective: &FieldValue<T>,
    target: &mut FieldValue<T>,
) -> bool {
    if parsed.is_missing() || parsed == effective {
        return false;
    }
    *target = parsed.clone();
    true
}

/// Genres are compared by name: parsed entries carry NFO-local provenance, so a
/// structural comparison would report a change on every scan.
fn diff_genres(
    parsed: &FieldValue<Vec<MetadataGenre>>,
    effective: &FieldValue<Vec<MetadataGenre>>,
    target: &mut FieldValue<Vec<MetadataGenre>>,
) -> bool {
    let (Some(parsed), Some(effective)) = (parsed.value(), effective.value()) else {
        return false;
    };
    if parsed
        .iter()
        .map(|g| &g.name)
        .eq(effective.iter().map(|g| &g.name))
    {
        return false;
    }
    *target = FieldValue::Value(parsed.clone());
    true
}

/// Cast is compared by identity (`name`/`role`/`tmdb_id`); `order` and `thumb`
/// have no stable sidecar round-trip and must not fake an edit.
fn diff_cast(
    parsed: &FieldValue<Vec<MetadataCast>>,
    effective: &FieldValue<Vec<MetadataCast>>,
    target: &mut FieldValue<Vec<MetadataCast>>,
) -> bool {
    let (Some(parsed), Some(effective)) = (parsed.value(), effective.value()) else {
        return false;
    };
    let identity = |cast: &Vec<MetadataCast>| {
        cast.iter()
            .map(|m| (m.name.clone(), m.role.clone(), m.tmdb_id.clone()))
            .collect::<Vec<_>>()
    };
    if identity(parsed) == identity(effective) {
        return false;
    }
    *target = FieldValue::Value(parsed.clone());
    true
}

/// Local edits: what `parsed` changed relative to the round-tripped `baseline`.
fn local_edits(
    parsed: &MetadataFields,
    baseline: &MetadataFields,
    scope: EntityScope,
) -> MetadataFields {
    let rendered = library::nfo::render_nfo(scope, baseline);
    let comparator = match library::nfo::parse_nfo_fields(&rendered, scope) {
        library::nfo::NfoReadOutcome::Valid(round_trip) => round_trip.fields,
        _ => baseline.clone(),
    };
    nfo_field_diff(parsed, &comparator).unwrap_or_default()
}

/// Overlay `diff` onto `target`: only fields `diff` carries are replaced.
fn overlay_fields(target: &mut MetadataFields, diff: &MetadataFields) {
    macro_rules! merge {
        ($($field:ident),* $(,)?) => {
            $(if !diff.$field.is_missing() {
                target.$field = diff.$field.clone();
            })*
        };
    }
    merge!(
        title,
        original_title,
        overview,
        tagline,
        release_date,
        end_date,
        content_rating,
        language,
        status,
        air_date,
        year,
        rating,
        vote_count,
        runtime_minutes,
        countries,
        studios,
        directors,
        creators,
        genres,
        cast,
    );
}

/// Refuse to replace a sidecar that changed since the export snapshot, or one that is
/// not a readable NFO: its bytes stay exactly as found (R17).
fn ensure_sidecar_replaceable(
    plan: &NfoExportRequest,
    observation: &NfoSidecarObservation,
) -> Result<(), StoreError> {
    let disk = observation.digest.as_deref();
    if disk != plan.expected_digest.as_deref() {
        return Err(StoreError::Conflict(format!(
            "nfo export {} refused: {} changed since the export snapshot",
            plan.export_id,
            plan.path.display()
        )));
    }
    if disk.is_none() {
        return Ok(());
    }
    if observation.parsed.is_none() {
        tracing::warn!(path = %plan.path.display(), "sidecar 不是可读 NFO，保留原字节不覆盖");
        return Err(StoreError::Conflict(format!(
            "nfo export {} refused to overwrite unreadable {}",
            plan.export_id,
            plan.path.display()
        )));
    }
    Ok(())
}
