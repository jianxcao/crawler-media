mod query_model_support;

use domain::{MediaId, User, UserId, UserRole};
use query_model_support::{run, setup};
use store::{Store, StoreError};

fn member(store: &Store, login: &str, role: UserRole) -> UserId {
    let id = UserId::new();
    store
        .insert_user(&User {
            id,
            login: login.into(),
            enabled: true,
            role,
        })
        .unwrap();
    id
}

fn finish(store: &Store, operation: &str) {
    for now in 1..10_000 {
        let status = store.advance_library_operation(operation, now, 1).unwrap();
        if status.state == "succeeded" {
            return;
        }
        assert_eq!(status.state, "applying", "{status:?}");
    }
    panic!("operation {operation} did not finish");
}

#[test]
fn user_cleanup_rejects_integrity_owner_before_removing_facts() {
    let (tmp, store, library, ids) = setup();
    let admin = member(&store, "owner-admin", UserRole::Admin);
    let user = member(&store, "owner-member", UserRole::Member);
    let media: MediaId = ids[0].parse().unwrap();
    store
        .upsert_unit(
            user,
            media,
            -1,
            -1,
            0,
            None,
            Some(true),
            None,
            None,
            None,
            false,
            50,
        )
        .unwrap();
    let before = store.unit_state(user, media, -1, -1).unwrap().unwrap();
    store
        .begin_library_integrity(library, true, "owner", 0)
        .unwrap();
    assert_eq!(
        store
            .advance_library_operation("owner", 1, 1)
            .unwrap()
            .state,
        "applying"
    );

    assert!(matches!(
        store.clean_user_library_data(user, admin),
        Err(StoreError::Protected(_))
    ));
    let after = store.unit_state(user, media, -1, -1).unwrap().unwrap();
    assert_eq!(after.favorite, before.favorite);
    assert_eq!(after.favorite_at, before.favorite_at);
    drop(store);

    let store = Store::open(tmp.path()).unwrap();
    finish(&store, "owner");
    assert!(
        store
            .get_user_library_summary(user, library, media)
            .unwrap()
            .is_some()
    );
    store.clean_user_library_data(user, admin).unwrap();
    store.finish_user_removal(user).unwrap();
    run(&store, library, true, "after-removal");
    assert!(store.get_user(user).unwrap().is_none());
    assert!(store.unit_rows(user, media).unwrap().is_empty());
    assert!(
        store
            .get_user_library_summary(user, library, media)
            .unwrap()
            .is_none()
    );
    assert!(
        !store
            .library_integrity_report("after-removal")
            .unwrap()
            .has_issues()
    );
}

#[test]
fn unrelated_user_cleanup_stays_available_during_integrity() {
    let (_tmp, store, library, _) = setup();
    let admin = member(&store, "unrelated-admin", UserRole::Admin);
    let user = member(&store, "unrelated-member", UserRole::Member);
    store
        .begin_library_integrity(library, true, "unrelated-owner", 0)
        .unwrap();
    store.clean_user_library_data(user, admin).unwrap();
}

#[test]
fn rejected_collection_deletion_keeps_collection_and_memberships_visible() {
    let (_tmp, store, library, ids) = setup();
    let user = member(&store, "collection-owner", UserRole::Member);
    let collection = store.create_collection(user, "Keep while frozen").unwrap();
    store.add_collection_item(&collection, &ids[0]).unwrap();
    store
        .begin_library_integrity(library, true, "collection-owner", 0)
        .unwrap();
    store
        .advance_library_operation("collection-owner", 1, 1)
        .unwrap();

    assert!(matches!(
        store.delete_collection(&collection),
        Err(StoreError::Protected(_))
    ));
    assert!(store.collection_owned_by(&collection, user).unwrap());
    assert_eq!(
        store.collection_item_ids(&collection).unwrap(),
        vec![ids[0].clone()]
    );
    assert_eq!(store.list_collections(user).unwrap().len(), 1);

    finish(&store, "collection-owner");
    assert_eq!(
        store.collection_item_ids(&collection).unwrap(),
        vec![ids[0].clone()]
    );
    assert!(store.delete_collection(&collection).unwrap());
    assert!(store.list_collections(user).unwrap().is_empty());
    run(&store, library, false, "collection-clean");
    assert!(
        !store
            .library_integrity_report("collection-clean")
            .unwrap()
            .has_issues()
    );
}
