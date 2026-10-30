//! SQL on one explicitly borrowed database connection.
use super::*;

impl crate::LibraryConnection<'_> {
    pub fn playback_watch_report(
        &self,
        user: Option<UserId>,
        visible: Option<&[MediaId]>,
        now: i64,
        days: u16,
        tz_minutes: i16,
    ) -> Result<WatchReport, StoreError> {
        if days == 0 || days > MAX_DAYS {
            return Err(StoreError::Invalid(format!(
                "watch window days must be 1..={MAX_DAYS}, got {days}"
            )));
        }
        if tz_minutes.abs() > MAX_TZ_MINUTES {
            return Err(StoreError::Invalid(format!(
                "tz offset minutes must be within ±{MAX_TZ_MINUTES}, got {tz_minutes}"
            )));
        }
        let window = i64::from(days) * 86_400;
        let tz_secs = i64::from(tz_minutes) * 60;
        let today = (now + tz_secs).div_euclid(86_400);
        let user_text = user.map(|id| id.to_string());

        self.watch_report_inner(&user_text, visible, now, days, tz_secs, today, window)
    }

    pub(super) fn watch_report_inner(
        &self,
        user: &Option<String>,
        visible: Option<&[MediaId]>,
        now: i64,
        days: u16,
        tz_secs: i64,
        today: i64,
        window: i64,
    ) -> Result<WatchReport, StoreError> {
        let visible_json = serde_json::to_string(&visible.unwrap_or_default())?;
        let visible_json = visible_json.as_str();
        let visibility = if visible.is_some() {
            VISIBLE_CLAUSE
        } else {
            ""
        };
        let current = self.window_report(
            user,
            now - window,
            i64::MAX,
            days,
            today - i64::from(days) + 1,
            tz_secs,
            visibility,
            visible_json,
        )?;
        let previous = self.window_report(
            user,
            now - 2 * window,
            now - window,
            days,
            today - 2 * i64::from(days) + 1,
            tz_secs,
            visibility,
            visible_json,
        )?;
        let hidden_titles = if visible.is_some() {
            let sql = format!(
                "{} SELECT COUNT(DISTINCT media_id) FROM scoped",
                scoped_cte(HIDDEN_CLAUSE)
            );
            let mut stmt = self.library.prepare(&sql)?;
            let count: i64 = stmt.query_row(
                named_params! { ":tz": tz_secs, ":start": now - window, ":end": i64::MAX, ":user": user, ":visible": visible_json },
                |row| row.get(0),
            )?;
            count.max(0) as usize
        } else {
            0
        };
        Ok(WatchReport {
            current,
            previous,
            hidden_titles,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn window_report(
        &self,
        user: &Option<String>,
        start: i64,
        end: i64,
        days: u16,
        first_day: i64,
        tz_secs: i64,
        visibility: &str,
        visible_json: &str,
    ) -> Result<WatchWindow, StoreError> {
        let cte = scoped_cte(visibility);
        Ok(WatchWindow {
            totals: self.window_totals(&cte, user, start, end, tz_secs, visible_json)?,
            days: self.window_days(
                &cte,
                user,
                start,
                end,
                tz_secs,
                days,
                first_day,
                visible_json,
            )?,
            hours: self.window_hours(&cte, user, start, end, tz_secs, visible_json)?,
            users: self.window_users(&cte, user, start, end, tz_secs, visible_json)?,
            clients: self.window_clients(&cte, user, start, end, tz_secs, visible_json)?,
            media: self.window_media(&cte, user, start, end, tz_secs, visible_json)?,
        })
    }

    pub(super) fn window_totals(
        &self,
        cte: &str,
        user: &Option<String>,
        start: i64,
        end: i64,
        tz_secs: i64,
        visible_json: &str,
    ) -> Result<WatchTotals, StoreError> {
        let sql = format!(
            "{cte} SELECT COUNT(*), COALESCE(SUM(watched_ms), 0),
                    COALESCE(SUM(completed), 0), COUNT(DISTINCT user_id) FROM scoped"
        );
        let mut stmt = self.library.prepare(&sql)?;
        Ok(stmt.query_row(
            named_params! { ":tz": tz_secs, ":start": start, ":end": end, ":user": user, ":visible": visible_json },
            |row| {
                Ok(WatchTotals {
                    plays: row.get(0)?,
                    watched_ms: row.get(1)?,
                    completed: row.get(2)?,
                    members: row.get(3)?,
                })
            },
        )?)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn window_days(
        &self,
        cte: &str,
        user: &Option<String>,
        start: i64,
        end: i64,
        tz_secs: i64,
        days: u16,
        first_day: i64,
        visible_json: &str,
    ) -> Result<Vec<WatchDay>, StoreError> {
        let sql = format!(
            "{cte} SELECT day, COUNT(*), COALESCE(SUM(watched_ms), 0),
                    COALESCE(SUM(completed), 0), COUNT(DISTINCT user_id)
             FROM scoped GROUP BY day"
        );
        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map(
            named_params! { ":tz": tz_secs, ":start": start, ":end": end, ":user": user, ":visible": visible_json },
            |row| {
                Ok(WatchDay {
                    day: row.get(0)?,
                    totals: WatchTotals {
                        plays: row.get(1)?,
                        watched_ms: row.get(2)?,
                        completed: row.get(3)?,
                        members: row.get(4)?,
                    },
                })
            },
        )?;
        let mut found: HashMap<i64, WatchTotals> = HashMap::new();
        for row in rows {
            let entry = row?;
            found.insert(entry.day, entry.totals);
        }
        Ok((0..i64::from(days))
            .map(|offset| {
                let day = first_day + offset;
                WatchDay {
                    day,
                    totals: found.get(&day).copied().unwrap_or_default(),
                }
            })
            .collect())
    }

    pub(super) fn window_hours(
        &self,
        cte: &str,
        user: &Option<String>,
        start: i64,
        end: i64,
        tz_secs: i64,
        visible_json: &str,
    ) -> Result<Vec<Vec<i64>>, StoreError> {
        let sql = format!(
            "{cte} SELECT ((day + 3) % 7 + 7) % 7 AS weekday, hour,
                    COALESCE(SUM(watched_ms), 0)
             FROM scoped GROUP BY weekday, hour"
        );
        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map(
            named_params! { ":tz": tz_secs, ":start": start, ":end": end, ":user": user, ":visible": visible_json },
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )?;
        let mut hours = vec![vec![0i64; 24]; 7];
        for row in rows {
            let (weekday, hour, watched) = row?;
            if (0..7).contains(&weekday) && (0..24).contains(&hour) {
                hours[weekday as usize][hour as usize] = watched;
            }
        }
        Ok(hours)
    }

    pub(super) fn window_users(
        &self,
        cte: &str,
        user: &Option<String>,
        start: i64,
        end: i64,
        tz_secs: i64,
        visible_json: &str,
    ) -> Result<Vec<WatchMember>, StoreError> {
        let sql = format!(
            "{cte} SELECT user_id, COUNT(*), COALESCE(SUM(watched_ms), 0),
                    COALESCE(SUM(completed), 0)
             FROM scoped GROUP BY user_id ORDER BY user_id"
        );
        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map(
            named_params! { ":tz": tz_secs, ":start": start, ":end": end, ":user": user, ":visible": visible_json },
            |row| {
                Ok(WatchMember {
                    user_id: user_id_from(row, 0)?,
                    totals: WatchTotals {
                        plays: row.get(1)?,
                        watched_ms: row.get(2)?,
                        completed: row.get(3)?,
                        members: 1,
                    },
                })
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub(super) fn window_clients(
        &self,
        cte: &str,
        user: &Option<String>,
        start: i64,
        end: i64,
        tz_secs: i64,
        visible_json: &str,
    ) -> Result<Vec<WatchClient>, StoreError> {
        let sql = format!(
            "{cte} SELECT COALESCE(client, '直连'), COUNT(*), COALESCE(SUM(watched_ms), 0)
             FROM scoped GROUP BY 1 ORDER BY 1"
        );
        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map(
            named_params! { ":tz": tz_secs, ":start": start, ":end": end, ":user": user, ":visible": visible_json },
            |row| {
                Ok(WatchClient {
                    client: row.get(0)?,
                    plays: row.get(1)?,
                    watched_ms: row.get(2)?,
                })
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub(super) fn window_media(
        &self,
        cte: &str,
        user: &Option<String>,
        start: i64,
        end: i64,
        tz_secs: i64,
        visible_json: &str,
    ) -> Result<Vec<WatchMedia>, StoreError> {
        let sql = format!(
            "{cte} SELECT media_id, COUNT(*), COALESCE(SUM(watched_ms), 0),
                    COUNT(DISTINCT user_id)
             FROM scoped GROUP BY media_id ORDER BY media_id"
        );
        let mut stmt = self.library.prepare(&sql)?;
        let rows = stmt.query_map(
            named_params! { ":tz": tz_secs, ":start": start, ":end": end, ":user": user, ":visible": visible_json },
            |row| {
                Ok(WatchMedia {
                    media_id: media_id_from(row, 0)?,
                    plays: row.get(1)?,
                    watched_ms: row.get(2)?,
                    members: row.get(3)?,
                })
            },
        )?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}
