//! JSON body extractor whose every rejection (syntax, type, unknown field) is a 400
//! `request.invalid`: request contracts reject obsolete input explicitly.

use axum::extract::{FromRequest, Request};
use axum::http::StatusCode;
use axum::response::Response;
use serde::de::DeserializeOwned;

pub(crate) struct StrictJson<T>(pub T);

impl<T, S> FromRequest<S> for StrictJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(crate::http::err(
                StatusCode::BAD_REQUEST,
                "request.invalid",
                &rejection.body_text(),
            )),
        }
    }
}
