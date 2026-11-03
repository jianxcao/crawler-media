//! 用户自建收藏合集：collections + collection_items。

use rusqlite::params;

use super::Store;
use super::StoreError;

mod cursor;
mod deletion;
mod facts;
mod reads;
pub use reads::{CollectionItemFacts, CollectionItemPage, CollectionRead};

pub struct UserCollection {
    pub id: String,
    pub name: String,
    pub sort_order: i64,
}

fn now() -> Result<i64, StoreError> {
    crate::time::unix_now_ms()
}

impl Store {
    pub fn list_collections(
        &self,
        user_id: domain::UserId,
    ) -> Result<Vec<UserCollection>, StoreError> {
        self.with_library_read(|db| db.list_collections(user_id))
    }

    pub fn collection_owned_by(
        &self,
        collection_id: &str,
        user_id: domain::UserId,
    ) -> Result<bool, StoreError> {
        self.with_library_read(|db| db.collection_owned_by(collection_id, user_id))
    }

    pub fn create_collection(
        &self,
        user_id: domain::UserId,
        name: &str,
    ) -> Result<String, StoreError> {
        self.with_library_write(|db| db.create_collection(user_id, name))
    }

    pub fn rename_collection(&self, id: &str, name: &str) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.rename_collection(id, name))
    }

    pub fn delete_collection(&self, id: &str) -> Result<bool, StoreError> {
        let exists = self.with_writer(crate::Db::Library, |conn| {
            Ok(conn.execute("UPDATE collections SET deleting=1 WHERE id=?1", [id])? > 0)
        })?;
        if !exists {
            return Ok(false);
        }
        while !self.advance_collection_deletion(id)? {}
        Ok(true)
    }

    pub fn collection_item_ids(&self, collection_id: &str) -> Result<Vec<String>, StoreError> {
        self.with_library_read(|db| db.collection_item_ids(collection_id))
    }

    pub fn add_collection_item(
        &self,
        collection_id: &str,
        media_item_id: &str,
    ) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.add_collection_item(collection_id, media_item_id))
    }

    pub fn remove_collection_item(
        &self,
        collection_id: &str,
        media_item_id: &str,
    ) -> Result<bool, StoreError> {
        self.with_library_write(|db| db.remove_collection_item(collection_id, media_item_id))
    }

    pub fn reorder_collection_items(
        &self,
        collection_id: &str,
        media_item_ids: &[String],
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.reorder_collection_items(collection_id, media_item_ids))
    }
}

fn collection_media(
    tx: &rusqlite::Connection,
    id: &str,
) -> Result<Vec<domain::MediaId>, StoreError> {
    let mut stmt = tx.prepare("SELECT media_id FROM collection_items WHERE collection_id=?1")?;
    stmt.query_map([id], |r| r.get::<_, String>(0))?
        .map(|row| Ok(row?.parse()?))
        .collect()
}

#[path = "collections/connection.rs"]
mod connection;
