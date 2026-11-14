//! NFO 解析：根元素识别、字段存在性（missing/empty/value）与 `NfoMeta` 值视图。

use std::path::Path;

/// 单个元素的文本存在性。元素缺失与元素存在但为空是两种不同事实：
/// `<plot/>` 是显式清空（`Empty`），没有 `<plot>` 才是 `Missing`。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum NfoText {
    #[default]
    Missing,
    Empty,
    Value(String),
}

impl NfoText {
    fn value(&self) -> Option<&str> {
        match self {
            Self::Value(text) => Some(text),
            _ => None,
        }
    }

    pub(crate) fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }

    /// 值视图：`Empty` 与 `Missing` 都折叠为 `None`（`NfoMeta` 只有“有无值”两档）。
    fn into_option(self) -> Option<String> {
        match self {
            Self::Value(text) => Some(text),
            _ => None,
        }
    }
}

/// 重复元素（genre/country/actor…）的存在性。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum NfoList<T> {
    #[default]
    Missing,
    Empty,
    Value(Vec<T>),
}

impl<T> NfoList<T> {
    fn into_vec(self) -> Vec<T> {
        match self {
            Self::Value(items) => items,
            _ => Vec::new(),
        }
    }
}

/// 一份 NFO 的存在性视图：区分“元素缺失”与“元素存在但为空”。
#[derive(Clone, Debug, Default)]
pub(crate) struct NfoPresence {
    pub(crate) title: NfoText,
    pub(crate) original_title: NfoText,
    pub(crate) year: NfoText,
    pub(crate) plot: NfoText,
    pub(crate) tagline: NfoText,
    pub(crate) premiered: NfoText,
    pub(crate) aired: NfoText,
    pub(crate) end_date: NfoText,
    pub(crate) rating: NfoText,
    pub(crate) votes: NfoText,
    pub(crate) runtime_minutes: NfoText,
    pub(crate) content_rating: NfoText,
    pub(crate) original_language: NfoText,
    pub(crate) status: NfoText,
    /// 根元素 `<thumb>`（episode 的剧集剧照 URL）。存在性语义与其它文本字段一致。
    pub(crate) thumb: NfoText,
    pub(crate) genres: NfoList<String>,
    pub(crate) countries: NfoList<String>,
    pub(crate) studios: NfoList<String>,
    pub(crate) directors: NfoList<String>,
    pub(crate) creators: NfoList<String>,
    pub(crate) cast: NfoList<CastMember>,
}

impl NfoPresence {
    fn vote_count(&self) -> Option<u64> {
        parse_vote_count(self.votes.value())
    }
}

