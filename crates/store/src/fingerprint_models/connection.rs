//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn replace_fingerprint_model_bundles(
        &self,
        set: &StoredFingerprintModelSet,
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.library)?;
        for bundle in &set.bundles {
            if bundle.model.media_id != set.media_id.to_string()
                || bundle.model.season != set.season
                || bundle.model.policy_key != set.policy_key
            {
                return Err(rejected_model(
                    &bundle.model,
                    "model outside replacement set",
                ));
            }
            self.put_fingerprint_model_bundle(bundle)?;
        }
        // Validate and insert every theme before retiring the previous set. The
        // surrounding transaction also preserves old members on any failure.
        tx.execute("DELETE FROM fingerprint_season_models WHERE media_id=?1 AND season=?2 AND policy_key=?3
            AND model_id NOT IN (SELECT value FROM json_each(?4))",
            params![set.media_id.to_string(),set.season,set.policy_key,set.encoded_ids])?;
        tx.commit()?;
        Ok(())
    }

    pub fn put_fingerprint_model_bundle(
        &self,
        bundle: &StoredFingerprintModelBundle,
    ) -> Result<(), StoreError> {
        let model = &bundle.model;
        let members = &bundle.members;
        let tx = crate::write_tx(self.library)?;
        validate_model_publication(&tx, model, members)?;
        if bundle.bindings.len() != bundle.samples.len() {
            return Err(rejected_model(
                model,
                "model source facts were not prepared",
            ));
        }
        for binding in &bundle.bindings {
            self.validate_model_reference_binding(model, binding)?;
        }
        for (sample, payload) in bundle.samples.iter().zip(&bundle.sample_payloads) {
            let exact: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM fingerprint_samples
                WHERE sample_id=?1 AND file_id=?2 AND source_version=?3 AND capture_profile_key=?4
                  AND kind=?5 AND window_start_ms=?6 AND window_end_ms=?7 AND pcm_duration_ms IS ?8
                  AND fingerprint_json=?9 AND metrics_json=?10)",
                params![
                    sample.sample_id,
                    sample.ledger_id,
                    sample.source_version,
                    sample.capture_profile_key,
                    sample.kind,
                    sample.window_start_ms,
                    sample.window_end_ms,
                    sample.pcm_duration_ms,
                    payload,
                    sample.metrics_json
                ],
                |row| row.get(0),
            )?;
            if !exact {
                return Err(rejected_model(
                    model,
                    "reference sample changed before publication",
                ));
            }
        }

        tx.execute(
            "INSERT INTO fingerprint_season_models (
                 model_id, media_id, season, kind, model_version,
                 membership_key, policy_key, model_json, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(model_id) DO UPDATE SET
                 media_id = excluded.media_id,
                 season = excluded.season,
                 kind = excluded.kind,
                 model_version = excluded.model_version,
                 membership_key = excluded.membership_key,
                 policy_key = excluded.policy_key,
                 model_json = excluded.model_json,
                 created_at_ms = excluded.created_at_ms",
            params![
                model.model_id,
                model.media_id,
                model.season,
                model.kind,
                model.model_version,
                model.membership_key,
                model.policy_key,
                model.model_json,
                model.created_at_ms,
            ],
        )?;

        tx.execute(
            "DELETE FROM fingerprint_model_members WHERE model_id = ?1",
            params![model.model_id],
        )?;

        for m in members {
            tx.execute(
                "INSERT INTO fingerprint_model_members (
                     model_id, sample_id, file_id, source_version
                 ) VALUES (?1, ?2, ?3, ?4)",
                params![m.model_id, m.sample_id, m.ledger_id, m.source_version],
            )?;
        }

        tx.commit()?;
        Ok(())
    }

    pub fn list_fingerprint_models(
        &self,
        media_id: &str,
        season: u32,
    ) -> Result<Vec<StoredFingerprintModel>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT model_id, media_id, season, kind, model_version,
                    membership_key, policy_key, model_json, created_at_ms
             FROM fingerprint_season_models
             WHERE media_id = ?1 AND season = ?2
             ORDER BY created_at_ms ASC, rowid ASC",
        )?;

        let rows = stmt.query_map(params![media_id, season], |row| {
            Ok(StoredFingerprintModel {
                model_id: row.get(0)?,
                media_id: row.get(1)?,
                season: row.get(2)?,
                kind: row.get(3)?,
                model_version: row.get(4)?,
                membership_key: row.get(5)?,
                policy_key: row.get(6)?,
                model_json: row.get(7)?,
                created_at_ms: row.get(8)?,
            })
        })?;

        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }
}

