//! Persisted roots and unfinished plans share the same root-key admission rule.
use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};

use super::{ReconfigurationPlan, unfinished_operation_predicate};
use crate::{StoreError, path_key::canonical_recorded_key};

pub(crate) fn ensure_root_keys_available<'a>(
    tx: &Connection,
    library_id: &str,
    keys: impl IntoIterator<Item = &'a str>,
) -> Result<(), StoreError> {
    let keys: HashSet<String> = keys
        .into_iter()
        .map(|key| canonical_recorded_key(tx, key))
        .collect();
    let mut reserved = keys.clone();
    let mut owners =
        tx.prepare("SELECT library_id FROM library_roots WHERE path_key = ?1 LIMIT 1")?;
    for key in &keys {
        let owner: Option<String> = owners
            .query_row(params![key], |row| row.get(0))
            .optional()?;
        if let Some(owner) = owner {
            if owner != library_id {
                return Err(conflict(key, &owner));
            }
            // Committed ownership takes precedence over an older conflicting plan.
            // That plan's owner must correct it; retaining this Library's root is legal.
            reserved.remove(key);
        }
    }
    if reserved.is_empty() {
        return Ok(());
    }
    let mut plans = tx.prepare(&format!(
        "SELECT operation_id, library_id, plan_json FROM library_operations
         WHERE kind = 'roots' AND library_id <> ?1 AND {}",
        unfinished_operation_predicate()
    ))?;
    let rows = plans.query_map([library_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        let (operation, owner, json) = row?;
        let plan: ReconfigurationPlan = serde_json::from_str(&json).map_err(|error| {
            StoreError::Invalid(format!("root operation {operation} plan: {error}"))
        })?;
        let roots = plan.roots.ok_or_else(|| {
            StoreError::Invalid(format!("root operation {operation} plan has no roots"))
        })?;
        for root in roots {
            let key = canonical_recorded_key(tx, &root.path_key);
            if reserved.contains(&key) {
                return Err(conflict(&key, &owner));
            }
        }
    }
    Ok(())
}

fn conflict(key: &str, owner: &str) -> StoreError {
    StoreError::Conflict(format!("root {key} is reserved by Library {owner}"))
}
