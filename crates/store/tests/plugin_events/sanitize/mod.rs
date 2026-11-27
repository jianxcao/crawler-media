mod adversarial;

use store::plugin_events::sanitize_delivery_error;

fn assert_redacted(cases: &[(&str, &str)]) {
    for (raw, secret) in cases {
        let out = sanitize_delivery_error(raw);
        assert!(
            !out.contains(secret),
            "leaked `{secret}` in `{raw}` -> `{out}`"
        );
        assert!(out.contains("[REDACTED]"), "missing marker in `{out}`");
    }
}

#[test]
fn delivery_error_sanitizer_scrubs_secrets_traps_and_truncates() {
    let sanitized = sanitize_delivery_error(
        "https://alice:SECRET_PW@pt.example/cb?passkey=PK123&token=TOK456 Bearer SECRET_BEARER",
    );
    assert!(!sanitized.contains("SECRET_PW"));
    assert!(!sanitized.contains("PK123"));
    assert!(!sanitized.contains("TOK456"));
    assert!(!sanitized.contains("SECRET_BEARER"));
    assert!(sanitized.chars().count() <= 256);

    let wasm_trap = sanitize_delivery_error("wasm trap: unreachable\nwasm backtrace: 0xdeadbeef");
    assert!(!wasm_trap.contains("0xdeadbeef"));
    assert!(!wasm_trap.is_empty());

    let long = sanitize_delivery_error(&"x".repeat(400));
    assert!(long.chars().count() <= 256);
}

