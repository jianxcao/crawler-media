use domain::{EntityScope, FieldValue, MediaAlias, MetadataCast, MetadataFields, MetadataGenre};

use super::parse::{
    NfoList, NfoPresence, NfoText, is_supported_root, parse_nfo_presence, parse_vote_count,
};

#[derive(Clone, Debug, PartialEq)]
pub enum NfoReadOutcome {
    Missing,
    Unknown { message: String },
    Invalid { message: String },
    Valid(NfoInput),
}

#[derive(Clone, Debug, PartialEq)]
pub struct NfoInput {
    pub scope: EntityScope,
    pub locale: Option<String>,
    pub fields: MetadataFields,
    pub work_aliases: Vec<MediaAlias>,
    pub source_version: String,
}

pub fn parse_nfo_fields(body: &str, scope: EntityScope) -> NfoReadOutcome {
    let document = match roxmltree::Document::parse(body) {
        Ok(doc) => doc,
        Err(e) => {
            return NfoReadOutcome::Invalid {
                message: format!("XML parse error: {e}"),
            };
        }
    };

    let root = document.root_element();
    let root_name = root.tag_name().name();
    if !is_supported_root(root_name) {
        return NfoReadOutcome::Unknown {
            message: format!("Unsupported NFO root element: {root_name}"),
        };
    }

    let presence = match parse_nfo_presence(body) {
        Some(presence) => presence,
        None => {
            return NfoReadOutcome::Invalid {
                message: "Failed to parse NFO metadata".into(),
            };
        }
    };

    let mut fields = MetadataFields::default();
    // 先按实体层取好日期：`presence` 的字段随后会被逐个移出（`NfoText` 不是 `Copy`）。
    let (release_date, air_date) = date_fields(scope, &presence);

    // 元素存在性决定字段存在：`<plot/>` 是 Empty（显式清空），没有 `<plot>` 才是 Missing。
    fields.title = text_field(presence.title);
    fields.original_title = text_field(presence.original_title);
    fields.overview = text_field(presence.plot);
    fields.tagline = text_field(presence.tagline);
    fields.year = number_field(presence.year);
    fields.rating = number_field(presence.rating);
    fields.vote_count = count_field(&presence.votes);
    fields.runtime_minutes = number_field(presence.runtime_minutes);
    fields.content_rating = text_field(presence.content_rating);
    fields.language = text_field(presence.original_language);
    fields.status = text_field(presence.status);
    // `<thumb>` 只映射到 episode 作用域：`still_url` 是剧集主图字段，Work/Season 的
    // `<thumb>` 是海报/季封，语义不同，不得盲映射。漏掉这一行，只有 NFO `<thumb>` 的剧集
    // 在「页内只读已采纳事实」下会静默丢主图。观测结果落 `library_metadata_inputs`。
    if matches!(scope, EntityScope::Episode { .. }) {
        fields.still_url = remote_thumb_field(presence.thumb);
    }
    // 日期与署名类字段：它们没有 provider 兜底，Task 11 的页内水合只读这份观测结果，
    // 漏映射就等于 NFO 里的 studios/directors/creators/premiered 静默消失。
    fields.release_date = release_date;
    fields.air_date = air_date;
    fields.end_date = text_field(presence.end_date);
    fields.studios = text_list_field(presence.studios);
    fields.directors = text_list_field(presence.directors);
    fields.creators = text_list_field(presence.creators);

    match presence.genres {
        NfoList::Missing => {}
        NfoList::Empty => fields.genres = FieldValue::Empty,
        NfoList::Value(names) => {
            fields.genres = FieldValue::Value(
                names
                    .into_iter()
                    .map(|name| {
                        let filter_key = genre_filter_key(&name);
                        MetadataGenre {
                            source: "nfo".into(),
                            source_id: None,
                            name,
                            filter_key,
                        }
                    })
                    .collect(),
            );
        }
    }

    // Countries mapping: the facet/filter contract (`docs/api-contracts/self.md`) uses the
    // ISO 3166-1 alpha-2 code, while NFO sidecars carry the country name.
    match presence.countries {
        NfoList::Missing => {}
        NfoList::Empty => fields.countries = FieldValue::Empty,
        NfoList::Value(names) => {
            fields.countries =
                FieldValue::Value(names.iter().map(|country| country_code(country)).collect());
        }
    }

    match presence.cast {
        NfoList::Missing => {}
        NfoList::Empty => fields.cast = FieldValue::Empty,
        NfoList::Value(cast) => {
            fields.cast = FieldValue::Value(
                cast.into_iter()
                    .map(|member| MetadataCast {
                        name: member.name,
                        role: member.role,
                        tmdb_id: member.tmdb_id,
                        thumb: member.thumb,
                        order: member.order,
                    })
                    .collect(),
            );
        }
    }

    NfoReadOutcome::Valid(NfoInput {
        scope,
        locale: None,
        fields,
        work_aliases: Vec::new(),
        source_version: "nfo_v1".into(),
    })
}