impl crate::LibraryConnection<'_> {
    pub(super) fn validate_model_reference_binding(
        &self,
        model: &StoredFingerprintModel,
        binding: &super::bundle::ModelReferenceBinding,
    ) -> Result<(), StoreError> {
        let current: bool = self.library.query_row(
            "SELECT EXISTS(SELECT 1 FROM file_details d
            JOIN library_files f ON f.file_id=d.file_id
            JOIN library_file_units u ON u.file_id=f.file_id
            WHERE d.file_id=?1 AND d.source_revision=?2 AND d.source_version=?3
            AND d.tracks_version=?4 AND d.audio_json=?5 AND d.video_json IS NOT NULL
            AND f.file_revision=d.source_revision AND f.source_version=d.source_version
            AND u.media_id=?6 AND u.season=?7 AND u.episode=?8)",
            params![
                binding.file,
                binding.revision,
                binding.source,
                binding.tracks_version,
                binding.audio,
                model.media_id,
                model.season,
                binding.episode
            ],
            |row| row.get(0),
        )?;
        if !current {
            return Err(rejected_model(
                model,
                "reference episode coverage or metadata changed",
            ));
        }
        Ok(())
    }

    pub(super) fn fingerprint_model_bundle_rows(
        &self,
        model_id: &str,
    ) -> Result<
        Option<(
            StoredFingerprintModel,
            Vec<StoredFingerprintModelMember>,
            Vec<crate::fingerprint_samples::FingerprintSampleRead>,
        )>,
        StoreError,
    > {
        let _snapshot = crate::read_tx(self.library)?;
        let model = self.library.query_row("SELECT model_id,media_id,season,kind,model_version,
            membership_key,policy_key,model_json,created_at_ms FROM fingerprint_season_models WHERE model_id=?1",
            [model_id],model_row).optional()?;
        let Some(model) = model else {
            return Ok(None);
        };
        let mut members = Vec::new();
        let mut samples = Vec::new();
        let mut after = String::new();
        loop {
            let mut statement=self.library.prepare("SELECT model_id,sample_id,file_id,source_version
                FROM fingerprint_model_members WHERE model_id=?1 AND sample_id>?2 ORDER BY sample_id LIMIT 16")?;
            let page = statement
                .query_map(params![model_id, after], |row| {
                    Ok(StoredFingerprintModelMember {
                        model_id: row.get(0)?,
                        sample_id: row.get(1)?,
                        ledger_id: row.get(2)?,
                        source_version: row.get(3)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            if page.is_empty() {
                break;
            }
            for member in page {
                let sample = self
                    .fingerprint_sample_read(&member.sample_id)?
                    .ok_or_else(|| rejected_model(&model, "exact reference sample missing"))?;
                after = member.sample_id.clone();
                members.push(member);
                samples.push(sample);
            }
        }
        validate_model_publication(self.library, &model, &members)?;
        Ok(Some((model, members, samples)))
    }

    pub(super) fn fingerprint_models_page(
        &self,
        media_id: &str,
        season: u32,
        kind: &str,
        policy_key: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<StoredFingerprintModel>, StoreError> {
        let mut statement = self.library.prepare(
            "SELECT model_id,media_id,season,kind,model_version,
            membership_key,policy_key,model_json,created_at_ms FROM fingerprint_season_models
            WHERE media_id=?1 AND season=?2 AND kind=?3 AND model_id>?4 AND policy_key=?6 ORDER BY model_id LIMIT ?5",
        )?;
        Ok(statement
            .query_map(
                params![
                    media_id,
                    season,
                    kind,
                    after.unwrap_or(""),
                    limit.min(16) as i64,
                    policy_key,
                ],
                model_row,
            )?
            .collect::<Result<Vec<_>, _>>()?)
    }
}

fn model_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredFingerprintModel> {
    Ok(StoredFingerprintModel {
        model_id: row.get(0)?,
        media_id: row.get(1)?,
        season: row.get(2)?,
        kind: row.get(3)?,
        model_version: row.get(4)?,
        membership_key: row.get(5)?,
        policy_key: row.get(6)?,
        model_json: row.get(7)?,
        created_at_ms: row.get(8)?,
    })
}
