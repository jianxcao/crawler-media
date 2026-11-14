//! P7: 目录前缀过滤改用键区间（`path_key::prefix_upper_bound`）而不是 `LIKE`。
//!
//! root 改指时要按「某一批目录」挑出 `library_metadata_inputs` 里的行。原来每条
//! 目录都跑一次 `input_key LIKE ? OR path_key LIKE ?`，既用不上索引（每条目录扫
//! 一遍整个 inputs 集），又把路径里真实存在的 `%` / `_` 当通配符——含这两个字符
//! 的目录会多匹配出**并不在它下面**的行，把别的作品的 NFO 搬到错误的根上。
//!
//! 这里锁的是替换后的语义：`[dir, 上界)` 范围与「目录本身 + 目录下路径」逐字
//! 对应，`%`/`_`/非 ASCII 一律按字面比较。

use store::path_key::prefix_upper_bound;

/// 目录下的路径键区间：`[dir/, 上界)`，不含目录本身。
fn directory_range(directory: &str) -> (String, String) {
    let lower = if directory.ends_with('/') {
        directory.to_string()
    } else {
        format!("{directory}/")
    };
    let upper = prefix_upper_bound(&lower);
    (lower, upper)
}

/// 语义基准：目录本身，或目录下的路径（逐字前缀，不做任何通配）。
///
/// 根目录 `/` 的「下面」就是所有绝对路径。
fn under_prefix(key: &str, directory: &str) -> bool {
    if directory == "/" {
        return key.starts_with('/');
    }
    key == directory
        || key
            .strip_prefix(directory)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// 新的范围谓词。
fn in_range(key: &str, directory: &str) -> bool {
    let (lower, upper) = directory_range(directory);
    key == directory || (key >= lower.as_str() && key < upper.as_str())
}

/// `%` / `_` 在 SQL `LIKE` 里是通配符，在范围谓词里是普通字符。
const DIRECTORIES: &[&str] = &[
    "/media/movies",
    "/media/m_v",
    "/media/100%",
    "/media/媒体库",
    "/media/with space",
    "/media/a.b",
    "/media/tail0",
    "/",
];

fn keys() -> Vec<String> {
    let mut keys: Vec<String> = Vec::new();
    for directory in DIRECTORIES {
        for leaf in [
            "",
            "x.mkv",
            "Sub/y.nfo",
            "%",
            "_",
            "0",
            "zz",
            "100%",
            "媒体.nfo",
            "a.b",
            "tail",
        ] {
            let base = directory.trim_end_matches('/');
            keys.push(if leaf.is_empty() {
                base.to_string()
            } else {
                format!("{base}/{leaf}")
            });
        }
    }
    // 近邻兄弟：与目录只差一个字符，绝不能被算作「在目录下」。
    keys.extend([
        "/media/mXv/x.mkv".into(),
        "/media/1000/x.mkv".into(),
        "/media/moviesX/y.nfo".into(),
        "/media/media/z.mkv".into(),
        "/media/tail1/x.mkv".into(),
        "/other/x.mkv".into(),
    ]);
    keys
}

#[test]
fn range_matches_prefix_semantics_for_every_edge_case_path() {
    let keys = keys();
    for directory in DIRECTORIES {
        let (lower, upper) = directory_range(directory);
        assert!(
            lower < upper,
            "{directory:?}: 区间必须非空（{lower:?} < {upper:?}）"
        );
        let ranged: Vec<&str> = keys
            .iter()
            .filter(|key| in_range(key, directory))
            .map(String::as_str)
            .collect();
        let prefixed: Vec<&str> = keys
            .iter()
            .filter(|key| under_prefix(key, directory))
            .map(String::as_str)
            .collect();
        assert_eq!(
            ranged, prefixed,
            "{directory:?}: 范围谓词与逐字前缀必须是同一批行"
        );
    }
}

#[test]
fn wildcard_characters_in_paths_are_literal() {
    // 含 `_` 的目录不能收下 `/media/mXv/...`（`LIKE '/media/m_v/%'` 会收下）。
    assert!(in_range("/media/m_v/x.mkv", "/media/m_v"));
    assert!(!in_range("/media/mXv/x.mkv", "/media/m_v"));
    // 含 `%` 的目录同理：`LIKE '/media/100%/%'` 会匹配 `/media/1000/...`。
    assert!(in_range("/media/100%/x.mkv", "/media/100%"));
    assert!(!in_range("/media/1000/x.mkv", "/media/100%"));
    // 非 ASCII 目录按字节序在同一段连续区间里。
    assert!(in_range("/media/媒体库/剧集/y.nfo", "/media/媒体库"));
    assert!(!in_range("/media/媒体/y.nfo", "/media/媒体库"));
}

#[test]
fn directory_itself_and_its_children_are_both_covered() {
    for directory in DIRECTORIES {
        // 根目录本身就是 `/`，没有「上一级」可拼兄弟目录。
        assert!(in_range(directory, directory), "{directory:?} 本身要命中");
        let base = directory.trim_end_matches('/');
        assert!(
            in_range(&format!("{base}/child.mkv"), directory),
            "{directory:?} 下的路径要命中"
        );
        if *directory != "/" {
            assert!(
                !in_range(&format!("{base}x/child.mkv"), directory),
                "{directory:?} 的兄弟目录不能命中"
            );
        }
    }
}
