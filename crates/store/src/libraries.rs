//! Library entity: one media library per kind holds multiple ordered roots
//! (first root = primary, where new content lands). The `libraries` table is
//! the display/config surface; `library_roots` rows belong to a library.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::str::FromStr;

use domain::MediaKind;
use rusqlite::OptionalExtension;
use rusqlite::params;

use super::library_ownership::{LibraryOwnership, classify_new_file};
use super::{Store, StoreError};
mod root_config;
pub use root_config::PreparedLibraryRoots;
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
        self.with_library_read(|db| db.list_libraries())
    }

    pub fn library_ownership(&self) -> Result<LibraryOwnership, StoreError> {
        self.with_library_read(|db| db.library_ownership())
    }
    /// Determine authorized libraries for a user according to their role and library access settings.
    pub fn authorized_library_ids(
        &self,
        user_id: domain::UserId,
    ) -> Result<HashSet<String>, StoreError> {
        let role = self.user_role(user_id)?;
        self.with_read(crate::Db::Library, |library| {
            crate::library_read::authorized_ids_on(
                library,
                user_id,
                role.parse().map_err(StoreError::Invalid)?,
            )
        })
    }

    pub fn get_library(&self, id: &str) -> Result<Option<Library>, StoreError> {
        self.with_library_read(|db| db.get_library(id))
    }

    /// Current configuration revision of one Library. Writers pin it in their
    /// `WriteScope` so a reconfiguration between plan and write is refused.
    pub fn library_config_revision(&self, id: &str) -> Result<i64, StoreError> {
        self.with_library_read(|db| db.library_config_revision(id))
    }

    pub fn default_library(&self, kind: MediaKind) -> Result<Option<Library>, StoreError> {
        self.with_library_read(|db| db.default_library(kind))
    }

    /// 严格按根目录前缀匹配媒体库：匹配时把**根目录和候选路径都**解析规范化，
    /// 再用 [`classify_new_file`] 的同一套规则判定；不回退到默认媒体库。
    /// 只用于**新文件入库**时的归属判定，已发布文件的归属读 `library_files.library_id`。
    ///
    /// 返回的是**记录形态**的 `Library`：`root_paths` 保留库里存的拼写。规范化副本
    /// 只用于这次匹配——调用方会拿自己的路径再和 `root_paths` 比一次（例如
    /// `auto_resolve::has_local_scan_metadata` 用它选 show root），把内部副本漏出去
    /// 会让这些比较在别名路径上全部落空（macOS `/var` 与 `/private/var`）。
    pub fn library_for_path_strict(
        &self,
        path: &std::path::Path,
        kind: MediaKind,
    ) -> Result<Option<Library>, StoreError> {
        let libraries = self.list_libraries()?;
        let resolved = Self::with_resolved_roots(&libraries)?;
        let norm_path = crate::library_paths::resolved_path(path).map_err(|error| {
            tracing::error!(path = %path.display(), %error, "解析严格 Library 路径失败");
            error
        })?;
        let Some(owner_id) =
            classify_new_file(&resolved, &norm_path, kind).map(|library| library.id.clone())
        else {
            return Ok(None);
        };
        Ok(libraries.into_iter().find(|library| library.id == owner_id))
    }

    /// Copies of `libraries` with every root path resolved and normalized
    /// (filesystem IO). This is the admission-time form the pure classifier needs;
    /// the copies are internal to the match, never handed to a caller.
    fn with_resolved_roots(libraries: &[Library]) -> Result<Vec<Library>, StoreError> {
        let mut resolved = Vec::with_capacity(libraries.len());
        for library in libraries {
            let mut roots = Vec::with_capacity(library.root_paths.len());
            for root in &library.root_paths {
                roots.push(crate::library_paths::resolved_path(root)?);
            }
            let mut copy = library.clone();
            copy.root_paths = roots;
            resolved.push(copy);
        }
        Ok(resolved)
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
        let roots = PreparedLibraryRoots::observe(root_paths)?;
        self.create_library_full_prepared(
            kind,
            name,
            &roots,
            access_mode,
            admin_visible,
            member_ids,
            match_rules,
            default_filter_id,
        )
    }

    /// Create from root observations made outside the caller's Store guard.
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
        self.with_library_write(|db| {
            db.create_library_full_prepared(
                kind,
                name,
                roots,
                access_mode,
                admin_visible,
                member_ids,
                match_rules,
                default_filter_id,
            )
        })
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
        self.with_library_write(|db| {
            db.set_library_switch_settings(
                id,
                realtime_watch,
                generate_thumbnails,
                extract_chapter_images,
                exclude_from_home,
                auto_series_collections,
            )
        })
    }

    pub fn set_library_cover_path(
        &self,
        id: &str,
        cover_path: Option<&str>,
    ) -> Result<(), StoreError> {
        self.with_library_write(|db| db.set_library_cover_path(id, cover_path))
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
        let prepared = root_paths.map(PreparedLibraryRoots::observe).transpose()?;
        self.with_library_write(|library| {
            library.update_library_full_prepared(
                id,
                name,
                prepared.as_ref(),
                access,
                match_rules,
                default_filter_id,
            )
        })
    }

    pub fn delete_library(&self, id: &str) -> Result<(), StoreError> {
        Err(StoreError::Protected(format!(
            "Library {id} 必须通过持久删除协调器删除，不能绕过任务排空"
        )))
    }

    pub fn set_default_library(&self, id: &str) -> Result<Library, StoreError> {
        self.with_library_write(|db| db.set_default_library(id))
    }

    pub fn reorder_libraries(&self, ids: &[&str]) -> Result<(), StoreError> {
        self.with_library_write(|db| db.reorder_libraries(ids))
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

#[path = "libraries/connection.rs"]
mod connection;
