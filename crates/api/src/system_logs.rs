use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::field::Visit;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;

pub const DEFAULT_CAPACITY: usize = 3000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SystemLogEntry {
    pub id: u64,
    pub timestamp: i64,
    pub level: String,
    pub target: String,
    pub message: String,
}

#[derive(Clone)]
pub struct LogBuffer {
    inner: Arc<RwLock<BufferInner>>,
    sender: broadcast::Sender<SystemLogEntry>,
}

struct BufferInner {
    entries: VecDeque<SystemLogEntry>,
    capacity: usize,
    next_id: u64,
}

impl LogBuffer {
    pub fn new(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(1024);
        Self {
            inner: Arc::new(RwLock::new(BufferInner {
                entries: VecDeque::with_capacity(capacity),
                capacity,
                next_id: 1,
            })),
            sender,
        }
    }

    pub fn push(&self, level: &str, target: &str, message: String) -> SystemLogEntry {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let mut lock = self.inner.write();
        let id = lock.next_id;
        lock.next_id = lock.next_id.wrapping_add(1);

        let entry = SystemLogEntry {
            id,
            timestamp: now,
            level: level.to_ascii_uppercase(),
            target: target.to_string(),
            message,
        };

        if lock.entries.len() >= lock.capacity {
            lock.entries.pop_front();
        }
        lock.entries.push_back(entry.clone());
        drop(lock);

        let _ = self.sender.send(entry.clone());
        entry
    }

    pub fn clear(&self) {
        let mut lock = self.inner.write();
        lock.entries.clear();
    }

    pub fn query(
        &self,
        level: Option<&str>,
        target: Option<&str>,
        keyword: Option<&str>,
        limit: Option<usize>,
    ) -> Vec<SystemLogEntry> {
        let lock = self.inner.read();
        let target_norm = target.map(|t| t.to_ascii_lowercase());
        let keyword_norm = keyword.map(|k| k.to_ascii_lowercase());
        let level_norm = level.map(|l| l.to_ascii_uppercase());

        let iter = lock.entries.iter().rev().filter(|entry| {
            if let Some(ref l) = level_norm {
                if !l.is_empty() && &entry.level != l {
                    return false;
                }
            }
            if let Some(ref t) = target_norm {
                if !t.is_empty() && !entry.target.to_ascii_lowercase().contains(t) {
                    return false;
                }
            }
            if let Some(ref k) = keyword_norm {
                if !k.is_empty() && !entry.message.to_ascii_lowercase().contains(k) {
                    return false;
                }
            }
            true
        });

        let take_count = limit.unwrap_or(lock.capacity);
        let mut results: Vec<SystemLogEntry> = iter.take(take_count).cloned().collect();
        // Return in chronological order (oldest to newest)
        results.reverse();
        results
    }

    pub fn subscribe(&self) -> broadcast::Receiver<SystemLogEntry> {
        self.sender.subscribe()
    }
}

pub struct LogBufferLayer {
    buffer: LogBuffer,
}

impl LogBufferLayer {
    pub fn new(buffer: LogBuffer) -> Self {
        Self { buffer }
    }
}

impl<S> Layer<S> for LogBufferLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let level_str = match *metadata.level() {
            Level::ERROR => "ERROR",
            Level::WARN => "WARN",
            Level::INFO => "INFO",
            Level::DEBUG => "DEBUG",
            Level::TRACE => "TRACE",
        };

        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);

        let mut full_msg = visitor.message.unwrap_or_default();
        if !visitor.fields.is_empty() {
            let fields_str = visitor
                .fields
                .into_iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(" ");
            if full_msg.is_empty() {
                full_msg = fields_str;
            } else {
                full_msg = format!("{full_msg} ({fields_str})");
            }
        }
        if !full_msg.is_empty() {
            self.buffer.push(level_str, metadata.target(), full_msg);
        }
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: Option<String>,
    fields: Vec<(String, String)>,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}").trim_matches('"').to_string());
        } else {
            self.fields.push((
                field.name().to_string(),
                format!("{value:?}").trim_matches('"').to_string(),
            ));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = Some(value.to_string());
        } else {
            self.fields
                .push((field.name().to_string(), value.to_string()));
        }
    }
}
