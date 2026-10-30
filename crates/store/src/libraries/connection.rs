//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn list_libraries(&self) -> Result<Vec<Library>, StoreError> {
        self.load_libraries()
    }

    pub fn library_ownership(&self) -> Result<LibraryOwnership, StoreError> {
        let libraries = self.load_libraries()?;
        let blocked = self.blocked_library_ids()?;
        Ok(LibraryOwnership::from_parts(libraries, blocked))
    }

    pub(super) fn load_libraries(&self) -> Result<Vec<Library>, StoreError> {
        let mut roots = self.roots_by_library()?;
        let mut stmt = self.library.prepare(&format!(
            "SELECT {LIBRARY_COLS} FROM libraries ORDER BY sort_order, name"
        ))?;
        let rows = stmt.query_map([], map_library_row)?;
        let mut libraries = Vec::new();
        for row in rows {
            let row = row?;
            let root_paths = roots.remove(&row.id).unwrap_or_default();
            libraries.push(row.into_library(root_paths)?);
        }
        Ok(libraries)
    }

    pub(super) fn roots_by_library(&self) -> Result<HashMap<String, Vec<PathBuf>>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT library_id, display_path FROM library_roots
             ORDER BY sort_order, display_path",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                PathBuf::from(row.get::<_, String>(1)?),
            ))
        })?;
        let mut roots: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for row in rows {
            let (library_id, path) = row?;
            roots.entry(library_id).or_default().push(path);
        }
        Ok(roots)
    }

    pub(super) fn blocked_library_ids(&self) -> Result<HashSet<String>, StoreError> {
        let mut stmt = self
            .library
            .prepare("SELECT library_id FROM library_removals")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut blocked = HashSet::new();
        for row in rows {
            blocked.insert(row?);
        }
        Ok(blocked)
    }

    pub fn get_library(&self, id: &str) -> Result<Option<Library>, StoreError> {
        let row = self
            .library
            .query_row(
                &format!("SELECT {LIBRARY_COLS} FROM libraries WHERE library_id = ?1"),
                params![id],
                map_library_row,
            )
            .optional()?;
        let Some(row) = row else { return Ok(None) };
        let root_paths = self.roots_of(id)?;
        Ok(Some(row.into_library(root_paths)?))
    }

    pub fn library_config_revision(&self, id: &str) -> Result<i64, StoreError> {
        let revision: Option<i64> = self
            .library
            .query_row(
                "SELECT config_revision FROM libraries WHERE library_id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        revision.ok_or_else(|| StoreError::Missing(format!("library {id}")))
    }

    pub fn default_library(&self, kind: MediaKind) -> Result<Option<Library>, StoreError> {
        let row = self
            .library
            .query_row(
                &format!(
                    "SELECT {LIBRARY_COLS} FROM libraries
                     WHERE kind = ?1 AND is_default = 1 ORDER BY sort_order, name LIMIT 1"
                ),
                params![kind.as_str()],
                map_library_row,
            )
            .optional()?;
        let Some(row) = row else { return Ok(None) };
        let root_paths = self.roots_of(&row.id)?;
        Ok(Some(row.into_library(root_paths)?))
    }

    pub(crate) fn roots_of(&self, library_id: &str) -> Result<Vec<PathBuf>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT display_path FROM library_roots WHERE library_id = ?1
             ORDER BY sort_order, display_path",
        )?;
        let rows = stmt.query_map(params![library_id], |row| row.get::<_, String>(0))?;
        let mut paths = Vec::new();
        for row in rows {
            paths.push(PathBuf::from(row?));
        }
        Ok(paths)
    }

    pub fn create_library_full_prepared(
        &self,
        kind: MediaKind,
        name: &str,
        roots: &PreparedLibraryRoots,
        access_mode: &str,
        admin_visible: bool,
        member_ids: &[domain::UserId],
        match_rules: &[serde_json::Value],
        default_filter_id: Option<&str>,
    ) -> Result<Library, StoreError> {
        self.ensure_removal_keys_available(roots.entries.iter().map(|(_, key)| key.as_str()))?;
        let id = uuid::Uuid::new_v4().to_string();
        let sort_order = self.next_sort_order()?;
        let has_default: bool = self
            .library
            .query_row(
                "SELECT 1 FROM libraries WHERE kind = ?1 AND is_default = 1 LIMIT 1",
                params![kind.as_str()],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        let is_default = if has_default { 0 } else { 1 };
        let member_ids_json = serde_json::to_string(
            &member_ids
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>(),
        )?;
        let match_rules_json = serde_json::to_string(match_rules)?;
        // The library row and its roots commit together: a rejected root must
        // not leave behind a Library the caller never successfully created.
        let tx = crate::write_tx(self.library)?;
        tx.execute(
            "INSERT INTO libraries
                (library_id, kind, name, is_default, sort_order, access_mode, admin_visible, member_ids_json, match_rules_json, default_filter_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                id,
                kind.as_str(),
                name,
                is_default,
                sort_order,
                access_mode,
                admin_visible as i64,
                member_ids_json,
                match_rules_json,
                default_filter_id,
            ],
        )?;
        Store::replace_roots(&tx, &id, roots)?;
        for (_, key) in &roots.entries {
            Store::reassign_files_under_root(&tx, &id, kind, key)?;
        }
        tx.commit()?;
        crate::path_key::reload_root_aliases(self.library)?;
        if let Err(error) = self.drain_one_root_batch() {
            tracing::error!(%error,library_id=id,"Root reassignment queued after a failed initial window");
        }
        self.get_library(&id)?
            .ok_or_else(|| StoreError::Missing(format!("library {id}")))
    }

    pub fn set_library_switch_settings(
        &self,
        id: &str,
        realtime_watch: Option<bool>,
        generate_thumbnails: Option<bool>,
        extract_chapter_images: Option<bool>,
        exclude_from_home: Option<bool>,
        auto_series_collections: Option<bool>,
    ) -> Result<(), StoreError> {
        self.ensure_library_mutable(id)?;
        let Some(current) = self.get_library(id)? else {
            return Err(StoreError::Missing(format!("library {id}")));
        };
        let realtime_watch = realtime_watch.unwrap_or(current.realtime_watch);
        let generate_thumbnails = generate_thumbnails.unwrap_or(current.generate_thumbnails);
        let extract_chapter_images =
            extract_chapter_images.unwrap_or(current.extract_chapter_images);
        let exclude_from_home = exclude_from_home.unwrap_or(current.exclude_from_home);
        let auto_series_collections =
            auto_series_collections.unwrap_or(current.auto_series_collections);
        self.library.execute(
            "UPDATE libraries SET
                realtime_watch = ?1,
                generate_thumbnails = ?2,
                extract_chapter_images = ?3,
                exclude_from_home = ?4,
                auto_series_collections = ?5
             WHERE library_id = ?6",
            params![
                realtime_watch as i64,
                generate_thumbnails as i64,
                extract_chapter_images as i64,
                exclude_from_home as i64,
                auto_series_collections as i64,
                id
            ],
        )?;
        Ok(())
    }

    pub fn set_library_cover_path(
        &self,
        id: &str,
        cover_path: Option<&str>,
    ) -> Result<(), StoreError> {
        self.ensure_library_mutable(id)?;
        self.library.execute(
            "UPDATE libraries SET cover_path = ?1, cover_revision = cover_revision + 1
             WHERE library_id = ?2",
            params![cover_path, id],
        )?;
        Ok(())
    }

    pub(super) fn ensure_recorded_files_stay_under(
        &self,
        library: &Library,
        proposed: &[PathBuf],
    ) -> Result<(), StoreError> {
        // Prepared roots and published file keys share the same canonical
        // identity; display paths can retain a /tmp or symlink spelling.
        let mut stmt = self
            .library
            .prepare("SELECT path_key FROM library_files WHERE library_id = ?1")?;
        let rows = stmt.query_map(params![library.id], |row| row.get::<_, String>(0))?;
        for row in rows {
            let path = PathBuf::from(row?);
            if !proposed.iter().any(|root| path.starts_with(root)) {
                tracing::warn!(library_id = %library.id, path = %path.display(),
                    "根目录改写会让已发布文件落到新的根目录之外");
                return Err(StoreError::Protected(format!(
                    "媒体库 {} 仍有台账文件位于现有根目录；先迁移或清理这些文件，再修改根目录",
                    library.name
                )));
            }
        }
        Ok(())
    }

    pub fn set_default_library(&self, id: &str) -> Result<Library, StoreError> {
        self.ensure_library_mutable(id)?;
        let Some(library) = self.get_library(id)? else {
            return Err(StoreError::Missing(format!("library {id}")));
        };
        for current in self.list_libraries()? {
            if current.kind == library.kind && current.is_default {
                self.ensure_library_mutable(&current.id)?;
            }
        }
        self.library.execute(
            "UPDATE libraries SET is_default = 0 WHERE kind = ?1 AND library_id NOT IN
             (SELECT library_id FROM library_removals WHERE state != 'succeeded')",
            params![library.kind.as_str()],
        )?;
        self.library.execute(
            "UPDATE libraries SET is_default = 1 WHERE library_id = ?1",
            params![id],
        )?;
        self.get_library(id)?
            .ok_or_else(|| StoreError::Missing(format!("library {id}")))
    }

    pub fn reorder_libraries(&self, ids: &[&str]) -> Result<(), StoreError> {
        for id in ids {
            self.ensure_library_mutable(id)?;
        }
        for (index, id) in ids.iter().enumerate() {
            self.library.execute(
                "UPDATE libraries SET sort_order = ?1 WHERE library_id = ?2",
                params![index as i32, id],
            )?;
        }
        Ok(())
    }

    pub(crate) fn next_sort_order(&self) -> Result<i32, StoreError> {
        let max: Option<i32> =
            self.library
                .query_row("SELECT MAX(sort_order) FROM libraries", [], |row| {
                    row.get(0)
                })?;
        Ok(max.map(|v| v + 1).unwrap_or(0))
    }
}

