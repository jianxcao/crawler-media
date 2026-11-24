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
        if let Some(default_lib) = store.default_library(kind).map_err(store_error)? {
            if !crate::http::routing_preview::user_can_auto_route_library(
                &default_lib,
                Some(user_id),
                is_admin,
            ) {
                return Err(invalid("指定的媒体库不存在"));
            }
            if !store
                .library_accepts_work(&default_lib.id)
                .map_err(store_error)?
            {
                return Err(err(
                    StatusCode::CONFLICT,
                    "subscription.library_unavailable",
                    "目标媒体库正在删除或重建",
                ));
            }
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
    match error {
        store::StoreError::ReadBusy
        | store::StoreError::ReadClosed
        | store::StoreError::ReadDeadline
        | store::StoreError::ReadCancelled
        | store::StoreError::WriteBusy => crate::http::store_wait::busy_response(),
        error => {
            tracing::error!(%error, "Subscribe target Library validation failed");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store.error",
                &error.to_string(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_maps_busy_and_deadline_store_errors_to_503_retry_after() {
        for err_variant in [
            store::StoreError::ReadBusy,
            store::StoreError::ReadDeadline,
            store::StoreError::WriteBusy,
        ] {
            let resp = store_error(err_variant);
            assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(
                resp.headers()
                    .get(axum::http::header::RETRY_AFTER)
                    .and_then(|v| v.to_str().ok()),
                Some("1")
            );
        }

        let tmp = tempfile::tempdir().unwrap();
        let store = Store::open(tmp.path().join("data")).unwrap();
        let user_id = UserId::new();
        store
            .insert_user_with_password(
                &domain::User {
                    id: user_id,
                    login: "admin".into(),
                    enabled: true,
                    role: domain::UserRole::Admin,
                },
                "secret",
            )
            .unwrap();
        store.reader_pool(store::Db::Library).close();
        let resp = resolve(&store, None, MediaKind::Tv, user_id).unwrap_err();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
