use api::probe_manager::season_schedule::{SeasonAnalysisSchedule, SeasonKey};

fn key(season: u32) -> SeasonKey {
    SeasonKey {
        media_id: domain::MediaId::new(),
        season,
    }
}

#[test]
fn arrivals_during_analysis_leave_one_followup_generation() {
    let key = key(1);
    let mut schedule = SeasonAnalysisSchedule::new(2_000, 10_000);
    schedule.mark_dirty(key, "a".into(), 0);
    schedule.mark_dirty(key, "b".into(), 500);
    assert!(schedule.take_due(2_499).is_none());
    let first = schedule.take_due(2_500).unwrap();
    assert_eq!(first.membership_digest, "b");
    schedule.mark_dirty(key, "c".into(), 3_000);
    assert!(schedule.take_due(5_000).is_none());
    schedule.finish(&first, 5_000);
    let second = schedule.take_due(5_000).unwrap();
    assert_eq!(second.membership_digest, "c");
    assert!(second.generation > first.generation);
    schedule.finish(&first, 6_000);
    schedule.mark_dirty(key, "d".into(), 6_000);
    assert!(schedule.take_due(8_000).is_none());
    schedule.finish(&second, 8_000);
    assert_eq!(schedule.take_due(8_000).unwrap().membership_digest, "d");
}

#[test]
fn identical_notifications_do_not_delay_or_repeat_work() {
    let key = key(1);
    let mut schedule = SeasonAnalysisSchedule::new(2_000, 10_000);
    schedule.mark_dirty(key, "a".into(), 0);
    schedule.mark_dirty(key, "a".into(), 1_999);
    let ticket = schedule.take_due(2_000).unwrap();
    schedule.mark_dirty(key, "a".into(), 2_500);
    schedule.finish(&ticket, 3_000);
    assert!(schedule.take_due(20_000).is_none());
    schedule.mark_dirty(key, "a".into(), 21_000);
    let fresh = schedule.take_due(23_000).unwrap();
    assert!(fresh.generation > ticket.generation);
    schedule.finish(&ticket, 24_000);
    assert!(schedule.take_due(24_000).is_none());
}

#[test]
fn continuous_arrivals_become_due_at_max_wait() {
    let key = key(1);
    let mut schedule = SeasonAnalysisSchedule::new(2_000, 10_000);
    for i in 0..10 {
        schedule.mark_dirty(key, i.to_string(), i * 1_000);
        assert!(schedule.take_due(i * 1_000).is_none());
    }
    assert_eq!(schedule.take_due(10_000).unwrap().membership_digest, "9");
}

#[test]
fn due_work_is_ordered_by_deadline_then_media_and_season() {
    let mut a = key(1);
    let mut b = key(1);
    if a.media_id.to_string() > b.media_id.to_string() {
        std::mem::swap(&mut a, &mut b);
    }
    let c = SeasonKey { season: 2, ..a };
    let early = key(9);
    let mut schedule = SeasonAnalysisSchedule::new(2_000, 10_000);
    for key in [b, c, a] {
        schedule.mark_dirty(key, "same".into(), 100);
    }
    schedule.mark_dirty(early, "early".into(), 0);
    for expected in [early, a, c, b] {
        let ticket = schedule.take_due(2_100).unwrap();
        assert_eq!(ticket.key, expected);
        schedule.finish(&ticket, 2_100);
    }
    assert!(schedule.take_due(i64::MAX).is_none());
}

#[test]
fn deadline_addition_saturates() {
    let mut schedule = SeasonAnalysisSchedule::new(2_000, 10_000);
    schedule.mark_dirty(key(1), "late".into(), i64::MAX - 1);
    assert!(schedule.take_due(i64::MAX - 1).is_none());
    assert!(schedule.take_due(i64::MAX).is_some());
}
