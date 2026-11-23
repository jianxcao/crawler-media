//! Early request validation; the Subscribe writer separately enforces deletion fences.
use std::str::FromStr;

use axum::{http::StatusCode, response::Response};
use domain::{LibraryId, MediaKind, UserId};

use crate::{Store, http::err};

pub(super) fn resolve(
    store: &Store,
    raw: Option<&str>,
    kind: MediaKind,
    user_id: UserId,
) -> Result<Option<LibraryId>, Response> {
    let is_admin = store.user_role(user_id).map_err(store_error)? == "admin";
    let Some(raw) = raw.filter(|value| !value.is_empty()) else {
        if let Some(default_lib) = store.default_library(kind).map_err(store_error)?
            && !crate::http::routing_preview::user_can_target_library(
                &default_lib,
                Some(user_id),
                is_admin,
            )
        {
            return Err(invalid("指定的媒体库不存在"));
        }
        return Ok(None);
    };
    let id = LibraryId::from_str(raw).map_err(|_| invalid("library_id 无效"))?;
    let library = store
        .get_library(raw)
        .map_err(store_error)?
        .filter(|lib| {
            crate::http::routing_preview::user_can_target_library(lib, Some(user_id), is_admin)
        })
        .ok_or_else(|| invalid("指定的媒体库不存在"))?;
    if library.kind != kind {
        return Err(invalid("订阅媒体类型与目标库类型不匹配"));
    }
    if !store.library_accepts_work(raw).map_err(store_error)? {
        return Err(err(
            StatusCode::CONFLICT,
            "subscription.library_unavailable",
            "目标媒体库正在删除或重建",
        ));
    }
    Ok(Some(id))
}

fn invalid(message: &str) -> Response {
    err(StatusCode::BAD_REQUEST, "subscription.invalid", message)
}

fn store_error(error: store::StoreError) -> Response {
    tracing::error!(%error, "Subscribe target Library validation failed");
    err(
        StatusCode::INTERNAL_SERVER_ERROR,
        "store.error",
        &error.to_string(),
    )
}
