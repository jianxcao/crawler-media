use rusqlite::{Connection, OptionalExtension, params};

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
        let tx = crate::write_tx(&self.library)?;
        validate_model_publication(&tx, model, members)?;

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

/// All model and member checks share the publication transaction: a late worker
/// cannot recreate a reference after a Media merge has committed its first phase.
fn validate_model_publication(
    tx: &Connection,
    model: &StoredFingerprintModel,
    members: &[StoredFingerprintModelMember],
) -> Result<(), StoreError> {
    let media = model.media_id.parse::<domain::MediaId>()?;
    crate::library_write::publish::ensure_media_writable(tx, media)?;
    let existing: Option<String> = tx
        .query_row(
            "SELECT media_id FROM fingerprint_season_models WHERE model_id=?1",
            [&model.model_id],
            |row| row.get(0),
        )
        .optional()?;
    if existing.as_ref().is_some_and(|old| old != &model.media_id) {
        return Err(rejected_model(model, "model Media changed"));
    }
    for member in members {
        if member.model_id != model.model_id {
            return Err(rejected_model(model, "member names a different model"));
        }
        validate_model_member(tx, model, member)?;
    }
    Ok(())
}

struct ModelMemberFile {
    media: String,
    source_version: String,
    library: String,
    root: String,
    path: String,
    kind: String,
    presence: String,
    publication: String,
    covers_season: bool,
}

fn validate_model_member(
    tx: &Connection,
    model: &StoredFingerprintModel,
    member: &StoredFingerprintModelMember,
) -> Result<(), StoreError> {
    let current: Option<ModelMemberFile> = tx
        .query_row(
            "SELECT media_id,source_version,library_id,root_id,path_key,kind,
                presence_state,publication_state,
                EXISTS(SELECT 1 FROM library_file_units u WHERE u.file_id=f.file_id
                       AND u.season=?2 AND u.episode>=0)
         FROM library_files f WHERE file_id=?1",
            params![member.ledger_id, model.season],
            |r| {
                Ok(ModelMemberFile {
                    media: r.get(0)?,
                    source_version: r.get(1)?,
                    library: r.get(2)?,
                    root: r.get(3)?,
                    path: r.get(4)?,
                    kind: r.get(5)?,
                    presence: r.get(6)?,
                    publication: r.get(7)?,
                    covers_season: r.get(8)?,
                })
            },
        )
        .optional()?;
    let Some(file) = current else {
        return Err(rejected_model(model, "member file is missing"));
    };
    if file.media != model.media_id
        || file.source_version != member.source_version
        || file.presence != "present"
        || file.publication != "published"
        || !file.covers_season
    {
        return Err(rejected_model(
            model,
            "member Media, source, publication or coverage changed",
        ));
    }
    crate::library_write::publish::ensure_library_writable(tx, &file.library)?;
    let kind = file
        .kind
        .parse::<domain::MediaKind>()
        .map_err(StoreError::Invalid)?;
    let owner =
        crate::library_key_owner::owning_root_key(tx, std::path::Path::new(&file.path), kind)?;
    if owner != Some((file.library, file.root)) {
        return Err(rejected_model(model, "member Library root changed"));
    }
    Ok(())
}

fn rejected_model(model: &StoredFingerprintModel, reason: &str) -> StoreError {
    tracing::warn!(model_id=%model.model_id, media_id=%model.media_id, %reason,
        "fingerprint model publication rejected");
    StoreError::Protected(format!("fingerprint model {}: {reason}", model.model_id))
}
