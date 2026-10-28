//! Validate the one current definition, derived from the production DDL itself.
use crate::StoreError;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::sync::OnceLock;

type Definitions = BTreeMap<(String, String), String>;
type Cached = OnceLock<Result<Definitions, String>>;

fn definitions(conn: &Connection) -> Result<Definitions, rusqlite::Error> {
    // Runtime fault-injection/diagnostic triggers are not data layouts. Tables,
    // indexes (including constraints in table SQL), and views are validated.
    conn.prepare(
        "SELECT type,name,sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'
                  AND type IN ('table','index','view') ORDER BY type,name",
    )?
    .query_map([], |row| Ok(((row.get(0)?, row.get(1)?), row.get(2)?)))?
    .collect()
}

fn expected(name: &str) -> Result<&'static Definitions, StoreError> {
    static APP: Cached = OnceLock::new();
    static LIBRARY: Cached = OnceLock::new();
    static SUBSCRIBE: Cached = OnceLock::new();
    static CACHE: Cached = OnceLock::new();
    let (cache, ddl) = match name {
        "app" => (&APP, super::layout::APP_DDL),
        "library" => (&LIBRARY, super::layout::LIBRARY_DDL),
        "subscribe" => (&SUBSCRIBE, super::layout::SUBSCRIBE_DDL),
        "cache" => (&CACHE, super::layout::CACHE_DDL),
        _ => return Err(StoreError::Invalid(format!("unknown database {name}"))),
    };
    match cache.get_or_init(|| {
        let conn = Connection::open_in_memory().map_err(|error| error.to_string())?;
        conn.execute_batch(ddl).map_err(|error| error.to_string())?;
        definitions(&conn).map_err(|error| error.to_string())
    }) {
        Ok(value) => Ok(value),
        Err(error) => Err(StoreError::Invalid(format!(
            "invalid current {name} DDL: {error}"
        ))),
    }
}

pub(super) fn validate(name: &str, conn: &Connection) -> Result<(), StoreError> {
    let expected = expected(name)?;
    let actual = definitions(conn)?;
    // A dropped table also drops its indexes; report the missing owner first.
    for family in ["table", "index", "view"] {
        for ((kind, object), definition) in expected.iter().filter(|((kind, _), _)| kind == family)
        {
            match actual.get(&(kind.clone(), object.clone())) {
                Some(found) if found == definition => {}
                Some(_) => {
                    return Err(super::layout::layout_error(
                        name,
                        format!("{name} database definition mismatch for {kind} {object}"),
                    ));
                }
                None => {
                    return Err(super::layout::layout_error(
                        name,
                        format!("{name} database schema is missing required {kind} {object}"),
                    ));
                }
            }
        }
    }
    if let Some((kind, object)) = actual.keys().find(|key| !expected.contains_key(*key)) {
        return Err(super::layout::layout_error(
            name,
            format!("{name} database holds unsupported {kind} {object}"),
        ));
    }
    Ok(())
}
