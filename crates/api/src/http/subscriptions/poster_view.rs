use domain::{LedgerId, Media, MediaId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SubscriptionPoster {
    Library(LedgerId),
    Catalog(MediaId),
    None,
}

pub(crate) fn poster_url(poster: SubscriptionPoster) -> Option<String> {
    match poster {
        SubscriptionPoster::Library(id) => {
            Some(crate::http::library::artwork_url("posters", id))
        }
        SubscriptionPoster::Catalog(id) => {
            Some(format!("/media/{id}/poster"))
        }
        SubscriptionPoster::None => None,
    }
}

pub(crate) fn determine_poster_from_candidate(
    media: &Media,
    owned_library_poster: Option<LedgerId>,
) -> SubscriptionPoster {
    if let Some(ledger_id) = owned_library_poster {
        return SubscriptionPoster::Library(ledger_id);
    }
    if media.tmdb_id.is_some() {
        return SubscriptionPoster::Catalog(media.id);
    }
    SubscriptionPoster::None
}
