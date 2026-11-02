//! Safe video file selection.
/// Review H-2: selection must never deselect every video of a task. Returns
/// true when `names` has video files but `keep` would keep none of them.
pub(crate) fn selection_drops_all_videos<'a>(
    mut names: impl Iterator<Item = &'a str> + Clone,
    keep: &dyn Fn(&str) -> bool,
) -> bool {
    const VIDEO: [&str; 10] = [
        "mkv", "mp4", "avi", "ts", "m2ts", "wmv", "mov", "rmvb", "flv", "webm",
    ];
    let is_video = |name: &str| {
        name.rsplit_once('.')
            .is_some_and(|(_, ext)| VIDEO.contains(&ext.to_ascii_lowercase().as_str()))
    };
    names.clone().any(is_video) && !names.any(|name| is_video(name) && keep(name))
}
