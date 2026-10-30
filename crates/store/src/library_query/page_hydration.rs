//! All cold page facts are captured by a fixed set of indexed, read-only batches.
mod metadata;
use crate::{Store, StoreError, StoredMediaMarker};
use domain::{EntityScope, LedgerId, LibraryId, MediaId, MetadataFields};
use rusqlite::Connection;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Default)]
pub struct HydratedFile {
    pub tracks: library::Tracks,
    pub chapters: Vec<library::ChapterMarker>,
    pub fields: HashMap<EntityScope, MetadataFields>,
    pub markers: HashMap<(i32, i32), StoredMediaMarker>,
}
#[derive(Clone)]
struct Owner {
    file: LedgerId,
    library: LibraryId,
    media: MediaId,
    locale: String,
}
const OWNERS_SQL: &str =
    "SELECT f.file_id,f.library_id,f.media_id,l.display_locale FROM json_each(?1) j
 CROSS JOIN library_files f ON f.file_id=j.value
 CROSS JOIN libraries l ON l.library_id=f.library_id";
const DETAILS_SQL: &str =
    "SELECT f.file_id,d.audio_json,d.subtitle_json,d.video_json,d.tracks_version,d.chapters_json
 FROM json_each(?1) j CROSS JOIN library_files f ON f.file_id=j.value
 CROSS JOIN file_details d ON d.file_id=f.file_id AND d.source_version=f.source_version";
const UNITS_SQL: &str = "SELECT u.file_id,u.season,u.episode FROM json_each(?1) j
 CROSS JOIN library_file_units u ON u.file_id=j.value";
const MARKERS_SQL: &str = "SELECT m.media_id,m.season,m.episode,m.intro_start_ms,m.intro_end_ms,
 m.outro_start_ms,m.outro_end_ms,m.source,m.locked,m.updated_at
 FROM json_each(?1) j CROSS JOIN media_markers m ON m.media_id=json_extract(j.value,'$[0]')
 AND m.season=json_extract(j.value,'$[1]') AND m.episode=json_extract(j.value,'$[2]')";

impl Store {
    pub fn page_hydration(
        &self,
        files: &[LedgerId],
    ) -> Result<HashMap<LedgerId, HydratedFile>, StoreError> {
        self.with_library_read(|db| db.page_hydration(files))
    }

