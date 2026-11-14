//! Browser concurrency and rendering tests (`indexer` crate).
//!
//! Split by scenario so each module stays readable: configuration and validation,
//! single-page rendering, and concurrency/queueing. The shared fixtures live in
//! [`support`].

mod concurrency;
mod config;
mod render;
mod support;
