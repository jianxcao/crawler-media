//! A bounded dispatch aid; SQLite owns every pending analysis obligation.
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SeasonKey {
    pub media_id: domain::MediaId,
    pub season: u32,
}

impl Ord for SeasonKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.media_id.to_string(), self.season).cmp(&(other.media_id.to_string(), other.season))
    }
}

impl PartialOrd for SeasonKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalysisTicket {
    pub key: SeasonKey,
    pub generation: u64,
    pub membership_digest: String,
}

struct Dirty {
    digest: String,
    first_ms: i64,
    last_ms: i64,
}

#[derive(Default)]
struct Entry {
    dirty: Option<Dirty>,
    running: Option<AnalysisTicket>,
}

/// Holds only the admitted horizon. Work outside it stays queued in Store.
pub struct SeasonAnalysisSchedule {
    quiet_ms: i64,
    max_wait_ms: i64,
    generation: u64,
    entries: BTreeMap<SeasonKey, Entry>,
}

impl SeasonAnalysisSchedule {
    pub fn new(quiet_ms: i64, max_wait_ms: i64) -> Self {
        Self {
            quiet_ms: quiet_ms.max(0),
            max_wait_ms: max_wait_ms.max(0),
            generation: 0,
            entries: BTreeMap::new(),
        }
    }

    pub fn mark_dirty(&mut self, key: SeasonKey, digest: String, now_ms: i64) {
        if !self.entries.contains_key(&key) && self.entries.len() >= 16 {
            return;
        }
        let entry = self.entries.entry(key).or_default();
        if let Some(dirty) = &mut entry.dirty {
            if dirty.digest != digest {
                dirty.digest = digest;
                dirty.last_ms = now_ms;
            }
        } else if !entry
            .running
            .as_ref()
            .is_some_and(|run| run.membership_digest == digest)
        {
            entry.dirty = Some(Dirty {
                digest,
                first_ms: now_ms,
                last_ms: now_ms,
            });
        }
    }

    pub fn take_due(&mut self, now_ms: i64) -> Option<AnalysisTicket> {
        let key = self
            .entries
            .iter()
            .filter_map(|(key, entry)| {
                if entry.running.is_some() {
                    return None;
                }
                let dirty = entry.dirty.as_ref()?;
                let due = dirty
                    .last_ms
                    .saturating_add(self.quiet_ms)
                    .min(dirty.first_ms.saturating_add(self.max_wait_ms));
                (due <= now_ms).then_some((due, *key))
            })
            .min()
            .map(|(_, key)| key)?;
        let entry = self.entries.get_mut(&key)?;
        let dirty = entry.dirty.take()?;
        self.generation = self.generation.checked_add(1)?;
        let ticket = AnalysisTicket {
            key,
            generation: self.generation,
            membership_digest: dirty.digest,
        };
        entry.running = Some(ticket.clone());
        Some(ticket)
    }

    pub fn finish(&mut self, ticket: &AnalysisTicket, _now_ms: i64) {
        let Some(entry) = self.entries.get_mut(&ticket.key) else {
            return;
        };
        if entry.running.as_ref() != Some(ticket) {
            return;
        }
        entry.running = None;
        if entry.dirty.is_none() {
            self.entries.remove(&ticket.key);
        }
    }
}
