use indexer::cdp::{CdpCookie, match_cookies_to_profiles};

#[test]
fn test_match_cookies_to_profiles_filters_by_domain_and_deduplicates() {
    let cookies = vec![
        CdpCookie {
            name: "c_secure_uid".into(),
            value: "12345".into(),
            domain: ".pterclub.net".into(),
            path: Some("/".into()),
            expires: None,
            http_only: Some(true),
            secure: Some(true),
        },
        CdpCookie {
            name: "c_secure_pass".into(),
            value: "abcdef".into(),
            domain: "pterclub.net".into(),
            path: Some("/".into()),
            expires: None,
            http_only: Some(true),
            secure: Some(true),
        },
        CdpCookie {
            name: "some_google_cookie".into(),
            value: "xyz".into(),
            domain: ".google.com".into(),
            path: Some("/".into()),
            expires: None,
            http_only: None,
            secure: None,
        },
        CdpCookie {
            name: "login_token".into(),
            value: "mteam_token".into(),
            domain: "kp.m-team.cc".into(),
            path: Some("/".into()),
            expires: None,
            http_only: Some(true),
            secure: Some(true),
        },
    ];

    let profiles = vec![
        ("pterclub".to_string(), "pterclub.net".to_string()),
        ("mteam".to_string(), "m-team.cc".to_string()),
        ("chdbits".to_string(), "chdbits.co".to_string()),
    ];

    let matched = match_cookies_to_profiles(&cookies, &profiles);
    assert_eq!(
        matched.len(),
        2,
        "应只匹配 pterclub 和 mteam，忽略无关的 google"
    );

    let pter = matched.iter().find(|m| m.profile_id == "pterclub").unwrap();
    assert_eq!(pter.cookie_count, 2);
    assert!(pter.cookie_header.contains("c_secure_uid=12345"));
    assert!(pter.cookie_header.contains("c_secure_pass=abcdef"));

    let mt = matched.iter().find(|m| m.profile_id == "mteam").unwrap();
    assert_eq!(mt.cookie_count, 1);
    assert!(mt.cookie_header.contains("login_token=mteam_token"));

    assert!(
        matched.iter().find(|m| m.profile_id == "chdbits").is_none(),
        "无 Cookie 的站点不包含"
    );
}

#[test]
fn fetch_cookies_from_unreachable_cdp_fails_promptly() {
    let start = std::time::Instant::now();
    let res = indexer::cdp::fetch_cookies_from_cdp("http://127.0.0.1:54321");
    assert!(res.is_err());
    assert!(
        start.elapsed() < std::time::Duration::from_secs(3),
        "本地不可达 CDP 应快速返回连接错误，绝不可无限挂起"
    );
}
