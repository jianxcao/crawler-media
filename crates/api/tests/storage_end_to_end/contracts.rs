//! Item identity and authenticated stream contracts, separate from the storage lifecycle.

use super::*;
use domain::TypedItemId;

/// 规范 ID 只有 `w1_/s1_/u1_` typed 形态；数字 ID 与物理 file id 不是 item id。
#[tokio::test]
async fn typed_item_ids_are_canonical_and_legacy_file_ids_are_refused() {
    let fixture = Fixture::new();
    let app = fixture.app.clone();
    let file_id = LedgerId::new();
    let media_id = fixture.seed_episode(file_id);
    let items = browse(&app, &fixture.library_id).await;
    assert_eq!(items.len(), 1, "条目必须出现");

    // 条目身份就是 MediaId 本身（字符串化的 UUID），与目录卡片的 DTO 一致。
    let card_id = items[0]["media_item_id"]
        .as_str()
        .expect("卡片必须带 media_item_id")
        .to_string();
    assert_eq!(card_id, media_id.to_string(), "卡片身份必须等于媒体身份");
    assert!(
        card_id.parse::<MediaId>().is_ok(),
        "卡片身份必须是可解析的 UUID 字符串"
    );

    // typed wire id 是规范形态，且必须能原样往返。
    let work = TypedItemId::Work(media_id);
    let work_wire = work.to_wire_string();
    assert!(work_wire.starts_with("w1_"), "work id 必须是 w1_ 前缀");
    assert_eq!(TypedItemId::from_wire_string(&work_wire), Some(work));
    let season = TypedItemId::Season {
        media: media_id,
        season: SEASON as u32,
    };
    let season_wire = season.to_wire_string();
    assert!(season_wire.starts_with("s1_"));
    assert_eq!(TypedItemId::from_wire_string(&season_wire), Some(season));
    let unit = TypedItemId::Unit {
        media: media_id,
        key: UnitKey::new(SEASON, EPISODE),
    };
    let unit_wire = unit.to_wire_string();
    assert!(unit_wire.starts_with("u1_"), "unit id 必须是 u1_ 前缀");
    assert_eq!(TypedItemId::from_wire_string(&unit_wire), Some(unit));

    // 物理 file id（裸 UUID）与数字 ID 都不是 typed item id。
    assert_eq!(TypedItemId::from_wire_string(&file_id.to_string()), None);
    assert_eq!(TypedItemId::from_wire_string("42"), None);
    assert_eq!(TypedItemId::from_wire_string("207890"), None);

    // TV 的整片 unit 与 S00/E00 都是合法坐标；电影没有坐标（由调用方传 WHOLE）。
    let whole = TypedItemId::Unit {
        media: media_id,
        key: UnitKey::WHOLE,
    };
    let whole_wire = whole.to_wire_string();
    assert!(UnitKey::WHOLE.is_whole());
    assert_eq!(TypedItemId::from_wire_string(&whole_wire), Some(whole));
    let zero = TypedItemId::Unit {
        media: media_id,
        key: UnitKey::new(0, 0),
    };
    assert!(!zero.to_wire_string().is_empty());
    assert_eq!(TypedItemId::from_wire_string("u1_not-a-uuid_1_3"), None);
    assert_eq!(
        TypedItemId::from_wire_string(&format!("u1_{}_1", media_id)),
        None,
        "缺少 episode 的 unit id 必须被拒"
    );

    // 旧 file-ID 逻辑路由必须拒绝，规范 ID 必须可用。
    let legacy = send(
        &app,
        "GET",
        &format!("/api/v1/libraries/{}/items/{file_id}", fixture.library_id),
        Some(TOKEN),
        Value::Null,
    )
    .await;
    assert_eq!(
        legacy.status(),
        StatusCode::NOT_FOUND,
        "物理 file id 不是 item id"
    );
    let canonical = send(
        &app,
        "GET",
        &format!("/api/v1/libraries/{}/items/{card_id}", fixture.library_id),
        Some(TOKEN),
        Value::Null,
    )
    .await;
    assert_eq!(canonical.status(), StatusCode::OK, "规范 typed id 必须可读");
}

/// Historical items re-verified in Task 13: an unknown cache source is a client error, and the
/// Web session stream URL (which carries only `api_key`) authenticates by its query token.
#[tokio::test]
async fn unknown_cache_source_is_400_and_api_key_session_stream_authenticates() {
    let fixture = Fixture::new();
    let app = fixture.app.clone();
    let unknown = send(
        &app,
        "DELETE",
        "/api/v1/catalog/cache?source=unknown&cache_key=k",
        Some(TOKEN),
        Value::Null,
    )
    .await;
    assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);

    let file_id = LedgerId::new();
    let media_id = fixture.seed_episode(file_id);
    // The seeded episode is a STRM; give it a real target so the stream resolves to a redirect.
    std::fs::write(
        fixture.root.join(EPISODE_NAME),
        "http://media.example.invalid/e03.mkv",
    )
    .unwrap();
    let psid = {
        let handle = fixture.state.store();
        let store = handle.as_ref();
        let user = store.user_by_token(TOKEN).unwrap().unwrap().id;
        let version = store.get_file_version(file_id).unwrap().unwrap();
        store
            .issue_play_session(
                user,
                "web-1",
                file_id,
                media_id,
                UnitKey::new(SEASON, EPISODE),
                &version,
                1,
            )
            .unwrap()
    };
    let uri = format!("/api/v1/playback/sessions/{psid}/stream?device_id=web-1&api_key={TOKEN}");
    let response = send(&app, "GET", &uri, None, Value::Null).await;
    assert_ne!(
        response.status(),
        StatusCode::UNAUTHORIZED,
        "api_key-only stream must authenticate"
    );
    assert!(
        response.status().is_success() || response.status().is_redirection(),
        "issued api_key stream: {}",
        response.status()
    );
}
