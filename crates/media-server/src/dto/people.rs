use serde_json::{Value, json};

use crate::provider::MediaPerson;

pub fn media_person_json(person: &MediaPerson) -> Value {
    let mut value = json!({
        "Id": person.id,
        "Name": person.name,
        "Role": person.role,
        "Type": person.person_type,
        "ProviderIds": person
            .tmdb_id
            .as_ref()
            .map(|id| json!({ "Tmdb": id }))
            .unwrap_or_else(|| json!({})),
    });
    if person.primary_image_url.is_some() {
        value["PrimaryImageTag"] = json!("profile");
    }
    value
}

pub fn person_item_json(person: &MediaPerson) -> Value {
    let mut value = json!({
        "Id": person.id,
        "Name": person.name,
        "Type": "Person",
        "IsFolder": true,
        "ProviderIds": person
            .tmdb_id
            .as_ref()
            .map(|id| json!({ "Tmdb": id }))
            .unwrap_or_else(|| json!({})),
    });
    if person.primary_image_url.is_some() {
        value["ImageTags"] = json!({ "Primary": "profile" });
    }
    value
}
