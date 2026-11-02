use domain::{MediaId, UserId};
use rusqlite::{Connection, params};
use store::Store;

#[test]
fn collection_creation_surfaces_an_invalid_existing_sort_value() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let user = UserId::new();
    let existing = store.create_collection(user, "Existing").unwrap();
    let db = Connection::open(dir.path().join("library.db")).unwrap();
    db.execute(
        "UPDATE collections SET sort_order='corrupt' WHERE id=?1",
        [&existing],
    )
    .unwrap();

    assert!(
        store.create_collection(user, "Rejected").is_err(),
        "a failed ordering read must not create a collection with a guessed order"
    );
    let count: i64 = db
        .query_row("SELECT COUNT(*) FROM collections", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn collection_addition_surfaces_an_invalid_existing_sort_value() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let collection = store.create_collection(UserId::new(), "Existing").unwrap();
    let db = Connection::open(dir.path().join("library.db")).unwrap();
    let first = MediaId::new();
    let second = MediaId::new();
    for media in [first, second] {
        db.execute(
            "INSERT INTO media(media_id,kind,title,identity_state)
             VALUES (?1,'movie','Fixture','confirmed')",
            [media.to_string()],
        )
        .unwrap();
    }
    store
        .add_collection_item(&collection, &first.to_string())
        .unwrap();
    db.execute(
        "UPDATE collection_items SET sort_order='corrupt' WHERE collection_id=?1",
        params![collection],
    )
    .unwrap();

    assert!(
        store
            .add_collection_item(&collection, &second.to_string())
            .is_err(),
        "a failed ordering read must not add a member with a guessed order"
    );
    assert_eq!(
        store.collection_item_ids(&collection).unwrap(),
        [first.to_string()]
    );
}
