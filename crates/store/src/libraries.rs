//! Library entity: one media library per kind holds multiple ordered roots
//! (first root = primary, where new content lands). The `libraries` table is
//! the display/config surface; `library_roots` rows belong to a library.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;

use domain::MediaKind;
use rusqlite::OptionalExtension;
use rusqlite::params;

use super::library_ownership::{LibraryOwnership, owner_with_default};
use super::{Store, StoreError};
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Library {
    pub id: String,
    pub kind: MediaKind,
    pub name: String,
    /// Ordered root paths; the first entry is the primary root.
    pub root_paths: Vec<PathBuf>,
    pub is_default: bool,
    pub sort_order: i32,
    /// "everyone" | "selected" — member visibility.
    pub access_mode: String,
    /// Whether the library appears in admin browse scope.
    pub admin_visible: bool,
    /// Member ids allowed when access_mode = "selected".
    pub member_ids: Vec<domain::UserId>,
    /// Whether to detect intros/outros from TheIntroDB and embedded chapters.
    pub detect_intros: bool,
    /// Whether to independently run audio fingerprint matching for TV episodes.
    pub enable_fingerprint: bool,
    /// Custom or auto-filled library cover image path.
    pub cover_path: Option<String>,
    /// Cover pointer revision for CAS publication (`cover_path` changes bump it).
    pub cover_revision: i64,
    /// Match rules for automated routing (`genres` and/or `origin_countries`).
    pub match_rules: Vec<serde_json::Value>,
    /// Default filter rule ID associated with this library (inherits for subscriptions).
    pub default_filter_id: Option<String>,
    /// Whether to watch filesystem directory changes in real-time.
    pub realtime_watch: bool,
    /// Whether to generate thumbnail frame from video when missing poster.
    pub generate_thumbnails: bool,
    /// Whether to extract chapter scene images.
    pub extract_chapter_images: bool,
    /// Whether to exclude items of this library from home page rows.
    pub exclude_from_home: bool,
    /// Whether to automatically create collections based on series.
    pub auto_series_collections: bool,
}

const LIBRARY_COLS: &str = "library_id, kind, name, is_default, sort_order, access_mode,
        admin_visible, member_ids_json,
        COALESCE(detect_intros, 0), COALESCE(enable_fingerprint, 0),
        cover_path, COALESCE(match_rules_json, '[]'), default_filter_id,
        COALESCE(realtime_watch, 1), COALESCE(generate_thumbnails, 1),
        COALESCE(extract_chapter_images, 1), COALESCE(exclude_from_home, 0),
        COALESCE(auto_series_collections, 1),
        COALESCE(cover_revision, 0)";

/// Raw `libraries` row; roots are attached separately so list/get/snapshot can
/// share one row mapping.
struct LibraryRow {
    id: String,
    kind: String,
    name: String,
    is_default: bool,
    sort_order: i32,
    access_mode: String,
    admin_visible: bool,
    member_ids: Vec<String>,
    detect_intros: bool,
    enable_fingerprint: bool,
    cover_path: Option<String>,
    match_rules: Vec<serde_json::Value>,
    default_filter_id: Option<String>,
    realtime_watch: bool,
    generate_thumbnails: bool,
    extract_chapter_images: bool,
    exclude_from_home: bool,
    auto_series_collections: bool,
    cover_revision: i64,
}

fn map_library_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<LibraryRow> {
    let member_ids: Vec<String> =
        serde_json::from_str(&row.get::<_, String>(7)?).unwrap_or_default();
    let match_rules: Vec<serde_json::Value> =
        serde_json::from_str(&row.get::<_, String>(11)?).unwrap_or_default();
    Ok(LibraryRow {
        id: row.get(0)?,
        kind: row.get(1)?,
        name: row.get(2)?,
        is_default: row.get::<_, i64>(3)? != 0,
        sort_order: row.get(4)?,
        access_mode: row.get(5)?,
        admin_visible: row.get::<_, i64>(6)? != 0,
        member_ids,
        detect_intros: row.get::<_, i64>(8)? != 0,
        enable_fingerprint: row.get::<_, i64>(9)? != 0,
        cover_path: row.get(10)?,
        match_rules,
        default_filter_id: row.get(12)?,
        realtime_watch: row.get::<_, i64>(13)? != 0,
        generate_thumbnails: row.get::<_, i64>(14)? != 0,
        extract_chapter_images: row.get::<_, i64>(15)? != 0,
        exclude_from_home: row.get::<_, i64>(16)? != 0,
        auto_series_collections: row.get::<_, i64>(17)? != 0,
        cover_revision: row.get(18)?,
    })
}

