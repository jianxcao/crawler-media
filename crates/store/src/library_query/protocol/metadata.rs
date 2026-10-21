//! 页内条目的元数据水合：把已经落库的快照读成 [`MetadataFields`]。
//!
//! 只读 Task 8 的观测输入（`library_metadata_inputs`）与 Task 7 的采纳结果
//! （`media_metadata` / `media_metadata_sources`）：不 stat、不解析现场 NFO、不读
//! cache、不访问网络。调用点在协议页选出条目之后，页外条目再多也不会被读。
//!
//! 优先级与 [`Store::library_item_detail`]（Web 浏览用的是同一条）一致：观测输入先生效，
//! 采纳只补观测缺失的字段。季/集作用域在此之上再叠一层 work 作用域兜底，对应 NFO 里
//! 用 `tvshow.nfo` 补分集字段的回退语义。

use crate::library_query::details::metadata::instance_default_locale;
use crate::library_query::details::{fill_from_adopted, library_display_locale};
use crate::{Store, StoreError};
use domain::{EntityScope, LibraryId, MediaId, MetadataFields};
use rusqlite::{OptionalExtension, params};

impl Store {
    /// One page item's metadata, from stored observations and adopted sources only.
    pub fn protocol_page_metadata(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        scope: EntityScope,
        locale: Option<&str>,
    ) -> Result<MetadataFields, StoreError> {
        let mut fields = self.scoped_page_fields(library_id, media_id, scope, locale)?;
        if !matches!(scope, EntityScope::Work) {
            let work = self.scoped_page_fields(library_id, media_id, EntityScope::Work, locale)?;
            overlay_missing(&mut fields, &work);
        }
        Ok(fields)
    }

    /// Observed inputs of one scope, then adopted sources of the same scope filling gaps.
    fn scoped_page_fields(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        scope: EntityScope,
        locale: Option<&str>,
    ) -> Result<MetadataFields, StoreError> {
        let mut fields = self.page_observed_fields(library_id, media_id, scope, locale)?;
        let Some(resolved) =
            self.resolved_metadata_locale(library_id, media_id, locale.unwrap_or(""))?
        else {
            return Ok(fields);
        };
        if matches!(scope, EntityScope::Work) {
            if let Some(adopted) = self.get_effective_metadata(media_id, &resolved)? {
                overlay_adopted_details(&mut fields, &adopted.details_json, media_id);
                fill_from_adopted(&mut fields, &adopted);
            }
        }
        let composed = self.effective_metadata_fields(library_id, media_id, scope, &resolved)?;
        overlay_missing(&mut fields, &composed.fields);
        Ok(fields)
    }

    /// Scrape inputs already stored for this entity scope.
    fn page_observed_fields(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        scope: EntityScope,
        locale: Option<&str>,
    ) -> Result<MetadataFields, StoreError> {
        if matches!(scope, EntityScope::Work) {
            // work 作用域复用 Web 浏览同一条读路径，避免两套 locale 语义。
            return self.metadata_input_fields(library_id, media_id, locale);
        }
        let Some(chosen) = self.scoped_input_locale(library_id, media_id, scope, locale)? else {
            return Ok(MetadataFields::default());
        };
        let (season, episode) = scope.season_episode();
        let fields_json: Option<String> = self
            .library
            .query_row(
                "SELECT fields_json FROM library_metadata_inputs
                 WHERE library_id = ?1 AND media_id = ?2 AND entity_scope = ?3
                   AND season = ?4 AND episode = ?5
                   AND parse_state = 'valid' AND locale = ?6
                 ORDER BY input_key
                 LIMIT 1",
                params![
                    library_id.to_string(),
                    media_id.to_string(),
                    scope.as_str(),
                    season,
                    episode,
                    chosen
                ],
                |row| row.get(0),
            )
            .optional()?;
        match fields_json {
            Some(fields_json) => Ok(serde_json::from_str(&fields_json)?),
            None => Ok(MetadataFields::default()),
        }
    }

    /// Locale of one season/episode scope's inputs: the same request → Library → instance
    /// chain the work-scope reader uses, plus the unbound `''` bucket as last resort.
    fn scoped_input_locale(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        scope: EntityScope,
        requested: Option<&str>,
    ) -> Result<Option<String>, StoreError> {
        let (season, episode) = scope.season_episode();
        let mut statement = self.library.prepare(
            "SELECT DISTINCT locale FROM library_metadata_inputs
             WHERE library_id = ?1 AND media_id = ?2 AND entity_scope = ?3
               AND season = ?4 AND episode = ?5 AND parse_state = 'valid'
             ORDER BY locale",
        )?;
        let locales = statement
            .query_map(
                params![
                    library_id.to_string(),
                    media_id.to_string(),
                    scope.as_str(),
                    season,
                    episode
                ],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        if locales.is_empty() {
            return Ok(None);
        }
        let chain = [
            requested.map(str::to_string),
            library_display_locale(self, library_id)?,
            instance_default_locale(self)?,
        ];
        for candidate in chain.into_iter().flatten() {
            let candidate = candidate.trim();
            if candidate.is_empty() {
                continue;
            }
            if let Some(exact) = locales.iter().find(|stored| stored.as_str() == candidate) {
                return Ok(Some(exact.clone()));
            }
            let prefix = candidate.split(['-', '_']).next().unwrap_or(candidate);
            if let Some(prefix_match) = locales
                .iter()
                .find(|stored| stored.split(['-', '_']).next().unwrap_or(stored) == prefix)
            {
                return Ok(Some(prefix_match.clone()));
            }
        }
        Ok(locales.into_iter().find(|stored| stored.is_empty()))
    }
}

/// Adopted `details_json.fields`: fills only what the observation layer left missing.
fn overlay_adopted_details(base: &mut MetadataFields, details_json: &str, media_id: MediaId) {
    let Ok(details) = serde_json::from_str::<serde_json::Value>(details_json) else {
        tracing::warn!(
            media_id = %media_id,
            "protocol_page_metadata: adopted details_json is not valid JSON, skipped"
        );
        return;
    };
    let Some(fields) = details.get("fields") else {
        return;
    };
    match serde_json::from_value::<MetadataFields>(fields.clone()) {
        Ok(adopted) => overlay_missing(base, &adopted),
        Err(error) => tracing::warn!(
            %error,
            media_id = %media_id,
            "protocol_page_metadata: adopted details_json fields are not MetadataFields, skipped"
        ),
    }
}

/// Fill the base's `missing` fields from the candidate. `empty` is an explicit conclusion
/// (`FieldValue::Empty`) and is never overwritten.
fn overlay_missing(base: &mut MetadataFields, candidate: &MetadataFields) {
    let (Ok(mut base_json), Ok(candidate_json)) = (
        serde_json::to_value(&*base),
        serde_json::to_value(candidate),
    ) else {
        return;
    };
    let (Some(base_fields), Some(candidate_fields)) =
        (base_json.as_object_mut(), candidate_json.as_object())
    else {
        return;
    };
    for (key, value) in candidate_fields {
        if is_missing(base_fields.get(key)) && !is_missing(Some(value)) {
            base_fields.insert(key.clone(), value.clone());
        }
    }
    if let Ok(merged) = serde_json::from_value::<MetadataFields>(base_json) {
        *base = merged;
    }
}

fn is_missing(value: Option<&serde_json::Value>) -> bool {
    value.is_none_or(|value| {
        value.get("state").and_then(serde_json::Value::as_str) == Some("missing")
    })
}
