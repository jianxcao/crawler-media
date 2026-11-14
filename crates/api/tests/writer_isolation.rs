//! Per-database writer isolation and concurrent Subscribe updates through public APIs.
#[path = "writer_isolation/cache.rs"]
mod cache;
#[path = "writer_isolation/jobs.rs"]
mod jobs;
#[path = "writer_isolation/support.rs"]
mod support;
#[path = "writer_isolation/transfer.rs"]
mod transfer;

#[path = "writer_isolation/completion.rs"]
mod completion;

#[path = "writer_isolation/pool_lifetime.rs"]
mod pool_lifetime;

#[path = "writer_isolation/manual.rs"]
mod manual;

#[path = "writer_isolation/catalog.rs"]
mod catalog;
