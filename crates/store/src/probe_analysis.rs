//! Durable capture-to-analysis ownership on existing probe units.
mod connection;
mod evidence;
mod publication;
mod read;
mod reconcile;
pub use publication::{ProbeAnalysisCommit, ProbeAnalysisOutput};
pub use read::{ProbeAnalysisFile, ProbeAnalysisRead};
pub(crate) use reconcile::transfer_reconciliation;
pub use reconcile::{
    ProbeAnalysisCursor, ProbeAnalysisFollowups, ProbeAnalysisReconcile, ProbeAnalysisRefreshState,
};

use crate::{ProbeDispatchId, ProbeDispatchStage, Store, StoreError};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug)]
pub struct ProbeAnalysisRequest {
    pub dispatch: ProbeDispatchId,
    pub target: domain::FileCoverage,
    pub season: u32,
    pub sampling_mode: String,
    pub analysis_policy_key: String,
    pub analysis_policy_json: String,
    pub capture_context_key: String,
    pub required_kinds: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProbeAnalysisContext {
    pub context_key: String,
    pub media_id: domain::MediaId,
    pub target: domain::FileCoverage,
    pub season: u32,
    pub sampling_mode: String,
    pub analysis_policy_key: String,
    pub analysis_policy_json: String,
    pub capture_context_key: String,
    pub required_kinds: Vec<String>,
    pub evidence: Vec<(String, String)>,
}

/// Prepared off-writer, including exact persisted payload guards. Payloads are
/// transient; the durable context stores only their identities, never words.
#[derive(Clone, Debug)]
pub struct ProbeAnalysisParticipant {
    dispatch: ProbeDispatchId,
    context: ProbeAnalysisContext,
    encoded: String,
    guards: Vec<evidence::EvidenceGuard>,
}

impl ProbeAnalysisParticipant {
    pub fn dispatch(&self) -> &ProbeDispatchId {
        &self.dispatch
    }
    pub fn context(&self) -> &ProbeAnalysisContext {
        &self.context
    }

    /// Qualify this target against the exact input snapshot used by Analysis.
    /// Template-only references are guarded separately from target captures.
    pub fn observed_by(&self, reference: &Self, selected_samples: &[&str]) -> bool {
        reference.dispatch.ledger_id == self.dispatch.ledger_id
            && reference.context.target == self.context.target
            && self.context.evidence.iter().all(|evidence| {
                reference.context.evidence.contains(evidence)
                    && (self.context.required_kinds.is_empty()
                        || evidence
                            .0
                            .strip_prefix("sample:")
                            .is_none_or(|sample| selected_samples.contains(&sample)))
            })
    }

    /// Retain the exact adaptive samples used by this comparison, plus source,
    /// metadata and Library guards. Historical alternate captures are not inputs.
    pub fn select_samples(mut self, ids: &[String]) -> Result<Self, StoreError> {
        if self.context.sampling_mode != "adaptive"
            || ids.is_empty()
            || ids.iter().any(|id| {
                !self
                    .guards
                    .iter()
                    .any(|guard| guard.id == format!("sample:{id}"))
            })
        {
            return Err(StoreError::Invalid(
                "selected analysis sample missing".into(),
            ));
        }
        self.guards.retain(|guard| {
            guard
                .id
                .strip_prefix("sample:")
                .is_none_or(|id| ids.iter().any(|selected| selected == id))
        });
        self.context.evidence = self
            .guards
            .iter()
            .map(|guard| Ok((guard.id.clone(), guard.identity()?)))
            .collect::<Result<_, StoreError>>()?;
        self.context.context_key.clear();
        self.context.context_key = digest(&self.context)?;
        self.encoded = serde_json::to_string(&self.context)?;
        Ok(self)
    }
}

#[derive(Clone, Debug)]
pub struct ProbeAnalysisClaim {
    pub token: String,
    pub participants: Vec<ProbeAnalysisParticipant>,
}

fn digest(value: &impl Serialize) -> Result<String, StoreError> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

impl Store {
    pub fn probe_analysis_context(
        &self,
        dispatch: &ProbeDispatchId,
    ) -> Result<Option<ProbeAnalysisContext>, StoreError> {
        use rusqlite::OptionalExtension;
        self.with_library_read(|db| {
            let encoded: Option<String> = db.library.query_row(
                "SELECT analysis_context_json FROM probe_job_units
                 WHERE job_id=?1 AND file_id=?2 AND stage='analysis' AND status IN ('queued','running')",
                rusqlite::params![dispatch.job_id, dispatch.ledger_id],
                |row| row.get(0),
            ).optional()?;
            encoded.map(|json| serde_json::from_str(&json).map_err(Into::into)).transpose()
        })
    }

