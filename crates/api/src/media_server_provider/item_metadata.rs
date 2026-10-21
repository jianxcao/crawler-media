use domain::{Media, UserId};
use media_server::provider::MediaItemMetadata;
use std::collections::BTreeMap;

use crate::Store;

mod artwork;
mod nfo;
mod people;

pub(super) use artwork::{backdrop_path, local_primary_image, primary_image_url};
use artwork::{primary_image_metadata, series_primary_image_tag};
use nfo::row_nfo_metadata;
pub(super) use people::{catalog_person_image_url, person_image_url, person_tmdb_id};
use people::{merge_people, people_from_fields, people_from_nfo};

/// Owned per-item playback and probe facts collected with the Jellyfin batch.
pub(super) struct ItemDbFacts {
    resume_state: Option<crate::store::UnitState>,
    tracks: library::Tracks,
    is_favorite: bool,
}

/// Resolve one row's own playback unit and (for series rows) its whole-show unit.
///
/// 单条目路径用它补齐事实；批量路径已经在快照里持有这些单元，直接传引用，
/// 不再为每个条目重查一次 `unit_state`。
pub(super) fn row_units(
    store: &Store,
    user_id: UserId,
    row: &domain::LedgerRow,
    is_series: bool,
) -> (Option<crate::store::PlaybackUnitSnapshot>, Option<crate::store::PlaybackUnitSnapshot>) {
    let unit = {
        let (season, episode) = row_unit_key(row);
        store
            .unit_state(user_id, row.media_id, season, episode)
            .ok()
            .flatten()
            .map(|state| crate::store::PlaybackUnitSnapshot { media_id: row.media_id, season, episode, state })
    };
    let whole_unit = is_series
        .then(|| {
            store
                .unit_state(user_id, row.media_id, crate::store::UNIT_WHOLE, crate::store::UNIT_WHOLE)
                .ok()
                .flatten()
                .map(|state| crate::store::PlaybackUnitSnapshot {
                    media_id: row.media_id,
                    season: crate::store::UNIT_WHOLE,
                    episode: crate::store::UNIT_WHOLE,
                    state,
                })
        })
        .flatten();
    (unit, whole_unit)
}

/// 台账行对应的单元键：有季/集用该集，否则用整片 `(-1,-1)`。
fn row_unit_key(row: &domain::LedgerRow) -> (i32, i32) {
    let season = row.season.map(|value| value as i32).unwrap_or(crate::store::UNIT_WHOLE);
    let episode = row.episode.map(|value| value as i32).unwrap_or(crate::store::UNIT_WHOLE);
    (season, episode)
}

/// Load one item's DB facts while the shared snapshot's Store guard is held.
pub(super) fn item_db_facts(
    store: &Store,
    row: &domain::LedgerRow,
    is_series: bool,
    unit: Option<&crate::store::PlaybackUnitSnapshot>,
    whole_unit: Option<&crate::store::PlaybackUnitSnapshot>,
) -> ItemDbFacts {
    let resume_state = row
        .season
        .zip(row.episode)
        .and_then(|_| unit.map(|unit| unit.state.clone()));
    let tracks = store.get_file_meta(&row.id.to_string()).ok().flatten().unwrap_or_default();
    let favorite = if is_series { whole_unit } else { unit };
    ItemDbFacts { resume_state, tracks, is_favorite: favorite.is_some_and(|unit| unit.state.favorite) }
}

/// 过滤/排序需要的公共字段（genre/studio/person/search 命中）。
///
/// 已采纳的元数据（Web 浏览用的同一份 `media_metadata`）是首选口径：有采纳行时不再
/// stat/读 NFO，只有没有采纳行的媒体才回退到 NFO。字段口径与 [`item_metadata`] 一致，
/// 避免两套语义。
pub(super) fn filter_facts(
    row: &domain::LedgerRow,
    media: &Media,
    is_series: bool,
    adopted: Option<&store::library_query::AdoptedFilterFacts>,
) -> MediaItemMetadata {
    if let Some(adopted) = adopted {
        return MediaItemMetadata {
            overview: adopted.overview.clone(),
            genres: adopted.genres.clone(),
            studios: adopted.studios.clone(),
            original_title: media.original_title.clone(),
            people: adopted
                .people
                .iter()
                .map(|person| crate::media_server_provider::MediaPerson {
                    id: person
                        .tmdb_id
                        .clone()
                        .unwrap_or_else(|| person.name.clone()),
                    name: person.name.clone(),
                    role: person.role.clone(),
                    person_type: "Actor".into(),
                    tmdb_id: person.tmdb_id.clone(),
                    primary_image_url: None,
                })
                .collect(),
            ..Default::default()
        };
    }
    let nfo = row_nfo_metadata(row, media, is_series);
    MediaItemMetadata {
        overview: nfo.as_ref().and_then(|metadata| metadata.plot.clone()),
        genres: nfo
            .as_ref()
            .map(|metadata| metadata.genres.clone())
            .unwrap_or_default(),
        studios: nfo
            .as_ref()
            .map(|metadata| metadata.studios.clone())
            .unwrap_or_default(),
        original_title: nfo
            .as_ref()
            .and_then(|metadata| metadata.original_title.clone())
            .or_else(|| media.original_title.clone()),
        people: people_from_nfo(nfo.as_ref()),
        ..Default::default()
    }
}

