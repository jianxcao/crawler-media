//! TMDB person + combined credits parsing (`/person/{id}?append_to_response=combined_credits`).

use domain::MediaKind;
use serde::Deserialize;

use crate::client::TmdbError;

/// One credit row in a person's combined credits (deduped movie/TV list).
#[derive(Clone, Debug, PartialEq)]
pub struct PersonCredit {
    pub kind: MediaKind,
    pub tmdb_id: i64,
    pub title: String,
    pub year: Option<u16>,
    pub poster_path: Option<String>,
}

/// TMDB person + combined credits for the person detail page.
#[derive(Clone, Debug, Default)]
pub struct PersonDetails {
    pub name: String,
    pub profile_path: Option<String>,
    pub items: Vec<PersonCredit>,
}

#[derive(Deserialize)]
struct PersonResponse {
    name: Option<String>,
    #[serde(default)]
    profile_path: Option<String>,
    #[serde(default)]
    combined_credits: PersonCredits,
}

#[derive(Deserialize, Default)]
struct PersonCredits {
    #[serde(default)]
    cast: Vec<PersonCreditRaw>,
}

#[derive(Deserialize)]
struct PersonCreditRaw {
    id: i64,
    #[serde(default)]
    media_type: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    first_air_date: Option<String>,
    #[serde(default)]
    poster_path: Option<String>,
}

/// Parse `GET /person/{id}?append_to_response=combined_credits`.
pub fn person_details(body: &str) -> Result<PersonDetails, TmdbError> {
    let parsed: PersonResponse = serde_json::from_str(body)?;
    let mut seen = std::collections::HashSet::new();
    let items = parsed
        .combined_credits
        .cast
        .into_iter()
        .filter_map(|credit| {
            let kind = match credit.media_type.as_deref() {
                Some("movie") => MediaKind::Movie,
                Some("tv") => MediaKind::Tv,
                _ => return None,
            };
            let title = credit.title.or(credit.name)?;
            if title.is_empty() || !seen.insert((kind.as_str().to_string(), credit.id)) {
                return None;
            }
            let year = credit
                .release_date
                .as_deref()
                .or(credit.first_air_date.as_deref())
                .and_then(|date| date.split('-').next())
                .and_then(|year| year.parse::<u16>().ok());
            Some(PersonCredit {
                kind,
                tmdb_id: credit.id,
                title,
                year,
                poster_path: credit.poster_path.filter(|p| !p.is_empty()),
            })
        })
        .collect();
    Ok(PersonDetails {
        name: parsed.name.unwrap_or_default(),
        profile_path: parsed.profile_path.filter(|p| !p.is_empty()),
        items,
    })
}
