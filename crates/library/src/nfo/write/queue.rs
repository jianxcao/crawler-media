//! 计算需要就地增删的 NFO 节点（身份、元数据标量/数字/列表）。

use domain::Media;

use super::super::parse::NfoMeta;
use super::elements::{
    XmlEdit, queue_cast, queue_list, queue_optional_number, queue_optional_scalar, queue_ratings,
    queue_scalar,
};

pub(super) fn queue_nfo_edits(
    body: &str,
    root: roxmltree::Node<'_, '_>,
    media: &Media,
    meta: Option<&NfoMeta>,
    is_episode: bool,
    existing: &NfoMeta,
    edits: &mut Vec<XmlEdit>,
    insertions: &mut String,
) {
    queue_nfo_identity(body, root, media, meta, is_episode, edits, insertions);
    if let Some(metadata) = meta {
        queue_nfo_metadata(body, root, metadata, existing, edits, insertions);
    }
}

fn queue_nfo_identity(
    body: &str,
    root: roxmltree::Node<'_, '_>,
    media: &Media,
    meta: Option<&NfoMeta>,
    is_episode: bool,
    edits: &mut Vec<XmlEdit>,
    insertions: &mut String,
) {
    let title = meta
        .and_then(|item| item.title.as_deref())
        .filter(|value| !value.is_empty());
    queue_scalar(
        body,
        root,
        &["title", "name", "localtitle"],
        "title",
        title.unwrap_or(&media.title),
        edits,
        insertions,
    );
    let original_title = meta
        .and_then(|item| item.original_title.as_deref())
        .filter(|value| !value.is_empty())
        .or(media.original_title.as_deref())
        .filter(|value| !value.is_empty());
    queue_optional_scalar(
        body,
        root,
        &["originaltitle", "original_title"],
        "originaltitle",
        original_title,
        edits,
        insertions,
    );
    if !is_episode {
        if let Some(year) = media.year {
            queue_scalar(
                body,
                root,
                &["year"],
                "year",
                &year.to_string(),
                edits,
                insertions,
            );
        }
    }
}

fn queue_nfo_metadata(
    body: &str,
    root: roxmltree::Node<'_, '_>,
    metadata: &NfoMeta,
    existing: &NfoMeta,
    edits: &mut Vec<XmlEdit>,
    insertions: &mut String,
) {
    queue_metadata_scalars(body, root, metadata, edits, insertions);
    queue_metadata_numbers(body, root, metadata, edits, insertions);
    queue_metadata_lists(body, root, metadata, edits, insertions);
    queue_cast(body, root, &metadata.cast, edits, insertions);
    if metadata.rating.is_some() || metadata.vote_count.is_some() {
        queue_ratings(
            body,
            root,
            metadata.rating.as_deref().or(existing.rating.as_deref()),
            metadata.vote_count.or(existing.vote_count),
            edits,
            insertions,
        );
    }
}

macro_rules! queue_fields {
    ($body:ident, $root:ident, $edits:ident, $insertions:ident; $queue:ident; $($aliases:expr => $output:literal => $value:expr),* $(,)?) => {
        $( $queue($body, $root, $aliases, $output, $value, $edits, $insertions); )*
    };
}

fn queue_metadata_scalars(
    body: &str,
    root: roxmltree::Node<'_, '_>,
    meta: &NfoMeta,
    edits: &mut Vec<XmlEdit>,
    insertions: &mut String,
) {
    queue_fields!(body, root, edits, insertions; queue_optional_scalar;
        &["plot", "overview", "outline"] => "plot" => meta.plot.as_deref(),
        &["tagline"] => "tagline" => meta.tagline.as_deref(),
        &["runtime", "runtime_minutes"] => "runtime" => meta.runtime_minutes.as_deref(),
        &["mpaa", "contentrating", "certification"] => "mpaa" => meta.content_rating.as_deref(),
        &["premiered", "firstaired", "release_date"] => "premiered" => meta.premiered.as_deref(),
        &["enddate", "ended", "lastaired", "end_date"] => "enddate" => meta.end_date.as_deref(),
        &["original_language", "originallanguage"] => "original_language" => meta.original_language.as_deref(),
        &["status"] => "status" => meta.status.as_deref(),
        &["thumb"] => "thumb" => meta.thumb.as_deref(),
        &["aired"] => "aired" => meta.aired.as_deref(),
    );
}

fn queue_metadata_numbers(
    body: &str,
    root: roxmltree::Node<'_, '_>,
    meta: &NfoMeta,
    edits: &mut Vec<XmlEdit>,
    insertions: &mut String,
) {
    queue_fields!(body, root, edits, insertions; queue_optional_number;
        &["season"] => "season" => meta.season,
        &["episode"] => "episode" => meta.episode,
        &["seasoncount", "numberofseasons", "number_of_seasons"] => "seasoncount" => meta.number_of_seasons,
        &["episodecount", "numberofepisodes", "number_of_episodes"] => "episodecount" => meta.number_of_episodes,
    );
}

fn queue_metadata_lists(
    body: &str,
    root: roxmltree::Node<'_, '_>,
    meta: &NfoMeta,
    edits: &mut Vec<XmlEdit>,
    insertions: &mut String,
) {
    queue_fields!(body, root, edits, insertions; queue_list;
        &["genre", "tag"] => "genre" => &meta.genres,
        &["country"] => "country" => &meta.countries,
        &["studio", "network", "productioncompany"] => "studio" => &meta.studios,
        &["director"] => "director" => &meta.directors,
        &["creator"] => "creator" => &meta.creators,
    );
}
