use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct EpisodeRow {
    pub season: u32,
    pub episode: u32,
    pub file_ids: Vec<String>,
    pub file_name: String,
    pub local_still: Option<String>,
    pub tmdb_still: Option<String>,
    pub backdrop: Option<String>,
    pub name: Option<String>,
    pub overview: Option<String>,
    pub position_ms: i64,
    pub played: bool,
    pub progress_percent: Option<i64>,
}

pub fn format_episodes_json(episodes: &[EpisodeRow], media_item_id: &str) -> Vec<Value> {
    episodes
        .iter()
        .map(|ep| {
            let still_url = ep
                .local_still
                .clone()
                .or_else(|| ep.tmdb_still.clone())
                .or_else(|| ep.backdrop.clone());
            let name = ep.name.clone().unwrap_or_else(|| ep.file_name.clone());
            json!({
                "episode_number": ep.episode,
                "name": name,
                "overview": ep.overview,
                "air_date": null,
                "still_url": still_url,
                "owned": true,
                "file_ids": ep.file_ids,
                "position_ms": ep.position_ms,
                "played": ep.played,
                "progress_percent": ep.progress_percent,
                "media_item_id": media_item_id,
                "season_number": ep.season,
            })
        })
        .collect()
}
