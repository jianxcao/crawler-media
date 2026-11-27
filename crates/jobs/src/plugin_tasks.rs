use std::str::FromStr;

use domain::{JobDefId, PluginTaskBindingId};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::{JobDef, JobError, JobIdentity, JobKind, JobScope, NewDef, Queue, QueueRead, Schedule};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginTaskSpec {
    pub binding_id: PluginTaskBindingId,
    pub generation: i64,
    pub definition_revision: i64,
    pub name: String,
    pub enabled: bool,
    pub schedule: Option<Schedule>,
    pub payload: String,
    pub timeout_secs: Option<u32>,
    pub concurrency_key: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PluginTaskFence {
    pub binding_id: PluginTaskBindingId,
    pub generation: i64,
    pub definition_revision: i64,
    pub closed: bool,
    pub def_id: Option<JobDefId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PluginTaskProjection {
    pub fence: PluginTaskFence,
    pub applied: bool,
}

fn validate_fence_coords(generation: i64, definition_revision: i64) -> Result<(), JobError> {
    if generation < 0 || definition_revision <= 0 {
        return Err(JobError::Identity(
            "invalid plugin task fence coordinates".into(),
        ));
    }
    Ok(())
}

fn read_fence(
    conn: &Connection,
    binding_id: PluginTaskBindingId,
) -> Result<Option<PluginTaskFence>, JobError> {
    let row: Option<(i64, i64, bool, Option<String>)> = conn
        .query_row(
            "SELECT generation, definition_revision, closed, def_id
             FROM plugin_task_fences WHERE binding_id = ?1",
            [binding_id.to_string()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((generation, definition_revision, closed, raw_def_id)) = row else {
        return Ok(None);
    };
    let def_id = raw_def_id
        .map(|s| JobDefId::from_str(&s))
        .transpose()
        .map_err(|e| JobError::Identity(e.to_string()))?;
    Ok(Some(PluginTaskFence {
        binding_id,
        generation,
        definition_revision,
        closed,
        def_id,
    }))
}

fn write_fence(conn: &Connection, fence: PluginTaskFence) -> Result<(), JobError> {
    conn.execute(
        "INSERT INTO plugin_task_fences
            (binding_id, generation, definition_revision, closed, def_id)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(binding_id) DO UPDATE SET
            generation = excluded.generation,
            definition_revision = excluded.definition_revision,
            closed = excluded.closed,
            def_id = excluded.def_id",
        params![
            fence.binding_id.to_string(),
            fence.generation,
            fence.definition_revision,
            fence.closed as i64,
            fence.def_id.map(|id| id.to_string()),
        ],
    )?;
    Ok(())
}

fn matches_spec(def: &JobDef, spec: &PluginTaskSpec) -> bool {
    def.name == spec.name
        && def.enabled == spec.enabled
        && def.schedule == spec.schedule
        && crate::store::same_payload(&def.payload, &spec.payload)
        && def.timeout_secs == spec.timeout_secs
        && def.concurrency_key == spec.concurrency_key
}

fn find_existing_def(
    tx: &Connection,
    existing: Option<PluginTaskFence>,
    binding_id: PluginTaskBindingId,
) -> Result<Option<JobDef>, JobError> {
    if let Some(id) = existing.and_then(|f| f.def_id)
        && let Some(def) = crate::schedule::get_def(tx, id)?
    {
        return Ok(Some(def));
    }
    crate::schedule::def_for_identity(tx, JobIdentity::plugin_task(binding_id))
}

fn upsert_task_def(
    tx: &Connection,
    existing: Option<PluginTaskFence>,
    spec: &PluginTaskSpec,
) -> Result<JobDefId, JobError> {
    if let Some(def) = find_existing_def(tx, existing, spec.binding_id)? {
        if existing.is_some_and(|f| spec.generation > f.generation)
            || !spec.enabled
            || (def.schedule.is_some() && spec.schedule.is_none())
        {
            crate::store::cancel_def_children(tx, def.id)?;
        }
        let sched_str = spec.schedule.as_ref().map(Schedule::as_str);
        tx.execute(
            "UPDATE job_defs
             SET name = ?1, enabled = ?2, schedule = ?3, payload = ?4,
                 timeout_secs = ?5, concurrency_key = ?6
             WHERE id = ?7",
            params![
                spec.name,
                spec.enabled as i64,
                sched_str,
                spec.payload,
                spec.timeout_secs,
                spec.concurrency_key,
                def.id.to_string(),
            ],
        )?;
        if spec.enabled {
            tx.execute(
                "UPDATE jobs
                 SET payload = ?1, concurrency_key = ?2, timeout_secs = ?3
                 WHERE def_id = ?4 AND status = 'queued'",
                params![
                    spec.payload,
                    spec.concurrency_key,
                    spec.timeout_secs,
                    def.id.to_string(),
                ],
            )?;
        }
        return Ok(def.id);
    }
    let created = crate::store::create_def(
        tx,
        NewDef {
            kind: JobKind::PluginTask,
            scope: JobScope::PluginTask(spec.binding_id),
            name: spec.name.clone(),
            enabled: spec.enabled,
            schedule: spec.schedule.clone(),
            payload: spec.payload.clone(),
            timeout_secs: spec.timeout_secs,
            concurrency_key: spec.concurrency_key.clone(),
        },
    )?;
    Ok(created.id)
}

impl Queue {
    /// Atomically project a plugin task definition under its generation/revision fence.
    pub fn project_plugin_task(
        &self,
        spec: &PluginTaskSpec,
    ) -> Result<PluginTaskProjection, JobError> {
        validate_fence_coords(spec.generation, spec.definition_revision)?;
        if spec.name.is_empty() {
            return Err(JobError::Identity(
                "plugin task name must not be empty".into(),
            ));
        }
        JobIdentity::plugin_task(spec.binding_id).validate_payload(&spec.payload)?;
        self.with_connection(|conn| {
            let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
            let existing = read_fence(&tx, spec.binding_id)?;
            if let Some(fence) = existing {
                if spec.generation < fence.generation
                    || (spec.generation == fence.generation
                        && spec.definition_revision < fence.definition_revision)
                    || (spec.generation == fence.generation && fence.closed)
                {
                    tx.commit()?;
                    return Ok(PluginTaskProjection {
                        fence,
                        applied: false,
                    });
                }
                if spec.generation == fence.generation
                    && spec.definition_revision == fence.definition_revision
                {
                    if let Some(def) = find_existing_def(&tx, existing, spec.binding_id)?
                        && !matches_spec(&def, spec)
                    {
                        return Err(JobError::Conflict(format!(
                            "plugin task definition mismatch for {} at {}:{}",
                            spec.binding_id, spec.generation, spec.definition_revision
                        )));
                    }
                    tx.commit()?;
                    return Ok(PluginTaskProjection {
                        fence,
                        applied: false,
                    });
                }
            }
            let def_id = upsert_task_def(&tx, existing, spec)?;
            let fence = PluginTaskFence {
                binding_id: spec.binding_id,
                generation: spec.generation,
                definition_revision: spec.definition_revision,
                closed: false,
                def_id: Some(def_id),
            };
            write_fence(&tx, fence)?;
            tx.commit()?;
            Ok(PluginTaskProjection {
                fence,
                applied: true,
            })
        })
    }

    /// Close a plugin task binding, disabling its definition and cancelling queued runs.
    pub fn close_plugin_task(
        &self,
        binding_id: PluginTaskBindingId,
        generation: i64,
        definition_revision: i64,
    ) -> Result<PluginTaskProjection, JobError> {
        validate_fence_coords(generation, definition_revision)?;
        self.with_connection(|conn| {
            let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
            let existing = read_fence(&tx, binding_id)?;
            if let Some(fence) = existing
                && (generation < fence.generation
                    || (generation == fence.generation
                        && definition_revision < fence.definition_revision)
                    || (generation == fence.generation
                        && definition_revision == fence.definition_revision
                        && fence.closed))
            {
                tx.commit()?;
                return Ok(PluginTaskProjection {
                    fence,
                    applied: false,
                });
            }
            let def_id = find_existing_def(&tx, existing, binding_id)?.map(|def| def.id);
            if let Some(id) = def_id {
                crate::store::set_def_enabled_by_id(&tx, id, false)?;
            }
            let fence = PluginTaskFence {
                binding_id,
                generation,
                definition_revision,
                closed: true,
                def_id,
            };
            write_fence(&tx, fence)?;
            tx.commit()?;
            Ok(PluginTaskProjection {
                fence,
                applied: true,
            })
        })
    }

    pub fn plugin_task_fence(
        &self,
        binding_id: PluginTaskBindingId,
    ) -> Result<Option<PluginTaskFence>, JobError> {
        self.with_connection(|conn| read_fence(conn, binding_id))
    }
}

impl QueueRead<'_> {
    pub fn plugin_task_fence(
        &self,
        binding_id: PluginTaskBindingId,
    ) -> Result<Option<PluginTaskFence>, JobError> {
        self.with_connection(|conn| read_fence(conn, binding_id))
    }
}
