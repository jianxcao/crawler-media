use super::Store;
use domain::{
    LibraryId, MediaKind, ProtocolItemQuery, ProtocolItemType, ProtocolParent, ProtocolSort,
    ProtocolSortKey, UserId,
};
const FIXTURE_NAMESPACE: uuid::Uuid = uuid::Uuid::NAMESPACE_OID;

fn fixture_id(label: &str) -> String {
    uuid::Uuid::new_v5(&FIXTURE_NAMESPACE, label.as_bytes()).to_string()
}
#[allow(dead_code)]
pub fn seed(store: &Store, kind: MediaKind, count: usize) -> LibraryId {
    let root = format!("/query-work/{}", kind.as_str());
    let lib = store
        .create_library(kind, "Work", &[root.as_str()], "everyone", true, &[])
        .unwrap();
    seed_into(store, kind, 0, count, lib.id.parse().unwrap(), &root);
    lib.id.parse().unwrap()
}
pub fn seed_into(
    store: &Store,
    kind: MediaKind,
    start: usize,
    count: usize,
    id: LibraryId,
    root: &str,
) {
    seed_into_with_genre(store, kind, start, count, id, root, false);
}
pub fn seed_into_with_genre(
    store: &Store,
    kind: MediaKind,
    start: usize,
    count: usize,
    id: LibraryId,
    root: &str,
    all_drama: bool,
) {
    let lib_id = id.to_string();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id.as_str() == lib_id.as_str())
        .unwrap()
        .id;
    let mut db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    db.pragma_update(None, "foreign_keys", true).unwrap();
    let tx = db.transaction().unwrap();
    for i in start..start + count {
        let m = fixture_id(&format!("query-media-{}-{i}", kind.as_str()));
        let f = fixture_id(&format!("query-file-{}-{i}", kind.as_str()));
        let title = format!("Item {i:06}");
        tx.execute(
            "INSERT INTO media(media_id,kind,title,year,identity_state) VALUES(?1,?2,?3,2024,'provisional')",
            rusqlite::params![m, kind.as_str(), title],
        )
        .unwrap();
        tx.execute("INSERT INTO library_files(file_id,library_id,root_id,media_id,kind,path_key,display_path,presence_state,publication_state,file_revision,source_version,quality_source,confidence)
             VALUES(?1,?2,?3,?4,?5,?6,?6,'present','published',1,'v1','probe','high')",rusqlite::params![f,lib_id,root_id,m,kind.as_str(),format!("{root}/{i}.mkv")]).unwrap();
        if kind == MediaKind::Movie {
            tx.execute("INSERT INTO library_file_units(file_id,library_id,media_id,season,episode) VALUES(?1,?2,?3,-1,-1)", rusqlite::params![f,lib_id,m]).unwrap();
        }
        let genre = if all_drama || i % 2 == 0 {
            "Drama"
        } else {
            "Comedy"
        };
        let fields = serde_json::json!({
            "genres":{"state":"value","value":[{"source":"fixture","name":genre,"filter_key":genre},{"source":"fixture","name":"All","filter_key":"All"}]},
            "year":{"state":"value","value":2024}
        });
        tx.execute("INSERT INTO library_metadata_inputs(library_id,media_id,entity_scope,season,episode,locale,input_key,root_id,path_key,parse_state,source_version,fields_json,present_fields_json,revision) VALUES(?1,?2,'work',-1,-1,'zh-CN',?3,?4,?3,'valid','nfo_v1',?5,'[\"genres\",\"year\"]',1)",rusqlite::params![lib_id,m,format!("{root}/{i}.nfo"),root_id,fields.to_string()]).unwrap();
    }
    tx.commit().unwrap();
    let op = format!("query-work-{id}-{}", uuid::Uuid::new_v4());
    store.begin_library_integrity(id, true, &op, 1000).unwrap();
    loop {
        let status = store.advance_library_operation(&op, 1001, 100).unwrap();
        if status.state != "applying" {
            assert_eq!(status.state, "succeeded");
            break;
        }
    }
}
#[allow(dead_code)]
pub fn protocol() -> ProtocolItemQuery {
    ProtocolItemQuery {
        user_id: fixture_id("query-user").parse::<UserId>().unwrap(),
        parent: ProtocolParent::Root,
        series_id: None,
        season: None,
        person_ids: vec![],
        studio_ids: vec![],
        search_term: None,
        genres: vec![],
        genre_ids: vec![],
        filters: vec![],
        is_favorite: None,
        is_played: None,
        include_types: vec![ProtocolItemType::Movie],
        recursive: true,
        is_folder: None,
        sort: vec![ProtocolSort {
            key: ProtocolSortKey::Name,
            descending: false,
        }],
        offset: 0,
        limit: 10,
    }
}
