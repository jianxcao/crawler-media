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
        let existing: Option<String> = library
            .query_row(
                "SELECT library_id FROM libraries WHERE kind = ?1 LIMIT 1",
                params![kind],
                |row| row.get(0),
            )
            .optional()?;

        let lib_id = match existing {
            Some(id) => id,
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                library.execute(
                    "INSERT INTO libraries (library_id, kind, name, is_default, sort_order)
                     VALUES (?1, ?2, ?3, 1, 0)",
                    params![id, kind, name],
                )?;
                id
            }
        };

        let root_str = root_path.display().to_string();
        let has_root: bool = library
            .query_row(
                "SELECT 1 FROM library_roots WHERE path_key = norm_path_key(?1)",
                params![root_str],
                |_| Ok(()),
            )
            .optional()?
            .is_some();

        if !has_root {
            let root_id = uuid::Uuid::new_v4().to_string();
            library.execute(
                "INSERT INTO library_roots (root_id, library_id, path_key, display_path, enabled, sort_order)
                 VALUES (?1, ?2, ?4, ?3, 1, 0)",
                params![
                    root_id,
                    lib_id,
                    root_str,
                    crate::path_key::canonical_root_key(&root_str)?
                ],
            )?;
        }
    }
    Ok(())
}
