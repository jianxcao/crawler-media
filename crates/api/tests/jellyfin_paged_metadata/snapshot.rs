//! Jellyfin pages render the composed Library snapshot, not live sidecars.

use super::*;

#[tokio::test]
async fn page_metadata_uses_composed_snapshot_and_ignores_live_nfo() {
    let fixture = tv_fixture(1, 1);
    let media = media_for_title(&fixture.state, "Show000");
    paged::adopt_show_metadata(&fixture.state, media);
    let sidecar = fixture.roots[0].join("Show000").join("tvshow.nfo");
    std::fs::write(
        &sidecar,
        b"<tvshow><plot>live plot</plot><studio>Live Studio</studio><genre>Comedy</genre></tvshow>",
    )
    .unwrap();
    let app = router(fixture.state.clone());
    let uri = format!(
        "/Items?ParentId={}&Recursive=true&IncludeItemTypes=Series&SortBy=SortName",
        fixture.library_id
    );
    let listing = get_json(&app, &uri).await;
    assert_eq!(items(&listing).len(), 1, "{listing}");
    assert_eq!(items(&listing)[0]["Overview"], "adopted plot", "{listing}");
    assert_eq!(
        items(&listing)[0]["Studios"][0]["Name"],
        "Studio X",
        "{listing}"
    );
    assert_eq!(items(&listing)[0]["Genres"][0], "Drama", "{listing}");

    std::fs::write(&sidecar, b"<tvshow><plot>broken").unwrap();
    let after = get_json(&app, &uri).await;
    assert_eq!(after, listing, "坏 NFO 不得改变页内元数据");
}