/// 锁外渲染：stat/外部字幕 IO 都在这里，调用方必须先释放 Store 锁。
///
/// `fields` 是调用方在锁内取好的页内合成快照（Task 8 观测输入 + Task 7 采纳）：页内条目
/// 只呈现已经落库的口径，不在这里读现场 NFO。
pub(super) fn item_metadata(
    facts: ItemDbFacts,
    fields: &domain::MetadataFields,
    browse: &dyn crate::browse_file_read::BrowseFileRead,
    row: &domain::LedgerRow,
    media: &Media,
    is_series: bool,
    child_counts: Option<(usize, usize)>,
) -> MediaItemMetadata {
    let ItemDbFacts {
        resume_state,
        mut tracks,
        is_favorite,
    } = facts;
    let (has_primary_image, primary_image_url) = primary_image_metadata(row, media, is_series);
    let primary_image_tag = series_primary_image_tag(row, media, is_series);
    let (child_count, local_season_count) = child_counts
        .map(|(episodes, seasons)| (Some(episodes), Some(seasons)))
        .unwrap_or((None, None));
    let season_count = fields
        .number_of_seasons
        .value()
        .map(|count| *count as usize)
        .or(local_season_count);
    match browse.external_subtitles(std::path::Path::new(&row.path)) {
        Ok(sidecars) => {
            for sidecar in sidecars {
                let already_present = tracks
                    .subtitles
                    .iter()
                    .any(|subtitle| subtitle.is_external && subtitle.path == sidecar.path);
                if !already_present {
                    tracks.subtitles.push(sidecar);
                }
            }
        }
        Err(error) => tracing::warn!(
            %error,
            path = %row.path,
            "failed to discover Jellyfin external subtitles"
        ),
    }

    let runtime_ticks = runtime_ticks(fields, &tracks, is_series);
    let mut item = metadata_from_fields(fields, media, is_series);
    item.runtime_ticks = runtime_ticks;
    item.is_favorite = is_favorite;
    item.resume_updated_at = resume_state
        .map(|state| state.updated_at)
        .unwrap_or_default();
    item.child_count = child_count;
    item.season_count = season_count;
    item.has_primary_image = has_primary_image;
    item.has_backdrop_image = backdrop_path(row, is_series).is_some();
    item.primary_image_tag = primary_image_tag;
    item.primary_image_url = primary_image_url;
    item.tracks = tracks;
    item
}

/// 合成字段 → Jellyfin 字段；缺失与空值一律留空，交给 `merge_catalog_metadata` 兜底。
fn metadata_from_fields(
    fields: &domain::MetadataFields,
    media: &Media,
    is_series: bool,
) -> MediaItemMetadata {
    MediaItemMetadata {
        overview: fields.overview.value().cloned(),
        community_rating: fields.rating.value().copied(),
        vote_count: fields.vote_count.value().copied(),
        genres: fields
            .genres
            .value()
            .map(|genres| genres.iter().map(|genre| genre.name.clone()).collect())
            .unwrap_or_default(),
        premiere_date: if is_series {
            fields.release_date.value().cloned()
        } else {
            fields
                .air_date
                .value()
                .cloned()
                .or_else(|| fields.release_date.value().cloned())
        },
        end_date: fields.end_date.value().cloned(),
        official_rating: fields.content_rating.value().cloned(),
        status: fields.status.value().cloned(),
        original_language: fields.language.value().cloned(),
        taglines: fields.tagline.value().cloned().into_iter().collect(),
        studios: fields.studios.value().cloned().unwrap_or_default(),
        production_locations: fields.countries.value().cloned().unwrap_or_default(),
        number_of_episodes: fields.number_of_episodes.value().copied(),
        production_year: fields
            .year
            .value()
            .map(|year| i32::from(*year))
            .or_else(|| media.year.map(i32::from)),
        original_title: fields
            .original_title
            .value()
            .cloned()
            .or_else(|| media.original_title.clone()),
        provider_ids: provider_ids(media),
        episode_name: (!is_series)
            .then(|| fields.title.value().cloned())
            .flatten(),
        people: people_from_fields(fields),
        ..Default::default()
    }
}

