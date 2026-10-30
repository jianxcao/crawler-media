use std::sync::OnceLock;

use parking_lot::{Condvar, Mutex};
use serde_json::Value;

use super::Douban;
use crate::{CatalogGet, TmdbError};

#[derive(Debug)]
pub struct DoubanSubject {
    pub subject: Value,
    pub celebrities: Value,
}

#[derive(Default)]
struct Helpers {
    active: Mutex<usize>,
    ready: Condvar,
}
fn helpers() -> &'static Helpers {
    static HELPERS: OnceLock<Helpers> = OnceLock::new();
    HELPERS.get_or_init(Helpers::default)
}
struct HelperPermit;
impl HelperPermit {
    fn acquire() -> Self {
        let mut active = helpers().active.lock();
        while *active >= 4 {
            helpers().ready.wait(&mut active);
        }
        *active += 1;
        Self
    }
}
impl Drop for HelperPermit {
    fn drop(&mut self) {
        *helpers().active.lock() -= 1;
        helpers().ready.notify_one();
    }
}

impl<H: CatalogGet + Sync> Douban<H> {
    pub fn subject_with_credits(&self, id: &str) -> Result<DoubanSubject, TmdbError> {
        let subject_path = format!("/rexxar/api/v2/movie/{id}?for_mobile=1");
        let credits_path = format!("/rexxar/api/v2/movie/{id}/celebrities?for_mobile=1");
        if let (Some(subject), Some(celebrities)) = (
            self.cache.get_fresh(&subject_path)?,
            self.cache.get_fresh(&credits_path)?,
        ) {
            match (decode_subject(&subject, id), decode_credits(&celebrities)) {
                (Ok(subject), Ok(celebrities)) => {
                    return Ok(DoubanSubject {
                        subject,
                        celebrities,
                    });
                }
                (Err(error @ TmdbError::NotFound(_)), _)
                | (_, Err(error @ TmdbError::NotFound(_))) => return Err(error),
                _ => {}
            }
        }
        let permit = HelperPermit::acquire();
        std::thread::scope(|scope| {
            let credits = std::thread::Builder::new()
                .name("douban-credits".into())
                .spawn_scoped(scope, move || {
                    let _permit = permit;
                    self.cache
                        .get_or_fetch(&self.http, &credits_path, decode_credits)
                })?;
            let subject = self
                .cache
                .get_or_fetch(&self.http, &subject_path, |body| decode_subject(body, id));
            // Join before propagating either component's failure.
            let celebrities = credits
                .join()
                .map_err(|_| TmdbError::Http("Douban credits worker interrupted".into()))?;
            Ok(DoubanSubject {
                subject: subject?,
                celebrities: celebrities?,
            })
        })
    }
}

fn decode_subject(body: &str, expected_id: &str) -> Result<Value, TmdbError> {
    let value = crate::contracts::json(body)?;
    let matches_id = value.get("id").is_some_and(|id| {
        id.as_str().map_or_else(
            || {
                id.as_u64().is_some_and(|actual| {
                    expected_id.bytes().all(|byte| byte.is_ascii_digit())
                        && (expected_id == "0" || !expected_id.starts_with('0'))
                        && expected_id.parse::<u64>().ok() == Some(actual)
                })
            },
            |actual| actual == expected_id,
        )
    });
    if !matches_id
        || !value
            .get("title")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    {
        return Err(TmdbError::Parse(
            "missing or mismatched Douban subject identity/title".into(),
        ));
    }
    Ok(value)
}

fn decode_credits(body: &str) -> Result<Value, TmdbError> {
    let value = crate::contracts::json(body)?;
    if !["actors", "directors"]
        .iter()
        .all(|field| value.get(*field).is_some_and(Value::is_array))
    {
        return Err(TmdbError::Parse(
            "missing Douban actors/directors arrays".into(),
        ));
    }
    Ok(value)
}
