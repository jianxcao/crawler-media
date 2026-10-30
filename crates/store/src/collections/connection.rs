//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn list_collections(
        &self,
        user_id: domain::UserId,
    ) -> Result<Vec<UserCollection>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT id, name, sort_order FROM collections WHERE user_id = ?1
             ORDER BY sort_order, created_at",
        )?;
        let rows = stmt.query_map(params![user_id.to_string()], |row| {
            Ok(UserCollection {
                id: row.get(0)?,
                name: row.get(1)?,
                sort_order: row.get(2)?,
            })
        })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn collection_owned_by(
        &self,
        collection_id: &str,
        user_id: domain::UserId,
    ) -> Result<bool, StoreError> {
        self.library
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM collections WHERE id = ?1 AND user_id = ?2)",
                params![collection_id, user_id.to_string()],
                |row| row.get(0),
            )
            .map_err(Into::into)
    }

    pub fn create_collection(
        &self,
        user_id: domain::UserId,
        name: &str,
    ) -> Result<String, StoreError> {
        let id = domain::CollectionId::new().to_string();
        let max: i64 = self
            .library
            .query_row(
                "SELECT COALESCE(MAX(sort_order), 0) FROM collections WHERE user_id = ?1",
                params![user_id.to_string()],
                |row| row.get(0),
            )
            .unwrap_or(0);
        self.library.execute(
            "INSERT INTO collections (id, user_id, visibility, name, sort_order, created_at)
             VALUES (?1, ?2, 'private', ?3, ?4, ?5)",
            params![id, user_id.to_string(), name, max + 1, now()],
        )?;
        Ok(id)
    }

    pub fn rename_collection(&self, id: &str, name: &str) -> Result<bool, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let affected = collection_media(&tx, id)?;
        for media in &affected {
            crate::library_operations::integrity_guard::ensure_media_integrity_writable(
                &tx, *media,
            )?;
        }
        let n = tx.execute(
            "UPDATE collections SET name = ?2 WHERE id = ?1",
            params![id, name],
        )?;
        for media in affected {
            crate::library_write::reduce::reduce_media_everywhere(&tx, media)?;
        }
        tx.commit()?;
        Ok(n > 0)
    }

    pub fn delete_collection(&self, id: &str) -> Result<bool, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let affected = collection_media(&tx, id)?;
        for media in &affected {
            crate::library_operations::integrity_guard::ensure_media_integrity_writable(
                &tx, *media,
            )?;
        }
        tx.execute(
            "DELETE FROM collection_items WHERE collection_id = ?1",
            params![id],
        )?;
        let n = tx.execute("DELETE FROM collections WHERE id = ?1", params![id])?;
        for media in affected {
            crate::library_write::reduce::reduce_media_everywhere(&tx, media)?;
        }
        tx.commit()?;
        Ok(n > 0)
    }

    pub fn collection_item_ids(&self, collection_id: &str) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT media_id FROM collection_items WHERE collection_id = ?1
             ORDER BY sort_order, added_at",
        )?;
        let rows = stmt.query_map(params![collection_id], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn add_collection_item(
        &self,
        collection_id: &str,
        media_item_id: &str,
    ) -> Result<bool, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let affected = vec![media_item_id.parse::<domain::MediaId>()?];
        for media in &affected {
            crate::library_operations::integrity_guard::ensure_media_integrity_writable(
                &tx, *media,
            )?;
        }
        let max: i64 = tx
            .query_row(
                "SELECT COALESCE(MAX(sort_order), 0) FROM collection_items WHERE collection_id = ?1",
                params![collection_id],
                |row| row.get(0),
            )
            .unwrap_or(0);
        let n = tx.execute(
            "INSERT OR IGNORE INTO collection_items (collection_id, media_id, sort_order, added_at)
             VALUES (?1, ?2, ?3, ?4)",
            params![collection_id, media_item_id, max + 1, now()],
        )?;
        for media in affected {
            crate::library_write::reduce::reduce_media_everywhere(&tx, media)?;
        }
        tx.commit()?;
        Ok(n > 0)
    }

    pub fn remove_collection_item(
        &self,
        collection_id: &str,
        media_item_id: &str,
    ) -> Result<bool, StoreError> {
        let tx = crate::write_tx(self.library)?;
        let affected = vec![media_item_id.parse::<domain::MediaId>()?];
        for media in &affected {
            crate::library_operations::integrity_guard::ensure_media_integrity_writable(
                &tx, *media,
            )?;
        }
        let n = tx.execute(
            "DELETE FROM collection_items WHERE collection_id = ?1 AND media_id = ?2",
            params![collection_id, media_item_id],
        )?;
        for media in affected {
            crate::library_write::reduce::reduce_media_everywhere(&tx, media)?;
        }
        tx.commit()?;
        Ok(n > 0)
    }

    pub fn reorder_collection_items(
        &self,
        collection_id: &str,
        media_item_ids: &[String],
    ) -> Result<(), StoreError> {
        let tx = crate::write_tx(self.library)?;
        let affected = collection_media(&tx, collection_id)?;
        for media in &affected {
            crate::library_operations::integrity_guard::ensure_media_integrity_writable(
                &tx, *media,
            )?;
        }
        for (index, media_item_id) in media_item_ids.iter().enumerate() {
            tx.execute(
                "UPDATE collection_items SET sort_order = ?3
                 WHERE collection_id = ?1 AND media_id = ?2",
                params![collection_id, media_item_id, index as i64],
            )?;
        }
        for media in affected {
            crate::library_write::reduce::reduce_media_everywhere(&tx, media)?;
        }
        tx.commit()?;
        Ok(())
    }
}
