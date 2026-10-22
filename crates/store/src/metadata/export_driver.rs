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
        let key = video.to_string_lossy();
        let row: Option<(String, String, String, String, i64, String)> = self
            .library
            .query_row(
                "SELECT media_id, library_id, root_id, file_id, file_revision, source_version
                 FROM library_files WHERE path_key = ?1 OR display_path = ?1 LIMIT 1",
                params![key.as_ref()],
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
            expected_digest: library::nfo::nfo_digest(sidecar)?,
            body: String::new(),
            output_digest: String::new(),
            fields: MetadataFields::default(),
            field_origins: BTreeMap::new(),
        }))
    }

    /// Turn edits someone else made to a generated sidecar into local overrides.
    ///
    /// Only a sidecar this library produced is inspected: without a recorded
    /// `export_digest` there is no baseline, and a hand-written NFO keeps its
    /// existing sidecar authority. Returns whether overrides were written.
    pub fn capture_nfo_edits(&self, request: &NfoExportRequest) -> Result<bool, StoreError> {
        let Some(anchor) = self.nfo_export_anchor(request)? else {
            return Ok(false);
        };
        let Some(disk) = library::nfo::nfo_digest(&request.path)? else {
            return Ok(false);
        };
        if Some(disk.as_str()) == anchor.digest.as_deref() {
            // Still exactly what we last registered: the file holds no edit.
            return Ok(false);
        }
        let Ok(body) = std::fs::read_to_string(&request.path) else {
            return Ok(false);
        };
        let library::nfo::NfoReadOutcome::Valid(parsed) =
            library::nfo::parse_nfo_fields(&body, request.scope)
        else {
            tracing::warn!(
                path = %request.path.display(),
                "generated NFO is no longer parseable; keeping the previous overrides"
            );
            return Ok(false);
        };
        let locale = self.nfo_export_locale(request)?;
        let effective = self.effective_metadata_fields(
            request.library_id,
            request.media_id,
            request.scope,
            &locale,
        )?;
        // R18: an edit is what changed relative to what we last *exported*, not relative
        // to today's provider value; otherwise an untouched field would be locked to the
        // previous provider value as soon as the provider moves on.
        let Some(baseline) = anchor.fields else {
            return Ok(false);
        };
        let Some(diff) = nfo_field_diff(&parsed.fields, &baseline) else {
            return Ok(false);
        };
        let override_locale = effective.actual_locale.as_str();
        let (mut merged, revision) = self
            .get_metadata_overrides(request.media_id, request.scope, override_locale)?
            .unwrap_or_default();
        merge_overrides(&mut merged, &diff);
        let revision = self.set_metadata_overrides(
            request.media_id,
            request.scope,
            override_locale,
            &merged,
            revision,
        )?;
        tracing::info!(
            path = %request.path.display(),
            media = %request.media_id,
            locale = override_locale,
            revision,
            "NFO sidecar 的外部编辑已升级为用户覆盖"
        );
        Ok(true)
    }

    /// Render effective metadata into the sidecar and register what was written.
    ///
    /// The intent is persisted *before* the filesystem is touched, so a crash
    /// between the two leaves a recoverable operation instead of a silent gap.
    pub fn export_nfo_sidecar(
        &self,
        request: &NfoExportRequest,
        now_ms: i64,
    ) -> Result<NfoExportOutcome, StoreError> {
        let locale = self.nfo_export_locale(request)?;
        let effective = self.effective_metadata_fields(
            request.library_id,
            request.media_id,
            request.scope,
            &locale,
        )?;
        let body = library::nfo::render_nfo(request.scope, &effective.fields);
        let output_digest = library::nfo::nfo_body_digest(&body);
        let plan = NfoExportRequest {
            locale,
            body,
            output_digest: output_digest.clone(),
            fields: effective.fields,
            field_origins: effective.field_origins,
            ..request.clone()
        };
        self.begin_nfo_export(&plan, now_ms)?;
        let disk = library::nfo::nfo_digest(&plan.path)?;
        let action = if disk.as_deref() == Some(output_digest.as_str()) {
            NfoExportAction::Confirmed
        } else {
            ensure_sidecar_replaceable(&plan, disk.as_deref())?;
            library::nfo::replace_nfo_atomically(&plan.path, &plan.body)?;
            NfoExportAction::Replaced
        };
        // Register (input + export tag) before the operation turns terminal: a crash in
        // between leaves a pending operation that recovery re-registers, never a
        // succeeded export whose sidecar is not an input.
        self.register_nfo_export(&plan)?;
        self.complete_nfo_export(&plan.operation_id, &output_digest, now_ms)?;
        tracing::info!(
            path = %plan.path.display(),
            media = %plan.media_id,
            ?action,
            export_id = %plan.export_id,
            "已导出 NFO sidecar"
        );
        Ok(NfoExportOutcome {
            action,
            export_id: plan.export_id,
            output_digest,
        })
    }

    /// Make the sidecar an input again and remember that we produced it.
    ///
    /// Order matters: the fresh observation must land first, otherwise the
    /// `export_*` tag would be overwritten by the next row write. The tag is
    /// written even when the observation found no change.
    pub fn register_nfo_export(&self, request: &NfoExportRequest) -> Result<bool, StoreError> {
        let video: Option<String> = self
            .library
            .query_row(
                "SELECT path_key FROM library_files WHERE file_id = ?1",
                params![request.file_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        let reconciled = match video {
            Some(video) => self.reconcile_nfo_inputs(Path::new(&video))?,
            None => false,
        };
        let tagged = self.tag_nfo_export(request)?;
        Ok(reconciled || tagged)
    }

    /// Every unfinished export, recovered by digest.
    ///
    /// One wedged export never blocks the others: a failing operation is
    /// reported and left unfinished, so the next recovery pass retries it.
    pub fn resume_pending_nfo_exports(
        &self,
        now_ms: i64,
    ) -> Result<Vec<NfoExportOutcome>, StoreError> {
        let mut outcomes = Vec::new();
        for request in self.pending_nfo_exports()? {
            match self.resume_nfo_export(&request, now_ms) {
                Ok(outcome) => outcomes.push(outcome),
                Err(error) => tracing::error!(
                    %error,
                    operation_id = %request.operation_id,
                    export_id = %request.export_id,
                    path = %request.path.display(),
                    "NFO 导出恢复失败；操作保持未完成，下一轮继续重试"
                ),
            }
        }
        Ok(outcomes)
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
        let disk = library::nfo::nfo_digest(&request.path)?;
        if disk.as_deref() == Some(request.output_digest.as_str()) {
            self.complete_nfo_export(&request.operation_id, &request.output_digest, now_ms)?;
            self.register_nfo_export(request)?;
            tracing::info!(
                path = %request.path.display(),
                export_id = %request.export_id,
                "崩溃恢复：sidecar 已替换，仅补登记"
            );
            return Ok(NfoExportOutcome {
                action: NfoExportAction::Registered,
                export_id: request.export_id.clone(),
                output_digest: request.output_digest.clone(),
            });
        }
        if !request.body.is_empty() && disk == request.expected_digest {
            library::nfo::replace_nfo_atomically(&request.path, &request.body)?;
            self.complete_nfo_export(&request.operation_id, &request.output_digest, now_ms)?;
            self.register_nfo_export(request)?;
            tracing::info!(
                path = %request.path.display(),
                export_id = %request.export_id,
                "崩溃恢复：sidecar 未替换，重放原子替换"
            );
            return Ok(NfoExportOutcome {
                action: NfoExportAction::Replaced,
                export_id: request.export_id.clone(),
                output_digest: request.output_digest.clone(),
            });
        }
        // Neither digest matches: the sidecar changed while this export was
        // pending. Escalate that edit into user overrides before re-rendering.
        if self.capture_nfo_edits(request)? {
            tracing::warn!(
                path = %request.path.display(),
                media = %request.media_id,
                export_id = %request.export_id,
                "崩溃恢复：sidecar 在导出期间被外部修改，已升级为用户覆盖后重新导出"
            );
            let fresh = NfoExportRequest {
                expected_digest: disk,
                ..request.clone()
            };
            return self.export_nfo_sidecar(&fresh, now_ms);
        }
        tracing::warn!(
            path = %request.path.display(),
            media = %request.media_id,
            export_id = %request.export_id,
            "崩溃恢复：sidecar 与意图不符且无法升级为用户覆盖，保留原文件等待下次重试"
        );
        Err(StoreError::Invalid(format!(
            "nfo export {} refused to overwrite externally changed {}",
            request.export_id,
            request.path.display()
        )))
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
        let key = request.path.to_string_lossy();
        let row: Option<(Option<String>, Option<String>)> = self
            .library
            .query_row(
                "SELECT export_digest, export_fields_json FROM library_metadata_inputs
                 WHERE library_id = ?1 AND media_id = ?2 AND entity_scope = ?3
                   AND season = ?4 AND episode = ?5 AND locale = ?6 AND input_key = ?7",
                params![
                    request.library_id.to_string(),
                    request.media_id.to_string(),
                    request.scope.as_str(),
                    season,
                    episode,
                    self.nfo_export_locale(request)?,
                    key.as_ref()
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((digest, fields_json)) = row else {
            return Ok(None);
        };
        let fields = match fields_json {
            Some(json) => Some(serde_json::from_str::<MetadataFields>(&json)?),
            None => None,
        };
        Ok(Some(NfoExportAnchor { digest, fields }))
    }

    fn tag_nfo_export(&self, request: &NfoExportRequest) -> Result<bool, StoreError> {
        let (season, episode) = request.scope.season_episode();
        let fields_json = serde_json::to_string(&request.fields)?;
        let key = request.path.to_string_lossy();
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
                key.as_ref()
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

/// Baseline digest of the sidecar as this library last registered it.
struct NfoExportAnchor {
    digest: Option<String>,
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

/// Overlay the captured diff onto existing overrides: an edit to one field must
/// not drop overrides captured earlier.
fn merge_overrides(target: &mut MetadataFields, diff: &MetadataFields) {
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
fn ensure_sidecar_replaceable(plan: &NfoExportRequest, disk: Option<&str>) -> Result<(), StoreError> {
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
    let readable = std::fs::read_to_string(&plan.path)
        .ok()
        .is_some_and(|body| {
            matches!(
                library::nfo::parse_nfo_fields(&body, plan.scope),
                library::nfo::NfoReadOutcome::Valid(_)
            )
        });
    if !readable {
        tracing::warn!(path = %plan.path.display(), "sidecar 不是可读 NFO，保留原字节不覆盖");
        return Err(StoreError::Conflict(format!(
            "nfo export {} refused to overwrite unreadable {}",
            plan.export_id,
            plan.path.display()
        )));
    }
    Ok(())
}
