//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn nfo_export_target_unobserved(
        &self,
        video: &Path,
        sidecar: &Path,
        scope: EntityScope,
        operation_id: &str,
        export_id: &str,
    ) -> Result<Option<NfoExportRequest>, StoreError> {
        let key = crate::path_key::canonical_recorded_key(self.library, &video.to_string_lossy());
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

    pub fn nfo_local_edits_observed(
        &self,
        request: &NfoExportRequest,
        observation: &NfoSidecarObservation,
    ) -> Result<Option<MetadataFields>, StoreError> {
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
        let baseline = self
            .nfo_export_anchor(request)?
            .and_then(|anchor| anchor.fields);
        Ok(Some(match baseline {
            Some(baseline) => local_edits(parsed, &baseline, request.scope),
            None => parsed.clone(),
        }))
    }

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

    pub fn plan_nfo_export_rerender(
        &self,
        request: &NfoExportRequest,
    ) -> Result<NfoExportRequest, StoreError> {
        let locale = if request.locale.is_empty() {
            self.nfo_export_locale(request)?
        } else {
            request.locale.clone()
        };
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
        Ok(NfoExportRequest {
            locale,
            body,
            output_digest,
            fields: effective.fields,
            field_origins: effective.field_origins,
            ..request.clone()
        })
    }

    pub fn plan_nfo_export_registration(
        &self,
        plan: &NfoExportRequest,
    ) -> Result<Option<super::super::nfo_input::NfoInputReconcilePlan>, StoreError> {
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

    pub(super) fn finish_planned_nfo_export(
        &self,
        plan: &NfoExportRequest,
        action: NfoExportAction,
        observed_inputs: Option<super::super::nfo_input::NfoInputReconcileObserved>,
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
        if let Err(error) = self.nfo_export_binding_current(plan) {
            self.fail_owned_nfo_export(plan, &error.to_string(), now_ms)?;
            return Err(error);
        }
        let locale = self.nfo_export_locale(plan)?;
        let tx = crate::write_tx(self.library)?;
        tag_nfo_export(&tx, plan, &locale)?;
        super::super::export::complete_nfo_export_on(
            &tx,
            &plan.operation_id,
            &plan.output_digest,
            now_ms,
        )?;
        tx.commit()?;
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

    pub(super) fn nfo_export_anchor(
        &self,
        request: &NfoExportRequest,
    ) -> Result<Option<NfoExportAnchor>, StoreError> {
        let (season, episode) = request.scope.season_episode();
        let key =
            crate::path_key::canonical_recorded_key(self.library, &request.path.to_string_lossy());
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
}

impl crate::LibraryConnection<'_> {
    pub(super) fn nfo_export_locale(
        &self,
        request: &NfoExportRequest,
    ) -> Result<String, StoreError> {
        Ok(
            crate::library_query::library_display_locale_on(self.library, request.library_id)?
                .unwrap_or_default(),
        )
    }
}
