use serde_json::{Value, json};

use crate::provider::MediaItemSnapshot;

pub fn render_chapters_json(snapshot: &MediaItemSnapshot) -> Vec<Value> {
    let mut chapters = snapshot
        .chapters
        .iter()
        .map(|chapter| {
            let mut value = json!({
                "StartPositionTicks": chapter.start_ms * 10_000,
                "Name": chapter.title.as_deref().unwrap_or("Chapter"),
            });
            if let Some(marker_type) = chapter.marker_type {
                let marker = match marker_type {
                    library::MarkerType::IntroStart => "IntroStart",
                    library::MarkerType::IntroEnd => "IntroEnd",
                    library::MarkerType::CreditsStart => "CreditsStart",
                };
                value["MarkerType"] = json!(marker);
            }
            value
        })
        .collect::<Vec<_>>();

    if let (Some(start_ms), Some(end_ms)) = (snapshot.intro_start_ms, snapshot.intro_end_ms) {
        chapters.retain(|chapter| chapter["MarkerType"] != "IntroStart");
        chapters.push(json!({
            "StartPositionTicks": start_ms * 10_000,
            "Name": "片头",
            "MarkerType": "IntroStart",
            "EndPositionTicks": end_ms * 10_000,
        }));
    }
    if let Some(start_ms) = snapshot.outro_start_ms {
        chapters.retain(|chapter| chapter["MarkerType"] != "CreditsStart");
        chapters.push(json!({
            "StartPositionTicks": start_ms * 10_000,
            "Name": "片尾",
            "MarkerType": "CreditsStart",
            "EndPositionTicks": snapshot.outro_end_ms.unwrap_or(start_ms + 60_000) * 10_000,
        }));
    }

    chapters.sort_by_key(|chapter| chapter["StartPositionTicks"].as_i64().unwrap_or(0));
    chapters
}