impl crate::LibraryConnection<'_> {
    pub fn update_library_full_prepared(
        &self,
        id: &str,
        name: Option<&str>,
        prepared_roots: Option<&PreparedLibraryRoots>,
        access: Option<(&str, bool, &[domain::UserId])>,
        match_rules: Option<&[serde_json::Value]>,
        default_filter_id: Option<Option<&str>>,
    ) -> Result<Library, StoreError> {
        self.ensure_library_mutable(id)?;
        let Some(library) = self.get_library(id)? else {
            return Err(StoreError::Missing(format!("library {id}")));
        };
        if let Some(roots) = prepared_roots {
            self.ensure_removal_keys_available(roots.entries.iter().map(|(_, key)| key.as_str()))?;
            let proposed: Vec<std::path::PathBuf> =
                roots.entries.iter().map(|(_, key)| key.into()).collect();
            self.ensure_recorded_files_stay_under(&library, &proposed)?;
        }
        // One transaction for the row updates and the root replacement, so a
        // rejected root cannot leave a partially updated Library behind.
        let tx = crate::write_tx(self.library)?;
        if let Some(name) = name
            && !name.trim().is_empty()
        {
            tx.execute(
                "UPDATE libraries SET name = ?1 WHERE library_id = ?2",
                params![name.trim(), id],
            )?;
        }
        if let Some(roots) = &prepared_roots {
            Store::replace_roots(&tx, id, roots)?;
            for (_, key) in &roots.entries {
                Store::reassign_files_under_root(&tx, id, library.kind, key)?;
            }
        }
        if let Some((access_mode, admin_visible, member_ids)) = access {
            tx.execute(
                "UPDATE libraries SET access_mode = ?1, admin_visible = ?2, member_ids_json = ?3
                 WHERE library_id = ?4",
                params![
                    access_mode,
                    admin_visible as i64,
                    serde_json::to_string(
                        &member_ids
                            .iter()
                            .map(|id| id.to_string())
                            .collect::<Vec<_>>()
                    )?,
                    id,
                ],
            )?;
        }
        if let Some(match_rules) = match_rules {
            tx.execute(
                "UPDATE libraries SET match_rules_json = ?1 WHERE library_id = ?2",
                params![serde_json::to_string(match_rules)?, id],
            )?;
        }
        if let Some(filter_id) = default_filter_id {
            tx.execute(
                "UPDATE libraries SET default_filter_id = ?1 WHERE library_id = ?2",
                params![filter_id, id],
            )?;
        }
        tx.commit()?;
        crate::path_key::reload_root_aliases(self.library)?;
        if let Err(error) = self.drain_one_root_batch() {
            tracing::error!(%error,library_id=id,"Root reassignment queued after a failed initial window");
        }
        self.get_library(id)?
            .ok_or_else(|| StoreError::Missing(format!("library {id}")))
    }
    pub fn update_library_settings(
        &self,
        id: &str,
        name: Option<&str>,
        access: Option<(&str, bool, &[domain::UserId])>,
        match_rules: Option<&[serde_json::Value]>,
        default_filter_id: Option<Option<&str>>,
    ) -> Result<Library, StoreError> {
        self.update_library_full_prepared(id, name, None, access, match_rules, default_filter_id)
    }
}
