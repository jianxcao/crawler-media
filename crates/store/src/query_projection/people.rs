use super::types::Person;
use crate::StoreError;
use rusqlite::{Connection, params};
use serde_json::Value;
use std::collections::BTreeSet;
pub(super) fn derive(nfo: &Value, adopted: &Value) -> BTreeSet<Person> {
    let mut out = BTreeSet::new();
    for fields in [nfo, adopted] {
        for (path, role) in [
            ("cast", "Actor"),
            ("directors", "Director"),
            ("creators", "Writer"),
        ] {
            for value in fields[path]["value"].as_array().into_iter().flatten() {
                let Some(name) = value
                    .as_str()
                    .or_else(|| value["name"].as_str())
                    .filter(|n| !n.trim().is_empty())
                else {
                    continue;
                };
                let tmdb = match &value["tmdb_id"] {
                    Value::String(s) => s.trim().to_string(),
                    Value::Number(n) => n.to_string(),
                    _ => String::new(),
                };
                let wire =
                    crate::protocol_index::compact_id(&crate::protocol_index::person_wire_id(
                        role,
                        name,
                        (!tmdb.is_empty()).then_some(tmdb.as_str()),
                    ));
                out.insert(Person {
                    role: role.into(),
                    name: name.to_lowercase(),
                    tmdb,
                    wire,
                });
            }
        }
    }
    out
}
pub(super) fn read(
    tx: &Connection,
    lib: &str,
    media: &str,
) -> Result<BTreeSet<Person>, StoreError> {
    Ok(tx.prepare_cached("SELECT role,name_key,tmdb_id,wire_id FROM library_protocol_people WHERE library_id=?1 AND media_id=?2")?.query_map(params![lib,media], |r|Ok(Person{role:r.get(0)?,name:r.get(1)?,tmdb:r.get(2)?,wire:r.get(3)?}))?.collect::<Result<_,_>>()?)
}
pub(super) fn sync(
    tx: &Connection,
    lib: &str,
    media: &str,
    after: &BTreeSet<Person>,
) -> Result<(), StoreError> {
    let before = read(tx, lib, media)?;
    for p in before.difference(after) {
        tx.execute("DELETE FROM library_protocol_people WHERE library_id=?1 AND media_id=?2 AND role=?3 AND name_key=?4 AND tmdb_id=?5",params![lib,media,p.role,p.name,p.tmdb])?;
    }
    for p in after.difference(&before) {
        tx.execute("INSERT INTO library_protocol_people(library_id,media_id,role,name_key,tmdb_id,wire_id) VALUES(?1,?2,?3,?4,?5,?6)",params![lib,media,p.role,p.name,p.tmdb,p.wire])?;
    }
    Ok(())
}