impl LibraryRow {
    fn into_library(self, root_paths: Vec<PathBuf>) -> Result<Library, StoreError> {
        Ok(Library {
            id: self.id,
            kind: MediaKind::from_str(&self.kind).map_err(StoreError::MediaKind)?,
            name: self.name,
            root_paths,
            is_default: self.is_default,
            sort_order: self.sort_order,
            access_mode: self.access_mode,
            admin_visible: self.admin_visible,
            member_ids: self
                .member_ids
                .into_iter()
                .filter_map(|id| domain::UserId::from_str(&id).ok())
                .collect(),
            detect_intros: self.detect_intros,
            enable_fingerprint: self.enable_fingerprint,
            cover_path: self.cover_path,
            cover_revision: self.cover_revision,
            match_rules: self.match_rules,
            default_filter_id: self.default_filter_id,
            realtime_watch: self.realtime_watch,
            generate_thumbnails: self.generate_thumbnails,
            extract_chapter_images: self.extract_chapter_images,
            exclude_from_home: self.exclude_from_home,
            auto_series_collections: self.auto_series_collections,
        })
    }
}

impl Store {
    pub fn list_libraries(&self) -> Result<Vec<Library>, StoreError> {
        self.load_libraries()
    }

    pub fn library_ownership(&self) -> Result<LibraryOwnership, StoreError> {
        let libraries = self.load_libraries()?;
        let blocked = self.blocked_library_ids()?;
        Ok(LibraryOwnership::from_parts(libraries, blocked))
    }
    /// Determine authorized libraries for a user according to their role and library access settings.
    pub fn authorized_library_ids(&self, user_id: domain::UserId) -> Result<HashSet<String>, StoreError> {
        let user_id_str = user_id.to_string();
        let role = self.user_role(user_id)?;
        let is_admin = role == "admin";

        let mut auth_stmt = self.library.prepare(
            "SELECT library_id, access_mode, admin_visible, member_ids_json
             FROM libraries WHERE lifecycle_state = 'ready'",
        )?;
        let mut allowed_libraries: HashSet<String> = HashSet::new();
        let mut auth_rows = auth_stmt.query([])?;
        while let Some(r) = auth_rows.next()? {
            let lid: String = r.get(0)?;
            let mode: String = r.get(1)?;
            let admin_visible: bool = r.get::<_, i64>(2)? != 0;
            let members_json: String = r.get(3)?;
            let is_allowed = if is_admin {
                admin_visible
            } else if mode == "everyone" {
                true
            } else {
                let members: Vec<String> = serde_json::from_str(&members_json).unwrap_or_default();
                members.contains(&user_id_str)
            };
            if is_allowed {
                allowed_libraries.insert(lid);
            }
        }
        Ok(allowed_libraries)
    }

    /// Libraries with their roots attached, via one batch roots query instead
    /// of one query per library.
    fn load_libraries(&self) -> Result<Vec<Library>, StoreError> {
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

    /// Every library's roots in one query. Per-library order stays
    /// `(sort_order, path)`: the global order restricted to one library is
    /// still that order.
    fn roots_by_library(&self) -> Result<HashMap<String, Vec<PathBuf>>, StoreError> {
        let mut stmt = self.library.prepare(
            "SELECT library_id, display_path FROM library_roots
             ORDER BY sort_order, display_path",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, PathBuf::from(row.get::<_, String>(1)?)))
        })?;
        let mut roots: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for row in rows {
            let (library_id, path) = row?;
            roots.entry(library_id).or_default().push(path);
        }
        Ok(roots)
    }

