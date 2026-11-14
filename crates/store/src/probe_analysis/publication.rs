use super::*;
use rusqlite::params;

pub struct ProbeAnalysisOutput {
    pub dispatch: ProbeDispatchId,
    pub detection_outcome_json: String,
    /// None preserves accepted marker/chapter facts when every kind is unresolved.
    pub replacement: Option<crate::MarkerResultReplacement>,
}

pub struct ProbeAnalysisCommit {
    pub claim: ProbeAnalysisClaim,
    pub outputs: Vec<ProbeAnalysisOutput>,
    pub references: Vec<ProbeAnalysisParticipant>,
    pub observations: Vec<crate::SourceObservation>,
    pub models: Vec<crate::StoredFingerprintModelSet>,
    pub followups: Vec<ProbeAnalysisFollowups>,
    pub refresh_job_id: Option<String>,
    pub refresh_error: Option<String>,
}

impl Store {
    pub fn complete_probe_analysis_checked(
        &self,
        commit: &ProbeAnalysisCommit,
    ) -> Result<Option<Vec<crate::ProbeJob>>, StoreError> {
        // Reject malformed outcome JSON before writer admission.
        let reference_keys = commit
            .outputs
            .iter()
            .map(|output| {
                let value: serde_json::Value =
                    serde_json::from_str(&output.detection_outcome_json)?;
                Ok(value
                    .get("available_reference_key")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned))
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        self.with_library_write(|db| db.complete_probe_analysis_checked(commit, &reference_keys))
    }
}

impl crate::LibraryConnection<'_> {
    fn complete_probe_analysis_checked(
        &self,
        commit: &ProbeAnalysisCommit,
        reference_keys: &[Option<String>],
    ) -> Result<Option<Vec<crate::ProbeJob>>, StoreError> {
        let tx = crate::write_tx(self.library)?;
        if !self.analysis_commit_current(commit, reference_keys)? {
            return Ok(None);
        }
        if commit.refresh_error.is_none() {
            for set in &commit.models {
                self.replace_fingerprint_model_bundles(set)?;
            }
            for page in &commit.followups {
                if !self.apply_probe_analysis_followups(page)? {
                    return Ok(None);
                }
            }
            for output in &commit.outputs {
                if let Some(replacement) = &output.replacement {
                    // The exact target/reference guards above replace broad season equality.
                    self.replace_marker_results_batch_inner(
                        std::slice::from_ref(replacement),
                        None,
                        None,
                    )?;
                }
            }
        }
        for output in &commit.outputs {
            let failed = commit.refresh_error.is_some();
            self.put_probe_detection_outcome(
                &output.dispatch.job_id,
                &output.dispatch.ledger_id,
                &output.detection_outcome_json,
            )?;
            self.finish_probe_unit_with_status(
                &output.dispatch.job_id,
                &output.dispatch.ledger_id,
                if failed { "failed" } else { "succeeded" },
                failed.then_some("verification_unresolved"),
                commit.refresh_error.as_deref(),
            )?;
            tx.execute("UPDATE probe_job_units SET analysis_claim_token=NULL WHERE job_id=?1 AND file_id=?2",
                params![output.dispatch.job_id,output.dispatch.ledger_id])?;
        }
        if let Some(job) = &commit.refresh_job_id {
            self.finish_probe_job(
                job,
                commit.refresh_error.is_none(),
                commit.refresh_error.as_deref(),
            )?;
        }
        // These owned terminal facts belong to this publication transaction.
        // A postcommit reader admission failure must not lose finalization.
        let ids = commit
            .outputs
            .iter()
            .map(|output| output.dispatch.job_id.as_str())
            .chain(commit.refresh_job_id.as_deref())
            .collect::<std::collections::BTreeSet<_>>();
        let mut terminal_jobs = Vec::new();
        for id in ids {
            let job = self
                .get_probe_job(id)?
                .ok_or_else(|| StoreError::Missing(format!("analysis job {id}")))?;
            if !job.is_active() {
                terminal_jobs.push(job);
            }
        }
        tx.commit()?;
        Ok(Some(terminal_jobs))
    }