    pub fn fail_probe_analysis_refresh_if_capture_failed(
        &self,
        job_id: &str,
    ) -> Result<Option<crate::ProbeJob>, StoreError> {
        self.with_library_write(|db| {
            let tx=crate::write_tx(db.library)?;
            let failed:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM probe_jobs j
                WHERE j.id=?1 AND j.kind='marker_refresh' AND j.status IN ('queued','running')
                AND EXISTS(SELECT 1 FROM probe_job_units u WHERE u.job_id=j.id AND u.status IN ('failed','partial'))
                AND NOT EXISTS(SELECT 1 FROM probe_job_units u WHERE u.job_id=j.id
                    AND u.status IN ('queued','running') AND u.stage<>'analysis'))",[job_id],|row|row.get(0))?;
            if !failed {return Ok(None);}
            let mut statement=tx.prepare("SELECT file_id FROM probe_job_units WHERE job_id=?1 AND status IN ('queued','running')")?;
            let files=statement.query_map([job_id],|row|row.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
            drop(statement);
            for file in files {
                db.finish_probe_unit_with_status(job_id,&file,"failed",Some("capture_incomplete"),Some("refresh capture incomplete"))?;
            }
            db.finish_probe_job(job_id,false,Some("refresh capture incomplete"))?;
            let job=db.get_probe_job(job_id)?.ok_or_else(||StoreError::Missing(format!("analysis refresh {job_id}")))?;
            tx.commit()?;Ok(Some(job))
        })
    }

    pub fn probe_analysis_job_participants(
        &self,
        job_id: &str,
    ) -> Result<Option<Vec<ProbeAnalysisParticipant>>, StoreError> {
        let units = self.probe_job_units(job_id)?;
        let surviving = units
            .iter()
            .filter(|unit| unit.status != "cancelled")
            .collect::<Vec<_>>();
        if surviving.is_empty()
            || surviving
                .iter()
                .any(|unit| unit.stage != ProbeDispatchStage::Analysis || unit.status != "queued")
        {
            return Ok(None);
        }
        let mut result = Vec::new();
        let mut seasons = std::collections::BTreeSet::new();
        let job = self
            .get_probe_job(job_id)?
            .ok_or_else(|| StoreError::Missing("analysis job missing".into()))?;
        let media = job.media_id.parse()?;
        for unit in surviving {
            let row = self
                .file_coverage_by_id(unit.ledger_id.parse()?)?
                .ok_or_else(|| StoreError::Missing("analysis file missing".into()))?;
            for covered in row.units {
                if covered.season >= 0 {
                    seasons.insert(covered.season as u32);
                }
            }
        }
        for season in seasons {
            let mut after: Option<(String, String)> = None;
            loop {
                let rows = self.with_library_read(|db| {
                    db.analysis_participant_rows(
                        media,
                        season,
                        after
                            .as_ref()
                            .map(|(job, file)| (job.as_str(), file.as_str())),
                        16,
                        Some(job_id),
                    )
                })?;
                let page = self.decode_participants(rows)?;
                let Some(last) = page.last() else {
                    break;
                };
                after = Some((
                    last.dispatch.job_id.clone(),
                    last.dispatch.ledger_id.clone(),
                ));
                result.extend(page);
            }
        }
        Ok(Some(result))
    }
    pub fn analysis_season_files_page(
        &self,
        media: domain::MediaId,
        season: u32,
        after: Option<(i32, String, String)>,
        limit: usize,
    ) -> Result<Vec<domain::FileCoverage>, StoreError> {
        use rusqlite::params;
        self.with_library_read(|db| {
            let (episode,library,file)=after.unwrap_or((-1,String::new(),String::new()));
            let mut statement=db.library.prepare("SELECT u.file_id FROM library_file_units u
                JOIN library_files f ON f.file_id=u.file_id JOIN libraries l ON l.library_id=u.library_id
                WHERE u.media_id=?1 AND u.season=?2 AND (u.episode,u.library_id,u.file_id)>(?3,?4,?5)
                AND f.presence_state='present' AND f.publication_state='published' AND l.lifecycle_state='ready'
                ORDER BY u.episode,u.library_id,u.file_id LIMIT ?6")?;
            let ids=statement.query_map(params![media.to_string(),season,episode,library,file,limit.min(16) as i64],
                |row|row.get::<_,String>(0))?.collect::<Result<Vec<_>,_>>()?;
            ids.iter().map(|id|db.file_coverage_by_id(id.parse()?)?
                .ok_or_else(||StoreError::Missing("analysis file disappeared".into()))).collect()
        })
    }
    pub fn prepare_probe_analysis(
        &self,
        request: &ProbeAnalysisRequest,
    ) -> Result<ProbeAnalysisParticipant, StoreError> {
        let guards = self.with_library_read(|db| db.analysis_evidence(request))?;
        let mut context = ProbeAnalysisContext {
            context_key: String::new(),
            media_id: request.target.file.media_id,
            target: request.target.clone(),
            season: request.season,
            sampling_mode: request.sampling_mode.clone(),
            analysis_policy_key: request.analysis_policy_key.clone(),
            analysis_policy_json: request.analysis_policy_json.clone(),
            capture_context_key: request.capture_context_key.clone(),
            required_kinds: request.required_kinds.clone(),
            evidence: guards
                .iter()
                .map(|guard| Ok((guard.id.clone(), guard.identity()?)))
                .collect::<Result<_, StoreError>>()?,
        };
        context.context_key = digest(&context)?;
        let encoded = serde_json::to_string(&context)?;
        Ok(ProbeAnalysisParticipant {
            dispatch: request.dispatch.clone(),
            context,
            encoded,
            guards,
        })
    }

    pub fn advance_probe_unit_to_analysis(
        &self,
        participant: &ProbeAnalysisParticipant,
    ) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.advance_probe_unit_to_analysis(participant))
    }

