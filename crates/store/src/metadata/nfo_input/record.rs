//! Publishing observed NFO inputs inside the write transaction.

use domain::{FileVersion, LedgerId, LibraryFile, LibraryId, MediaId, MetadataFields};
use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;

use super::{NfoObservation, NfoParseState};

/// 落库一份观察只需要文件的归属键，不需要整份 [`LibraryFile`]。
///
/// 已有文件的 rescan 无法凭空造出 `LibraryFile`（也没有按路径读回整行的 API），
/// 因此观察落库统一走这个 target：新入库路径用 [`NfoInputTarget::from_file`]，
/// rescan 路径用 [`crate::Store::reconcile_nfo_inputs`] 从库里读出的列。
#[derive(Clone, Debug)]
pub struct NfoInputTarget {
    pub library_id: LibraryId,
    pub media_id: MediaId,
    pub root_id: String,
    /// 绝对值路径，与 `library_files.path_key` 同一口径。
    pub path_key: String,
    pub file_id: LedgerId,
    pub version: FileVersion,
}

impl NfoInputTarget {
    pub fn from_file(file: &LibraryFile) -> Self {
        Self {
            library_id: file.library_id,
            media_id: file.media_id,
            root_id: file.root_id.clone(),
            path_key: crate::path_key::normalize_path_key(&file.path.to_string_lossy()),
            file_id: file.file_id,
            version: file.version.clone(),
        }
    }

    /// Rewrite the video key through the same root-alias snapshot the sidecar uses.
    pub fn with_canonical_path(mut self, conn: &Connection) -> Self {
        self.path_key = crate::path_key::canonical_path_key(
            conn,
            &crate::path_key::canonical_existing_key(&self.path_key),
        );
        self
    }
}

/// Publish one file's observations; returns false for stale ones (caller reobserves).
///
/// 所有观察先一起过 CAS 校验，再逐条落库，保证「要么整份文件的一批输入都按本次观察更新，
/// 要么一条都不动」。`Invalid` / `Unknown` 不写也不删：解析不了的 NFO 保留上一份有效输入，
/// 由 scrape/adopted 质量更高的来源继续生效。
pub fn record_nfo_observations(
    tx: &Connection,
    target: &NfoInputTarget,
    observations: &[NfoObservation],
) -> Result<bool, StoreError> {
    for observation in observations {
        if !observation_is_current(tx, target, observation)? {
            return Ok(false);
        }
    }
    for observation in observations {
        match observation.state {
            NfoParseState::Valid => match bound_locale(tx, target, &observation.input_key)? {
                // Import binding: a sidecar keeps the locale it was first recorded in.
                Some(locale) if locale != observation.locale => {
                    let bound = NfoObservation {
                        locale,
                        ..observation.clone()
                    };
                    upsert_valid_input(tx, target, &bound)?;
                }
                _ => upsert_valid_input(tx, target, observation)?,
            },
            NfoParseState::Missing => {
                // sidecar 已消失：解除它留下的所有输入（含旧 scope / 旧 locale 的行）。
                tx.execute(
                    "DELETE FROM library_metadata_inputs
                     WHERE library_id = ?1 AND media_id = ?2 AND input_key = ?3",
                    params![
                        target.library_id.to_string(),
                        target.media_id.to_string(),
                        observation.input_key
                    ],
                )?;
            }
            NfoParseState::Invalid | NfoParseState::Unknown => {}
        }
    }
    Ok(true)
}

/// Locale a registered sidecar was imported in; `None` for a first-seen sidecar, which
/// then adopts the observation's (current display) locale.
fn bound_locale(
    tx: &Connection,
    target: &NfoInputTarget,
    input_key: &str,
) -> Result<Option<String>, StoreError> {
    tx.prepare_cached(
        "SELECT locale FROM library_metadata_inputs
         WHERE library_id = ?1 AND media_id = ?2 AND input_key = ?3 LIMIT 1",
    )?
    .query_row(
        params![
            target.library_id.to_string(),
            target.media_id.to_string(),
            input_key
        ],
        |row| row.get(0),
    )
    .optional()
    .map_err(Into::into)
}

