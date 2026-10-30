//! Batch database facts; filesystem rendering happens after the reader is returned.
use super::*;

pub(super) fn facts<'a>(
    item: &LightItem,
    hydration: Option<&'a store::HydratedFile>,
) -> (
    item_metadata::ItemDbFacts,
    domain::MetadataFields,
    Vec<library::ChapterMarker>,
    Option<&'a store::StoredMediaMarker>,
) {
    let coords = snapshot_unit_coords(item.snapshot.unit);
    let scope = item_metadata::page_scope(coords, &item.snapshot.media, item.snapshot.is_series);
    let facts = item_metadata::item_db_facts_with_tracks(
        hydration
            .map(|facts| facts.tracks.clone())
            .unwrap_or_default(),
        coords,
        item.snapshot.is_series,
        item.unit.as_ref(),
        item.whole_unit.as_ref(),
    );
    let fields = hydration
        .and_then(|facts| facts.fields.get(&scope))
        .cloned()
        .unwrap_or_default();
    let chapters = hydration
        .map(|facts| facts.chapters.clone())
        .unwrap_or_default();
    let (season, episode) = unit_columns(coords);
    let marker_key = (season.unwrap_or(1) as i32, episode.unwrap_or(1) as i32);
    let marker = hydration.and_then(|facts| facts.markers.get(&marker_key));
    (facts, fields, chapters, marker)
}
