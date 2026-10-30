//! folder 父级：协议给的是目录路径，库内行存的是 `library_roots.root_id`。
//!
//! 作用域层（`selection::scope`）只按可见库的 `root_paths` 认路径，这里把路径换成
//! 谓词要用的 `root_id`。路径不在任何 root 上时返回 `None`，调用方落空页——绝不
//! 退化成整库。

use rusqlite::OptionalExtension;

use crate::{Store, StoreError};

impl Store {
    /// 目录路径 → `library_roots.root_id`（同一路径多库时取最靠前的 root）。
    pub fn protocol_folder_root_id(&self, root_path: &str) -> Result<Option<String>, StoreError> {
        self.with_library_read(|db| db.protocol_folder_root_id(root_path))
    }
}

#[path = "folder/connection.rs"]
mod connection;
