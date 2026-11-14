//! Indexed owner candidates, including first eligible coordinates within a scope.
use super::{scope::Sql, sql::Purpose};

pub(super) struct Candidate {
    pub from: String,
    pub filter: String,
    pub library: String,
    pub media: String,
    pub season: String,
    pub episode: String,
    pub activity: String,
    pub order: String,
}
impl Candidate {
    pub fn fields(&self) -> String {
        format!(
            "{},{},{},{},{}",
            self.library, self.media, self.season, self.episode, self.activity
        )
    }
    pub fn beats(&self, other: &Self, purpose: Purpose) -> String {
        let coordinate = format!("({},{},{})", self.season, self.episode, self.library);
        let other_coordinate = format!("({},{},{})", other.season, other.episode, other.library);
        match purpose {
            Purpose::Next => format!("{coordinate}<{other_coordinate}"),
            Purpose::Resume => format!(
                "({a}>{b} OR ({a}={b} AND (({s},{e})>({os},{oe}) OR (({s},{e})=({os},{oe}) AND {l}<{ol}))))",
                a = self.activity,
                b = other.activity,
                s = self.season,
                e = self.episode,
                os = other.season,
                oe = other.episode,
                l = self.library,
                ol = other.library
            ),
        }
    }
}

pub(super) fn candidates(
    scope: &Sql,
    purpose: Purpose,
    whole: bool,
    prefix: &str,
    probe: bool,
) -> Vec<Candidate> {
    let mut rows = vec![state(scope, purpose, false, prefix, probe)];
    if matches!(purpose, Purpose::Next) {
        rows.push(baseline(scope, false, prefix));
        if whole {
            rows.push(state(scope, purpose, true, &format!("{prefix}w"), probe));
            rows.push(baseline(scope, true, &format!("{prefix}w")));
        }
    }
    rows
}

fn state(scope: &Sql, purpose: Purpose, whole: bool, prefix: &str, probe: bool) -> Candidate {
    let i = format!("{prefix}i");
    let u = format!("{prefix}u");
    let c = format!("{prefix}c");
    let scoped = scope.root.is_some() || scope.season.is_some() || whole;
    let from = state_from(&i, &u, &c, scoped, whole, purpose, probe);
    let mut filter = format!(
        "{u}.user_id=?1 AND {}",
        scope.cards.replace("i.", &format!("{i}."))
    );
    let (season, episode, activity, order) = match purpose {
        Purpose::Next => {
            filter.push_str(&format!(
                " AND {i}.kind{}'tv'",
                if whole { "<>" } else { "=" }
            ));
            if whole {
                filter.push_str(&format!(
                    " AND {c}.season=-1 AND {c}.episode=-1 {}",
                    scope.coordinate_scope(&c)
                ));
            } else {
                filter.push_str(&format!(" AND {u}.next_season IS NOT NULL"));
                if scoped {
                    filter.push_str(&first_unit(scope, &c, &i, Some(&u), purpose));
                }
            }
            let coordinate = if scoped { c.as_str() } else { u.as_str() };
            let head = if scoped { "" } else { "next_" };
            (
                format!("{coordinate}.{head}season"),
                format!("{coordinate}.{head}episode"),
                format!("{u}.activity_at"),
                if whole {
                    String::new()
                } else {
                    format!("{u}.activity_at DESC,{u}.media_id")
                },
            )
        }
        Purpose::Resume if scoped => {
            filter.push_str(&format!(" AND {c}.user_id=?1 AND {c}.position_ms>0"));
            filter.push_str(&first_unit(scope, &c, &i, Some(&u), purpose));
            (
                format!("{c}.season"),
                format!("{c}.episode"),
                format!("{c}.watch_activity_at"),
                format!("{c}.watch_activity_at DESC,{c}.media_id"),
            )
        }
        Purpose::Resume => {
            filter.push_str(&format!(" AND {u}.resume_season IS NOT NULL"));
            (
                format!("{u}.resume_season"),
                format!("{u}.resume_episode"),
                format!("{u}.resume_activity_at"),
                format!("{u}.resume_activity_at DESC,{u}.media_id"),
            )
        }
    };
    Candidate {
        from,
        filter,
        library: format!("{i}.library_id"),
        media: format!("{i}.media_id"),
        season,
        episode,
        activity,
        order,
    }
}

