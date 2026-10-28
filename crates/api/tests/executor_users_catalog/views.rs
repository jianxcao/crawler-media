use super::*;

#[test]
fn aggregate_cache_preserves_provider_fields_and_deduplicates_keys() {
    let f = Fixture::new();
    {
        let handle = f.state.store();
        let store = handle.lock();
        for (source, key, body, at) in [
            (
                "tmdb",
                "/movie/1?language=en-US",
                json!({"title":"English","release_date":"2020-01-01","genres":[{"name":"Drama"}]}),
                300,
            ),
            (
                "tmdb",
                "/movie/1?language=zh-CN",
                json!({"title":"中文","overview":"Plot","poster_path":"/poster.jpg"}),
                200,
            ),
            (
                "tmdb",
                "/search/tv?query=other",
                json!({"results":[{"id":2,"name":"TV","first_air_date":"2022-01-01"},{"id":2,"name":"TV"}]}),
                150,
            ),
            (
                "douban",
                "/search_subjects?type=tv",
                json!({"subjects":[{"id":"3","title":"Douban","rate":"8.5","cover":"https://images.invalid/3.jpg"}]}),
                140,
            ),
            (
                "anilist",
                "/Page",
                json!({"data":{"Page":{"media":[{"id":4,"title":{"native":"AniList","romaji":"Original"},"format":"MOVIE","averageScore":80,"startDate":{"year":2024},"genres":["Action"]}]}}}),
                130,
            ),
            (
                "bangumi",
                "/search?type=6",
                json!({"list":[{"id":5,"name_cn":"Bangumi","name":"Original","air_date":"2025-01-01","score":9.0}]}),
                120,
            ),
        ] {
            store
                .put_catalog_cache(source, key, &body.to_string(), at, None)
                .unwrap();
        }
    }
    let response = runtime()
        .block_on(router(f.state).oneshot(request(
            "GET",
            "/api/v1/catalog/cache?view=aggregate",
            Value::Null,
        )))
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    let body: Value = runtime().block_on(async {
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap()).unwrap()
    });
    let entries = body["data"].as_array().unwrap();
    assert_eq!(entries.len(), 5);
    let movie = entries
        .iter()
        .find(|entry| entry["id"] == "tmdb_movie_1")
        .unwrap();
    assert_eq!(movie["title"], "中文");
    assert_eq!(movie["year"], 2020);
    assert_eq!(movie["overview"], "Plot");
    assert_eq!(
        movie["poster_url"],
        "https://image.tmdb.org/t/p/w500/poster.jpg"
    );
    assert_eq!(movie["genres"], json!(["Drama"]));
    assert_eq!(movie["cached_keys"].as_array().unwrap().len(), 3);
    let tv = entries
        .iter()
        .find(|entry| entry["id"] == "tmdb_tv_2")
        .unwrap();
    assert_eq!(tv["year"], 2022);
    assert_eq!(tv["cached_keys"].as_array().unwrap().len(), 1);
    let douban = entries
        .iter()
        .find(|entry| entry["source"] == "douban")
        .unwrap();
    assert_eq!(douban["kind"], "tv");
    assert_eq!(douban["rating"], 8.5);
    let anilist = entries
        .iter()
        .find(|entry| entry["source"] == "anilist")
        .unwrap();
    assert_eq!(anilist["kind"], "movie");
    assert_eq!(anilist["rating"], 8.0);
    assert_eq!(anilist["original_title"], "Original");
    let bangumi = entries
        .iter()
        .find(|entry| entry["source"] == "bangumi")
        .unwrap();
    assert_eq!(bangumi["kind"], "movie");
    assert_eq!(bangumi["year"], 2025);
}
