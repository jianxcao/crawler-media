use std::str::FromStr;

use domain::{JobId, PluginOperationId};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::{Job, JobError, JobIdentity, JobKind, JobScope, Queue, QueueRead};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewPluginOperationJob {
    pub operation_id: PluginOperationId,
    pub kind: JobKind,
    pub sequence: u32,
    pub generation: Option<i64>,
    pub request_key: String,
    pub payload: String,
    pub run_after: i64,
    pub concurrency_key: Option<String>,
    pub timeout_secs: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginOperationReceipt {
    pub operation_id: PluginOperationId,
    pub kind: JobKind,
    pub sequence: u32,
    pub job_id: JobId,
    pub generation: Option<i64>,
    pub request_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginOperationProjection {
    pub receipt: PluginOperationReceipt,
    pub created: bool,
    pub job: Option<Job>,
}

fn validate_kind(kind: JobKind) -> Result<(), JobError> {
    if matches!(kind, JobKind::PluginCommand | JobKind::PluginReconcile) {
        Ok(())
    } else {
        Err(JobError::Identity(format!(
            "unsupported plugin operation job kind: {}",
            kind.as_str()
        )))
    }
}

fn read_receipt(
    conn: &Connection,
    operation_id: PluginOperationId,
    kind: JobKind,
    sequence: u32,
) -> Result<Option<PluginOperationReceipt>, JobError> {
    validate_kind(kind)?;
    let row: Option<(String, Option<i64>, String)> = conn
        .query_row(
            "SELECT job_id, generation, request_key
             FROM plugin_operation_receipts
             WHERE operation_id = ?1 AND kind = ?2 AND sequence = ?3",
            params![operation_id.to_string(), kind.as_str(), i64::from(sequence)],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((raw_job_id, generation, request_key)) = row else {
        return Ok(None);
    };
    let job_id = JobId::from_str(&raw_job_id).map_err(|e| JobError::Identity(e.to_string()))?;
    Ok(Some(PluginOperationReceipt {
        operation_id,
        kind,
        sequence,
        job_id,
        generation,
        request_key,
    }))
}

impl Queue {
    /// Atomically project a durable plugin operation receipt and enqueue one Job.
    pub fn project_plugin_operation(
        &self,
        spec: NewPluginOperationJob,
    ) -> Result<PluginOperationProjection, JobError> {
        validate_kind(spec.kind)?;
        if spec.generation.is_some_and(|g| g < 0) {
            return Err(JobError::Identity("negative plugin generation".into()));
        }
        if spec.request_key.is_empty() || spec.request_key.len() > 256 {
            return Err(JobError::Identity("invalid plugin request_key".into()));
        }
        let identity = JobIdentity {
            kind: spec.kind,
            scope: JobScope::PluginOperation(spec.operation_id),
        };
        identity.validate_payload(&spec.payload)?;
        self.with_connection(|conn| {
            let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
            if let Some(receipt) = read_receipt(&tx, spec.operation_id, spec.kind, spec.sequence)? {
                if receipt.generation != spec.generation || receipt.request_key != spec.request_key
                {
                    return Err(JobError::Conflict(format!(
                        "plugin operation projection mismatch for {}:{}:{}",
                        spec.operation_id,
                        spec.kind.as_str(),
                        spec.sequence
                    )));
                }
                let job = crate::store::get_job(&tx, receipt.job_id)?;
                tx.commit()?;
                return Ok(PluginOperationProjection {
                    receipt,
                    created: false,
                    job,
                });
            }
            let job = crate::store::insert_job(
                &tx,
                None,
                identity,
                &spec.payload,
                spec.run_after,
                spec.concurrency_key.as_deref(),
                spec.timeout_secs,
            )?;
            let receipt = PluginOperationReceipt {
                operation_id: spec.operation_id,
                kind: spec.kind,
                sequence: spec.sequence,
                job_id: job.id,
                generation: spec.generation,
                request_key: spec.request_key,
            };
            tx.execute(
                "INSERT INTO plugin_operation_receipts
                    (operation_id, kind, sequence, job_id, generation, request_key)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    receipt.operation_id.to_string(),
                    receipt.kind.as_str(),
                    i64::from(receipt.sequence),
                    receipt.job_id.to_string(),
                    receipt.generation,
                    receipt.request_key,
                ],
            )?;
            tx.commit()?;
            Ok(PluginOperationProjection {
                receipt,
                created: true,
                job: Some(job),
            })
        })
    }

    pub fn plugin_operation_receipt(
        &self,
        operation_id: PluginOperationId,
        kind: JobKind,
        sequence: u32,
    ) -> Result<Option<PluginOperationReceipt>, JobError> {
        self.with_connection(|conn| read_receipt(conn, operation_id, kind, sequence))
    }
}

impl QueueRead<'_> {
    pub fn plugin_operation_receipt(
        &self,
        operation_id: PluginOperationId,
        kind: JobKind,
        sequence: u32,
    ) -> Result<Option<PluginOperationReceipt>, JobError> {
        self.with_connection(|conn| read_receipt(conn, operation_id, kind, sequence))
    }
}
