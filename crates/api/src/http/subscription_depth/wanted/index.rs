//! Index only requested units; each pending title is parsed once. First-match
//! precedence is retained without expanding arbitrary release ranges.
use std::collections::{BTreeSet, HashMap, HashSet};

pub(crate) type Unit = (Option<u32>, Option<u32>);
pub(crate) struct PendingUnitIndex(HashMap<Unit, usize>);

impl PendingUnitIndex {
    pub(crate) fn new(pending: &[(i32, crate::store::PendingDownload)], units: &[Unit]) -> Self {
        let mut remaining: BTreeSet<Unit> = units.iter().copied().collect();
        let mut index = HashMap::new();
        for (position, (_, pending)) in pending.iter().enumerate() {
            if remaining.is_empty() {
                break;
            }
            let release = release::parse(&pending.torrent.title);
            let selected: Vec<_> = match (release.season, release.episode_span()) {
                (Some(season), Some((from, to))) => remaining
                    .range((Some(season), Some(from))..=(Some(season), Some(to)))
                    .copied()
                    .collect(),
                (season, None) => remaining
                    .get(&(season, None))
                    .copied()
                    .into_iter()
                    .collect(),
                (None, Some(_)) => remaining.get(&(None, None)).copied().into_iter().collect(),
            };
            for unit in selected {
                remaining.remove(&unit);
                index.insert(unit, position);
            }
        }
        Self(index)
    }

    pub(crate) fn get<'a>(
        &self,
        unit: Unit,
        pending: &'a [(i32, crate::store::PendingDownload)],
    ) -> Option<&'a crate::store::PendingDownload> {
        self.0.get(&unit).map(|position| &pending[*position].1)
    }
}

pub(crate) fn coverage_labels(
    rows: &[domain::FileCoverage],
    media: domain::MediaId,
    units: &[Unit],
) -> HashMap<Unit, Option<String>> {
    let requested: HashSet<_> = units.iter().copied().collect();
    let mut labels = HashMap::new();
    for row in rows.iter().filter(|row| row.file.media_id == media) {
        let label = match (row.file.resolution.as_deref(), row.file.codec.as_deref()) {
            (Some(resolution), Some(codec)) => Some(format!("{resolution} {codec}")),
            (Some(value), None) | (None, Some(value)) => Some(value.to_owned()),
            (None, None) => None,
        };
        for unit in &row.units {
            let key = (
                u32::try_from(unit.season).ok(),
                u32::try_from(unit.episode).ok(),
            );
            if requested.contains(&key) {
                labels.entry(key).or_insert_with(|| label.clone());
            }
        }
    }
    labels
}
