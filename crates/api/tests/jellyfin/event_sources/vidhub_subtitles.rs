//! VidHub carries MediaSourceId in the subtitle path, without a session query.
use super::*;

async fn subtitle(
    f: &Fixture,
    prefix: &str,
    source: &str,
    index: u64,
    query: &str,
    device: &str,
) -> axum::response::Response {
    f.app
        .clone()
        .oneshot(
            Request::get(format!(
                "{prefix}/Videos/{}/{source}/Subtitles/{index}/Stream.srt{query}",
                f.item
            ))
            .header("authorization", format!("Bearer {}", f.token))
            .header("X-Emby-Device-Id", device)
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap()
}

fn subtitle_index(info: &Value) -> u64 {
    info["MediaSources"][0]["MediaStreams"]
        .as_array()
        .unwrap()
        .iter()
        .find(|track| track["Type"] == "Subtitle")
        .unwrap()["Index"]
        .as_u64()
        .unwrap()
}

#[tokio::test]
async fn vidhub_subtitles_follow_the_path_source_after_preference_changes() {
    for prefix in ["", "/emby"] {
        let f = Fixture::new();
        std::fs::write(f._tmp.path().join("matrix.srt"), b"subtitle A").unwrap();
        let info = f.info(prefix).await;
        let source = info["MediaSources"][0]["Id"].as_str().unwrap();
        let session = info["PlaySessionId"].as_str().unwrap();
        let index = subtitle_index(&info);
        f.publish_new_preferred_source();
        std::fs::write(f._tmp.path().join("new-preferred.srt"), b"subtitle B").unwrap();
        for query in [String::new(), format!("?PlaySessionId={session}")] {
            let response = subtitle(&f, prefix, source, index, &query, DEVICE).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(
                to_bytes(response.into_body(), 1024).await.unwrap().as_ref(),
                b"subtitle A"
            );
        }
        f.replace_source();
        let stale = subtitle(&f, prefix, source, index, "", DEVICE).await;
        assert_eq!(stale.status(), StatusCode::NOT_FOUND);
    }
}

#[tokio::test]
async fn vidhub_subtitles_reject_conflicting_sources_and_invalid_bindings() {
    for prefix in ["", "/emby"] {
        let f = Fixture::new();
        std::fs::write(f._tmp.path().join("matrix.srt"), b"subtitle A").unwrap();
        let info = f.info(prefix).await;
        let source = info["MediaSources"][0]["Id"].as_str().unwrap();
        let session = info["PlaySessionId"].as_str().unwrap();
        let index = subtitle_index(&info);
        let other = f.other_media_source();
        for (path_source, query, device, expected) in [
            (
                source,
                format!("?MediaSourceId={other}"),
                DEVICE,
                StatusCode::BAD_REQUEST,
            ),
            (other.as_str(), String::new(), DEVICE, StatusCode::NOT_FOUND),
            (
                source,
                "?PlaySessionId=unknown".into(),
                DEVICE,
                StatusCode::NOT_FOUND,
            ),
            (
                source,
                format!("?PlaySessionId={session}"),
                "wrong-device",
                StatusCode::NOT_FOUND,
            ),
        ] {
            let response = subtitle(&f, prefix, path_source, index, &query, device).await;
            assert_eq!(response.status(), expected);
        }
    }
}
