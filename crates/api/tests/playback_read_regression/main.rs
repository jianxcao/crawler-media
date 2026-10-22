//! B3 播放读路径批量化回归：collection 真实收藏态、favorites 排序键、activity 字段、
//! 以及同一 Media 跨多个 Library 时 `library_id` 仍为 null。

#[path = "../support/mod.rs"]
mod support;

mod common;
mod favorites;
mod activity_and_stream;
