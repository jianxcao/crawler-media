use crate::{Store, StoreError};

impl Store {
    pub fn put_setting(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.with_app(|app| app.put_setting(key, value))
    }

    pub fn delete_setting(&self, key: &str) -> Result<bool, StoreError> {
        self.with_app(|app| app.delete_setting(key))
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        self.with_app_read(|app| app.get_setting(key))
    }

    /// 保存一次搜索的结果快照（按 vertical 独立，保留最近 50 条）。
    pub fn insert_search_snapshot(
        &self,
        id: &str,
        vertical: &str,
        query: &str,
        payload: &str,
        now: i64,
    ) -> Result<(), StoreError> {
        self.with_app(|app| app.insert_search_snapshot(id, vertical, query, payload, now))
    }

    /// `(vertical, query, payload, created_at)`。
    pub fn get_search_snapshot(
        &self,
        id: &str,
    ) -> Result<Option<(String, String, String, i64)>, StoreError> {
        self.with_app_read(|app| app.get_search_snapshot(id))
    }

    pub fn delete_search_snapshot(&self, id: &str) -> Result<bool, StoreError> {
        self.with_app(|app| app.delete_search_snapshot(id))
    }

    pub fn clear_search_snapshots(&self) -> Result<(), StoreError> {
        self.with_app(|app| app.clear_search_snapshots())
    }
}