    /// Library ids that have a `library_removals` row, in any state (including
    /// `succeeded`) — matching `library_accepts_work`, which does not filter
    /// on state.
    fn blocked_library_ids(&self) -> Result<HashSet<String>, StoreError> {
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

    /// Current configuration revision of one Library. Writers pin it in their
    /// `WriteScope` so a reconfiguration between plan and write is refused.
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

    /// 把文件路径归属到唯一的媒体库：**最长根目录前缀**获胜（嵌套库不被父库
    /// 抢走）；两条根目录前缀长度相同的候选视为歧义，返回 None（不擅自归属）。
    pub fn library_for_path(
        &self,
        path: &std::path::Path,
        kind: MediaKind,
    ) -> Result<Option<Library>, StoreError> {
        let libraries = self.load_libraries()?;
        Ok(owner_with_default(&libraries, path, kind).cloned())
    }

    /// 严格按根目录前缀匹配媒体库：不回退到默认媒体库。
    pub fn library_for_path_strict(
        &self,
        path: &std::path::Path,
        kind: MediaKind,
    ) -> Result<Option<Library>, StoreError> {
        let libraries = self.list_libraries()?;
        let mut best: Option<(usize, Library)> = None;
        let mut ambiguous = false;
        let norm_path = crate::library_paths::resolved_path(path).map_err(|error| {
            tracing::error!(path = %path.display(), %error, "解析严格 Library 路径失败");
            error
        })?;
        for library in libraries {
            if library.kind != kind {
                continue;
            }
            let mut depth = None;
            for root in &library.root_paths {
                let norm_root = crate::library_paths::resolved_path(root)?;
                if norm_path.starts_with(&norm_root) {
                    depth = Some(depth.unwrap_or(0).max(norm_root.components().count()));
                }
            }
            let Some(depth) = depth else { continue };
            match &best {
                Some((best_depth, _)) if *best_depth > depth => {}
                Some((best_depth, _)) if *best_depth == depth => ambiguous = true,
                _ => {
                    best = Some((depth, library));
                    ambiguous = false;
                }
            }
        }
        if ambiguous {
            Ok(None)
        } else {
            Ok(best.map(|(_, l)| l))
        }
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

    pub fn create_library(
        &self,
        kind: MediaKind,
        name: &str,
        root_paths: &[&str],
        access_mode: &str,
        admin_visible: bool,
        member_ids: &[domain::UserId],
    ) -> Result<Library, StoreError> {
        self.create_library_with_rules(
            kind,
            name,
            root_paths,
            access_mode,
            admin_visible,
            member_ids,
            &[],
        )
    }

    pub fn create_library_with_rules(
        &self,
        kind: MediaKind,
        name: &str,
        root_paths: &[&str],
        access_mode: &str,
        admin_visible: bool,
        member_ids: &[domain::UserId],
        match_rules: &[serde_json::Value],
    ) -> Result<Library, StoreError> {
        self.create_library_full(
            kind,
            name,
            root_paths,
            access_mode,
            admin_visible,
            member_ids,
            match_rules,
            None,
        )
    }

    pub fn create_library_full(
        &self,
        kind: MediaKind,
        name: &str,
        root_paths: &[&str],
        access_mode: &str,
        admin_visible: bool,
        member_ids: &[domain::UserId],
        match_rules: &[serde_json::Value],
        default_filter_id: Option<&str>,
    ) -> Result<Library, StoreError> {
        self.ensure_removal_roots_available(root_paths)?;
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
        let member_ids_json =
            serde_json::to_string(&member_ids.iter().map(|id| id.to_string()).collect::<Vec<_>>())?;
        let match_rules_json = serde_json::to_string(match_rules)?;
        // The library row and its roots commit together: a rejected root must
        // not leave behind a Library the caller never successfully created.
        let tx = self.library.unchecked_transaction()?;
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
        Self::replace_roots(&tx, &id, kind, root_paths)?;
        for root in root_paths {
            Self::reassign_files_under_root(&tx, &id, root)?;
        }
        tx.commit()?;
        Ok(self
            .get_library(&id)?
            .ok_or_else(|| StoreError::Missing(format!("library {id}")))?)
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

    pub fn set_library_intro_settings(
        &self,
        id: &str,
        detect_intros: bool,
        enable_fingerprint: bool,
    ) -> Result<(), StoreError> {
        self.ensure_library_mutable(id)?;
        self.library.execute(
            "UPDATE libraries SET detect_intros = ?1, enable_fingerprint = ?2 WHERE library_id = ?3",
            params![detect_intros as i64, enable_fingerprint as i64, id],
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

    pub fn update_library(
        &self,
        id: &str,
        name: Option<&str>,
        root_paths: Option<&[&str]>,
        access: Option<(&str, bool, &[domain::UserId])>,
    ) -> Result<Library, StoreError> {
        self.update_library_with_rules(id, name, root_paths, access, None)
    }

    pub fn update_library_with_rules(
        &self,
        id: &str,
        name: Option<&str>,
        root_paths: Option<&[&str]>,
        access: Option<(&str, bool, &[domain::UserId])>,
        match_rules: Option<&[serde_json::Value]>,
    ) -> Result<Library, StoreError> {
        self.update_library_full(id, name, root_paths, access, match_rules, None)
    }

    pub fn update_library_full(
        &self,
        id: &str,
        name: Option<&str>,
        root_paths: Option<&[&str]>,
        access: Option<(&str, bool, &[domain::UserId])>,
        match_rules: Option<&[serde_json::Value]>,
        default_filter_id: Option<Option<&str>>,
    ) -> Result<Library, StoreError> {
        self.ensure_library_mutable(id)?;
        let Some(library) = self.get_library(id)? else {
            return Err(StoreError::Missing(format!("library {id}")));
        };
        if let Some(root_paths) = root_paths {
            self.ensure_removal_roots_available(root_paths)?;
            let proposed: Vec<PathBuf> = root_paths.iter().map(PathBuf::from).collect();
            for row in self.list_ledger()? {
                let Some(media) = self.get_media(row.media_id)? else {
                    continue;
                };
                if media.kind != library.kind
                    || self
                        .library_for_path(std::path::Path::new(&row.path), media.kind)?
                        .map(|owner| owner.id != library.id)
                        .unwrap_or(true)
                {
                    continue;
                }
                if !proposed
                    .iter()
                    .any(|root| std::path::Path::new(&row.path).starts_with(root))
                {
                    return Err(StoreError::Protected(format!(
                        "媒体库 {} 仍有台账文件位于现有根目录；先迁移或清理这些文件，再修改根目录",
                        library.name
                    )));
                }
            }
        }
        // One transaction for the row updates and the root replacement, so a
        // rejected root cannot leave a partially updated Library behind.
        let tx = self.library.unchecked_transaction()?;
        if let Some(name) = name {
            if !name.trim().is_empty() {
                tx.execute(
                    "UPDATE libraries SET name = ?1 WHERE library_id = ?2",
                    params![name.trim(), id],
                )?;
            }
        }
        if let Some(root_paths) = root_paths {
            Self::replace_roots(&tx, id, library.kind, root_paths)?;
            for root in root_paths {
                Self::reassign_files_under_root(&tx, id, root)?;
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
        Ok(self
            .get_library(id)?
            .ok_or_else(|| StoreError::Missing(format!("library {id}")))?)
    }

    pub fn delete_library(&self, id: &str) -> Result<(), StoreError> {
        Err(StoreError::Protected(format!(
            "Library {id} 必须通过持久删除协调器删除，不能绕过任务排空"
        )))
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
        Ok(self
            .get_library(id)?
            .ok_or_else(|| StoreError::Missing(format!("library {id}")))?)
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

    fn replace_roots(
        conn: &rusqlite::Connection,
        library_id: &str,
        _kind: MediaKind,
        root_paths: &[&str],
    ) -> Result<(), StoreError> {
        let mut stmt = conn.prepare("SELECT root_id, display_path FROM library_roots WHERE library_id = ?1")?;
        let existing = stmt
            .query_map(params![library_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for (index, path) in root_paths.iter().enumerate() {
            let path = path.trim();
            if path.is_empty() {
                continue;
            }
            if let Some((root_id, _)) = existing.iter().find(|(_, p)| p == path) {
                conn.execute(
                    "UPDATE library_roots SET sort_order = ?1 WHERE root_id = ?2",
                    params![index as i32, root_id],
                )?;
            } else {
                conn.execute(
                    "INSERT INTO library_roots (root_id, library_id, path_key, display_path, enabled, sort_order)
                     VALUES (?1, ?2, ?3, ?3, 1, ?4)",
                    params![
                        uuid::Uuid::new_v4().to_string(),
                        library_id,
                        path,
                        index as i32
                    ],
                )?;
            }
        }
        for (root_id, path) in existing {
            if !root_paths.iter().any(|p| p.trim() == path) {
                conn.execute("DELETE FROM library_roots WHERE root_id = ?1", params![root_id])?;
            }
        }
        Ok(())
    }

    /// Re-own every file whose path falls under `root_path` to `library_id`.
    ///
    /// Adding a nested root must not leave rows pointing at the previous Library:
    /// path-based reads would attribute the file to the new root while the persisted
    /// row still belonged to the old one, and the old Library could no longer be
    /// deleted because its roots still carried files. Rows and both affected
    /// summaries move inside the caller's transaction.
    pub(crate) fn reassign_files_under_root(
        conn: &rusqlite::Connection,
        library_id: &str,
        root_path: &str,
    ) -> Result<(), StoreError> {
        let root_path = root_path.trim();
        if root_path.is_empty() {
            return Ok(());
        }
        let new_root_id: Option<String> = conn
            .query_row(
                "SELECT root_id FROM library_roots WHERE library_id = ?1 AND path_key = ?2",
                params![library_id, root_path],
                |row| row.get(0),
            )
            .optional()?;
        let Some(new_root_id) = new_root_id else {
            return Ok(());
        };
        let affected: Vec<(String, String)> = {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT library_id, media_id FROM library_files
                 WHERE library_id <> ?1
                   AND substr(path_key, 1, length(?2) + 1) = ?2 || '/'",
            )?;
            let rows = stmt.query_map(params![library_id, root_path], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        let moved = conn.execute(
            "UPDATE library_files SET library_id = ?1, root_id = ?2
             WHERE substr(path_key, 1, length(?3) + 1) = ?3 || '/'
               AND (library_id <> ?1 OR root_id <> ?2)",
            params![library_id, new_root_id, root_path],
        )?;
        if moved == 0 {
            return Ok(());
        }
        conn.execute(
            "UPDATE library_file_units SET library_id = ?1
             WHERE file_id IN (
                 SELECT file_id FROM library_files WHERE library_id = ?1 AND root_id = ?2
             )",
            params![library_id, new_root_id],
        )?;
        let new_library: domain::LibraryId = library_id.parse()?;
        let mut reduced = HashSet::new();
        for (old_library, media) in affected {
            let media_id: domain::MediaId = media.parse()?;
            if reduced.insert((old_library.clone(), media.clone())) {
                super::library_write::reduce::reduce_library_media(
                    conn,
                    old_library.parse()?,
                    media_id,
                )?;
            }
            super::library_write::reduce::reduce_library_media(conn, new_library, media_id)?;
        }
        tracing::info!(%library_id, root = %root_path, files = moved, "Re-owned files under a new Library root");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejected_root_leaves_no_library_row() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("data")).unwrap();
        let first_root = temp.path().join("first-root");
        let second_root = temp.path().join("second-root");
        store
            .create_library_full(
                MediaKind::Movie,
                "First",
                &[first_root.to_str().unwrap()],
                "everyone",
                true,
                &[],
                &[],
                None,
            )
            .unwrap();
        let before = store.list_libraries().unwrap().len();

        let result = store.create_library_full(
            MediaKind::Movie,
            "Rejected",
            &[second_root.to_str().unwrap(), first_root.to_str().unwrap()],
            "everyone",
            true,
            &[],
            &[],
            None,
        );

        assert!(
            matches!(result, Err(StoreError::Sqlite(_))),
            "duplicate root path_key must be rejected: {result:?}"
        );
        let libraries = store.list_libraries().unwrap();
        assert_eq!(libraries.len(), before, "no orphan library row remains");
        assert!(libraries.iter().all(|library| library.name != "Rejected"));
        assert!(
            libraries
                .iter()
                .all(|library| !library.root_paths.contains(&second_root)),
            "the partially inserted root must be rolled back"
        );
    }
}
