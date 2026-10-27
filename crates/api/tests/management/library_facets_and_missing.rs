use std::collections::HashMap;
use std::sync::Arc;

use api::router;
use downloader::MemoryDownloader;
use parking_lot::Mutex;
use serde_json::Value;
use tower::ServiceExt;

use super::common::*;

fn app(tmp: &tempfile::TempDir) -> axum::Router {
    let fetcher = Arc::new(Fixtures {
        requests: Mutex::new(Vec::new()),
        bodies: HashMap::new(),
    });
    let downloader = Arc::new(MemoryDownloader::new(tmp.path().join("stage")));
    router(state(tmp.path(), fetcher, downloader))
}

#[tokio::test]
async fn item_index_and_facets_return_structured_dtos() {
    let tmp = tempfile::tempdir().unwrap();
    let app = app(&tmp);

    let movie_dir = tmp.path().join("data/library/movies");
    std::fs::create_dir_all(&movie_dir).unwrap();
    std::fs::write(movie_dir.join("Alien.1979.2160p.mkv"), b"1").unwrap();
    std::fs::write(movie_dir.join("Avatar.2009.1080p.mkv"), b"2").unwrap();
    std::fs::write(movie_dir.join("Blade.1998.1080p.mkv"), b"3").unwrap();

    let libs = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                "/api/v1/libraries",
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let movie_id = libs
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["kind"] == "movie")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    // 扫描
    scan_and_wait(&app, &movie_id).await;

    // 1. 测试 item-index 桶聚合
    let index_res = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{movie_id}/item-index?sort=title&order=asc"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    let buckets = index_res.as_array().unwrap();
    assert!(!buckets.is_empty(), "item-index 应返回非空首字母桶");
    // "Alien" 与 "Avatar" 首字母均为 A → 第一档，count 2。
    let a_bucket = buckets.iter().find(|b| b["initial"] == "A").unwrap();
    assert_eq!(a_bucket["count"], 2);
    // Task 26：绝对下标已删除，线上只有不透明游标。第一档从第 0 条开始 = 首页。
    assert!(
        a_bucket["offset"].is_null(),
        "item-index 不得再回显绝对下标: {a_bucket}"
    );
    assert!(
        a_bucket["cursor"].is_null(),
        "第一档不需要游标（直接取首页）: {a_bucket}"
    );
    // "Blade" 首字母 B，count 1，并且带一个能直达这一档第一格的游标。
    let b_bucket = buckets.iter().find(|b| b["initial"] == "B").unwrap();
    assert_eq!(b_bucket["count"], 1);
    let cursor = b_bucket["cursor"]
        .as_str()
        .expect("后续档必须给出直达游标");
    // 断言真正重要的那件事：用这一档的游标跳过去，返回的就是这一档的第一条。
    let jump = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                &format!(
                    "/api/v1/libraries/{movie_id}/items?sort=title&order=asc&cursor={}",
                    urlencode(cursor)
                ),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    // `json_data` 已经把信封的 `data` 取出来了：这里就是一页条目。
    let jumped = jump
        .as_array()
        .unwrap_or_else(|| panic!("跳转请求没有返回条目页: {jump}"));
    assert_eq!(jumped.len(), 1, "B 档只有一部: {jump}");
    assert!(
        jumped[0]["title"]
            .as_str()
            .unwrap_or_default()
            .starts_with("Blade"),
        "跳转必须落在 B 档第一条: {jump}"
    );

    // 2. 测试 facets 结构化 DTO
    let facets_res = json_data(
        app.clone()
            .oneshot(request(
                "GET",
                &format!("/api/v1/libraries/{movie_id}/facets"),
                Some("management-secret"),
                Value::Null,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(facets_res["total"], 3);
    let resolutions = facets_res["resolutions"].as_array().unwrap();
    assert!(!resolutions.is_empty());
    // 每个分辨率应该是一个对象 { value, label, count } 而不是裸字符串
    for res_item in resolutions {
        assert!(res_item.get("value").is_some());
        assert!(res_item.get("label").is_some());
        assert!(res_item.get("count").is_some());
    }
}

/// 游标是服务端签发的不透明 JSON，必须整体转义后再拼进查询串。
fn urlencode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in raw.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}
