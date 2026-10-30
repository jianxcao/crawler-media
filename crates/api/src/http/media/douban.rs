//! Douban subject detail and response mapping.
use super::*;

/// `GET /media/douban/{id}` — douban subject id → TMDB detail (kind probed
/// via the catalog, since douban ids don't encode movie/tv).
pub(crate) async fn douban_detail(
    State(state): State<ApiState>,
    Path(id): Path<String>,
) -> Response {
    let state_clone = state.clone();
    let id_clone = id.clone();
    let res = state
        .http_work
        .run(MEDIA_DEADLINE, move || {
            douban_detail_blocking(&state_clone, &id_clone)
        })
        .await;
    match res {
        Ok(resp) => resp,
        Err(error) => blocking_error(error, "media.timeout"),
    }
}

fn douban_detail_blocking(state: &ApiState, id: &str) -> Response {
    // 优先本地 media 表映射（订阅/认领时 douban_id → tmdb_id 已入库）：
    // 有映射走完整 TMDB 详情。
    let store = state.store.as_ref();
    let local_media = store.get_media_by_alias("douban", id).ok().flatten();
    if let Some(media) = local_media
        && let Some(tmdb_id) = media.tmdb_id
    {
        let kind = if media.kind == domain::MediaKind::Tv {
            "tv"
        } else {
            "movie"
        };
        return media_detail_inner(state, kind, &tmdb_id);
    }

    match state.catalog.douban_subject_with_credits(id) {
        Ok(pair) => douban_subject_response(id, &pair.subject, Some(&pair.celebrities)),
        Err(media::TmdbError::NotFound(_)) => {
            err(StatusCode::NOT_FOUND, "media.missing", "条目不存在")
        }
        Err(
            error @ (media::TmdbError::Http(_)
            | media::TmdbError::Parse(_)
            | media::TmdbError::Json(_)),
        ) => {
            tracing::warn!(subject_id = id, error_kind = ?std::mem::discriminant(&error), "Douban subject source failed");
            err(
                StatusCode::BAD_GATEWAY,
                "media.upstream",
                "豆瓣详情暂时不可用",
            )
        }
        Err(error) => {
            tracing::error!(subject_id = id, error_kind = ?std::mem::discriminant(&error), "Douban subject cache failed");
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "media.cache",
                "豆瓣详情缓存读取失败",
            )
        }
    }
}

fn douban_subject_response(id: &str, data: &Value, douban_celebrities: Option<&Value>) -> Response {
    let is_tv = data.get("type").and_then(|v| v.as_str()) == Some("tv")
        || data.get("is_tv").and_then(|v| v.as_bool()).unwrap_or(false);
    let kind = if is_tv { "tv" } else { "movie" };
    let title = data
        .get("title")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let year = data
        .get("year")
        .and_then(|v| v.as_str())
        .and_then(|y| y.parse::<u16>().ok());
    let poster_url = data
        .get("cover_url")
        .and_then(|v| v.as_str())
        .or_else(|| {
            data.get("pic")
                .and_then(|p| p.get("large"))
                .and_then(|v| v.as_str())
        })
        .map(str::to_string);
    let rating = data
        .get("rating")
        .and_then(|r| r.get("value"))
        .and_then(|v| v.as_f64());

    let posters: Vec<Value> = poster_url
        .as_deref()
        .map(|p| {
            vec![json!({
                "preview_url": p,
                "full_url": p,
                "width": 300,
                "height": 450,
            })]
        })
        .unwrap_or_default();

    let backdrops: Vec<Value> = poster_url
        .as_deref()
        .map(|p| {
            vec![json!({
                "preview_url": p,
                "full_url": p,
                "width": 300,
                "height": 450,
            })]
        })
        .unwrap_or_default();

    ok(json!({
        "item": {
            "id": id,
            "title": title,
            "year": year,
            "kind": kind,
            "tmdb_id": Value::Null,
            "poster_url": poster_url.clone(),
            "backdrop_url": poster_url.clone(),
            "rating": rating,
        },
        "info": douban_info(data, douban_celebrities),
        "videos": [],
        "backdrops": backdrops,
        "posters": posters,
        "related": [],
        "library_links": [],
    }))
    .into_response()
}

fn douban_cast(data: &Value, douban_celebrities: Option<&Value>) -> Vec<Value> {
    if let Some(celeb_actors) = douban_celebrities
        .and_then(|c| c.get("actors"))
        .and_then(|v| v.as_array())
    {
        celeb_actors.iter().take(16).map(|a| {
            json!({
                "name": a.get("name").and_then(|v| v.as_str()).unwrap_or_default(),
                "role": a.get("character").and_then(|v| v.as_str()),
                "tmdb_person_id": Value::Null,
                "avatar_url": a.get("avatar").and_then(|av| av.get("large").or_else(|| av.get("normal"))).and_then(|v| v.as_str()),
            })
        }).collect()
    } else {
        data.get("actors").and_then(|v| v.as_array()).map(|arr| {
            arr.iter().take(10).map(|a| {
                json!({
                    "name": a.get("name").and_then(|v| v.as_str()).unwrap_or_default(),
                    "role": a.get("character").and_then(|v| v.as_str()),
                    "tmdb_person_id": Value::Null,
                    "avatar_url": a.get("avatar").and_then(|av| av.get("large").or_else(|| av.get("normal"))).and_then(|v| v.as_str()),
                })
            }).collect()
        }).unwrap_or_default()
    }
}

fn douban_info(data: &Value, douban_celebrities: Option<&Value>) -> Value {
    let rating = data
        .get("rating")
        .and_then(|r| r.get("value"))
        .and_then(Value::as_f64);
    let overview = data
        .get("intro")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let genres: Vec<Value> = data
        .get("genres")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let cast = douban_cast(data, douban_celebrities);

    let directors: Vec<Value> = data
        .get("directors")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|d| d.get("name")).cloned().collect())
        .unwrap_or_default();
    let countries = data
        .get("countries")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.as_str())
                .collect::<Vec<_>>()
                .join(" / ")
        })
        .unwrap_or_default();
    let languages = data
        .get("languages")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|l| l.as_str())
                .collect::<Vec<_>>()
                .join(" / ")
        })
        .unwrap_or_default();
    let released = data
        .get("release_date")
        .and_then(|v| v.as_str())
        .or_else(|| {
            data.get("pubdate")
                .and_then(|p| p.as_array())
                .and_then(|arr| arr.first())
                .and_then(|v| v.as_str())
        })
        .unwrap_or_default();
    let aliases: Vec<Value> = data
        .get("aka")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    json!({
        "overview": overview,
        "rating": rating.map(|r| format!("{r:.1}")),
        "runtime": Value::Null,
        "genres": genres,
        "cast": cast,
        "directors": directors,
        "director_credits": [],
        "country": countries,
        "language": languages,
        "released": released,
        "aliases": aliases,
    })
}