    pub fn pending_probe_analysis_keys(
        &self,
        after: Option<(domain::MediaId, u32)>,
        limit: usize,
    ) -> Result<Vec<(domain::MediaId, u32)>, StoreError> {
        self.with_library_read(|db| db.pending_probe_analysis_keys(after, limit.min(16)))
    }

    pub fn probe_analysis_participants(
        &self,
        media: domain::MediaId,
        season: u32,
        after: Option<(&str, &str)>,
        limit: usize,
    ) -> Result<Vec<ProbeAnalysisParticipant>, StoreError> {
        let rows = self.with_library_read(|db| {
            db.analysis_participant_rows(media, season, after, limit.min(16), None)
        })?;
        self.decode_participants(rows)
    }

    pub fn claim_probe_analysis(
        &self,
        token: &str,
        participants: &[ProbeAnalysisParticipant],
    ) -> Result<Option<ProbeAnalysisClaim>, StoreError> {
        if token.is_empty() || participants.is_empty() {
            return Ok(None);
        }
        self.with_library_write(|db| db.claim_probe_analysis(token, participants))
            .map(|claimed| {
                claimed.then(|| ProbeAnalysisClaim {
                    token: token.into(),
                    participants: participants.to_vec(),
                })
            })
    }

    pub fn requeue_probe_analysis(&self, claim: &ProbeAnalysisClaim) -> Result<(), StoreError> {
        self.with_library_write(|db| db.requeue_probe_analysis(claim))
    }

    pub fn request_probe_analysis_capture(
        &self,
        claim: &ProbeAnalysisClaim,
        id: &ProbeDispatchId,
        plan_json: &str,
    ) -> Result<bool, StoreError> {
        let _: serde_json::Value = serde_json::from_str(plan_json)?;
        let Some(participant) = claim.participants.iter().find(|p| p.dispatch == *id) else {
            return Ok(false);
        };
        self.with_library_write(|db| {
            let changed = db.library.execute(
                "UPDATE probe_job_units SET stage='fingerprint',status='queued',
                sampling_plan_json=?5,analysis_context_json=NULL,analysis_claim_token=NULL
                WHERE job_id=?1 AND file_id=?2 AND stage='analysis' AND status='running'
                AND analysis_context_json=?3 AND analysis_claim_token=?4",
                rusqlite::params![
                    id.job_id,
                    id.ledger_id,
                    participant.encoded,
                    claim.token,
                    plan_json
                ],
            )?;
            Ok(changed == 1)
        })
    }
}

impl Store {
    fn decode_participants(
        &self,
        rows: Vec<connection::ParticipantRow>,
    ) -> Result<Vec<ProbeAnalysisParticipant>, StoreError> {
        let mut participants = Vec::new();
        for (dispatch, encoded, guards) in rows {
            let mut context: ProbeAnalysisContext = serde_json::from_str(&encoded)?;
            if !self.with_library_read(|db| db.analysis_target_current(&context))?
                || guards.is_none()
            {
                self.with_library_write(|db| {
                    db.library.execute("UPDATE probe_job_units SET stage='metadata',analysis_context_json=NULL,
                        analysis_claim_token=NULL,analysis_refresh_json=NULL WHERE job_id=?1 AND file_id=?2
                        AND analysis_context_json=?3 AND status='queued' AND stage='analysis'",
                        rusqlite::params![dispatch.job_id,dispatch.ledger_id,encoded])?;
                    Ok(())
                })?;
                continue;
            }
            let Some(guards) = guards else {
                continue;
            };
            let evidence = guards
                .iter()
                .map(|guard| Ok((guard.id.clone(), guard.identity()?)))
                .collect::<Result<Vec<_>, StoreError>>()?;
            if evidence != context.evidence {
                context.evidence = evidence;
                context.context_key.clear();
                context.context_key = digest(&context)?;
                let replacement = serde_json::to_string(&context)?;
                let changed=self.with_library_write(|db| {
                    if !db.analysis_target_current(&context)? || !db.analysis_evidence_matches(&guards)? {return Ok(false);}
                    Ok(db.library.execute("UPDATE probe_job_units SET analysis_context_json=?4,
                        analysis_refresh_json=NULL WHERE job_id=?1 AND file_id=?2 AND analysis_context_json=?3
                        AND stage='analysis' AND status='queued'",
                        rusqlite::params![dispatch.job_id,dispatch.ledger_id,encoded,replacement])?==1)
                })?;
                if changed {
                    participants.push(ProbeAnalysisParticipant {
                        dispatch,
                        encoded: replacement,
                        context,
                        guards,
                    });
                }
            } else {
                participants.push(ProbeAnalysisParticipant {
                    dispatch,
                    encoded,
                    context,
                    guards,
                });
            }
        }
        Ok(participants)
    }
}
