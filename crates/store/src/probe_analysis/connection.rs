use super::*;
use rusqlite::{OptionalExtension, params};

pub(super) type ParticipantRow = (
    ProbeDispatchId,
    String,
    Option<Vec<evidence::EvidenceGuard>>,
);

impl crate::LibraryConnection<'_> {
    pub(super) fn advance_probe_unit_to_analysis(
        &self,
        participant: &ProbeAnalysisParticipant,
    ) -> Result<bool, StoreError> {
        if participant.dispatch.stage != ProbeDispatchStage::Fingerprint {
            return Ok(false);
        }
        if participant.dispatch.ledger_id != participant.context.target.file.file_id.to_string() {
            return Ok(false);
        }
        let tx = crate::write_tx(self.library)?;
        if !self.analysis_target_current(&participant.context)?
            || !self.analysis_evidence_matches(&participant.guards)?
        {
            return Ok(false);
        }
        for kind in &participant.context.required_kinds {
            if !matches!(kind.as_str(), "intro" | "outro") {
                return Ok(false);
            }
            let complete: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM probe_stage_state
                WHERE file_id=?1 AND context_key=?2 AND stage=?3 AND status IN ('succeeded','not_applicable'))",
                params![participant.dispatch.ledger_id,participant.context.capture_context_key,kind], |row| row.get(0))?;
            if !complete {
                return Ok(false);
            }
        }
        let changed = tx.execute(
            "UPDATE probe_job_units SET stage='analysis',status='queued',
            analysis_context_json=?3,analysis_claim_token=NULL,analysis_refresh_json=NULL
            WHERE job_id=?1 AND file_id=?2 AND stage='fingerprint' AND status='running'
              AND EXISTS(SELECT 1 FROM probe_jobs WHERE id=?1 AND status IN ('queued','running'))",
            params![
                participant.dispatch.job_id,
                participant.dispatch.ledger_id,
                participant.encoded
            ],
        )?;
        tx.commit()?;
        Ok(changed == 1)
    }

    pub(super) fn analysis_target_current(
        &self,
        context: &ProbeAnalysisContext,
    ) -> Result<bool, StoreError> {
        let expected = &context.target;
        if context.media_id != expected.file.media_id
            || !expected
                .units
                .iter()
                .any(|unit| unit.season == context.season as i32)
        {
            return Ok(false);
        }
        let Some(current) = self.file_coverage_by_id(expected.file.file_id)? else {
            return Ok(false);
        };
        if current.file.version != expected.file.version
            || current.file.library_id != expected.file.library_id
            || current.file.root_id != expected.file.root_id
            || current.path_key != expected.path_key
            || current.file.path != expected.file.path
            || current.file.media_id != expected.file.media_id
            || current.units != expected.units
            || current.file.presence != domain::FilePresence::Present
            || current.file.publication != domain::FilePublication::Published
        {
            return Ok(false);
        }
        for result in [
            crate::library_write::publish::ensure_library_writable(
                self.library,
                &expected.file.library_id.to_string(),
            ),
            crate::library_write::publish::ensure_media_writable(
                self.library,
                expected.file.media_id,
            ),
        ] {
            match result {
                Ok(()) => {}
                Err(StoreError::Protected(_)) | Err(StoreError::Missing(_)) => return Ok(false),
                Err(error) => return Err(error),
            }
        }
        let owner = crate::library_key_owner::owning_root_key(
            self.library,
            std::path::Path::new(&current.path_key),
            current.file.kind,
        )?;
        Ok(owner
            == Some((
                expected.file.library_id.to_string(),
                expected.file.root_id.clone(),
            )))
    }

    pub(super) fn pending_probe_analysis_keys(
        &self,
        after: Option<(domain::MediaId, u32)>,
        limit: usize,
    ) -> Result<Vec<(domain::MediaId, u32)>, StoreError> {
        let (media, season) = after
            .map(|(id, season)| (id.to_string(), season))
            .unwrap_or_default();
        let mut result = Vec::new();
        let (mut media, mut season) = (media, season);
        // One index seek per distinct key, never GROUP BY over the backlog.
        for _ in 0..limit {
            let same = self.next_analysis_key(&media, season, true)?;
            let next = match same {
                Some(next) => Some(next),
                None => self.next_analysis_key(&media, season, false)?,
            };
            let Some((next_media, next_season)) = next else {
                break;
            };
            result.push((next_media.parse()?, next_season));
            media = next_media;
            season = next_season;
        }
        Ok(result)
    }

    fn next_analysis_key(
        &self,
        media: &str,
        season: u32,
        same_media: bool,
    ) -> Result<Option<(String, u32)>, StoreError> {
        let predicate = if same_media {
            "json_extract(u.analysis_context_json,'$.media_id')=?1 AND json_extract(u.analysis_context_json,'$.season')>?2"
        } else {
            "json_extract(u.analysis_context_json,'$.media_id')>?1 AND ?2 IS NOT NULL"
        };
        let order = if same_media {
            "json_extract(u.analysis_context_json,'$.season'),u.job_id,u.file_id"
        } else {
            "json_extract(u.analysis_context_json,'$.media_id'),json_extract(u.analysis_context_json,'$.season'),u.job_id,u.file_id"
        };
        let sql=format!("SELECT json_extract(u.analysis_context_json,'$.media_id'),json_extract(u.analysis_context_json,'$.season')
            FROM probe_job_units u INDEXED BY probe_analysis_season
            WHERE u.stage='analysis' AND u.status='queued' AND {predicate}
            AND EXISTS(SELECT 1 FROM probe_jobs j WHERE j.id=u.job_id AND j.status IN ('queued','running'))
            ORDER BY {order} LIMIT 1");
        self.library
            .query_row(&sql, params![media, season], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()
            .map_err(Into::into)
    }

    pub(super) fn analysis_participant_rows(
        &self,
        media: domain::MediaId,
        season: u32,
        after: Option<(&str, &str)>,
        limit: usize,
        job_filter: Option<&str>,
    ) -> Result<Vec<ParticipantRow>, StoreError> {
        let _snapshot = crate::write_tx::read_tx(self.library)?;
        let (job, file) = after.unwrap_or(("", ""));
        let sql=format!("SELECT u.job_id,u.file_id,u.analysis_context_json
            FROM probe_job_units u INDEXED BY probe_analysis_season WHERE u.stage='analysis' AND u.status='queued'
            AND json_extract(u.analysis_context_json,'$.media_id')=?1
            AND json_extract(u.analysis_context_json,'$.season')=?2 AND (u.job_id,u.file_id)>(?3,?4)
            AND EXISTS(SELECT 1 FROM probe_jobs j WHERE j.id=u.job_id AND j.status IN ('queued','running'))
            {} ORDER BY u.job_id,u.file_id LIMIT ?5",if job_filter.is_some(){"AND u.job_id=?6"}else{"AND ?6 IS NULL"});
        let mut statement = self.library.prepare(&sql)?;
        let rows = statement
            .query_map(
                params![
                    media.to_string(),
                    season,
                    job,
                    file,
                    limit as i64,
                    job_filter
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        rows.into_iter()
            .map(|(job_id, ledger_id, encoded)| {
                let context: ProbeAnalysisContext = serde_json::from_str(&encoded)?;
                let dispatch = ProbeDispatchId {
                    job_id,
                    ledger_id,
                    stage: ProbeDispatchStage::Analysis,
                };
                let guards = match self.analysis_evidence(&ProbeAnalysisRequest {
                    dispatch: dispatch.clone(),
                    target: context.target.clone(),
                    season: context.season,
                    sampling_mode: context.sampling_mode.clone(),
                    analysis_policy_key: context.analysis_policy_key.clone(),
                    analysis_policy_json: context.analysis_policy_json.clone(),
                    capture_context_key: context.capture_context_key.clone(),
                    required_kinds: context.required_kinds.clone(),
                }) {
                    Ok(mut guards) => {
                        guards.retain(|guard| {
                            !guard.id.starts_with("sample:")
                                || context.evidence.iter().any(|(id, _)| id == &guard.id)
                        });
                        let missing = context.evidence.iter().any(|(id, _)| {
                            id.starts_with("sample:") && !guards.iter().any(|guard| &guard.id == id)
                        });
                        (!missing).then_some(guards)
                    }
                    Err(StoreError::Missing(_)) => None,
                    Err(error) => return Err(error),
                };
                Ok((dispatch, encoded, guards))
            })
            .collect()
    }

    pub(super) fn claim_probe_analysis(
        &self,
        token: &str,
        participants: &[ProbeAnalysisParticipant],
    ) -> Result<bool, StoreError> {
        let tx = crate::write_tx(self.library)?;
        for participant in participants {
            if participant.dispatch.ledger_id != participant.context.target.file.file_id.to_string()
            {
                return Ok(false);
            }
            if !self.analysis_target_current(&participant.context)?
                || !self.analysis_evidence_matches(&participant.guards)?
            {
                return Ok(false);
            }
            let changed = tx.execute("UPDATE probe_job_units SET status='running',analysis_claim_token=?4
                WHERE job_id=?1 AND file_id=?2 AND analysis_context_json=?3 AND stage='analysis' AND status='queued'
                AND EXISTS(SELECT 1 FROM probe_jobs WHERE id=?1 AND status IN ('queued','running'))",
                params![participant.dispatch.job_id,participant.dispatch.ledger_id,participant.encoded,token])?;
            if changed != 1 {
                return Ok(false);
            }
        }
        tx.commit()?;
        Ok(true)
    }

    pub(super) fn requeue_probe_analysis(
        &self,
        claim: &ProbeAnalysisClaim,
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.library)?;
        for participant in &claim.participants {
            tx.execute("UPDATE probe_job_units SET status='queued',analysis_claim_token=NULL
                WHERE job_id=?1 AND file_id=?2 AND analysis_context_json=?3 AND analysis_claim_token=?4
                AND stage='analysis' AND status='running'",
                params![participant.dispatch.job_id,participant.dispatch.ledger_id,participant.encoded,claim.token])?;
        }
        tx.commit()?;
        Ok(())
    }
}
