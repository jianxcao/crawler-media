use super::*;

/// 改根（scope 变化）后统计仍按 `library_id` 归属，不复用也不丢失已有行。
#[tokio::test]
async fn library_scope_change_does_not_reuse_old_statistics() {
    let seeded = empty_fixture();
    let tv_other;
    let old_root;
    {
        let store_arc = seeded.fixture.state.store();
        let guard = store_arc.as_ref();
        let (id, root) = new_library(
            guard,
            seeded.fixture.dir.path(),
            "TV Other",
            MediaKind::Tv,
            "everyone",
            true,
        );
        tv_other = id;
        let library = library_of(guard, &tv_other);
        seed_rows(guard, &library, &root, "T", MediaKind::Tv, 2, 1, 0);
        old_root = root;
    }
    let before = seeded.list().await;
    let other_before = StatsFixture::stats(&before, &tv_other);
    assert_eq!(other_before["file_count"].as_u64(), Some(2));
    assert_eq!(
        other_before["total_size_bytes"].as_u64(),
        Some(2 * FAKE_SIZE)
    );

    // 改范围：给同一个库再加一个空根（既有根保留，已发布行不动）。
    let extra = seeded.fixture.dir.path().join("tv-other-extra");
    std::fs::create_dir_all(&extra).unwrap();
    let old_str = old_root.display().to_string();
    let extra_str = extra.display().to_string();
    seeded
        .fixture
        .state
        .store()
        .update_library_full(
            &tv_other,
            None,
            Some(&[old_str.as_str(), extra_str.as_str()]),
            None,
            None,
            None,
        )
        .unwrap();

    let after = seeded.list().await;
    let other_after = StatsFixture::stats(&after, &tv_other);
    assert_eq!(
        other_after["item_count"].as_u64(),
        Some(2),
        "scope 变化后作品数仍必须来自库表：{other_after}"
    );
    assert_eq!(
        other_after["file_count"].as_u64(),
        Some(2),
        "scope 变化后文件数仍必须来自库表：{other_after}"
    );
    assert_eq!(
        other_after["strm_file_count"].as_u64(),
        Some(0),
        "scope 变化后 strm 文件数仍必须来自库表：{other_after}"
    );
    assert_eq!(
        other_after["total_size_bytes"].as_u64(),
        Some(2 * FAKE_SIZE),
        "同一 library_id 的已发布体积不得因加根而丢失：{other_after}"
    );
}

/// 同数量文件的 `.mkv` → `.strm` 变化后，物理体积必须按新发布事实归零。
#[tokio::test]
async fn same_count_strm_change_does_not_reuse_physical_bytes_as_fresh() {
    let seeded = empty_fixture();
    let library_id = seeded.fixture.library_id.clone();
    let store_arc = seeded.fixture.state.store();
    let (root_id, media_id, file_id, from, to) = {
        let guard = store_arc.as_ref();
        let library = library_of(guard, &library_id);
        let root_id = root_id_of(guard, &library_id);
        let media_id = insert_media(guard, MediaKind::Movie, "A-00000");
        let file_id = LedgerId::new();
        let from = seeded.fixture.root.join("A-00000.0.mkv");
        publish_file(
            guard,
            &library,
            &root_id,
            media_id,
            file_id,
            new_file_version(),
            &from,
            FAKE_SIZE,
        );
        (
            root_id,
            media_id,
            file_id,
            from,
            seeded.fixture.root.join("A-00000.0.strm"),
        )
    };
    let before = seeded.list().await;
    let old = StatsFixture::stats(&before, &library_id);
    assert_eq!(old["total_size_bytes"].as_u64(), Some(FAKE_SIZE));
    assert_eq!(old["stale"], false, "库表聚合必须是最新值");

    {
        let guard = store_arc.as_ref();
        assert!(
            store::test_support::force_file_path(
                guard,
                &from.display().to_string(),
                &to.display().to_string()
            )
            .unwrap()
        );
        // 扫描器观察到指针文件后按新体积重发布同一文件（同一 revision）。
        let version = guard
            .get_file_version(file_id)
            .unwrap()
            .expect("file version");
        let library = library_of(guard, &library_id);
        publish_file(
            guard, &library, &root_id, media_id, file_id, version, &to, 0,
        );
    }

    let after = seeded.list().await;
    let stats = StatsFixture::stats(&after, &library_id);
    assert_eq!(stats["file_count"].as_u64(), Some(1));
    assert_eq!(stats["strm_file_count"].as_u64(), Some(1));
    assert_eq!(
        stats["total_size_bytes"].as_u64(),
        Some(0),
        "同数量 .strm 替换后不得复用旧的物理体积：{stats}"
    );
}
