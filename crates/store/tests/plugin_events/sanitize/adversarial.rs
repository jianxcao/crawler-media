use std::time::{Duration, Instant};

use store::Store;
use store::plugin_events::{
    ClaimDueDeliveriesInput, EmitOutboxEventInput, MarkDeadLetterInput, OutboxDb,
    ScheduleRetryInput, SeedDeliveryInput, sanitize_delivery_error,
};

fn assert_redacted_and_idempotent(cases: &[(&str, &str)]) {
    for (raw, secret) in cases {
        let out = sanitize_delivery_error(raw);
        assert!(
            !out.contains(secret),
            "leaked `{secret}` in `{raw}` -> `{out}`"
        );
        assert!(out.contains("[REDACTED]"), "missing marker in `{out}`");
        assert!(out.chars().count() <= 256, "exceeded 256 chars: `{out}`");
        let twice = sanitize_delivery_error(&out);
        assert_eq!(out, twice, "not idempotent for `{raw}`");
    }
}

#[test]
fn sanitizer_handles_backslash_flood_without_quadratic_dos() {
    let bs_10k = format!("{} token=SECRET_BS10K", "\\".repeat(10_000));
    let bs_30k = format!("{} token=SECRET_BS30K", "\\".repeat(30_000));
    let bs_100k = format!("{} token=SECRET_BS100K", "\\".repeat(100_000));
    let alt_bs = format!("{} token=SECRET_ALTBS", "\"\\".repeat(15_000));
    let lbrack_10k = format!("{} token=SECRET_LBRACK", "[a".repeat(10_000));
    let open_10k = format!("{}{}", "[".repeat(10_000), "a] token=SECRET_OPEN10K");
    let dbl_brack = format!(
        "{}{}",
        "[[a".repeat(5_000),
        r#"{"token":"SECRET_DBLBRACK"}}}}}}}}"#
    );

    for (raw, secret) in [
        (bs_10k.as_str(), "SECRET_BS10K"),
        (bs_30k.as_str(), "SECRET_BS30K"),
        (bs_100k.as_str(), "SECRET_BS100K"),
        (alt_bs.as_str(), "SECRET_ALTBS"),
        (lbrack_10k.as_str(), "SECRET_LBRACK"),
        (open_10k.as_str(), "SECRET_OPEN10K"),
        (dbl_brack.as_str(), "SECRET_DBLBRACK"),
    ] {
        let t0 = Instant::now();
        let out = sanitize_delivery_error(raw);
        let elapsed = t0.elapsed();
        assert!(
            elapsed < Duration::from_millis(200),
            "DoS latency {elapsed:?} for len={} (`{secret}`)",
            raw.len()
        );
        assert!(!out.contains(secret), "leaked `{secret}` -> `{out}`");
        assert!(out.chars().count() <= 256);
        assert_eq!(out, sanitize_delivery_error(&out));
    }

    let bs33 = format!(
        r#"{b33}"token{b31}": {b31}"SECRET_BS33{b31}""#,
        b33 = "\\".repeat(33),
        b31 = "\\".repeat(31)
    );
    let bs7 = format!(r#"{b}"token{b}": {b}"SECRET_BS7{b}""#, b = "\\".repeat(7));
    let bs15 = format!(r#"{b}"token{b}": {b}"SECRET_BS15{b}""#, b = "\\".repeat(15));
    assert_redacted_and_idempotent(&[
        (bs33.as_str(), "SECRET_BS33"),
        (bs7.as_str(), "SECRET_BS7"),
        (bs15.as_str(), "SECRET_BS15"),
    ]);
}

#[test]
fn sanitizer_redacts_newline_and_nbsp_after_kv_and_header_separators() {
    let cases = [
        ("token=\nSECRET_KVNL", "SECRET_KVNL"),
        ("token=\u{00A0}SECRET_KVNBSP", "SECRET_KVNBSP"),
        ("x-api-key:\nSECRET_HDRNL", "SECRET_HDRNL"),
        ("{\"error\": \"x-api-key:\nSECRET_HJL2\"}", "SECRET_HJL2"),
        ("x-api-key:\u{00A0}SECRET_HNBSP", "SECRET_HNBSP"),
        ("token\u{00A0}=SECRET_NBSP_LHS", "SECRET_NBSP_LHS"),
        (
            "{bad, \"token\":\u{00A0}\"SECRET_JSON_NBSP\"}",
            "SECRET_JSON_NBSP",
        ),
    ];
    assert_redacted_and_idempotent(&cases);

    let blank_line_kv = sanitize_delivery_error("token=\n\nother_ok=1");
    assert!(
        blank_line_kv.contains("other_ok=1"),
        "swallowed sibling: {blank_line_kv}"
    );
    assert_eq!(blank_line_kv, sanitize_delivery_error(&blank_line_kv));
}

#[test]
fn sanitizer_redacts_multiline_pretty_truncated_containers() {
    let cases = [
        (
            "{bad, \"credentials\": {\n  \"u\": \"SECRET_PTO1\"\n",
            "SECRET_PTO1",
        ),
        ("{bad, \"tokens\": [\n  \"SECRET_PTA1\"\n", "SECRET_PTA1"),
        ("token={\n  \"u\": \"SECRET_PKT1\"\n", "SECRET_PKT1"),
        (
            "Authorization: {\n  \"u\": \"SECRET_PHT1\"\n",
            "SECRET_PHT1",
        ),
        (
            "Authorization: {\n  \"u\": \"SECRET_ML1\",\n  \"p\": \"SECRET_ML2\"\n",
            "SECRET_ML2",
        ),
        (
            r#"{"error": "Authorization: Basic {\"user\": \"SECRET_ESC_TRUNC\"", "other": "ok"}"#,
            "SECRET_ESC_TRUNC",
        ),
    ];
    assert_redacted_and_idempotent(&cases);

    let esc_trunc = sanitize_delivery_error(
        r#"{"error": "Authorization: Basic {\"user\": \"SECRET_ESC_TRUNC\"", "other": "ok"}"#,
    );
    assert!(
        esc_trunc.contains("\"other\"") && esc_trunc.contains("\"ok\""),
        "swallowed sibling field: {esc_trunc}"
    );
}

#[test]
fn sanitizer_redacts_prefixed_header_and_kv_containers() {
    let cases = [
        (
            "Authorization: Custom {\"u\": \"SECRET_HPO1\"}",
            "SECRET_HPO1",
        ),
        (
            r#"{"error": "Authorization: Custom {\"u\": \"SECRET_HPO2\"}"}"#,
            "SECRET_HPO2",
        ),
        (
            "Authorization: Basic {\"user\": \"SECRET_HDR_PREFIX_OBJ\"",
            "SECRET_HDR_PREFIX_OBJ",
        ),
        (
            "Authorization: Basic {\"user\": \"SECRET_CLOSED_BASIC\"}",
            "SECRET_CLOSED_BASIC",
        ),
        (
            r#"{"error": "Authorization: Basic {\"user\": \"SECRET_JSON_BASIC\"}"}"#,
            "SECRET_JSON_BASIC",
        ),
        (
            r#"token=prefix{"user": "SECRET_KV_PREFIX_OBJ""#,
            "SECRET_KV_PREFIX_OBJ",
        ),
        (r#"token=prefix{"user": "SECRET_9"}&ok=1"#, "SECRET_9"),
        (
            r#"{"error": "Authorization: Basic {\"user\": \"SECRET_JSON_OTHER\"}", "other": "ok"}"#,
            "SECRET_JSON_OTHER",
        ),
    ];
    assert_redacted_and_idempotent(&cases);

    let kv_ok = sanitize_delivery_error(r#"token=prefix{"user": "SECRET_9"}&ok=1"#);
    assert!(!kv_ok.contains("SECRET_9"));
    assert!(kv_ok.contains("[REDACTED]"));
    assert_eq!(kv_ok, sanitize_delivery_error(&kv_ok));

    let json_other = sanitize_delivery_error(
        r#"{"error": "Authorization: Basic {\"user\": \"SECRET_JSON_OTHER\"}", "other": "ok"}"#,
    );
    assert!(
        json_other.contains("\"other\"") && json_other.contains("\"ok\""),
        "lost sibling json field: {json_other}"
    );
}

#[test]
fn sanitizer_redacts_bearer_containers_non_sep_quotes_and_normal_bearer() {
    let cases = [
        (
            "Authorization: Bearer {\"user\": \"SECRET_BEARER_OBJ\"",
            "SECRET_BEARER_OBJ",
        ),
        (
            "Authorization: Bearer {\"user\": \"SECRET_CLOSED_BEARER\"}",
            "SECRET_CLOSED_BEARER",
        ),
        (
            "Authorization: Bearer [\"SECRET_CLOSED_BEARER_ARR\"]",
            "SECRET_CLOSED_BEARER_ARR",
        ),
        ("x-api-key: foo\"SECRET_HNS1\"", "SECRET_HNS1"),
        ("Authorization: Basic/\"SECRET_HNS2\"", "SECRET_HNS2"),
    ];
    assert_redacted_and_idempotent(&cases);

    let normal = sanitize_delivery_error("Authorization: Bearer SECRET_NORMAL");
    assert_eq!(normal, "Authorization: [REDACTED]");
    assert_eq!(normal, sanitize_delivery_error(&normal));
}

#[test]
fn sanitizer_handles_nonadj_flood_unicode_keys_and_repeated_quotes() {
    let na70 = format!(
        "{}{}",
        "[a".repeat(70),
        r#"{"\u0074oken": "SECRET_NA_UKEY"}"#
    );
    let na65 = format!("{}{}", "{a".repeat(65), r#"{"\u0074oken": "SECRET_NA65"}"#);
    let c65 = format!("{}{}", "[1,".repeat(65), r#"{"\u0074oken": "SECRET_C65"}"#);
    let c100 = format!(
        "{}{}",
        "[1,".repeat(100),
        r#"{"\u0074oken": "SECRET_C100"}"#
    );
    let cases = [
        (na70.as_str(), "SECRET_NA_UKEY"),
        (na65.as_str(), "SECRET_NA65"),
        (c65.as_str(), "SECRET_C65"),
        (
            r#"{bad, "\u0074oken": "SECRET_BAD_UKEY"}"#,
            "SECRET_BAD_UKEY",
        ),
        (
            r#"[[{\"error\": \"Bearer \\\"SECRET_ESC_ARR\\\"\"}]]"#,
            "SECRET_ESC_ARR",
        ),
        (
            r#"got Bearer \u0022\u0022SECRET_UDD\u0022\u0022"#,
            "SECRET_UDD",
        ),
        (r#"got Bearer """SECRET_TQ1""""#, "SECRET_TQ1"),
        (r#"token="""SECRET_TQ2""""#, "SECRET_TQ2"),
        (r#"{"token": """SECRET_TQ3"""}"#, "SECRET_TQ3"),
        (r#"x-api-key: """SECRET_TQ4""""#, "SECRET_TQ4"),
        (r#"got Bearer "" SECRET_DDS1"""#, "SECRET_DDS1"),
        (r#"token="" SECRET_DDS2"""#, "SECRET_DDS2"),
        (r#"{"token": "" SECRET_DDS3""}"#, "SECRET_DDS3"),
        (
            "got Bearer \"\"\u{00a0}SECRET_NBSP_DD\"\"",
            "SECRET_NBSP_DD",
        ),
    ];
    assert_redacted_and_idempotent(&cases);

    let c100_out = sanitize_delivery_error(&c100);
    assert!(!c100_out.contains("SECRET_C100"));
    assert!(c100_out.chars().count() <= 256);
    assert_eq!(c100_out, sanitize_delivery_error(&c100_out));

    let empty_kv = sanitize_delivery_error(r#"token="" status=ok"#);
    assert!(
        empty_kv.contains("status=ok"),
        "swallowed sibling: {empty_kv}"
    );
    assert_eq!(empty_kv, sanitize_delivery_error(&empty_kv));

    let empty_json = sanitize_delivery_error(r#"{"token": "", "code": 401}"#);
    assert!(empty_json.contains("401"), "swallowed code: {empty_json}");
    assert_eq!(empty_json, sanitize_delivery_error(&empty_json));

    let triple_empty = sanitize_delivery_error(r#"{"token": """, "code": 401}"#);
    assert!(triple_empty.contains("[REDACTED]"));
    assert_eq!(triple_empty, sanitize_delivery_error(&triple_empty));

    let nl_empty = sanitize_delivery_error("token=\"\"\nstatus=\"\"");
    assert!(
        nl_empty.contains("status=\"\""),
        "swallowed status: {nl_empty}"
    );
    assert_eq!(nl_empty, sanitize_delivery_error(&nl_empty));
}

#[test]
fn sanitizer_truncation_at_256_chars_is_strictly_idempotent_across_offsets() {
    for n in 200..=260 {
        let inputs = [
            format!(r#"{{"msg":"{}","token":"SECRET_PAD"}}"#, "a".repeat(n)),
            format!("{} token=SECRET_PAD ok", "a".repeat(n)),
            format!(r#"{{"msg":"{}","note":"a   b"}}"#, "x".repeat(n)),
            format!(
                r#"{{"msg":"{}","note":"x   token=SECRET_PAD"}}"#,
                "a".repeat(n)
            ),
            format!(r#"{{"msg":"{}","token":null}}"#, "a".repeat(n)),
        ];
        for raw in inputs {
            let out = sanitize_delivery_error(&raw);
            assert!(!out.contains("SECRET_PAD"), "leaked at n={n}: {out}");
            assert!(out.chars().count() <= 256, "len > 256 at n={n}");
            assert_eq!(
                out,
                sanitize_delivery_error(&out),
                "non-idemp at n={n}: {out}"
            );
        }
    }
    for n in 220..=235 {
        let raw = format!(r#"{{"msg":"{}","token":"SECRET_PAD"}}"#, "🔥".repeat(n));
        let out = sanitize_delivery_error(&raw);
        assert!(!out.contains("SECRET_PAD"), "leaked utf8 at n={n}: {out}");
        assert!(out.chars().count() <= 256);
        assert_eq!(
            out,
            sanitize_delivery_error(&out),
            "non-idemp utf8 at n={n}"
        );
    }
}

#[test]
fn sanitizer_persists_redacted_errors_via_schedule_retry_and_mark_dead_letter() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(dir.path()).expect("open store");
    for (idx, eid) in ["ev-san-1", "ev-san-2"].iter().enumerate() {
        let ts = 1000 + idx as i64;
        store
            .emit_outbox_event(
                OutboxDb::Library,
                &EmitOutboxEventInput {
                    event_id: (*eid).into(),
                    schema_version: 1,
                    event_domain: "transfer".into(),
                    event_kind: "transfer.completed".into(),
                    entity_type: "media".into(),
                    entity_id: format!("m-{idx}"),
                    entity_sequence: None,
                    entity_revision: 1,
                    cause_id: format!("cause-{eid}"),
                    subject: "system".into(),
                    site_id: None,
                    subscribe_id: None,
                    library_id: Some("lib-1".into()),
                    handles_json: "[]".into(),
                    payload_json: "{}".into(),
                    occurred_at: ts,
                    created_at: ts,
                },
            )
            .expect("emit");
        store
            .seed_subscriber_deliveries(
                OutboxDb::Library,
                &[SeedDeliveryInput {
                    event_id: (*eid).into(),
                    plugin_id: "p1".into(),
                    next_attempt_at: 1000,
                }],
                ts,
            )
            .expect("seed");
    }

    let claimed = store
        .claim_due_deliveries(
            OutboxDb::Library,
            &ClaimDueDeliveriesInput {
                plugin_id: Some("p1".into()),
                now_ms: 2000,
                lease_duration_ms: 5_000,
                limit: Some(10),
            },
        )
        .expect("claim");
    assert_eq!(claimed.len(), 2);

    let retried = store
        .schedule_retry(
            OutboxDb::Library,
            &ScheduleRetryInput {
                event_id: claimed[0].event.event_id.clone(),
                plugin_id: "p1".into(),
                claim_nonce: claimed[0].claim_nonce.clone(),
                next_attempt_at: 3000,
                error: "Authorization: Basic {\"user\": \"SECRET_PERSIST_RETRY\"".into(),
                now_ms: 2100,
            },
        )
        .expect("retry");
    assert_eq!(
        retried.last_error.as_deref(),
        Some("Authorization: [REDACTED]")
    );

    let dead = store
        .mark_dead_letter(
            OutboxDb::Library,
            &MarkDeadLetterInput {
                event_id: claimed[1].event.event_id.clone(),
                plugin_id: "p1".into(),
                claim_nonce: claimed[1].claim_nonce.clone(),
                error: "Authorization: Bearer SECRET_NORMAL".into(),
                now_ms: 2100,
            },
        )
        .expect("dead letter");
    assert_eq!(
        dead.last_error.as_deref(),
        Some("Authorization: [REDACTED]")
    );
}

#[test]
fn sanitizer_handles_multibyte_unicode_whitespace_and_utf8_boundaries_without_panic() {
    let cases = [
        ("Bearer\u{00A0}secret_token", "secret_token"),
        ("Bearer\u{3000}secret_token", "secret_token"),
        ("Bearer\u{2003}秘密_secret_token", "秘密_secret_token"),
        (
            "上游错误：Bearer\u{00A0}秘密_bearer_val 状态=失败",
            "秘密_bearer_val",
        ),
        ("前缀 token=\u{3000}秘密_kv_val 后缀=正常", "秘密_kv_val"),
        (
            "{bad, \"token\"\u{00A0}:\u{3000}\"SECRET_QKV_UTF8\"}",
            "SECRET_QKV_UTF8",
        ),
    ];
    assert_redacted_and_idempotent(&cases);

    let prefixed = sanitize_delivery_error("上游错误：Bearer\u{00A0}秘密_bearer_val 状态=失败");
    assert!(prefixed.contains("上游错误："));
    assert!(prefixed.contains("状态=失败"));

    let trailing_hdr_ws = sanitize_delivery_error(r#"{"error": "Authorization:\n   "}"#);
    assert!(trailing_hdr_ws.contains("[REDACTED]"));
    assert_eq!(trailing_hdr_ws, sanitize_delivery_error(&trailing_hdr_ws));
}