pub(super) fn merge_catalog_metadata(
    mut item: MediaItemMetadata,
    media: &Media,
    catalog: &media::ItemMeta,
    episode: Option<&media::EpisodeMeta>,
) -> MediaItemMetadata {
    let episode_overview = episode.and_then(|metadata| metadata.overview.clone());
    if item.overview.as_deref().is_none_or(str::is_empty) {
        item.overview = episode_overview.or_else(|| catalog.overview.clone());
    }
    if item.community_rating.is_none() {
        item.community_rating = episode
            .and_then(|metadata| metadata.rating.as_deref())
            .or(catalog.rating.as_deref())
            .and_then(|rating| rating.parse().ok());
    }
    item.vote_count = item.vote_count.or_else(|| {
        episode
            .and_then(|metadata| metadata.vote_count)
            .or(catalog.vote_count)
    });
    item.genres = merge_names(item.genres, catalog.genres.iter().cloned());
    item.production_year = item.production_year.or_else(|| {
        catalog
            .release_date
            .as_deref()
            .and_then(|date| date.get(..4))
            .and_then(|year| year.parse().ok())
    });
    item.original_title = item.original_title.or_else(|| media.original_title.clone());
    if item.premiere_date.is_none() {
        item.premiere_date = episode
            .and_then(|metadata| metadata.air_date.clone())
            .or_else(|| catalog.release_date.clone());
    }
    item.end_date = item.end_date.or_else(|| catalog.last_air_date.clone());
    item.official_rating = item
        .official_rating
        .or_else(|| catalog.content_rating.clone());
    item.status = item.status.or_else(|| catalog.status.clone());
    item.original_language = item
        .original_language
        .or_else(|| catalog.original_language.clone());
    if item.taglines.is_empty() {
        item.taglines = catalog.tagline.clone().into_iter().collect();
    }
    item.studios = merge_names(item.studios, catalog.studios.iter().cloned());
    item.production_locations = merge_names(
        item.production_locations,
        catalog.origin_countries.iter().cloned(),
    );
    item.number_of_episodes = catalog.number_of_episodes.or(item.number_of_episodes);
    item.season_count = catalog
        .number_of_seasons
        .map(|count| count as usize)
        .or(item.season_count);
    if item.runtime_ticks.is_none() {
        item.runtime_ticks = episode
            .and_then(|metadata| metadata.runtime_minutes)
            .or_else(|| catalog.runtime_minutes.as_deref()?.parse::<u32>().ok())
            .and_then(minutes_to_ticks);
    }

    let tmdb_nfo = crate::scrape_metadata::nfo_from_tmdb(media, catalog);
    let catalog_people = people_from_nfo(Some(&tmdb_nfo));
    merge_people(&mut item.people, catalog_people);
    if let Some(episode) = episode {
        if item.episode_name.as_deref().is_none_or(str::is_empty) {
            item.episode_name = episode.name.clone();
        }
    }
    item
}

fn merge_names(mut current: Vec<String>, fallback: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = current
        .iter()
        .map(|name| name.to_lowercase())
        .collect::<std::collections::HashSet<_>>();
    for name in fallback {
        if seen.insert(name.to_lowercase()) {
            current.push(name);
        }
    }
    current
}

fn minutes_to_ticks(minutes: u32) -> Option<i64> {
    i64::from(minutes).checked_mul(60)?.checked_mul(10_000_000)
}

fn provider_ids(media: &Media) -> BTreeMap<String, String> {
    [
        ("Tmdb", media.tmdb_id.as_ref()),
        ("Tvdb", media.tvdb_id.as_ref()),
        ("Douban", media.douban_id.as_ref()),
        ("Bangumi", media.bangumi_id.as_ref()),
        ("AniList", media.anilist_id.as_ref()),
    ]
    .into_iter()
    .filter_map(|(name, value)| value.map(|value| (name.to_string(), value.clone())))
    .collect()
}

fn runtime_ticks(
    fields: &domain::MetadataFields,
    tracks: &library::Tracks,
    is_series: bool,
) -> Option<i64> {
    if is_series {
        return None;
    }
    fields
        .runtime_minutes
        .value()
        .and_then(|minutes| minutes_to_ticks(*minutes))
        .or_else(|| {
            tracks
                .video
                .as_ref()
                .and_then(|video| video.duration_secs)
                .map(|seconds| (seconds * 10_000_000.0).round() as i64)
        })
}

/// 页内水合的实体范围：Series 行按 work 打分，剧集行按 episode 打分。
pub(super) fn page_scope(
    row: &domain::LedgerRow,
    media: &Media,
    is_series: bool,
) -> domain::EntityScope {
    if !is_series && media.kind == domain::MediaKind::Tv {
        if let (Some(season), Some(episode)) = (row.season, row.episode) {
            return domain::EntityScope::Episode { season, episode };
        }
    }
    domain::EntityScope::Work
}

/// 锁内取页内合成元数据（Task 8 观测输入 + Task 7 采纳），调用方持有 Store 锁。
pub(super) fn page_fields(
    store: &Store,
    row: &domain::LedgerRow,
    media: &Media,
    is_series: bool,
) -> Result<domain::MetadataFields, String> {
    let Some(library_id) = crate::http::playback::library_id_for(store, media, &[row]) else {
        tracing::warn!(
            media_id = %row.media_id,
            file_id = %row.id,
            "page metadata skipped: 无法定位条目所属库"
        );
        return Ok(domain::MetadataFields::default());
    };
    let Ok(library_id) = library_id.parse::<domain::LibraryId>() else {
        tracing::warn!(
            library_id = %library_id,
            media_id = %row.media_id,
            "page metadata skipped: 库 ID 不是 UUID"
        );
        return Ok(domain::MetadataFields::default());
    };
    store
        .protocol_page_metadata(
            library_id,
            row.media_id,
            page_scope(row, media, is_series),
            None,
        )
        .map_err(|error| error.to_string())
}
