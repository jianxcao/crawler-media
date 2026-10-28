use crate::{ApiState, http::err};
use axum::http::StatusCode;
use axum::response::Response;
use parking_lot::Mutex;
use store::{Store, StoreError};

pub(super) enum Error {
    Store(StoreError),
    Admission(Response),
}
impl From<StoreError> for Error {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::Store(StoreError::Io(error))
    }
}
impl Error {
    pub fn response(self) -> Response {
        match self {
            Self::Admission(response) => response,
            Self::Store(StoreError::Invalid(message)) => {
                err(StatusCode::BAD_REQUEST, "library.path_outside", &message)
            }
            Self::Store(StoreError::Protected(message)) => {
                err(StatusCode::CONFLICT, "library.write_conflict", &message)
            }
            Self::Store(error) => {
                tracing::error!(%error, "Claim publication failed");
                err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "store.error",
                    &error.to_string(),
                )
            }
        }
    }
}

pub(super) struct Access<'a> {
    pub shared: &'a Mutex<Store>,
    pub state: Option<&'a ApiState>,
}
impl Access<'_> {
    pub fn with_store<T>(
        &self,
        work: impl FnOnce(&Store) -> Result<T, StoreError>,
    ) -> Result<T, Error> {
        let Some(store) = self
            .shared
            .try_lock_for(crate::http::store_wait::STORE_WRITE_WAIT)
        else {
            return Err(Error::Admission(crate::http::store_wait::busy_response()));
        };
        if let Some(state) = self.state {
            state
                .require_writable()
                .and_then(|_| state.require_write_admitted())
                .map_err(Error::Admission)?;
        }
        work(&store).map_err(Error::Store)
    }
}