#[test]
fn delivery_error_sanitizer_redacts_whitespace_quoted_escaped_and_compound_keys() {
    let cases = [
        ("got Bearer  SECRET_DBL", "SECRET_DBL"),
        ("Bearer\tSECRET_TAB", "SECRET_TAB"),
        ("token=\"SECRET_QUOTED\"", "SECRET_QUOTED"),
        ("token='SECRET_SQUOTED'", "SECRET_SQUOTED"),
        ("token= SECRET_SPACE_EQ", "SECRET_SPACE_EQ"),
        (r#"{"token": "abc\"SECRET_ESC_JSON"}"#, "SECRET_ESC_JSON"),
        (r#"{\"token\": \"abc\\\"SECRET_ESC2\"}"#, "SECRET_ESC2"),
        ("err my_token=SECRET_MY", "SECRET_MY"),
        ("err api-key=SECRET_AK", "SECRET_AK"),
        ("api-key: SECRET_AK_HDR", "SECRET_AK_HDR"),
        ("x-custom-secret: SECRET_HDR2", "SECRET_HDR2"),
        ("db_passphrase=\"SECRET_PASS\"", "SECRET_PASS"),
        ("proxy_auth_cookie='SECRET_CK'", "SECRET_CK"),
        ("Cookie: foo=\"SECRET_CK2\"; bar='SECRET_CK3'", "SECRET_CK2"),
    ];
    assert_redacted(&cases);
    assert_eq!(
        sanitize_delivery_error(r#"{"author": "alice", "status": "ok"}"#),
        r#"{"author": "alice", "status": "ok"}"#
    );
}

#[test]
fn sanitizer_redacts_escaped_json_bearer_headers_and_cookies() {
    let cases = [
        (
            r#"{"error": "Authorization: Bearer \"SECRET_AB\""}"#,
            "SECRET_AB",
        ),
        (r#"{"error": "x-api-key: \"SECRET_JK\""}"#, "SECRET_JK"),
        (
            r#"{"error": "Cookie: sid=\"SECRET_JC\"; theme=dark"}"#,
            "SECRET_JC",
        ),
        (r#"{"error": "got Bearer \"SECRET_JB\""}"#, "SECRET_JB"),
        (r#"{"error": "api-key: \"SECRET_X\""}"#, "SECRET_X"),
        (
            r#"{"error": "Authorization: Bearer \u0022SECRET_U\u0022"}"#,
            "SECRET_U",
        ),
        (r#"{"error": "Bearer\tSECRET_JT"}"#, "SECRET_JT"),
        (
            r#"{\"error\": \"Authorization: Bearer \\\"SECRET_D\\\"\"}"#,
            "SECRET_D",
        ),
        (
            r#"Authorization: {"a": "SECRET_HDR_OBJ"}"#,
            "SECRET_HDR_OBJ",
        ),
        (r#"token={"a": "SECRET_OBJ"}"#, "SECRET_OBJ"),
    ];
    assert_redacted(&cases);
}

#[test]
fn sanitizer_redacts_plain_and_malformed_escaped_quotes() {
    let cases = [
        (r#"got Bearer \"SECRET_F\""#, "SECRET_F"),
        (r#"got Bearer " SECRET_BS""#, "SECRET_BS"),
        (r#"got Bearer \" SECRET_BS2\""#, "SECRET_BS2"),
        (r#"token=\'SECRET_SQ\'"#, "SECRET_SQ"),
        (r#"{"token": \"SECRET_Y\"}"#, "SECRET_Y"),
        (r#"{bad, "token": 987654321}"#, "987654321"),
        (
            r#"https://u:SECRET_U@host/p?my_token=SECRET_Q\""#,
            "SECRET_U",
        ),
        (
            r#"https://u:SECRET_U@host/p?my_token=SECRET_Q\""#,
            "SECRET_Q",
        ),
    ];
    assert_redacted(&cases);
}

#[test]
fn sanitizer_redacts_json_primitives_and_nested_structures() {
    let cases = [
        (r#"{"token": 987654321}"#, "987654321"),
        (r#"{"token": true}"#, "true"),
        (r#"{"token": false}"#, "false"),
        (
            r#"{"credentials": {"u":"SECRET_U1","p":"SECRET_P1"}, "tokens": ["SECRET_X1","SECRET_Y1"]}"#,
            "SECRET_U1",
        ),
        (
            r#"{"credentials": {"u":"SECRET_U1","p":"SECRET_P1"}, "tokens": ["SECRET_X1","SECRET_Y1"]}"#,
            "SECRET_X1",
        ),
        (
            r#"{"outer": "{\"token\": \"SECRET_NEST\"}"}"#,
            "SECRET_NEST",
        ),
        (
            r#"{"error": "{\"headers\": {\"x-api-key\": \"SECRET_NESTED\"}, \"token\": 987654321}"}"#,
            "SECRET_NESTED",
        ),
        (
            r#"HTTP 401: {"error": "x-api-key: \"SECRET_E\""} retry"#,
            "SECRET_E",
        ),
        (
            r#"{"a1":{"a2":{"a3":{"a4":{"a5":{"a6":{"a7":{"a8":{"a9":{"token":"SECRET_DEEP"}}}}}}}}}}"#,
            "SECRET_DEEP",
        ),
        (
            r#"{"a1":{"a2":{"a3":{"a4":{"a5":{"a6":{"a7":{"a8":{"a9":{"token":987654321}}}}}}}}}}"#,
            "987654321",
        ),
        (r#"{"token": \"SECRET_TRUNC""#, "SECRET_TRUNC"),
        (
            r#"{"error": "x-api-key: \"SECRET_TRUNC2\""#,
            "SECRET_TRUNC2",
        ),
    ];
    assert_redacted(&cases);

    let adv = format!("{} token=SECRET_ADV", "[".repeat(500));
    let adv_out = sanitize_delivery_error(&adv);
    assert!(!adv_out.contains("SECRET_ADV"));
    assert!(adv_out.chars().count() <= 256);
}

#[test]
fn sanitizer_handles_camelcase_and_preserves_benign_inputs() {
    let cc = sanitize_delivery_error(r#"{"accessToken": "SECRET_S1", "authorName": "alice"}"#);
    assert!(!cc.contains("SECRET_S1"));
    assert!(cc.contains("[REDACTED]"));
    assert!(cc.contains("authorName"));
    assert!(cc.contains("alice"));

    let cc2 = sanitize_delivery_error(
        r#"{"accessToken": "SECRET_CC", "author": "alice", "passage": "north"}"#,
    );
    assert!(!cc2.contains("SECRET_CC"));
    assert!(cc2.contains("alice"));
    assert!(cc2.contains("north"));

    let preserved = [
        r#"{"author": "alice", "status": "ok"}"#,
        r#"{"author": "alice", "detail": " "}"#,
        "author=jane",
        "authority denied by upstream",
        "passage blocked at gate",
        "passenger=42 onboard",
        "passive mode enabled",
    ];
    for input in preserved {
        assert_eq!(sanitize_delivery_error(input), input);
    }

    assert_eq!(
        sanitize_delivery_error(r#"{"author": "alice", "detail": "   "}"#),
        r#"{"author": "alice", "detail": "   "}"#
    );
    assert_eq!(
        sanitize_delivery_error(r#"prefix   {"author": "alice", "status": "ok"}   suffix"#),
        r#"prefix {"author": "alice", "status": "ok"} suffix"#
    );
    assert_eq!(
        sanitize_delivery_error(r#"{"token": null}"#),
        r#"{"token": null}"#
    );
}

#[test]
fn sanitizer_handles_traps_whitespace_utf8_truncation_and_idempotence() {
    assert_eq!(
        sanitize_delivery_error("wasm trap: unreachable\n0xdeadbeef"),
        "plugin delivery failed"
    );
    assert_eq!(
        sanitize_delivery_error("wasmtime::runtime::func 0xdeadbeef"),
        "plugin delivery failed"
    );
    assert_eq!(
        sanitize_delivery_error("lookup failed at 0xdeadbeef"),
        "lookup failed at [addr]"
    );
    assert_eq!(
        sanitize_delivery_error("   \t\n  "),
        "plugin delivery failed"
    );

    let utf8_long = "🔥中".repeat(140);
    let truncated = sanitize_delivery_error(&utf8_long);
    assert_eq!(truncated.chars().count(), 256);
    assert!(truncated.ends_with("..."));

    let idempotence_cases = [
        r#"{"error": "Authorization: Bearer \"SECRET_AB\""}"#,
        r#"{"error": "x-api-key: \"SECRET_JK\""}"#,
        r#"{"error": "Cookie: sid=\"SECRET_JC\"; theme=dark"}"#,
        r#"{"token": 987654321}"#,
        r#"{"token": \"SECRET_Y\"}"#,
        r#"got Bearer " SECRET_BS""#,
        r#"token=\'SECRET_SQ\'"#,
        r#"{"author": "alice", "status": "ok"}"#,
        r#"{"accessToken": "S1", "authorName": "alice"}"#,
        "lookup failed at 0xdeadbeef",
        "[Bearer SECRET_BRACKET]",
        "token=[REDACTED]",
    ];
    for case in idempotence_cases {
        let once = sanitize_delivery_error(case);
        let twice = sanitize_delivery_error(&once);
        assert_eq!(once, twice, "not idempotent for `{case}`");
    }
}

#[test]
fn sanitizer_redacts_header_prefixes_with_quoted_secrets() {
    let depth10_exact = r#"{"k1":{"k2":{"k3":{"k4":{"k5":{"k6":{"k7":{"k8":{"k9":{"k10":{"error":"x-api-key: foo \"SECRET_D20H\""}}}}}}}}}}}"#;
    let cases = [
        (
            r#"{"error": "Authorization: Basic \"SECRET_B1\""}"#,
            "SECRET_B1",
        ),
        (
            r#"{"error": "Authorization: Token \"SECRET_TK1\""}"#,
            "SECRET_TK1",
        ),
        (
            r#"{"error": "Authorization: Custom \"SECRET_HP4\""}"#,
            "SECRET_HP4",
        ),
        (
            r#"{"error": "x-api-key: prefix \"SECRET_HP1\""}"#,
            "SECRET_HP1",
        ),
        (
            r#"{"error": "Cookie: prefix \"SECRET_CKP\""}"#,
            "SECRET_CKP",
        ),
        (r#"x-api-key: prefix "SECRET_HP2""#, "SECRET_HP2"),
        ("x-api-key: prefix 'SECRET_HP3'", "SECRET_HP3"),
        (r#"{"error": "x-api-key: foo \"SECRET_TJ2\""#, "SECRET_TJ2"),
        (depth10_exact, "SECRET_D20H"),
        (
            r#"[\"x-api-key: prefix \\\"SECRET_EA3\\\"\"]"#,
            "SECRET_EA3",
        ),
    ];
    assert_redacted(&cases);
    for (raw, _) in cases {
        let once = sanitize_delivery_error(raw);
        assert_eq!(
            once,
            sanitize_delivery_error(&once),
            "not idempotent: {raw}"
        );
    }

    let valid_outer = sanitize_delivery_error(r#"{"error": "x-api-key: foo ", "code": 401}"#);
    assert!(
        valid_outer.contains(r#""code":401"#),
        "lost code: {valid_outer}"
    );

    let malformed_outer = sanitize_delivery_error(r#"{"error": "x-api-key: foo ", "code": 401"#);
    assert!(malformed_outer.contains("[REDACTED]"));
    assert_eq!(malformed_outer, sanitize_delivery_error(&malformed_outer));
}

#[test]
fn sanitizer_redacts_escaped_slash_urls_and_triple_escaped_json() {
    let pe4 = format!(
        "{} {}",
        "[".repeat(70),
        r#"{"url": "https:\/\/u:SECRET_PE4@host\/p"}"#
    );
    let cases = [
        (
            r#"fetch failed: https:\/\/u:SECRET_RU1@host\/p?token=SECRET_RU2#frag"#,
            "SECRET_RU1",
        ),
        (
            r#"fetch failed: https:\/\/u:SECRET_RU1@host\/p?token=SECRET_RU2#frag"#,
            "SECRET_RU2",
        ),
        (r#"{"url": "https:\/\/u:SECRET_TU1@host\/p"#, "SECRET_TU1"),
        (
            r#"{ \"url\": \"https:\\/\\/u:SECRET_DSU@host\\/p\" }"#,
            "SECRET_DSU",
        ),
        (
            r#"{ \"url\": \"https:\/\/u:SECRET_DSU2@host\/p\" }"#,
            "SECRET_DSU2",
        ),
        (pe4.as_str(), "SECRET_PE4"),
        (
            r#"{\\\"error\\\": \\\"Bearer \\\\\\\"SECRET_T1\\\\\\\"\\\"}"#,
            "SECRET_T1",
        ),
        (
            r#"{\\\"error\\\": \\\"x-api-key: \\\\\\\"SECRET_T2\\\\\\\"\\\"}"#,
            "SECRET_T2",
        ),
        (r#"{\\\"token\\\": \\\\\\\"SECRET_T3\\\\\\\"}"#, "SECRET_T3"),
    ];
    assert_redacted(&cases);
    for (raw, _) in cases {
        let once = sanitize_delivery_error(raw);
        assert_eq!(
            once,
            sanitize_delivery_error(&once),
            "not idempotent: {raw}"
        );
    }
}

#[test]
fn sanitizer_handles_probe_exhaustion_unicode_quotes_unquoted_and_doubled_quotes() {
    let pe2 = format!(
        "{} {}",
        "[".repeat(70),
        r#"{"error": "got Bearer \u0022SECRET_PE2\u0022"}"#
    );
    let interleaved = format!(
        "{}{}",
        "[ {".repeat(40),
        r#"{"token":"SECRET_INTERLEAVED"}"#
    );
    let flood = format!("{} token=SECRET_FLOOD", "[".repeat(10_000));
    let cases = [
        (pe2.as_str(), "SECRET_PE2"),
        (interleaved.as_str(), "SECRET_INTERLEAVED"),
        (
            r#"{"error": "got Bearer \u0022SECRET_TUB\u0022""#,
            "SECRET_TUB",
        ),
        (r#"{bad, "token": \u0022SECRET_MU1\u0022}"#, "SECRET_MU1"),
        (r#"{bad, "token": \u0027SECRET_MU2\u0027}"#, "SECRET_MU2"),
        (r#"{bad, "token": SECRET_MUV1}"#, "SECRET_MUV1"),
        (r#"got Bearer ""SECRET_DDQ"""#, "SECRET_DDQ"),
        (r#"got Bearer \"\"SECRET_DEDQ\"\""#, "SECRET_DEDQ"),
        (r#"token=""SECRET_KDDQ"""#, "SECRET_KDDQ"),
        ("token=''SECRET_KDSQ''", "SECRET_KDSQ"),
        (r#"{"token": ""SECRET_JDDQ""}"#, "SECRET_JDDQ"),
    ];
    assert_redacted(&cases);
    for (raw, _) in cases {
        let once = sanitize_delivery_error(raw);
        assert_eq!(
            once,
            sanitize_delivery_error(&once),
            "not idempotent: {raw}"
        );
    }

    let flood_out = sanitize_delivery_error(&flood);
    assert!(!flood_out.contains("SECRET_FLOOD"));
    assert!(flood_out.chars().count() <= 256);
    assert_eq!(flood_out, sanitize_delivery_error(&flood_out));

    let empty_kv = sanitize_delivery_error(r#"token="" status=ok"#);
    assert!(
        empty_kv.contains("status=ok"),
        "swallowed sibling: {empty_kv}"
    );
}

#[test]
fn sanitizer_redacts_truncated_containers_and_preserves_idempotence() {
    let prob_trunc = format!(
        "{}{}",
        "[".repeat(70),
        r#"{"credentials":{"user":"SECRET_PROB_TRUNC""#
    );
    let cases = [
        (r#"{bad, "tokens": ["SECRET_ARR_TRUNC""#, "SECRET_ARR_TRUNC"),
        (
            r#"{bad, "credentials": {"user": "SECRET_OBJ_TRUNC""#,
            "SECRET_OBJ_TRUNC",
        ),
        (r#"token={"user": "SECRET_KV_TRUNC""#, "SECRET_KV_TRUNC"),
        (r#"tokens=["SECRET_KVARR_TRUNC""#, "SECRET_KVARR_TRUNC"),
        (
            r#"Authorization: {"user": "SECRET_HDR_TRUNC""#,
            "SECRET_HDR_TRUNC",
        ),
        (
            r#"Authorization: ["SECRET_HDR_ARR_TRUNC""#,
            "SECRET_HDR_ARR_TRUNC",
        ),
        (prob_trunc.as_str(), "SECRET_PROB_TRUNC"),
    ];
    assert_redacted(&cases);

    for (raw, _) in cases {
        let once = sanitize_delivery_error(raw);
        let twice = sanitize_delivery_error(&once);
        assert_eq!(once, twice, "not idempotent for `{raw}`");
    }
}
