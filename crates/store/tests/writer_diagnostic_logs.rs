//! Recursive observers suppress callbacks, while internal failures still log promptly.
use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
use std::sync::{Arc, Mutex};
use store::{Db, Store};
use tracing::{
    Event, Metadata, Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id, Record},
};

#[derive(Clone, Default)]
struct Logs(Arc<Mutex<Vec<String>>>);
struct Message(String);
impl Visit for Message {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}
impl Subscriber for Logs {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut message = Message(String::new());
        event.record(&mut message);
        self.0.lock().unwrap().push(message.0);
    }
}

#[test]
fn reentrant_writer_cleanup_failure_logs_before_the_observer_scope_returns() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let writers = store.writers();
    let sink = writers.clone();
    let logs = Logs::default();
    let messages = logs.clone();
    tracing::subscriber::with_default(logs, || {
        store.with_writer_diagnostics(move |_| {
            let result: Result<(), _> = sink.with_writer(Db::Library, |db| {
                db.authorizer(Some(|context: AuthContext<'_>| match context.action {
                    AuthAction::Transaction { operation: TransactionOperation::Rollback } => Authorization::Deny,
                    _ => Authorization::Allow,
                }));
                db.execute_batch("BEGIN IMMEDIATE")?;
                Err(store::StoreError::Protected("observer business error".into()))
            });
            assert!(matches!(result, Err(store::StoreError::Protected(message)) if message == "observer business error"));
        }, || writers.with_writer(Db::Library, |_| Ok(())).unwrap());
        // Assert before any unrelated write could accidentally flush a stranded error.
        let captured = messages.0.lock().unwrap();
        assert!(
            captured
                .iter()
                .any(|message| message.contains("failed to clean up Store writer transaction")),
            "recursive diagnostic delivery must not strand cleanup failure logs: {captured:?}"
        );
    });
    writers
        .with_writer(Db::Library, |db| {
            db.authorizer(None::<fn(AuthContext<'_>) -> Authorization>);
            db.execute_batch("ROLLBACK")?;
            Ok(())
        })
        .unwrap();
}
