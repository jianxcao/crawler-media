//! Completion is public only after both independent database commits finish.
use super::support::*;
use axum::http::StatusCode;
use domain::MediaKind;
use rusqlite::Connection;
use std::time::Duration;
use tower::ServiceExt;

#[test]
fn blocked_subscribe_detachment_keeps_http_removal_cleaning_until_commit() {
    let fixture = Fixture::new(MediaKind::Movie);
    let runtime = runtime();
    let mut subscribe = Connection::open(fixture.temp.path().join("data/subscribe.db")).unwrap();
    let blocked = subscribe
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    runtime.block_on(async {
        let accepted = delete(&fixture).await;
        let id = accepted["data"]["operation_id"].as_str().unwrap();
        let receipt = finalized(&fixture, id).await;
        assert_eq!(fixture.raw_binding(), Some(fixture.removed.id.clone()));
        // A WAL reader can observe the Library commit while the Subscribe writer
        // is blocked. GET must stay responsive and report the existing cleaning state.
        let pending = status(&fixture, id).await;
        blocked.commit().unwrap();
        removed(&fixture, id).await;
        assert_eq!(fixture.raw_binding(), None);
        assert_eq!(raw_receipt(&fixture, id), receipt);
        assert_eq!(status(&fixture, id).await["data"]["state"], "succeeded");
        assert_eq!(
            pending["data"]["state"], "cleaning",
            "Library finalization alone cannot expose completion while Subscribe commit is blocked"
        );
    });
}

#[test]
fn failed_subscribe_detachment_retries_without_rewriting_the_succeeded_receipt() {
    let fixture = Fixture::new(MediaKind::Tv);
    let subscribe = Connection::open(fixture.temp.path().join("data/subscribe.db")).unwrap();
    subscribe
        .execute_batch("CREATE TRIGGER fail_library_detach BEFORE UPDATE OF library_id ON subscribes WHEN OLD.library_id IS NOT NULL AND NEW.library_id IS NULL BEGIN SELECT RAISE(ABORT,'injected one-attempt detach failure'); END;")
        .unwrap();
    runtime().block_on(async {
        let accepted = delete(&fixture).await;
        let id = accepted["data"]["operation_id"].as_str().unwrap();
        let receipt = finalized(&fixture, id).await;
        // Repeated DELETE waits for the first cleanup's Store window to finish,
        // proving the aborting trigger has rejected its detachment attempt.
        let repeated = delete(&fixture).await;
        assert_eq!(repeated["data"]["operation_id"], id);
        assert_eq!(fixture.raw_binding(), Some(fixture.removed.id.clone()));
        let pending = status(&fixture, id).await;
        subscribe
            .execute_batch("DROP TRIGGER fail_library_detach")
            .unwrap();
        // Let the existing automatic driver retry; no extra DELETE or startup
        // recovery is allowed to complete the detachment for this assertion.
        tokio::time::timeout(Duration::from_secs(8), async {
            while fixture.raw_binding().is_some() {
                assert_eq!(raw_receipt(&fixture, id), receipt);
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("first automatic retry must finish the independent Subscribe commit");
        assert_eq!(raw_receipt(&fixture, id), receipt);
        assert_eq!(status(&fixture, id).await["data"]["state"], "succeeded");
        assert_eq!(
            repeated["data"]["state"], "cleaning",
            "repeated DELETE cannot expose completion after Subscribe detachment failed"
        );
        assert_eq!(pending["data"]["state"], "cleaning");
    });
}

async fn delete(fixture: &Fixture) -> serde_json::Value {
    let response = fixture
        .app()
        .oneshot(request(
            "DELETE",
            &format!("/api/v1/libraries/{}", fixture.removed.id),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    json(response).await
}

async fn status(fixture: &Fixture, id: &str) -> serde_json::Value {
    let response = tokio::time::timeout(
        Duration::from_secs(1),
        fixture
            .app()
            .oneshot(request("GET", &format!("/api/v1/library-removals/{id}"))),
    )
    .await
    .expect("removal status must remain readable while Subscribe commit is blocked")
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    json(response).await
}

async fn finalized(fixture: &Fixture, id: &str) -> store::LibraryRemoval {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let receipt = raw_receipt(fixture, id);
            if receipt.state == "succeeded" {
                return receipt;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("Library finalization must commit independently of Subscribe detachment")
}

fn raw_receipt(fixture: &Fixture, id: &str) -> store::LibraryRemoval {
    Connection::open(fixture.temp.path().join("data/library.db"))
        .unwrap()
        .query_row(
            "SELECT operation_id,library_id,state,removed_ledger_count,error FROM library_removals WHERE operation_id=?1",
            [id],
            |row| Ok(store::LibraryRemoval {
                operation_id: row.get(0)?,
                library_id: row.get(1)?,
                state: row.get(2)?,
                removed_ledger_count: row.get(3)?,
                error: row.get(4)?,
            }),
        )
        .unwrap()
}
