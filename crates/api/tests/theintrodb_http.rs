#[path = "support/theintrodb.rs"]
mod fixture;
use fixture::Server;

#[test]
fn v4_episode_response_uses_bearer_auth_and_millisecond_ranges() {
    let server = Server::new(
        200,
        r#"{"tvdb_id":169,"type":"tv","season":1,"episode":2,"group_duration_ms":2700000,"intro":[{"start_ms":10500,"end_ms":70500,"confidence":0.87,"submission_count":12}],"credits":[{"start_ms":2600000,"end_ms":2690000,"confidence":0.9,"submission_count":12}]}"#,
        true,
    );
    let markers = server
        .client(true)
        .get_episode_markers("123", 1, 2, Some(2700000))
        .unwrap()
        .unwrap();
    assert_eq!(
        (markers.intro_start_ms, markers.intro_end_ms),
        (Some(10500), Some(70500))
    );
    assert_eq!(
        (markers.outro_start_ms, markers.outro_end_ms),
        (Some(2600000), Some(2690000))
    );
}

#[test]
fn anonymous_v4_lookup_can_read_accepted_submissions() {
    let server = Server::new(
        200,
        r#"{"tvdb_id":169,"type":"tv","season":1,"episode":2,"intro":[{"start_ms":null,"end_ms":60000}]}"#,
        false,
    );
    let markers = server
        .client(false)
        .get_episode_markers("123", 1, 2, Some(2700000))
        .unwrap()
        .unwrap();
    assert_eq!(
        (markers.intro_start_ms, markers.intro_end_ms),
        (Some(0), Some(60000))
    );
}

#[test]
fn only_structured_no_submissions_is_a_normal_empty_lookup() {
    let server = Server::new(
        404,
        r#"{"error":"No accepted submissions","code":"no_submissions"}"#,
        true,
    );
    assert_eq!(
        server
            .client(true)
            .get_episode_markers("123", 1, 2, Some(2700000))
            .unwrap(),
        None
    );
}

#[test]
fn obsolete_route_404_and_identity_mismatch_are_not_cached_as_absent_segments() {
    for body in [
        "Cannot GET /media",
        r#"{"error":"No such episode","code":"episode_not_found"}"#,
    ] {
        let server = Server::new(404, body, true);
        assert!(
            server
                .client(true)
                .get_episode_markers("123", 1, 2, Some(2700000))
                .is_err()
        );
    }
}

#[test]
fn v4_zero_length_absence_and_multi_segment_responses_do_not_create_one_large_skip_range() {
    for body in [
        r#"{"tvdb_id":169,"type":"tv","season":1,"episode":2,"intro":[{"start_ms":null,"end_ms":0}],"credits":[{"start_ms":0,"end_ms":null}]}"#,
        r#"{"tvdb_id":169,"type":"tv","season":1,"episode":2,"intro":[{"start_ms":10000,"end_ms":70000},{"start_ms":90000,"end_ms":120000}]}"#,
    ] {
        let server = Server::new(200, body, true);
        assert_eq!(
            server
                .client(true)
                .get_episode_markers("123", 1, 2, Some(2700000))
                .unwrap(),
            None
        );
    }
}

#[test]
fn invalid_ranges_and_http_authentication_errors_remain_errors() {
    for (status, body) in [
        (401, r#"{"error":"Unauthorized"}"#),
        (
            429,
            r#"{"error":"Too Many Requests","code":"usage_limit_exceeded"}"#,
        ),
        (
            200,
            r#"{"tvdb_id":169,"type":"tv","season":1,"episode":2,"intro":[{"start_ms":70000,"end_ms":10000}]}"#,
        ),
    ] {
        let server = Server::new(status, body, true);
        assert!(
            server
                .client(true)
                .get_episode_markers("123", 1, 2, Some(2700000))
                .is_err()
        );
    }
}
