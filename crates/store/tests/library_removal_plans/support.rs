use domain::{Media, MediaId, MediaKind, Subscribe, SubscribeId};
use rusqlite::Connection;
use store::Store;

pub struct Fixture {
    pub temp: tempfile::TempDir,
    pub store: Store,
}
impl Fixture {
    pub fn new() -> Self {
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let store = Store::open(temp.path().join("data")).unwrap();
        Self { temp, store }
    }
    pub fn connection(&self, database: &str) -> Connection {
        let connection = Connection::open(self.temp.path().join("data").join(database)).unwrap();
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .unwrap();
        connection
    }
    pub fn library(&self, name: &str) -> store::Library {
        let root = self.temp.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        self.store
            .create_library(
                MediaKind::Movie,
                name,
                &[root.to_str().unwrap()],
                "everyone",
                true,
                &[],
            )
            .unwrap()
    }
    pub fn media(&self, title: &str) -> Media {
        let media = Media {
            id: MediaId::new(),
            kind: MediaKind::Movie,
            title: title.into(),
            year: None,
            original_title: None,
            aliases: vec![],
            tmdb_id: None,
            douban_id: None,
            tvdb_id: None,
            bangumi_id: None,
            anilist_id: None,
        };
        self.store.insert_media(&media).unwrap();
        media
    }
    pub fn subscribe(&self, library: &store::Library) -> SubscribeId {
        let media = self.media("Kept by Subscribe");
        let subscribe = Subscribe {
            id: SubscribeId::new(),
            user_id: domain::UserId::new(),
            media_id: media.id,
            coverage: domain::Coverage::Movie,
            fetch_mode: domain::FetchMode::Search,
            filter_id: domain::FilterId::new(),
            wash_cut: false,
            wash_cut_filter_id: None,
            keep_old_versions: false,
            full_season_pack: false,
            downloader_id: None,
            library_id: Some(library.id.parse().unwrap()),
            tracking_state: "active".into(),
            follow_future: false,
            search_interval_secs: 1800,
        };
        self.store.insert_subscribe(&subscribe).unwrap();
        subscribe.id
    }
    pub fn remove(&self, library: &store::Library) -> store::LibraryRemoval {
        let operation = self.store.begin_library_removal(&library.id).unwrap();
        self.store
            .set_removal_state(&operation.operation_id, "cleaning", None)
            .unwrap();
        self.store
            .clean_library_removal(&operation.operation_id)
            .unwrap()
    }
}

pub const PROJECTIONS: [&str; 4] = [
    "user_library_items",
    "library_item_terms",
    "library_protocol_terms",
    "library_protocol_people",
];

pub fn counts(connection: &Connection, media: MediaId) -> Vec<i64> {
    PROJECTIONS
        .iter()
        .map(|table| {
            connection
                .query_row(
                    &format!("SELECT COUNT(*) FROM {table} WHERE media_id=?1"),
                    [media.to_string()],
                    |row| row.get(0),
                )
                .unwrap()
        })
        .collect()
}

// Read the statements the implementation actually owns, including its formatted
// OWNED loop, rather than maintaining a second test-only list of DELETE shapes.
pub fn production_removal_sql() -> Vec<String> {
    let orphan = include_str!("../../src/library_write/orphan_media.rs");
    let owned = orphan
        .split("const OWNED:")
        .nth(1)
        .unwrap()
        .split("];")
        .next()
        .unwrap();
    let literals = quoted_literals(orphan);
    let template = literals
        .iter()
        .find(|sql| sql.starts_with("DELETE FROM {table}"))
        .unwrap();
    let mut queries = quoted_literals(owned)
        .into_iter()
        .map(|table| template.replace("{table}", &table))
        .collect::<Vec<_>>();
    queries.extend(
        literals
            .into_iter()
            .filter(|sql| sql.starts_with("DELETE FROM ") && !sql.contains("{table}")),
    );
    let retirement = include_str!("../../src/ledger_cleanup/probe_unit_retirement.rs");
    let retirement = retirement.split("/// Recompute").next().unwrap();
    queries.extend(
        quoted_literals(retirement)
            .into_iter()
            .filter(|sql| sql.starts_with("SELECT ") || sql.starts_with("DELETE ")),
    );
    queries
}

fn quoted_literals(source: &str) -> Vec<String> {
    source
        .split('"')
        .enumerate()
        .filter_map(|(index, value)| (index % 2 == 1).then_some(value.to_owned()))
        .collect()
}