/// The observation is only publishable while the file source and Library config still match.
fn observation_is_current(
    tx: &Connection,
    target: &NfoInputTarget,
    observation: &NfoObservation,
) -> Result<bool, StoreError> {
    if target.version.source_version != observation.expected_source_version {
        return Ok(false);
    }
    let config_revision: Option<i64> = tx
        .query_row(
            "SELECT config_revision FROM libraries WHERE library_id = ?1",
            params![target.library_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if config_revision != Some(observation.expected_config_revision) {
        return Ok(false);
    }
    let current_source: Option<String> = tx
        .query_row(
            "SELECT source_version FROM library_files WHERE file_id = ?1 AND library_id = ?2",
            params![target.file_id.to_string(), target.library_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    Ok(current_source.as_deref() == Some(observation.expected_source_version.as_str()))
}

/// Upsert one sidecar's valid fields under its exact entity + locale + input_key.
fn upsert_valid_input(
    tx: &Connection,
    target: &NfoInputTarget,
    observation: &NfoObservation,
) -> Result<(), StoreError> {
    let root_exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM library_roots WHERE root_id = ?1 AND library_id = ?2)",
        params![target.root_id, target.library_id.to_string()],
        |row| row.get(0),
    )?;
    if !root_exists {
        return Err(StoreError::Protected(format!(
            "NFO input outside a known Library root for {}",
            observation.input_key
        )));
    }
    let (entity, season, episode) = (
        observation.scope.as_str(),
        observation.scope.season_episode().0,
        observation.scope.season_episode().1,
    );
    // 一份 sidecar 只维护自己那一层、自己那一种语言：换了实体层（例如改成
    // `<episodedetails>`）时旧行必须让位。语言在首次导入时绑定（复核沿用已登记 locale），
    // 切换 display locale 不会走到这里改写语言。
    tx.execute(
        "DELETE FROM library_metadata_inputs
         WHERE library_id = ?1 AND media_id = ?2 AND input_key = ?3
           AND NOT (locale = ?4 AND entity_scope = ?5 AND season = ?6 AND episode = ?7)",
        params![
            target.library_id.to_string(),
            target.media_id.to_string(),
            observation.input_key,
            observation.locale,
            entity,
            season,
            episode
        ],
    )?;
    let fields_json = serde_json::to_string(&observation.fields)?;
    let present_json = serde_json::to_string(&present_field_names(&observation.fields)?)?;
    // 每次扫描都会重放同一份 sidecar：内容一个字节都没变就别再写一次，
    // 否则同一集/同一部片每次扫描都升 `revision` 并触发一次卡片重算。
    if input_row_matches(
        tx,
        target,
        observation,
        entity,
        (season, episode),
        &fields_json,
        &present_json,
    )? {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO library_metadata_inputs
            (library_id,media_id,entity_scope,season,episode,locale,input_key,root_id,path_key,
             parse_state,source_version,fields_json,present_fields_json,provenance_json,revision)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,'valid','nfo_v1',?10,?11,?12,1)
         ON CONFLICT(library_id,media_id,entity_scope,season,episode,locale,input_key)
         DO UPDATE SET root_id=excluded.root_id,path_key=excluded.path_key,parse_state='valid',
             source_version=excluded.source_version,fields_json=excluded.fields_json,
             present_fields_json=excluded.present_fields_json,provenance_json=excluded.provenance_json,
             revision=library_metadata_inputs.revision+1",
        params![
            target.library_id.to_string(),
            target.media_id.to_string(),
            entity,
            season,
            episode,
            observation.locale,
            observation.input_key,
            target.root_id,
            target.path_key,
            fields_json,
            present_json,
            observation.provenance_json
        ],
    )?;
    Ok(())
}

/// 已有精确同键行且内容完全一致时，本次观察不需要写库。
#[allow(clippy::too_many_arguments)]
fn input_row_matches(
    tx: &Connection,
    target: &NfoInputTarget,
    observation: &NfoObservation,
    entity: &str,
    (season, episode): (i32, i32),
    fields_json: &str,
    present_json: &str,
) -> Result<bool, StoreError> {
    let existing: Option<(String, String, String, String, String, String)> = tx
        .query_row(
            "SELECT root_id, path_key, parse_state, source_version, fields_json,
                    present_fields_json, provenance_json
             FROM library_metadata_inputs
             WHERE library_id = ?1 AND media_id = ?2 AND entity_scope = ?3
               AND season = ?4 AND episode = ?5 AND locale = ?6 AND input_key = ?7",
            params![
                target.library_id.to_string(),
                target.media_id.to_string(),
                entity,
                season,
                episode,
                observation.locale,
                observation.input_key
            ],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((root_id, path_key, state, source_version, stored_fields, stored_present)) = existing
    else {
        return Ok(false);
    };
    Ok(root_id == target.root_id
        && path_key == target.path_key
        && state == "valid"
        && source_version == "nfo_v1"
        && stored_fields == fields_json
        && stored_present == present_json)
}

fn present_field_names(fields: &MetadataFields) -> Result<Vec<String>, StoreError> {
    let object = serde_json::to_value(fields)?;
    Ok(object
        .as_object()
        .map(|object| {
            object
                .iter()
                .filter(|(_, value)| {
                    value.get("state").and_then(|state| state.as_str()) != Some("missing")
                })
                .map(|(name, _)| name.clone())
                .collect()
        })
        .unwrap_or_default())
}
