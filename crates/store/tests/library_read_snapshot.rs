use domain::{MediaKind, User, UserId, UserRole};
use std::{
    num::NonZeroUsize,
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};
use store::library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery};
use store::{
    LibraryReadError, LibraryReadRequest, LibraryReadResult, QueryBudget, ReadCredential, Store,
};

#[path = "query_work/support.rs"]
mod support;

fn admitted(store: &Store) -> (ReadCredential, UserId) {
    let user = User {
        id: UserId::new(),
        login: "b2-reader".into(),
        enabled: true,
        role: UserRole::Member,
    };
    store.insert_user(&user).unwrap();
    let app = rusqlite::Connection::open(store.app_db_path()).unwrap();
    app.execute(
        "INSERT INTO user_tokens(token,user_id,created_at) VALUES (?1,?2,strftime('%s','now'))",
        rusqlite::params!["b2-original", user.id.to_string()],
    )
    .unwrap();
    (ReadCredential::new(user.id, "b2-original".into()), user.id)
}

fn browse(library_id: domain::LibraryId, user_id: Option<UserId>) -> LibraryReadRequest {
    LibraryReadRequest::Browse(LibraryQuery {
        library_id,
        user_id,
        filter: BrowseFilter::default(),
        order: BrowseOrder::Title,
        descending: false,
        page: BrowsePage::Offset {
            start: 0,
            limit: 10,
        },
        include_total: true,
    })
}

fn budget() -> QueryBudget {
    QueryBudget {
        deadline: Instant::now() + Duration::from_secs(10),
        cancelled: Arc::new(AtomicBool::new(false)),
    }
}

#[test]
fn context_requires_original_credential_and_matching_personalization() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (credential, user) = admitted(&store);
    let library = support::seed(&store, MediaKind::Movie, 2);
    assert!(
        store
            .prepare_library_read(
                &ReadCredential::new(user, "revoked".into()),
                &browse(library, None)
            )
            .is_err()
    );
    assert!(matches!(
        store.prepare_library_read(&credential, &browse(library, Some(UserId::new()))),
        Err(LibraryReadError::Unauthorized)
    ));
    assert!(
        store
            .prepare_library_read(&credential, &browse(library, None))
            .is_ok()
    );
}

#[test]
fn revoked_role_token_or_library_acl_invalidates_context() {
    for change in [
        "token",
        "role",
        "disabled",
        "acl",
        "locale",
        "new_library",
        "reindex",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let (credential, user) = admitted(&store);
        let library = support::seed(&store, MediaKind::Movie, 2);
        let context = store
            .prepare_library_read(&credential, &browse(library, None))
            .unwrap();
        let app = rusqlite::Connection::open(store.app_db_path()).unwrap();
        let db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
        match change {
            "token" => {
                app.execute("INSERT INTO user_tokens(token,user_id,created_at) VALUES (?1,?2,strftime('%s','now'))", rusqlite::params!["b2-other", user.to_string()]).unwrap();
                app.execute("DELETE FROM user_tokens WHERE token='b2-original'", [])
                    .unwrap();
                assert_eq!(store.user_by_token("b2-other").unwrap().unwrap().id, user);
            }
            "role" => {
                app.execute(
                    "UPDATE users SET role='admin' WHERE id=?1",
                    [user.to_string()],
                )
                .unwrap();
            }
            "disabled" => {
                app.execute("UPDATE users SET enabled=0 WHERE id=?1", [user.to_string()])
                    .unwrap();
            }
            "acl" => {
                db.execute("UPDATE libraries SET access_mode='members', member_ids_json='[]' WHERE library_id=?1", [library.to_string()]).unwrap();
            }
            "locale" => {
                db.execute(
                    "UPDATE libraries SET display_locale='en-US' WHERE library_id=?1",
                    [library.to_string()],
                )
                .unwrap();
            }
            "new_library" => {
                store
                    .create_library(
                        MediaKind::Movie,
                        "Second",
                        &["/b2/second"],
                        "everyone",
                        true,
                        &[],
                    )
                    .unwrap();
            }
            "reindex" => {
                db.execute(
                    "UPDATE libraries SET lifecycle_state='reindexing' WHERE library_id=?1",
                    [library.to_string()],
                )
                .unwrap();
            }
            _ => unreachable!(),
        }
        if change == "acl" {
            let pool =
                store::test_support::reader_pool_for_fixture(&store, NonZeroUsize::new(1).unwrap());
            assert!(matches!(
                pool.try_checkout().unwrap().execute(&context, &budget()),
                Err(LibraryReadError::Unauthorized)
            ));
        }
        let expected_unauthorized = matches!(change, "acl" | "token" | "disabled");
        assert!(
            match store.validate_library_read(&context) {
                Err(LibraryReadError::Unauthorized) => expected_unauthorized,
                Err(LibraryReadError::ContextChanged) => !expected_unauthorized,
                _ => false,
            },
            "{change}"
        );
    }
}