fn baseline(scope: &Sql, whole: bool, prefix: &str) -> Candidate {
    let i = format!("{prefix}b");
    let c = format!("{prefix}l");
    let scoped = scope.root.is_some() || scope.season.is_some() || whole;
    let mut from = format!("library_items {i} INDEXED BY items_media");
    let mut filter = format!(
        "{} AND {i}.kind{}'tv' AND NOT EXISTS(SELECT 1 FROM user_library_items absent WHERE absent.user_id=?1 AND absent.library_id={i}.library_id AND absent.media_id={i}.media_id)",
        scope.cards.replace("i.", &format!("{i}.")),
        if whole { "<>" } else { "=" }
    );
    if scoped {
        from.push_str(&format!(
            " CROSS JOIN library_units {c} ON {c}.library_id={i}.library_id AND {c}.media_id={i}.media_id"
        ));
        if whole {
            filter.push_str(&format!(
                " AND {c}.season=-1 AND {c}.episode=-1 AND {c}.available_file_count>0 {}",
                scope.coordinate_scope(&c)
            ));
        } else {
            filter.push_str(&first_unit(scope, &c, &i, None, Purpose::Next));
        }
    } else {
        filter.push_str(&format!(" AND {i}.first_owned_season IS NOT NULL"));
    }
    let coordinate = if scoped { c.as_str() } else { i.as_str() };
    let head = if scoped { "" } else { "first_owned_" };
    Candidate {
        from,
        filter,
        library: format!("{i}.library_id"),
        media: format!("{i}.media_id"),
        season: format!("{coordinate}.{head}season"),
        episode: format!("{coordinate}.{head}episode"),
        activity: "0".into(),
        order: format!("{i}.media_id"),
    }
}

fn first_unit(scope: &Sql, c: &str, i: &str, user: Option<&str>, purpose: Purpose) -> String {
    let probe = format!("{c}p");
    let (table, owner, eligibility, order) = match user {
        Some(u) if matches!(purpose, Purpose::Resume) => (
            format!(
                "user_library_continue_units {probe} INDEXED BY {}",
                if scope.season.is_some() {
                    "user_continue_owner_season_resume"
                } else {
                    "user_continue_owner_resume"
                }
            ),
            format!("{probe}.user_id={u}.user_id AND "),
            format!("{probe}.position_ms>0"),
            if scope.season.is_some() {
                format!("{probe}.watch_activity_at DESC,{probe}.episode DESC")
            } else {
                format!("{probe}.watch_activity_at DESC,{probe}.season DESC,{probe}.episode DESC")
            },
        ),
        Some(u) => (
            format!("user_library_continue_units {probe}"),
            format!("{probe}.user_id={u}.user_id AND "),
            format!("{probe}.season>=0"),
            format!("{probe}.season,{probe}.episode"),
        ),
        None => (
            format!("library_units {probe}"),
            String::new(),
            format!("{probe}.available_file_count>0 AND {probe}.season>=0"),
            format!("{probe}.season,{probe}.episode"),
        ),
    };
    format!(
        " AND ({c}.season,{c}.episode)=(SELECT {probe}.season,{probe}.episode FROM {table} WHERE {owner}{probe}.library_id={i}.library_id AND {probe}.media_id={i}.media_id AND {eligibility} {} ORDER BY {order} LIMIT 1)",
        scope.coordinate_scope(&probe)
    )
}

// Better-owner probes start at the candidate's media, never at user activity.
fn owner_from(i: &str, u: &str, c: &str, scoped: bool, purpose: Purpose) -> String {
    let mut from = format!(
        "library_items {i} INDEXED BY items_media CROSS JOIN user_library_items {u} ON {u}.user_id=?1 AND {u}.library_id={i}.library_id AND {u}.media_id={i}.media_id"
    );
    if scoped {
        let index = if matches!(purpose, Purpose::Resume) {
            " INDEXED BY user_continue_owner_resume"
        } else {
            ""
        };
        from.push_str(&format!(" CROSS JOIN user_library_continue_units {c}{index} ON {c}.user_id=?1 AND {c}.library_id={i}.library_id AND {c}.media_id={i}.media_id"));
    }
    from
}

fn state_from(
    i: &str,
    u: &str,
    c: &str,
    scoped: bool,
    whole: bool,
    purpose: Purpose,
    probe: bool,
) -> String {
    let index = match purpose {
        Purpose::Next if whole => "user_continue_scope",
        Purpose::Next => "user_items_next",
        Purpose::Resume if scoped => "user_continue_resume",
        Purpose::Resume => "user_items_resume",
    };
    let unit_driven = whole || (scoped && matches!(purpose, Purpose::Resume));
    if probe {
        owner_from(i, u, c, scoped, purpose)
    } else if unit_driven {
        format!(
            "user_library_continue_units {c} INDEXED BY {index} JOIN user_library_items {u} ON {u}.user_id={c}.user_id AND {u}.library_id={c}.library_id AND {u}.media_id={c}.media_id JOIN library_items {i} ON {i}.library_id={c}.library_id AND {i}.media_id={c}.media_id"
        )
    } else {
        let unit = if scoped {
            format!(
                " JOIN user_library_continue_units {c} ON {c}.user_id={u}.user_id AND {c}.library_id={u}.library_id AND {c}.media_id={u}.media_id"
            )
        } else {
            String::new()
        };
        format!(
            "user_library_items {u} INDEXED BY {index} JOIN library_items {i} ON {i}.library_id={u}.library_id AND {i}.media_id={u}.media_id{unit}"
        )
    }
}
