use rusqlite::params;

use super::{Store, StoreError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredFingerprintModel {
    pub model_id: String,
    pub media_id: String,
    pub season: u32,
    pub kind: String,
    pub model_version: u32,
    pub membership_key: String,
    pub policy_key: String,
    pub model_json: String,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredFingerprintModelMember {
    pub model_id: String,
    pub sample_id: String,
    pub ledger_id: String,
    pub source_version: String,
}

impl Store {
    pub fn put_fingerprint_model(
        &self,
        model: &StoredFingerprintModel,
        members: &[StoredFingerprintModelMember],
    ) -> Result<(), StoreError> {
        let tx = self.library.unchecked_transaction()?;

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
                     model_id, sample_id, ledger_id, source_version
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

    pub fn delete_fingerprint_model(&self, model_id: &str) -> Result<(), StoreError> {
        let tx = self.library.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM fingerprint_model_members WHERE model_id = ?1",
            params![model_id],
        )?;
        tx.execute(
            "DELETE FROM fingerprint_season_models WHERE model_id = ?1",
            params![model_id],
        )?;
        tx.commit()?;
        Ok(())
    }
}