/// 根元素与 NFO 内部声明的季集坐标（不建完整字段）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NfoHeader {
    /// 小写化的根元素名（`movie` / `tvshow` / `episodedetails` / `season` …）。
    pub root: String,
    pub season: Option<u32>,
    pub episode: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NfoMeta {
    pub title: Option<String>,
    pub original_title: Option<String>,
    pub year: Option<String>,
    pub plot: Option<String>,
    pub tagline: Option<String>,
    pub rating: Option<String>,
    pub vote_count: Option<u64>,
    pub runtime_minutes: Option<String>,
    pub premiered: Option<String>,
    pub end_date: Option<String>,
    pub content_rating: Option<String>,
    pub original_language: Option<String>,
    pub status: Option<String>,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub aired: Option<String>,
    pub number_of_seasons: Option<u32>,
    pub number_of_episodes: Option<u32>,
    pub thumb: Option<String>,
    pub genres: Vec<String>,
    pub countries: Vec<String>,
    pub studios: Vec<String>,
    pub studio_logo: Option<String>,
    pub directors: Vec<String>,
    pub creators: Vec<String>,
    pub cast: Vec<CastMember>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CastMember {
    pub name: String,
    pub role: Option<String>,
    /// TMDB person id（写进 <actor><tmdbid>，供人物页跳转）。
    pub tmdb_id: Option<String>,
    /// TMDB 头像路径（写进 <actor><thumb>）。
    pub thumb: Option<String>,
    /// 演员在影片演员表中的顺序（写进 <actor><order>）。
    pub order: Option<u32>,
}

/// Parse an NFO file if present; missing/unreadable/invalid XML → None.
pub fn read_nfo(path: &Path) -> Option<NfoMeta> {
    let body = std::fs::read_to_string(path).ok()?;
    parse_nfo(&body)
}

pub fn parse_nfo(body: &str) -> Option<NfoMeta> {
    let document = roxmltree::Document::parse(body).ok()?;
    let root = document.root_element();
    let presence = presence_of(root)?;
    let vote_count = presence.vote_count();
    Some(NfoMeta {
        title: presence.title.into_option(),
        original_title: presence.original_title.into_option(),
        year: presence.year.into_option(),
        plot: presence.plot.into_option(),
        tagline: presence.tagline.into_option(),
        rating: presence.rating.into_option(),
        vote_count,
        runtime_minutes: presence.runtime_minutes.into_option(),
        premiered: presence.premiered.into_option(),
        end_date: presence.end_date.into_option(),
        content_rating: presence.content_rating.into_option(),
        original_language: presence.original_language.into_option(),
        status: presence.status.into_option(),
        season: child_u32(root, &["season", "seasonnumber"]),
        episode: child_u32(root, &["episode"]),
        aired: presence.aired.into_option(),
        number_of_seasons: child_u32(
            root,
            &["seasoncount", "numberofseasons", "number_of_seasons"],
        ),
        number_of_episodes: child_u32(
            root,
            &["episodecount", "numberofepisodes", "number_of_episodes"],
        ),
        thumb: child_text(root, &["thumb"]),
        genres: presence.genres.into_vec(),
        countries: presence.countries.into_vec(),
        studios: presence.studios.into_vec(),
        studio_logo: None,
        directors: presence.directors.into_vec(),
        creators: presence.creators.into_vec(),
        cast: presence.cast.into_vec(),
    })
}

/// 解析根元素与季集坐标；不支持的根元素返回 None。
pub fn parse_nfo_header(body: &str) -> Option<NfoHeader> {
    let document = roxmltree::Document::parse(body).ok()?;
    let root = document.root_element();
    if !is_supported_root(root.tag_name().name()) {
        return None;
    }
    Some(NfoHeader {
        root: root.tag_name().name().to_ascii_lowercase(),
        season: child_u32(root, &["season", "seasonnumber"]),
        episode: child_u32(root, &["episode"]),
    })
}

/// 存在性视图；不支持的根元素返回 None。
pub(crate) fn parse_nfo_presence(body: &str) -> Option<NfoPresence> {
    let document = roxmltree::Document::parse(body).ok()?;
    presence_of(document.root_element())
}

fn presence_of(root: roxmltree::Node<'_, '_>) -> Option<NfoPresence> {
    if !is_supported_root(root.tag_name().name()) {
        return None;
    }
    let (rating, votes) = rating_presence(root);
    Some(NfoPresence {
        title: child_text_state(root, &["title", "name", "localtitle"]),
        original_title: child_text_state(root, &["originaltitle", "original_title"]),
        year: child_text_state(root, &["year"]),
        plot: child_text_state(root, &["plot", "overview", "outline"]),
        tagline: child_text_state(root, &["tagline"]),
        premiered: child_text_state(root, &["premiered", "firstaired", "release_date"]),
        aired: child_text_state(root, &["aired", "air_date"]),
        end_date: child_text_state(root, &["enddate", "ended", "lastaired", "end_date"]),
        rating,
        votes,
        runtime_minutes: child_text_state(root, &["runtime", "runtime_minutes"]),
        content_rating: child_text_state(root, &["mpaa", "contentrating", "certification"]),
        original_language: child_text_state(root, &["original_language", "originallanguage"]),
        status: child_text_state(root, &["status"]),
        thumb: child_text_state(root, &["thumb"]),
        genres: child_texts_state(root, &["genre", "tag"]),
        countries: child_texts_state(root, &["country"]),
        studios: child_texts_state(root, &["studio", "network", "productioncompany"]),
        directors: child_texts_state(root, &["director"]),
        creators: child_texts_state(root, &["creator"]),
        cast: cast_presence(root),
    })
}

/// `rating`/`votes` 需要先看 `<ratings>` 块，再回落到根上的 `<rating>`/`<votes>`。
fn rating_presence(root: roxmltree::Node<'_, '_>) -> (NfoText, NfoText) {
    let ratings = root
        .children()
        .find(|child| child.is_element() && name_is(child.tag_name().name(), "ratings"));
    let entries: Vec<_> = ratings
        .into_iter()
        .flat_map(|node| node.children())
        .filter(|node| node.is_element() && name_is(node.tag_name().name(), "rating"))
        .collect();
    let selected = entries
        .iter()
        .copied()
        .find(|entry| {
            entry.attribute("name").is_some_and(|name| {
                matches!(name.to_ascii_lowercase().as_str(), "themoviedb" | "tmdb")
            })
        })
        .or_else(|| {
            entries.iter().copied().find(|entry| {
                entry
                    .attribute("default")
                    .is_some_and(|value| value.eq_ignore_ascii_case("true"))
            })
        })
        .or_else(|| entries.first().copied());

    if let Some(entry) = selected {
        let votes = child_text_state(entry, &["votes"]);
        let votes = if votes.is_missing() {
            child_text_state(root, &["votes"])
        } else {
            votes
        };
        return (element_text_state(entry, &["value"]), votes);
    }
    (
        child_text_state(root, &["rating"]),
        child_text_state(root, &["votes"]),
    )
}

pub(crate) fn is_supported_root(name: &str) -> bool {
    [
        "movie",
        "tvshow",
        "tvseries",
        "series",
        "episodedetails",
        "episode",
        "season",
        "video",
        "musicvideo",
    ]
    .iter()
    .any(|candidate| name_is(name, candidate))
}

pub(crate) fn name_is(actual: &str, expected: &str) -> bool {
    actual.eq_ignore_ascii_case(expected)
}

fn child_nodes<'a>(
    node: roxmltree::Node<'a, 'a>,
    names: &'a [&'a str],
) -> impl Iterator<Item = roxmltree::Node<'a, 'a>> + 'a {
    node.children().filter(move |child| {
        child.is_element()
            && names
                .iter()
                .any(|name| name_is(child.tag_name().name(), name))
    })
}

