use super::*;
use rusqlite::{OptionalExtension, params};

/// Move unfinished reconciliation before its owning unit is retired. The
/// receiver resumes the keyset suffix, or restarts if current evidence changed.
pub(crate) fn transfer_reconciliation(
    tx: &rusqlite::Connection,
    job: &str,
    file: &str,
) -> Result<(), StoreError> {
    let receiver: Option<(String, String, String)> = tx.query_row(
        "SELECT live.job_id,live.file_id,old.analysis_refresh_json
         FROM probe_job_units old JOIN probe_job_units live
           ON json_extract(live.analysis_context_json,'$.media_id')=json_extract(old.analysis_context_json,'$.media_id')
          AND json_extract(live.analysis_context_json,'$.season')=json_extract(old.analysis_context_json,'$.season')
         JOIN probe_jobs j ON j.id=live.job_id
         JOIN library_files f ON f.file_id=live.file_id
         JOIN libraries l ON l.library_id=f.library_id
         JOIN library_roots root ON root.root_id=f.root_id AND root.enabled=1
         WHERE old.job_id=?1 AND old.file_id=?2 AND old.stage='analysis'
           AND json_extract(old.analysis_refresh_json,'$.reconcile_done')=0
           AND live.stage='analysis' AND live.status IN ('queued','running')
           AND j.status IN ('queued','running') AND live.file_id<>?2
           AND f.presence_state='present' AND f.publication_state='published'
           AND l.lifecycle_state='ready' AND l.detect_intros=1 AND l.enable_fingerprint=1
           AND NOT EXISTS(SELECT 1 FROM library_removals r WHERE r.library_id=f.library_id)
           AND json_extract(live.analysis_context_json,'$.analysis_policy_key')=json_extract(old.analysis_context_json,'$.analysis_policy_key')
           AND json_extract(live.analysis_context_json,'$.sampling_mode')=json_extract(old.analysis_context_json,'$.sampling_mode')
         ORDER BY live.job_id,live.file_id LIMIT 1",
        params![job,file], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    ).optional()?;
    let receiver = match receiver {
        Some(receiver) => Some(receiver),
        None => create_reconciliation_successor(tx, job, file)?,
    };
    if let Some((receiver_job, receiver_file, state)) = receiver {
        merge_reconciliation(tx, &receiver_job, &receiver_file, &state)?;
        tx.execute("UPDATE probe_job_units SET analysis_refresh_json=json_set(analysis_refresh_json,'$.reconcile_done',1)
            WHERE job_id=?1 AND file_id=?2",params![job,file])?;
    }
    Ok(())
}

fn merge_reconciliation(
    tx: &rusqlite::Connection,
    job: &str,
    file: &str,
    incoming: &str,
) -> Result<(), StoreError> {
    tx.execute("UPDATE probe_job_units SET analysis_refresh_json=CASE
        WHEN analysis_refresh_json IS NULL OR json_extract(analysis_refresh_json,'$.reconcile_done')=1 THEN ?3
        ELSE json_set(analysis_refresh_json,'$.reconcile_after',CASE
          WHEN json_extract(analysis_refresh_json,'$.wanted_reference_key')<>json_extract(?3,'$.wanted_reference_key')
            OR json_extract(analysis_refresh_json,'$.reconcile_after') IS NULL
            OR json_extract(?3,'$.reconcile_after') IS NULL THEN NULL
          WHEN (json_extract(json_extract(?3,'$.reconcile_after'),'$[0]'),
                json_extract(json_extract(?3,'$.reconcile_after'),'$[1]'),
                json_extract(json_extract(?3,'$.reconcile_after'),'$[2]')) <
               (json_extract(json_extract(analysis_refresh_json,'$.reconcile_after'),'$[0]'),
                json_extract(json_extract(analysis_refresh_json,'$.reconcile_after'),'$[1]'),
                json_extract(json_extract(analysis_refresh_json,'$.reconcile_after'),'$[2]'))
          THEN json_extract(?3,'$.reconcile_after')
          ELSE json_extract(analysis_refresh_json,'$.reconcile_after') END) END
        WHERE job_id=?1 AND file_id=?2", params![job,file,incoming])?;
    Ok(())
}

fn create_reconciliation_successor(
    tx: &rusqlite::Connection,
    job: &str,
    file: &str,
) -> Result<Option<(String, String, String)>, StoreError> {
    let candidate: Option<(String,String,String,String)> = tx.query_row(
        "SELECT prior.job_id,prior.file_id,old.analysis_refresh_json,lower(hex(randomblob(16)))
         FROM probe_job_units old JOIN library_file_units coverage
           ON coverage.media_id=json_extract(old.analysis_context_json,'$.media_id')
          AND coverage.season=json_extract(old.analysis_context_json,'$.season')
         JOIN library_files f ON f.file_id=coverage.file_id
         JOIN libraries l ON l.library_id=f.library_id
         JOIN library_roots root ON root.root_id=f.root_id AND root.enabled=1
         JOIN probe_job_units prior ON prior.file_id=f.file_id
         WHERE old.job_id=?1 AND old.file_id=?2 AND old.stage='analysis'
           AND json_extract(old.analysis_refresh_json,'$.reconcile_done')=0
           AND f.file_id<>?2 AND f.presence_state='present' AND f.publication_state='published'
           AND l.lifecycle_state='ready' AND l.detect_intros=1 AND l.enable_fingerprint=1
           AND NOT EXISTS(SELECT 1 FROM library_removals r WHERE r.library_id=f.library_id)
           AND NOT EXISTS(SELECT 1 FROM probe_job_units active WHERE active.file_id=f.file_id AND active.status IN ('queued','running'))
           AND prior.stage='analysis' AND prior.status='succeeded'
           AND json_extract(prior.analysis_context_json,'$.target.file.version.revision')=f.file_revision
           AND json_extract(prior.analysis_context_json,'$.target.file.version.source_version')=f.source_version
           AND json_extract(prior.analysis_context_json,'$.analysis_policy_key')=json_extract(old.analysis_context_json,'$.analysis_policy_key')
           AND json_extract(prior.analysis_context_json,'$.sampling_mode')=json_extract(old.analysis_context_json,'$.sampling_mode')
           AND (EXISTS(SELECT 1 FROM fingerprint_cache c WHERE c.file_id=f.file_id)
             OR EXISTS(SELECT 1 FROM fingerprint_samples s WHERE s.file_id=f.file_id AND s.source_version=f.source_version))
         LIMIT 1",params![job,file],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
    ).optional()?;
    let Some((previous, file, state, new_job)) = candidate else {
        return Ok(None);
    };
    tx.execute("INSERT INTO probe_jobs(id,kind,media_id,season,scope_key,status,total,created_at_ms)
        SELECT ?1,'fingerprint_probe',media_id,season,'ledger:'||?2,'queued',1,?4 FROM probe_jobs WHERE id=?3",
        params![new_job,file,previous,crate::unix_now()*1000])?;
    tx.execute("INSERT INTO probe_job_units(job_id,file_id,kind,force_fingerprint,reuse_fingerprint_cache,
        overwrite_markers,status,reuse_media_info_cache,stage,sampling_plan_json,analysis_context_json,analysis_refresh_json)
        SELECT ?1,file_id,kind,1,1,0,'queued',1,'analysis',sampling_plan_json,analysis_context_json,?4
        FROM probe_job_units WHERE job_id=?2 AND file_id=?3",params![new_job,previous,file,state])?;
    Ok(Some((new_job, file, state)))
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ProbeAnalysisCursor(pub i32, pub String, pub String);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProbeAnalysisRefreshState {
    pub wanted_reference_key: String,
    pub reconcile_after: Option<String>,
    pub reconcile_done: bool,
}

pub struct ProbeAnalysisReconcile {
    pub trigger: ProbeAnalysisParticipant,
    pub expected_cursor: Option<String>,
    pub reference_key: String,
    pub candidates: Vec<ProbeAnalysisParticipant>,
    pub next_cursor: Option<String>,
}

/// One bounded page of independently scoped obligations, prepared off-writer
/// and committed atomically with the model change that requires them.
pub struct ProbeAnalysisFollowups {
    pub(super) trigger: ProbeAnalysisParticipant,
    pub(super) candidates: Vec<ProbeAnalysisParticipant>,
    wanted: String,
    ids: Vec<String>,
}

impl ProbeAnalysisFollowups {
    pub fn new(
        trigger: ProbeAnalysisParticipant,
        candidates: Vec<ProbeAnalysisParticipant>,
        reference_key: String,
    ) -> Result<Self, StoreError> {
        if candidates.len() > 16 {
            return Err(StoreError::Invalid(
                "analysis followup page exceeds 16".into(),
            ));
        }
        let wanted = serde_json::to_string(&ProbeAnalysisRefreshState {
            wanted_reference_key: reference_key.clone(),
            reconcile_after: None,
            reconcile_done: true,
        })?;
        let ids = candidates
            .iter()
            .map(|_| uuid::Uuid::new_v4().to_string())
            .collect();
        Ok(Self {
            trigger,
            candidates,
            wanted,
            ids,
        })
    }
}

impl Store {
    pub fn probe_analysis_refresh_state(
        &self,
        id: &ProbeDispatchId,
    ) -> Result<Option<ProbeAnalysisRefreshState>, StoreError> {
        let raw: Option<String> = self.with_library_read(|db| {
            Ok(db.library.query_row(
            "SELECT analysis_refresh_json FROM probe_job_units WHERE job_id=?1 AND file_id=?2",
            params![id.job_id,id.ledger_id],|row|row.get(0)).optional()?.flatten())
        })?;
        raw.map(|raw| serde_json::from_str(&raw).map_err(Into::into))
            .transpose()
    }

    pub fn refresh_probe_analysis_targets(
        &self,
        page: &ProbeAnalysisReconcile,
    ) -> Result<bool, StoreError> {
        if page.candidates.len() > 16 {
            return Err(StoreError::Invalid(
                "analysis reconciliation page exceeds 16".into(),
            ));
        }
        let state = serde_json::to_string(&serde_json::json!({
            "wanted_reference_key":page.reference_key,"reconcile_after":page.next_cursor,
            "reconcile_done":page.next_cursor.is_none(),
        }))?;
        let ids = page
            .candidates
            .iter()
            .map(|_| uuid::Uuid::new_v4().to_string())
            .collect::<Vec<_>>();
        let wanted = serde_json::to_string(&ProbeAnalysisRefreshState {
            wanted_reference_key: page.reference_key.clone(),
            reconcile_after: None,
            reconcile_done: true,
        })?;
        self.with_library_write(|db| db.refresh_probe_analysis_targets(page, &state, &wanted, &ids))
    }
}

impl crate::LibraryConnection<'_> {
    fn refresh_probe_analysis_targets(
        &self,
        page: &ProbeAnalysisReconcile,
        state: &str,
        wanted: &str,
        ids: &[String],
    ) -> Result<bool, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let trigger = &page.trigger;
        let changed=tx.execute("UPDATE probe_job_units SET analysis_refresh_json=?5
            WHERE job_id=?1 AND file_id=?2 AND analysis_context_json=?3 AND stage='analysis'
            AND status IN ('queued','running')
            AND json_extract(analysis_refresh_json,'$.reconcile_after') IS ?4
            AND (analysis_refresh_json IS NULL OR json_extract(analysis_refresh_json,'$.wanted_reference_key')<>?6
                 OR json_extract(analysis_refresh_json,'$.reconcile_done')=0)",
            params![trigger.dispatch.job_id,trigger.dispatch.ledger_id,trigger.encoded,page.expected_cursor,state,page.reference_key])?;
        if changed != 1 {
            return Ok(false);
        }
        if !self.reconcile_candidates(
            trigger,
            &page.candidates,
            Some(&page.reference_key),
            wanted,
            ids,
        )? {
            return Ok(false);
        }
        tx.commit()?;
        Ok(true)
    }

    pub(super) fn apply_probe_analysis_followups(
        &self,
        page: &ProbeAnalysisFollowups,
    ) -> Result<bool, StoreError> {
        // Model dependencies may change while the selected reference set stays identical.
        // The owning claim makes this atomic page idempotent; reference equality does not.
        self.reconcile_candidates(
            &page.trigger,
            &page.candidates,
            None,
            &page.wanted,
            &page.ids,
        )
    }

    fn reconcile_candidates(
        &self,
        trigger: &ProbeAnalysisParticipant,
        candidates: &[ProbeAnalysisParticipant],
        reference_key: Option<&str>,
        wanted: &str,
        ids: &[String],
    ) -> Result<bool, StoreError> {
        let tx = self.library;
        for (candidate, job_id) in candidates.iter().zip(ids) {
            if candidate.dispatch.ledger_id == trigger.dispatch.ledger_id {
                continue;
            }
            if candidate.context.media_id != trigger.context.media_id
                || candidate.context.season != trigger.context.season
                || !self.analysis_target_current(&candidate.context)?
                || !self.analysis_evidence_matches(&candidate.guards)?
            {
                return Ok(false);
            }
            let id = &candidate.dispatch.ledger_id;
            let active = self.active_probe_unit_for_ledger(id)?;
            if let Some(active) = active {
                if active.stage == ProbeDispatchStage::Analysis {
                    merge_reconciliation(tx, &active.job_id, id, wanted)?;
                }
                continue;
            }
            let previous:Option<String>=tx.query_row("SELECT json_extract(u.detection_outcome_json,'$.available_reference_key')
                FROM probe_job_units u JOIN probe_jobs j ON j.id=u.job_id WHERE u.file_id=?1
                  AND u.detection_outcome_json IS NOT NULL ORDER BY j.created_at_ms DESC,j.id DESC LIMIT 1",
                [id],|row|row.get(0)).optional()?.flatten();
            if reference_key.is_some() && previous.as_deref() == reference_key {
                continue;
            }
            let spec = crate::ProbeJobUnitSpec {
                ledger_id: id,
                kind: "tv",
                force_fingerprint: true,
                reuse_fingerprint_cache: true,
                overwrite_markers: false,
                reuse_media_info_cache: true,
            };
            if !self.create_probe_job(
                job_id,
                "fingerprint_probe",
                &candidate.context.media_id.to_string(),
                Some(candidate.context.season),
                &format!("ledger:{id}"),
                &[spec],
            )? {
                continue;
            }
            tx.execute("UPDATE probe_job_units SET stage='analysis',analysis_context_json=?3,analysis_refresh_json=?4
                ,sampling_plan_json=(SELECT old.sampling_plan_json FROM probe_job_units old JOIN probe_jobs j ON j.id=old.job_id
                    WHERE old.file_id=?2 AND old.job_id<>?1 AND old.sampling_plan_json IS NOT NULL ORDER BY j.created_at_ms DESC,j.id DESC LIMIT 1)
                WHERE job_id=?1 AND file_id=?2",params![job_id,id,candidate.encoded,wanted])?;
        }
        Ok(true)
    }
}
