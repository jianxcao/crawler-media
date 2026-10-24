//! Database layout for the Store's SQLite files.
//!
//! `Store::open` initializes or validates each file through
//! [`layout::initialize_or_validate`], which applies the v1 DDL of
//! `schema/v1/*.sql` to a fresh file and rejects a file that belongs to another
//! layout. There is no migration ladder: a database is either the current
//! layout or it is refused.

use rusqlite::{Connection, OptionalExtension, params};

use super::StoreError;

pub mod layout;
#[path = "schema/v1/objects.rs"]
pub mod v1;

/// Insert the built-in naming templates, the default Filter and the default
/// Libraries with their roots. Idempotent: existing rows are kept.
pub fn seed_defaults(
    app: &Connection,
    library: &Connection,
    data_dir: &std::path::Path,
) -> Result<(), StoreError> {
    let movie_root = data_dir.join("library/movies");
    let tv_root = data_dir.join("library/tv");
    std::fs::create_dir_all(&movie_root)?;
    std::fs::create_dir_all(&tv_root)?;
    app.execute(
        "INSERT OR IGNORE INTO naming_templates (kind, pattern) VALUES (?1, ?2)",
        params![
            "movie",
            "{title} ({year})/{title} ({year}){part} - {resolution}{ext}"
        ],
    )?;
    app.execute(
        "INSERT OR IGNORE INTO naming_templates (kind, pattern) VALUES (?1, ?2)",
        params![
            "tv",
            "{title} ({year})/Season {season}/{title} - {season_episode}{part}{ext}"
        ],
    )?;
    let filter_id = uuid::Uuid::new_v4().to_string();
    // JSON atom form only (see maps::serialize_atoms): rows in the older DSL shape are refused.
    let atoms = r#"[{"priority":100,"rule":"resolution","exclude":false,"value":"2160p"},{"priority":50,"rule":"resolution","exclude":false,"value":"1080p"}]"#;
    app.execute(
        "INSERT INTO filters (id, name, atoms_json)
         SELECT ?1, '默认', ?2 WHERE NOT EXISTS (SELECT 1 FROM filters)",
        params![filter_id, atoms],
    )?;
    app.execute(
        "INSERT OR IGNORE INTO settings (key, value) VALUES ('default_filter_id', ?1)",
        params![filter_id],
    )?;
    seed_libraries_and_roots(library, &movie_root, &tv_root)?;
    Ok(())
}

fn seed_libraries_and_roots(
    library: &Connection,
    movie_root: &std::path::Path,
    tv_root: &std::path::Path,
) -> Result<(), StoreError> {
    for (kind, name, root_path) in [("movie", "电影库", movie_root), ("tv", "剧集库", tv_root)]
    {
        let root_str = root_path.display().to_string();
        let root_key = crate::path_key::canonical_root_key(&root_str)?;
        // The existence check and both rows share the write lock. A failed root
        // insert must not leave a Library that makes every later open skip this kind.
        let tx = crate::write_tx(library)?;
        let exists: bool = tx
            .query_row(
                "SELECT 1 FROM libraries WHERE kind = ?1 LIMIT 1",
                params![kind],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if exists {
            // A kind that already has a Library keeps the roots an operator saved.
            // Re-adding the built-in path here would undo a deletion or replacement (F09).
            continue;
        }
        let lib_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO libraries (library_id, kind, name, is_default, sort_order)
             VALUES (?1, ?2, ?3, 1, 0)",
            params![lib_id, kind, name],
        )?;
        tx.execute(
            "INSERT INTO library_roots (root_id, library_id, path_key, display_path, enabled, sort_order)
             VALUES (?1, ?2, ?4, ?3, 1, 0)",
            params![
                uuid::Uuid::new_v4().to_string(),
                lib_id,
                root_str,
                root_key
            ],
        )?;
        tx.commit()?;
        tracing::info!(library_id = %lib_id, kind, root = %root_str,
            "default Library and root initialized");
    }
    Ok(())
}
