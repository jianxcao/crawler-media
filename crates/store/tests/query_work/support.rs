use domain::{
    LibraryId, MediaKind, ProtocolItemQuery, ProtocolItemType, ProtocolParent, ProtocolSort,
    ProtocolSortKey, UserId,
};
use store::Store;
pub fn seed(store: &Store, kind: MediaKind, count: usize) -> LibraryId {
    let root = format!("/query-work/{}", kind.as_str());
    let lib = store
        .create_library(kind, "Work", &[root.as_str()], "everyone", true, &[])
        .unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|r| r.library_id.as_str() == lib.id.as_str())
        .unwrap()
        .id;
    let mut db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    db.pragma_update(None, "foreign_keys", true).unwrap();
    let tx = db.transaction().unwrap();
    for i in 0..count {
        let m = domain::MediaId::new().to_string();
        let f = domain::LedgerId::new().to_string();
        let title = format!("Item {i:06}");
        tx.execute(
            "INSERT INTO media(media_id,kind,title,identity_state) VALUES(?1,?2,?3,'provisional')",
            rusqlite::params![m, kind.as_str(), title],
        )
        .unwrap();
        tx.execute("INSERT INTO library_files(file_id,library_id,root_id,media_id,kind,path_key,display_path,presence_state,publication_state,file_revision,source_version,quality_source,confidence)
             VALUES(?1,?2,?3,?4,?5,?6,?6,'present','published',1,'v1','probe','high')",rusqlite::params![f,lib.id,root_id,m,kind.as_str(),format!("{root}/{i}.mkv")]).unwrap();
        tx.execute("INSERT INTO library_items(library_id,media_id,kind,title,title_sort,year,identity_state,metadata_revision,item_revision,added_at_sort,stable_shuffle_key,file_count,available_file_count,preferred_file_id)
             VALUES(?1,?2,?3,?4,?4,2024,'provisional',1,1,?5,?4,1,1,?6)",rusqlite::params![lib.id,m,kind.as_str(),title,i,f]).unwrap();
        tx.execute("INSERT INTO library_item_terms(library_id,media_id,term_kind,term_value) VALUES(?1,?2,'genre',?3)",rusqlite::params![lib.id,m,if i%2==0 {"Drama"}else{"Comedy"}]).unwrap();
    }
    tx.commit().unwrap();
    lib.id.parse().unwrap()
}
pub fn protocol() -> ProtocolItemQuery {
    ProtocolItemQuery {
        user_id: UserId::new(),
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
