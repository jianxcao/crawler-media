use domain::{
    EntityScope, FieldValue, MediaAlias, MetadataCast, MetadataFields, MetadataGenre,
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
    if !crate::nfo::is_supported_root(root_name) {
        return NfoReadOutcome::Unknown {
            message: format!("Unsupported NFO root element: {root_name}"),
        };
    }

    let parsed = match crate::nfo::parse_nfo(body) {
        Some(m) => m,
        None => {
            return NfoReadOutcome::Invalid {
                message: "Failed to parse NFO metadata".into(),
            };
        }
    };

    let mut fields = MetadataFields::default();

    if let Some(t) = parsed.title {
        fields.title = if t.is_empty() { FieldValue::Empty } else { FieldValue::Value(t) };
    }
    if let Some(ot) = parsed.original_title {
        fields.original_title = if ot.is_empty() { FieldValue::Empty } else { FieldValue::Value(ot) };
    }
    if let Some(p) = parsed.plot {
        fields.overview = if p.is_empty() { FieldValue::Empty } else { FieldValue::Value(p) };
    }
    if let Some(tag) = parsed.tagline {
        fields.tagline = if tag.is_empty() { FieldValue::Empty } else { FieldValue::Value(tag) };
    }
    if let Some(y) = parsed.year.and_then(|s| s.parse::<u16>().ok()) {
        fields.year = FieldValue::Value(y);
    }
    if let Some(r) = parsed.rating.and_then(|s| s.parse::<f64>().ok()) {
        fields.rating = FieldValue::Value(r);
    }
    if let Some(v) = parsed.vote_count {
        fields.vote_count = FieldValue::Value(v);
    }
    if let Some(rt) = parsed.runtime_minutes.and_then(|s| s.parse::<u32>().ok()) {
        fields.runtime_minutes = FieldValue::Value(rt);
    }
    if let Some(cr) = parsed.content_rating {
        fields.content_rating = if cr.is_empty() { FieldValue::Empty } else { FieldValue::Value(cr) };
    }
    if let Some(lang) = parsed.original_language {
        fields.language = if lang.is_empty() { FieldValue::Empty } else { FieldValue::Value(lang) };
    }
    if let Some(st) = parsed.status {
        fields.status = if st.is_empty() { FieldValue::Empty } else { FieldValue::Value(st) };
    }

    // Genres mapping
    if !parsed.genres.is_empty() {
        let mut genres = Vec::new();
        for g_name in parsed.genres {
            let filter_key = genre_filter_key(&g_name);
            genres.push(MetadataGenre {
                source: "nfo".into(),
                source_id: None,
                name: g_name,
                filter_key,
            });
        }
        fields.genres = FieldValue::Value(genres);
    }

    // Cast mapping
    if !parsed.cast.is_empty() {
        let mut cast = Vec::new();
        for c in parsed.cast {
            cast.push(MetadataCast {
                name: c.name,
                role: c.role,
                tmdb_id: c.tmdb_id,
                thumb: c.thumb,
                order: c.order,
            });
        }
        fields.cast = FieldValue::Value(cast);
    }

    NfoReadOutcome::Valid(NfoInput {
        scope,
        locale: None,
        fields,
        work_aliases: Vec::new(),
        source_version: "nfo_v1".into(),
    })
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
