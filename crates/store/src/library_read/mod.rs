mod budget;
mod context;
mod lease;
mod pool;
mod query;

pub(crate) use budget::{WriterQueryPolicy, active_query_budget, check_writer_budget};
pub(crate) use context::authorized_ids_on;
pub use context::{LibraryReadContext, LibraryReadRequest, ReadCredential};
pub use lease::{LibraryReadLease, QueryBudget};
pub use pool::LibraryReadPool;
pub use query::LibraryReadResult;

#[derive(Debug, thiserror::Error)]
pub enum LibraryReadError {
    #[error("library read pool is busy")]
    Busy,
    #[error("library read pool is closed")]
    Closed,
    #[error("library read deadline expired")]
    Deadline,
    #[error("library read was cancelled")]
    Cancelled,
    #[error(transparent)]
    Store(#[from] crate::StoreError),
    #[error("library read context changed")]
    ContextChanged,
    #[error("unauthorized library read")]
    Unauthorized,
    #[error("nested Store query budget stage")]
    NestedBudget,
}
