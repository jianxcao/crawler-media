use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::store::StoreError;

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

impl ApiError {
    pub(crate) fn with(status: StatusCode, code: &'static str, message: String) -> Self {
        Self {
            status,
            code,
            message,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn internal(message: String) -> Self {
        Self::with(StatusCode::INTERNAL_SERVER_ERROR, "internal.error", message)
    }

    pub(crate) fn invalid(code: &'static str, message: String) -> Self {
        Self::with(StatusCode::BAD_REQUEST, code, message)
    }

    pub(crate) fn missing(code: &'static str, message: String) -> Self {
        Self::with(StatusCode::NOT_FOUND, code, message)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(json!({
                "ok": false,
                "error": {
                    "code": self.code,
                    "message": self.message,
                }
            })),
        )
            .into_response()
    }
}

impl From<StoreError> for ApiError {
    fn from(error: StoreError) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "store.error",
            message: error.to_string(),
        }
    }
}

impl From<subscribe::SubscribeError> for ApiError {
    fn from(error: subscribe::SubscribeError) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            code: "upstream.subscribe_error",
            message: error.to_string(),
        }
    }
}