#[test]
fn reader_and_store_share_page_facts_and_playback_does_not_invalidate() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (credential, user) = admitted(&store);
    let library = support::seed(&store, MediaKind::Movie, 3);
    let request = browse(library, Some(user));
    let context = store.prepare_library_read(&credential, &request).unwrap();
    let LibraryReadRequest::Browse(ref q) = request else {
        unreachable!()
    };
    let expected = store.library_item_page(q).unwrap();
    let ids = expected
        .items
        .iter()
        .map(|item| item.media_id)
        .collect::<Vec<_>>();
    let expected_facts = store.library_item_facts(library, &ids, Some(user)).unwrap();
    let pool = store::test_support::reader_pool_for_fixture(&store, NonZeroUsize::new(1).unwrap());
    let mut lease = pool.try_checkout().unwrap();
    let LibraryReadResult::Browse {
        page,
        facts,
        stills,
    } = lease.execute(&context, &budget()).unwrap()
    else {
        panic!("wrong result")
    };
    assert_eq!(page, expected);
    assert_eq!(facts, expected_facts);
    assert!(stills.is_empty());
    drop(lease);
    let db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    db.execute(
        "INSERT INTO playback_units(user_id,media_id,season,episode) VALUES (?1,?2,-1,-1)",
        rusqlite::params![user.to_string(), ids[0].to_string()],
    )
    .unwrap();
    store.validate_library_read(&context).unwrap();
}

#[test]
fn protocol_and_facets_use_the_same_borrowed_cores() {
    use domain::ProtocolParent;
    use store::library_query::{FacetRequest, FacetTier};
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (credential, user) = admitted(&store);
    let library = support::seed(&store, MediaKind::Movie, 3);
    let pool = store::test_support::reader_pool_for_fixture(&store, NonZeroUsize::new(1).unwrap());
    let facets = FacetRequest {
        library_id: library,
        user_id: Some(user),
        filter: BrowseFilter::default(),
        tier: FacetTier::All,
    };
    let expected_facets = store.library_facets(&facets).unwrap();
    let context = store
        .prepare_library_read(&credential, &LibraryReadRequest::Facets(facets))
        .unwrap();
    let mut lease = pool.try_checkout().unwrap();
    let LibraryReadResult::Facets(actual_facets) = lease.execute(&context, &budget()).unwrap()
    else {
        panic!("wrong result")
    };
    assert_eq!(actual_facets, expected_facets);
    drop(lease);
    let mut request = support::protocol();
    request.user_id = user;
    request.parent = ProtocolParent::Library(library);
    let expected_page = store.protocol_item_page(&request).unwrap();
    let expected_inputs = store
        .protocol_page_inputs(user, &expected_page.items)
        .unwrap();
    let context = store
        .prepare_library_read(&credential, &LibraryReadRequest::Protocol(request))
        .unwrap();
    let mut lease = pool.try_checkout().unwrap();
    let LibraryReadResult::Protocol {
        page,
        inputs,
        series,
    } = lease.execute(&context, &budget()).unwrap()
    else {
        panic!("wrong result")
    };
    assert_eq!(page, expected_page);
    assert_eq!(inputs.files.len(), expected_inputs.files.len());
    assert_eq!(inputs.media.len(), expected_inputs.media.len());
    assert_eq!(inputs.coverage.len(), expected_inputs.coverage.len());
    assert_eq!(inputs.user_items.len(), expected_inputs.user_items.len());
    assert!(series.values().all(|scalar| scalar.episodes == 0));
}