    fn analysis_commit_current(
        &self,
        commit: &ProbeAnalysisCommit,
        reference_keys: &[Option<String>],
    ) -> Result<bool, StoreError> {
        if commit.outputs.len() != commit.claim.participants.len() || commit.outputs.is_empty() {
            return Ok(false);
        }
        if !model_followups_scoped(commit) {
            return Ok(false);
        }
        let mut outputs = std::collections::HashSet::new();
        for participant in &commit.claim.participants {
            let id = &participant.dispatch;
            let Some(output_index) = commit
                .outputs
                .iter()
                .position(|output| output.dispatch == *id)
            else {
                return Ok(false);
            };
            let current: bool = self.library.query_row(
                "SELECT EXISTS(SELECT 1 FROM probe_job_units u
                JOIN probe_jobs j ON j.id=u.job_id WHERE u.job_id=?1 AND u.file_id=?2
                AND u.analysis_context_json=?3 AND u.analysis_claim_token=?4 AND u.stage='analysis'
                AND u.status='running' AND j.status IN ('queued','running')
                AND (u.analysis_refresh_json IS NULL OR
                    (json_extract(u.analysis_refresh_json,'$.reconcile_done')=1
                     AND json_extract(u.analysis_refresh_json,'$.wanted_reference_key') IS ?5)))",
                params![
                    id.job_id,
                    id.ledger_id,
                    participant.encoded,
                    commit.claim.token,
                    reference_keys[output_index]
                ],
                |row| row.get(0),
            )?;
            if !current {
                let state: Option<String> = self.library.query_row("SELECT analysis_refresh_json FROM probe_job_units WHERE job_id=?1 AND file_id=?2", params![id.job_id,id.ledger_id],|row|row.get(0))?;
                tracing::debug!(job_id=%id.job_id,file_id=%id.ledger_id,context_key=%participant.context.context_key,token=%commit.claim.token,reference_key=?reference_keys[output_index],refresh_state=?state,"analysis claim guard rejected");
                return Ok(false);
            }
            let Some(output) = commit.outputs.iter().find(|output| output.dispatch == *id) else {
                return Ok(false);
            };
            if !outputs.insert((&id.job_id, &id.ledger_id)) {
                return Ok(false);
            }
            if let Some(replacement) = &output.replacement
                && (replacement.targets.len() != 1
                    || replacement.targets[0] != participant.context.target
                    || replacement
                        .markers
                        .iter()
                        .any(|marker| marker.file_id.to_string() != id.ledger_id)
                    || replacement
                        .chapter_updates
                        .iter()
                        .any(|(file, _)| file != &id.ledger_id))
            {
                return Ok(false);
            }
        }
        if let Some(job_id) = &commit.refresh_job_id {
            let surviving: i64 = self.library.query_row(
                "SELECT COUNT(*) FROM probe_job_units
                WHERE job_id=?1 AND status<>'cancelled'",
                [job_id],
                |row| row.get(0),
            )?;
            if surviving as usize
                != commit
                    .claim
                    .participants
                    .iter()
                    .filter(|p| p.dispatch.job_id == *job_id)
                    .count()
            {
                return Ok(false);
            }
        }
        let mut observed = std::collections::HashSet::new();
        for participant in commit.claim.participants.iter().chain(&commit.references) {
            if !self.analysis_target_current(&participant.context)?
                || !self.analysis_evidence_matches(&participant.guards)?
            {
                tracing::debug!(job_id=%participant.dispatch.job_id,file_id=%participant.dispatch.ledger_id,"analysis target/evidence guard rejected");
                return Ok(false);
            }
            let file = &participant.context.target.file;
            let Some(observation) = commit
                .observations
                .iter()
                .find(|o| o.file_id == file.file_id)
            else {
                tracing::debug!(file_id=%file.file_id,"analysis source observation absent");
                return Ok(false);
            };
            if observation.expected != file.version
                || observation.presence != domain::FilePresence::Present
            {
                return Ok(false);
            }
            observed.insert(file.file_id);
        }
        Ok(observed.len() == commit.observations.len())
    }
}

fn model_followups_scoped(commit: &ProbeAnalysisCommit) -> bool {
    let participants = &commit.claim.participants;
    commit.models.iter().all(|set| {
        participants.iter().any(|participant| {
            participant.context.media_id == set.media_id
                && participant.context.season == set.season
                && participant.context.analysis_policy_key == set.policy_key
        })
    }) && commit.followups.iter().all(|page| {
        participants
            .iter()
            .any(|p| p.dispatch == page.trigger.dispatch)
            && commit.models.iter().any(|set| {
                set.media_id == page.trigger.context.media_id
                    && set.season == page.trigger.context.season
                    && set.policy_key == page.trigger.context.analysis_policy_key
            })
            && page.candidates.iter().all(|candidate| {
                candidate.context.analysis_policy_key == page.trigger.context.analysis_policy_key
                    && candidate.context.sampling_mode == page.trigger.context.sampling_mode
                    && commit
                        .references
                        .iter()
                        .any(|reference| reference.encoded == candidate.encoded)
                    && !participants
                        .iter()
                        .any(|p| p.dispatch.ledger_id == candidate.dispatch.ledger_id)
            })
    })
}
