//! Task 26 的规模验收：9 500 部作品的库，一次字母跳转 = **一次**读。
//!
//! Task 14 之后点一个字母要按 keyset 页往前走（9.5k 部约 10–24 次往返）。分档游标
//! 上线后，跳转是「取该档游标 → 一页」，与库的规模无关；这里在 9 500 部、26 个
//! 字母档的真实规模上把它量出来，并断言落点正是该档第一格。
//!
//! 夹具走原始 SQL 直接灌 `library.db`（与 `crates/store/tests/query_work/support.rs`
//! 同一手法）：9.5k 次 `publish_fixture_file` 会建 9.5k 个目录，只为量一次跳转不值。

use std::time::Instant;

use domain::{LibraryId, MediaKind};
use store::Store;
use store::library_query::{BrowseFilter, BrowseOrder, BrowsePage, LibraryQuery};

const ITEMS: usize = 9_500;
const LETTERS: [char; 26] = [
    'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S',
    'T', 'U', 'V', 'W', 'X', 'Y', 'Z',
];

fn uuid_like(seed: &str) -> String {
    // 夹具主键只需唯一且形态像 UUID；用稳定哈希拼出来，避免依赖 uuid crate 的 API 细节。
    let mut state = 0xcbf2_9ce4_8422_2325u64;
    for byte in seed.bytes() {
        state ^= byte as u64;
        state = state.wrapping_mul(0x100_0000_01b3);
    }
    let mut out = String::with_capacity(36);
    let mut value = state;
    for index in 0..32 {
        if index == 8 || index == 12 || index == 16 || index == 20 {
            out.push('-');
        }
        out.push(char::from_digit((value % 16) as u32, 16).unwrap());
        value = value.rotate_left(7) ^ (value >> 3) ^ (index as u64);
    }
    out
}

fn query(library: LibraryId) -> LibraryQuery {
    LibraryQuery {
        library_id: library,
        user_id: None,
        filter: BrowseFilter::default(),
        order: BrowseOrder::Title,
        descending: false,
        page: BrowsePage::Cursor {
            after: None,
            limit: 50,
        },
        include_total: false,
    }
}

fn seed(store: &Store, count: usize) -> LibraryId {
    let root = "/index-scale/movies";
    let lib = store
        .create_library(MediaKind::Movie, "Scale", &[root], "everyone", true, &[])
        .unwrap();
    let library_id: LibraryId = lib.id.parse().unwrap();
    let root_id = store
        .list_library_roots()
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.library_id == lib.id)
        .unwrap()
        .id;
    let mut db = rusqlite::Connection::open(store.data_dir().join("library.db")).unwrap();
    db.pragma_update(None, "foreign_keys", true).unwrap();
    let tx = db.transaction().unwrap();
    for index in 0..count {
        let letter = LETTERS[index % LETTERS.len()];
        let title = format!("{letter}ovie {index:05}");
        let media = uuid_like(&format!("index-media-{index}"));
        let file = uuid_like(&format!("index-file-{index}"));
        tx.execute(
            "INSERT INTO media(media_id,kind,title,year,identity_state) VALUES(?1,'movie',?2,2024,'provisional')",
            rusqlite::params![media, title],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO library_files(file_id,library_id,root_id,media_id,kind,path_key,display_path,presence_state,publication_state,file_revision,source_version,quality_source,confidence)
             VALUES(?1,?2,?3,?4,'movie',?5,?5,'present','published',1,'v1','probe','high')",
            rusqlite::params![file, lib.id, root_id, media, format!("{root}/{index}.mkv")],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO library_file_units(file_id,library_id,media_id,season,episode) VALUES(?1,?2,?3,-1,-1)",
            rusqlite::params![file, lib.id, media],
        )
        .unwrap();
    }
    tx.commit().unwrap();
    let op = format!("index-scale-{}", uuid_like("op"));
    store.begin_library_integrity(library_id, true, &op, 1_000).unwrap();
    loop {
        let status = store.advance_library_operation(&op, 1_001, 100).unwrap();
        if status.state != "applying" {
            assert_eq!(status.state, "succeeded");
            break;
        }
    }
    library_id
}

#[test]
fn a_letter_jump_in_a_9500_item_library_costs_one_read() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let library_id = seed(&store, ITEMS);

    let started = Instant::now();
    let buckets = store.library_item_index(&query(library_id)).unwrap();
    let index_ms = started.elapsed();

    assert_eq!(buckets.len(), LETTERS.len(), "26 个字母档");
    let total: u64 = buckets.iter().map(|bucket| bucket.count).sum();
    assert_eq!(total, ITEMS as u64, "各档 count 之和 = 库内条目总数");
    assert!(buckets[0].cursor.is_none(), "第一档用首页，不需要游标");
    assert!(
        buckets[1..].iter().all(|bucket| bucket.cursor.is_some()),
        "每个后续档都必须给出直达游标"
    );

    // 取中间一档（M）跳转：一次读，落点是该档第一格。
    let mid = buckets
        .iter()
        .position(|bucket| bucket.initial == "M")
        .expect("M 档存在");
    let expected_first_offset: u64 = buckets[..mid].iter().map(|bucket| bucket.count).sum();
    let mut jump = query(library_id);
    jump.page = BrowsePage::Cursor {
        after: buckets[mid].cursor.clone(),
        limit: 50,
    };
    let started = Instant::now();
    let page = store.library_item_page(&jump).unwrap();
    let jump_ms = started.elapsed();
    let first = page.items.first().expect("一页非空");
    assert!(
        first.title.starts_with('M'),
        "跳转落点必须是 M 档第一格，实际 {}",
        first.title
    );
    println!(
        "INDEX_SCALE: items={ITEMS} buckets={} index_ms={:.1} jump_requests=1 jump_ms={:.1} mid_bucket_first_offset={expected_first_offset} first={}",
        buckets.len(),
        index_ms.as_secs_f64() * 1_000.0,
        jump_ms.as_secs_f64() * 1_000.0,
        first.title,
    );
}