#[test]
fn gallery_stills_are_read_with_the_selected_page() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (credential, user) = admitted(&store);
    let library = support::seed(&store, MediaKind::Movie, 2);
    let db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    let media: String = db
        .query_row(
            "SELECT media_id FROM library_items WHERE library_id=?1 ORDER BY media_id LIMIT 1",
            [library.to_string()],
            |r| r.get(0),
        )
        .unwrap();
    db.execute(
        "UPDATE library_items SET has_still=1 WHERE library_id=?1 AND media_id=?2",
        rusqlite::params![library.to_string(), media],
    )
    .unwrap();
    db.execute(
        "UPDATE library_files SET has_still=1 WHERE library_id=?1 AND media_id=?2",
        rusqlite::params![library.to_string(), media],
    )
    .unwrap();
    let LibraryReadRequest::Browse(mut query) = browse(library, Some(user)) else {
        unreachable!()
    };
    query.filter.require_image = true;
    let expected = store.library_item_page(&query).unwrap();
    let ids = expected
        .items
        .iter()
        .map(|item| item.media_id)
        .collect::<Vec<_>>();
    let expected_stills = store.library_item_stills(library, &ids).unwrap();
    assert_eq!(expected_stills.len(), 1);
    let context = store
        .prepare_library_read(&credential, &LibraryReadRequest::Browse(query))
        .unwrap();
    let pool = store::test_support::reader_pool_for_fixture(&store, NonZeroUsize::new(1).unwrap());
    let LibraryReadResult::Browse { page, stills, .. } = pool
        .try_checkout()
        .unwrap()
        .execute(&context, &budget())
        .unwrap()
    else {
        panic!("wrong result")
    };
    assert_eq!(page, expected);
    assert_eq!(stills, expected_stills);
}

#[test]
fn frozen_removal_is_absent_from_fresh_roster_and_invalidates_old_context() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (credential, user) = admitted(&store);
    let library = support::seed(&store, MediaKind::Movie, 2);
    store
        .create_library(
            MediaKind::Movie,
            "Keep",
            &["/b2/keep"],
            "everyone",
            true,
            &[],
        )
        .unwrap();
    let old = store
        .prepare_library_read(&credential, &browse(library, Some(user)))
        .unwrap();
    store.begin_library_removal(&library.to_string()).unwrap();
    assert!(matches!(
        store.validate_library_read(&old),
        Err(LibraryReadError::Unauthorized)
    ));
    let pool = store::test_support::reader_pool_for_fixture(&store, NonZeroUsize::new(1).unwrap());
    assert!(matches!(
        pool.try_checkout().unwrap().execute(&old, &budget()),
        Err(LibraryReadError::Unauthorized)
    ));
    let fresh = store
        .prepare_library_read(&credential, &browse(library, Some(user)))
        .unwrap();
    assert!(matches!(
        pool.try_checkout().unwrap().execute(&fresh, &budget()),
        Err(LibraryReadError::Unauthorized)
    ));
}

#[test]
fn integrity_protection_is_preserved_as_store_error() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let (credential, user) = admitted(&store);
    let library = support::seed(&store, MediaKind::Movie, 2);
    let db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    db.execute(
        "UPDATE libraries SET integrity_state='failed' WHERE library_id=?1",
        [library.to_string()],
    )
    .unwrap();
    let context = store
        .prepare_library_read(&credential, &browse(library, Some(user)))
        .unwrap();
    let pool = store::test_support::reader_pool_for_fixture(&store, NonZeroUsize::new(1).unwrap());
    assert!(matches!(
        pool.try_checkout().unwrap().execute(&context, &budget()),
        Err(LibraryReadError::Store(store::StoreError::Protected(_)))
    ));
}