/// 单值文本字段：`Missing` 保持 `Missing`，元素存在但为空是 `Empty`。
fn text_field(text: NfoText) -> FieldValue<String> {
    match text {
        NfoText::Missing => FieldValue::Missing,
        NfoText::Empty => FieldValue::Empty,
        NfoText::Value(value) => FieldValue::Value(value),
    }
}

/// 剧照取「远端 URL」形态的 `<thumb>`：本地引用（相对路径、绝对路径）属于文件系统旁车口径。
///
/// 页内主图事实会盖住已采纳的远端剧照，而页内主图的渲染器只认远端 URL；把本地引用写进
/// `still_url` 会让「本地引用 + 已采纳剧照」的剧集页内主图反而消失。存在性语义不变：
/// `<thumb/>` 仍是 Empty（显式清空），没有 `<thumb>` 才是 Missing。
fn remote_thumb_field(thumb: NfoText) -> FieldValue<String> {
    match thumb {
        NfoText::Value(value) if is_remote_url(&value) => FieldValue::Value(value),
        NfoText::Value(_) => FieldValue::Missing,
        other => text_field(other),
    }
}

fn is_remote_url(value: &str) -> bool {
    let Some((scheme, rest)) = value.split_once("://") else {
        return false;
    };
    !rest.is_empty()
        && (scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
}

/// 上映/首播日期落点按 Jellyfin/Kodi 语义区分实体层。
///
/// work/season 的 `<premiered>` 是上映日期 → `release_date`；episode 的 `<premiered>` 与
/// `<aired>` 同义（都是首播日期）→ `air_date`，且 `<aired>` 是 episode NFO 的正式字段，
/// 两者同时出现时以它为准，不把同一个日期同时写进两个字段。
fn date_fields(
    scope: EntityScope,
    presence: &NfoPresence,
) -> (FieldValue<String>, FieldValue<String>) {
    match scope {
        EntityScope::Episode { .. } => {
            let air_date = if presence.aired.is_missing() {
                text_field(presence.premiered.clone())
            } else {
                text_field(presence.aired.clone())
            };
            (FieldValue::Missing, air_date)
        }
        _ => (
            text_field(presence.premiered.clone()),
            text_field(presence.aired.clone()),
        ),
    }
}

/// 重复元素文本字段（studio/director/creator）：同样保留“缺失 vs 存在但为空”的差别。
fn text_list_field(list: NfoList<String>) -> FieldValue<Vec<String>> {
    match list {
        NfoList::Missing => FieldValue::Missing,
        NfoList::Empty => FieldValue::Empty,
        NfoList::Value(values) => FieldValue::Value(values),
    }
}

/// 单值数字字段：元素存在但无法解析成目标类型时记 `Empty`，不冒充 `Missing`。
fn number_field<T: std::str::FromStr>(text: NfoText) -> FieldValue<T> {
    match text {
        NfoText::Missing => FieldValue::Missing,
        NfoText::Empty => FieldValue::Empty,
        NfoText::Value(value) => value
            .parse()
            .map(FieldValue::Value)
            .unwrap_or(FieldValue::Empty),
    }
}

/// `<votes>` 允许 `1,500,000` 这类分隔符写法，解析失败同样记 `Empty`。
fn count_field(text: &NfoText) -> FieldValue<u64> {
    match text {
        NfoText::Missing => FieldValue::Missing,
        NfoText::Empty => FieldValue::Empty,
        NfoText::Value(raw) => parse_vote_count(Some(raw))
            .map(FieldValue::Value)
            .unwrap_or(FieldValue::Empty),
    }
}

fn country_code(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.len() == 2 && trimmed.chars().all(|c| c.is_ascii_alphabetic()) {
        return trimmed.to_ascii_uppercase();
    }
    // Kodi/TMM write the English country name; map the common ones to ISO 3166-1 alpha-2.
    match trimmed.to_ascii_lowercase().as_str() {
        "united states" | "united states of america" | "usa" => "US".into(),
        "united kingdom" | "great britain" | "uk" => "GB".into(),
        "china" | "mainland china" => "CN".into(),
        "japan" => "JP".into(),
        "south korea" | "korea" | "republic of korea" => "KR".into(),
        "france" => "FR".into(),
        "germany" => "DE".into(),
        "hong kong" => "HK".into(),
        "taiwan" => "TW".into(),
        "canada" => "CA".into(),
        "australia" => "AU".into(),
        "india" => "IN".into(),
        "italy" => "IT".into(),
        "spain" => "ES".into(),
        "russia" | "russian federation" => "RU".into(),
        "thailand" => "TH".into(),
        "singapore" => "SG".into(),
        "brazil" => "BR".into(),
        "mexico" => "MX".into(),
        "sweden" => "SE".into(),
        "norway" => "NO".into(),
        "denmark" => "DK".into(),
        "finland" => "FI".into(),
        "netherlands" => "NL".into(),
        "belgium" => "BE".into(),
        "switzerland" => "CH".into(),
        "austria" => "AT".into(),
        "poland" => "PL".into(),
        "czech republic" | "czechia" => "CZ".into(),
        "ireland" => "IE".into(),
        "new zealand" => "NZ".into(),
        "south africa" => "ZA".into(),
        "argentina" => "AR".into(),
        "turkey" => "TR".into(),
        "iran" => "IR".into(),
        "israel" => "IL".into(),
        "philippines" => "PH".into(),
        "indonesia" => "ID".into(),
        "malaysia" => "MY".into(),
        "vietnam" => "VN".into(),
        "ukraine" => "UA".into(),
        "portugal" => "PT".into(),
        "greece" => "GR".into(),
        "egypt" => "EG".into(),
        _ => trimmed.to_string(),
    }
}

fn genre_filter_key(name: &str) -> String {
    // Map standard genres to TMDB IDs if known
    match name.to_ascii_lowercase().as_str() {
        "action" | "动作" => "28".into(),
        "adventure" | "冒险" => "12".into(),
        "animation" | "动画" => "16".into(),
        "comedy" | "喜剧" => "35".into(),
        "crime" | "犯罪" => "80".into(),
        "documentary" | "纪录" => "99".into(),
        "drama" | "剧情" => "18".into(),
        "family" | "家庭" => "10751".into(),
        "fantasy" | "奇幻" => "14".into(),
        "history" | "历史" => "36".into(),
        "horror" | "恐怖" => "27".into(),
        "music" | "音乐" => "10402".into(),
        "mystery" | "悬疑" => "9648".into(),
        "romance" | "爱情" => "10749".into(),
        "science fiction" | "sci-fi" | "科幻" => "878".into(),
        "thriller" | "惊悚" => "53".into(),
        "war" | "战争" => "10752".into(),
        "western" | "西部" => "37".into(),
        other => format!("name:{}", other.to_lowercase()),
    }
}