    /// Explain the very statements executed by the production page readers.
    #[cfg(any(test, feature = "test-support"))]
    pub fn page_read_query_plans(&self) -> Result<Vec<String>, StoreError> {
        self.with_library_read(|db| db.page_read_query_plans())
    }
}
fn hydrate_on(
    conn: &Connection,
    files: &[LedgerId],
) -> Result<HashMap<LedgerId, HydratedFile>, StoreError> {
    let ids: HashSet<_> = files.iter().copied().collect();
    let encoded = serde_json::to_string(&ids)?;
    let owners = load_owners(conn, &encoded)?;
    let mut result = owners
        .iter()
        .map(|o| (o.file, HydratedFile::default()))
        .collect::<HashMap<_, _>>();
    load_details(conn, &encoded, &mut result)?;
    let mut scopes: HashMap<LedgerId, HashSet<EntityScope>> = owners
        .iter()
        .map(|o| (o.file, HashSet::from([EntityScope::Work])))
        .collect();
    let mut stmt = conn.prepare(UNITS_SQL)?;
    let mut rows = stmt.query([&encoded])?;
    while let Some(row) = rows.next()? {
        let id = row.get::<_, String>(0)?.parse()?;
        let (season, episode): (i32, i32) = (row.get(1)?, row.get(2)?);
        if season >= 0
            && episode >= 0
            && let Some(scopes) = scopes.get_mut(&id)
        {
            scopes.insert(EntityScope::Season(season as u32));
            scopes.insert(EntityScope::Episode {
                season: season as u32,
                episode: episode as u32,
            });
        }
    }
    metadata::hydrate(conn, &owners, &scopes, &mut result)?;
    load_markers(conn, &owners, &scopes, &mut result)?;
    Ok(result)
}
fn load_owners(conn: &Connection, encoded: &str) -> Result<Vec<Owner>, StoreError> {
    let mut stmt = conn.prepare(OWNERS_SQL)?;
    let rows = stmt.query_map([encoded], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get(3)?,
        ))
    })?;
    rows.map(|row| {
        let (file, library, media, locale) = row?;
        Ok(Owner {
            file: file.parse()?,
            library: library.parse()?,
            media: media.parse()?,
            locale,
        })
    })
    .collect()
}
fn load_details(
    conn: &Connection,
    encoded: &str,
    result: &mut HashMap<LedgerId, HydratedFile>,
) -> Result<(), StoreError> {
    let mut stmt = conn.prepare(DETAILS_SQL)?;
    let mut rows = stmt.query([encoded])?;
    while let Some(row) = rows.next()? {
        let file = row.get::<_, String>(0)?.parse()?;
        let Some(facts) = result.get_mut(&file) else {
            continue;
        };
        let video: Option<String> = row.get(3)?;
        let version: i64 = row.get(4)?;
        if version >= crate::library::CURRENT_TRACKS_VERSION && video.is_some() {
            facts.tracks = library::Tracks {
                audio: serde_json::from_str(&row.get::<_, String>(1)?).unwrap_or_default(),
                subtitles: serde_json::from_str(&row.get::<_, String>(2)?).unwrap_or_default(),
                video: video.and_then(|raw| serde_json::from_str(&raw).ok()),
            };
        }
        facts.chapters = row
            .get::<_, Option<String>>(5)?
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default();
    }
    Ok(())
}
fn load_markers(
    conn: &Connection,
    owners: &[Owner],
    scopes: &HashMap<LedgerId, HashSet<EntityScope>>,
    result: &mut HashMap<LedgerId, HydratedFile>,
) -> Result<(), StoreError> {
    let mut keys = HashSet::new();
    let mut file_keys = HashMap::new();
    for owner in owners {
        let wanted = marker_coordinates(scopes.get(&owner.file));
        keys.extend(
            wanted
                .iter()
                .map(|(season, episode)| (owner.media, *season, *episode)),
        );
        file_keys.insert(owner.file, wanted);
    }
    let encoded = serde_json::to_string(&keys)?;
    let mut stmt = conn.prepare(MARKERS_SQL)?;
    let mut rows = stmt.query([encoded])?;
    let mut markers: HashMap<(MediaId, i32, i32), StoredMediaMarker> = HashMap::new();
    while let Some(row) = rows.next()? {
        let media = row.get::<_, String>(0)?.parse()?;
        let (season, episode): (u32, u32) = (row.get(1)?, row.get(2)?);
        markers.insert(
            (media, season as i32, episode as i32),
            StoredMediaMarker {
                media_id: media,
                season,
                episode,
                intro_start_ms: row.get(3)?,
                intro_end_ms: row.get(4)?,
                outro_start_ms: row.get(5)?,
                outro_end_ms: row.get(6)?,
                source: row.get(7)?,
                locked: row.get::<_, i64>(8)? != 0,
                updated_at: row.get(9)?,
            },
        );
    }
    for owner in owners {
        if let (Some(facts), Some(wanted)) =
            (result.get_mut(&owner.file), file_keys.get(&owner.file))
        {
            facts.markers = wanted
                .iter()
                .filter_map(|coords| {
                    markers
                        .get(&(owner.media, coords.0, coords.1))
                        .map(|marker| (*coords, marker.clone()))
                })
                .collect();
        }
    }
    Ok(())
}

fn marker_coordinates(scopes: Option<&HashSet<EntityScope>>) -> HashSet<(i32, i32)> {
    let mut keys = HashSet::from([(1, 1)]);
    if let Some(scopes) = scopes {
        for scope in scopes {
            match scope {
                EntityScope::Episode { season, episode } => {
                    keys.insert((*season as i32, *episode as i32));
                }
                EntityScope::Season(season) => {
                    keys.insert((*season as i32, 1));
                }
                EntityScope::Work => {}
            }
        }
    }
    keys
}

#[path = "page_hydration/connection.rs"]
mod connection;