fn node_text(node: roxmltree::Node<'_, '_>) -> Option<String> {
    node.text()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

pub(crate) fn child_text(node: roxmltree::Node<'_, '_>, names: &[&str]) -> Option<String> {
    child_nodes(node, names).find_map(node_text)
}

fn child_u32(node: roxmltree::Node<'_, '_>, names: &[&str]) -> Option<u32> {
    child_text(node, names)?.parse().ok()
}

/// 元素存在性：任一别名元素出现即视为存在；返回第一个带文本的别名。
fn child_text_state(node: roxmltree::Node<'_, '_>, names: &[&str]) -> NfoText {
    let mut seen = false;
    for child in child_nodes(node, names) {
        seen = true;
        if let Some(text) = node_text(child) {
            return NfoText::Value(text);
        }
    }
    if seen {
        NfoText::Empty
    } else {
        NfoText::Missing
    }
}

/// 元素自身文本或 `<value>` 子元素文本；两者都没有则是 `Empty`。
fn element_text_state(node: roxmltree::Node<'_, '_>, names: &[&str]) -> NfoText {
    let state = child_text_state(node, names);
    if state.is_missing() {
        self_text_state(node)
    } else {
        state
    }
}

fn self_text_state(node: roxmltree::Node<'_, '_>) -> NfoText {
    match node_text(node) {
        Some(text) => NfoText::Value(text),
        None => NfoText::Empty,
    }
}

fn child_texts_state(node: roxmltree::Node<'_, '_>, names: &[&str]) -> NfoList<String> {
    let mut seen = false;
    let mut values = Vec::new();
    for child in child_nodes(node, names) {
        seen = true;
        if let Some(text) = node_text(child) {
            values.push(text);
        }
    }
    if !seen {
        NfoList::Missing
    } else if values.is_empty() {
        NfoList::Empty
    } else {
        NfoList::Value(values)
    }
}

fn cast_presence(root: roxmltree::Node<'_, '_>) -> NfoList<CastMember> {
    let mut seen = false;
    let mut cast = Vec::new();
    for actor in root
        .children()
        .filter(|node| node.is_element() && name_is(node.tag_name().name(), "actor"))
    {
        seen = true;
        let Some(name) = child_text(actor, &["name"]) else {
            continue;
        };
        cast.push(CastMember {
            name,
            role: child_text(actor, &["role"]),
            tmdb_id: child_text(actor, &["tmdbid"]),
            thumb: child_text(actor, &["thumb"]),
            order: child_u32(actor, &["order"]),
        });
    }
    cast.sort_by_key(|member| member.order.unwrap_or(u32::MAX));
    if !seen {
        return NfoList::Missing;
    }
    if cast.is_empty() {
        return NfoList::Empty;
    }
    NfoList::Value(cast)
}

pub(crate) fn parse_vote_count(value: Option<&str>) -> Option<u64> {
    value?
        .chars()
        .filter(|character| character.is_ascii_digit())
        .collect::<String>()
        .parse()
        .ok()
}
