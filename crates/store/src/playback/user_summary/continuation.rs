//! Continuation pointers use concrete available ownership and global user facts.
use super::*;

pub(super) fn apply(
    summary: &mut Summary,
    is_tv: bool,
    available: &[(i32, i32)],
    facts: &HashMap<(i32, i32), PlaybackFact>,
) {
    let qualifying =
        |fact: &&PlaybackFact| fact.position_ms > 0 || fact.played || fact.play_count > 0;
    summary.activity_present = facts.values().any(|fact| qualifying(&fact));
    summary.activity_at = facts
        .values()
        .filter(qualifying)
        .map(|fact| fact.watch_activity_at)
        .max()
        .unwrap_or(0);
    summary.all_watch_at = facts
        .values()
        .map(|fact| fact.watch_activity_at)
        .max()
        .unwrap_or(0);
    let anchor = facts
        .values()
        .filter(|fact| fact.season >= 0 && fact.episode >= 0)
        .max_by_key(|fact| (fact.watch_activity_at, fact.season, fact.episode))
        .map(|fact| (fact.season, fact.episode));
    summary.activity_season = anchor.map(|(season, _)| season);
    summary.activity_episode = anchor.map(|(_, episode)| episode);
    let unplayed = |&&(season, episode): &&(i32, i32)| {
        season >= 0 && episode >= 0 && !effective_played(is_tv, facts, season, episode)
    };
    let next = is_tv
        .then(|| available.iter().filter(unplayed).min().copied())
        .flatten();
    summary.next_season = next.map(|(season, _)| season);
    summary.next_episode = next.map(|(_, episode)| episode);
    let activity_next = is_tv
        .then(|| {
            available
                .iter()
                .filter(unplayed)
                .filter(|&&unit| unit >= anchor.unwrap_or((0, 0)))
                .min()
                .copied()
        })
        .flatten();
    summary.activity_next_season = activity_next.map(|(season, _)| season);
    summary.activity_next_episode = activity_next.map(|(_, episode)| episode);
    let owned: HashSet<_> = available.iter().copied().collect();
    let resume = facts
        .values()
        .filter(|fact| owned.contains(&(fact.season, fact.episode)) && fact.position_ms > 0)
        .filter(|fact| !effective_played(is_tv, facts, fact.season, fact.episode))
        .max_by_key(|fact| (fact.watch_activity_at, fact.season, fact.episode));
    summary.resume_season = resume.map(|fact| fact.season);
    summary.resume_episode = resume.map(|fact| fact.episode);
    summary.resume_activity_at = resume.map(|fact| fact.watch_activity_at).unwrap_or(0);
}
