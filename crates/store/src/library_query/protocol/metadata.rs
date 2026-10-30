//! 页内条目的元数据水合：把已经落库的快照读成 [`MetadataFields`]。
//!
//! 只读 Task 8 的观测输入（`library_metadata_inputs`）与 Task 7 的采纳结果
//! （`media_metadata` / `media_metadata_sources`）：不 stat、不解析现场 NFO、不读
//! cache、不访问网络。调用点在协议页选出条目之后，页外条目再多也不会被读。
//!
//! 优先级与 [`Store::library_item_detail`]（Web 浏览用的是同一条）一致：观测输入先生效，
//! 采纳只补观测缺失的字段。季/集作用域在此之上再叠一层 work 作用域兜底，对应 NFO 里
//! 用 `tvshow.nfo` 补分集字段的回退语义。

use crate::library_query::details::fill_from_adopted;
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
        self.with_library_read(|db| db.protocol_page_metadata(library_id, media_id, scope, locale))
    }
}

/// Adopted `details_json.fields`: fills only what the observation layer left missing.
pub(crate) fn overlay_adopted_details(
    base: &mut MetadataFields,
    details_json: &str,
    media_id: MediaId,
) {
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
pub(crate) fn overlay_missing(base: &mut MetadataFields, candidate: &MetadataFields) {
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

#[path = "metadata/connection.rs"]
mod connection;
