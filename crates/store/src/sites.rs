use domain::{Filter, FilterId, Site, SiteId};
use rusqlite::{OptionalExtension, params};

use super::StoreError;
use super::maps::{SITE_COLUMNS, map_site, parse_atoms, serialize_atoms};

/// Raw `filters` columns; atom decoding happens outside the row closure so a
/// stale shape can surface as `StoreError::Invalid` instead of a SQL error.
type RawFilterRow = (String, String, String, i64);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialEpoch(String);

impl CredentialEpoch {
    pub fn storage_key(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteObservation {
    site: Site,
    revision: i64,
    epoch: CredentialEpoch,
}
impl SiteObservation {
    pub fn site(&self) -> &Site {
        &self.site
    }
    pub fn revision(&self) -> i64 {
        self.revision
    }
    pub fn credential_epoch(&self) -> &CredentialEpoch {
        &self.epoch
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManualCredentialWrite {
    Unchanged,
    Replace,
}

fn raw_filter_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawFilterRow> {
    Ok((
        row.get::<_, String>(0)?,
        row.get::<_, String>(1)?,
        row.get::<_, String>(2)?,
        row.get::<_, i64>(3).unwrap_or(0),
    ))
}

fn build_filter(
    (raw_id, name, atoms_json, keep_old_versions): RawFilterRow,
) -> Result<Filter, StoreError> {
    Ok(Filter {
        id: super::maps::parse_id(raw_id, 0)?,
        name,
        atoms: parse_atoms(&atoms_json)?,
        keep_old_versions: keep_old_versions != 0,
    })
}

impl crate::writers::App<'_> {
    pub fn insert_site(&self, site: &Site) -> Result<(), StoreError> {
        if site.timezone.trim().is_empty() {
            return Err(StoreError::Invalid(
                "site timezone must not be empty".into(),
            ));
        }
        self.app.execute(
            "INSERT INTO sites (
                id, name, url, profile_id, cookie, api_key, rss_url, proxy,
                rate_limit_per_minute, cdp_url, downloader_id, enabled, maintenance_via,
                browser_search, timezone, revision, credential_epoch
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, 1, ?16)",
            params![
                site.id.to_string(),
                site.name,
                site.url,
                site.profile_id,
                site.cookie,
                site.api_key,
                site.rss_url,
                site.proxy,
                site.rate_limit_per_minute,
                site.cdp_url,
                site.downloader_id.map(|id| id.to_string()),
                site.enabled as i64,
                site.maintenance_via.as_str(),
                site.browser_search as i64,
                site.timezone,
                uuid::Uuid::new_v4().to_string(),
            ],
        )?;
        Ok(())
    }

    pub fn get_site(&self, id: SiteId) -> Result<Option<Site>, StoreError> {
        self.app
            .query_row(
                &format!("SELECT {SITE_COLUMNS} FROM sites WHERE id = ?1"),
                params![id.to_string()],
                map_site,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn observe_site(&self, id: SiteId) -> Result<Option<SiteObservation>, StoreError> {
        self.app
            .query_row(
                &format!(
                    "SELECT {SITE_COLUMNS}, revision, credential_epoch FROM sites WHERE id = ?1"
                ),
                params![id.to_string()],
                |row| {
                    Ok(SiteObservation {
                        site: map_site(row)?,
                        revision: row.get(15)?,
                        epoch: CredentialEpoch(row.get(16)?),
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn list_enabled_sites(&self) -> Result<Vec<Site>, StoreError> {
        let mut stmt = self.app.prepare(&format!(
            "SELECT {SITE_COLUMNS} FROM sites WHERE enabled = 1"
        ))?;
        let rows = stmt.query_map([], map_site)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn list_sites(&self) -> Result<Vec<Site>, StoreError> {
        let mut stmt = self
            .app
            .prepare(&format!("SELECT {SITE_COLUMNS} FROM sites"))?;
        let rows = stmt.query_map([], map_site)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub fn set_site_enabled(&self, id: SiteId, enabled: bool) -> Result<bool, StoreError> {
        let n = self.app.execute(
            "UPDATE sites SET enabled = ?2, revision = revision + 1 WHERE id = ?1 AND revision < 9223372036854775807",
            params![id.to_string(), enabled as i64],
        )?;
        Ok(n > 0)
    }

    pub fn delete_site(&self, id: SiteId) -> Result<bool, StoreError> {
        let tx = crate::write_tx(self.app)?;
        let n = tx.execute("DELETE FROM sites WHERE id = ?1", params![id.to_string()])?;
        if n > 0 {
            crate::plugins::purge_retired_site_scopes(&tx, id, None)?;
        }
        tx.commit()?;
        Ok(n > 0)
    }

    /// Publish a cookie only while the captured revision and epoch are current.
    pub fn save_site_if_current(
        &self,
        observed: &SiteObservation,
        cookie: Option<&str>,
    ) -> Result<bool, StoreError> {
        let n = self.app.execute(
            "UPDATE sites SET cookie = ?4, revision = revision + 1 WHERE id = ?1 AND revision = ?2 AND credential_epoch = ?3 AND revision < 9223372036854775807",
            params![observed.site.id.to_string(), observed.revision, observed.epoch.0, cookie],
        )?;
        Ok(n > 0)
    }

    pub fn save_manual_site_if_current(
        &self,
        site: &Site,
        expected: &SiteObservation,
        intent: ManualCredentialWrite,
    ) -> Result<bool, StoreError> {
        if site.id != expected.site.id {
            return Ok(false);
        }
        self.execute_site_update(site, Some(expected), intent)
    }

    pub fn save_site(
        &self,
        site: &Site,
        intent: ManualCredentialWrite,
    ) -> Result<bool, StoreError> {
        self.execute_site_update(site, None, intent)
    }

    fn execute_site_update(
        &self,
        site: &Site,
        expected: Option<&SiteObservation>,
        intent: ManualCredentialWrite,
    ) -> Result<bool, StoreError> {
        if site.timezone.trim().is_empty() {
            return Err(StoreError::Invalid(
                "site timezone must not be empty".into(),
            ));
        }
        let tx = crate::write_tx(self.app)?;
        let prior_epoch: Option<String> = tx
            .query_row(
                "SELECT credential_epoch FROM sites WHERE id = ?1",
                params![site.id.to_string()],
                |r| r.get(0),
            )
            .optional()?;
        let next_epoch = uuid::Uuid::new_v4().to_string();
        let n = tx.execute(
            "UPDATE sites SET name = ?2, url = ?3, profile_id = ?4, cookie = ?5, api_key = ?6,
                    rss_url = ?7, proxy = ?8, rate_limit_per_minute = ?9, cdp_url = ?10,
                    downloader_id = ?11, enabled = ?12, maintenance_via = ?13, browser_search = ?14,
                    timezone = ?15, revision = revision + 1,
                    credential_epoch = CASE WHEN ?16 OR url != ?3 THEN ?17 ELSE credential_epoch END
             WHERE id = ?1 AND revision < 9223372036854775807
                   AND (?18 IS NULL OR (revision = ?18 AND credential_epoch = ?19))
                   AND (?16 OR (cookie IS ?5 AND api_key IS ?6 AND rss_url IS ?7))",
            params![
                site.id.to_string(),
                site.name,
                site.url,
                site.profile_id,
                site.cookie,
                site.api_key,
                site.rss_url,
                site.proxy,
                site.rate_limit_per_minute,
                site.cdp_url,
                site.downloader_id.map(|id| id.to_string()),
                site.enabled as i64,
                site.maintenance_via.as_str(),
                site.browser_search as i64,
                site.timezone,
                intent == ManualCredentialWrite::Replace,
                next_epoch,
                expected.map(|obs| obs.revision),
                expected.map(|obs| obs.epoch.0.as_str()),
            ],
        )?;
        if n == 0 {
            if let Some(obs) = expected {
                if let Some(current) = self.observe_site(site.id)?
                    && current.revision == obs.revision
                    && current.epoch == obs.epoch
                {
                    return Err(StoreError::CredentialConflict(
                        "credential change requires replacement intent or site revision exhausted"
                            .into(),
                    ));
                }
                return Ok(false);
            }
            if self.get_site(site.id)?.is_some() {
                return Err(StoreError::CredentialConflict(
                    "credential change requires replacement intent or site revision exhausted"
                        .into(),
                ));
            }
            return Ok(false);
        }
        let current_epoch: String = tx.query_row(
            "SELECT credential_epoch FROM sites WHERE id = ?1",
            params![site.id.to_string()],
            |r| r.get(0),
        )?;
        if let Some(old_epoch) = prior_epoch.as_deref()
            && old_epoch != current_epoch
        {
            crate::plugins::purge_retired_site_scopes(&tx, site.id, Some(old_epoch))?;
        }
        tx.commit()?;
        Ok(true)
    }

    pub fn insert_filter(&self, filter: &Filter) -> Result<(), StoreError> {
        let atoms = serialize_atoms(&filter.atoms);
        self.app.execute(
            "INSERT INTO filters (id, name, atoms_json, keep_old_versions) VALUES (?1, ?2, ?3, ?4)",
            params![
                filter.id.to_string(),
                filter.name,
                atoms,
                filter.keep_old_versions as i64
            ],
        )?;
        Ok(())
    }

    pub fn save_filter(&self, filter: &Filter) -> Result<(), StoreError> {
        let atoms = serialize_atoms(&filter.atoms);
        self.app.execute(
            "UPDATE filters SET name = ?1, atoms_json = ?2, keep_old_versions = ?3 WHERE id = ?4",
            params![
                filter.name,
                atoms,
                filter.keep_old_versions as i64,
                filter.id.to_string()
            ],
        )?;
        Ok(())
    }

    pub fn delete_filter(&self, id: FilterId) -> Result<bool, StoreError> {
        let n = self
            .app
            .execute("DELETE FROM filters WHERE id = ?1", params![id.to_string()])?;
        Ok(n > 0)
    }

    pub fn get_filter(&self, id: FilterId) -> Result<Option<Filter>, StoreError> {
        let row = self
            .app
            .query_row(
                "SELECT id, name, atoms_json, keep_old_versions FROM filters WHERE id = ?1",
                params![id.to_string()],
                raw_filter_row,
            )
            .optional()?;
        row.map(build_filter).transpose()
    }

    pub fn list_filters(&self) -> Result<Vec<Filter>, StoreError> {
        let mut stmt = self
            .app
            .prepare("SELECT id, name, atoms_json, keep_old_versions FROM filters")?;
        let rows = stmt.query_map([], raw_filter_row)?;
        let raw = rows.collect::<Result<Vec<_>, _>>()?;
        raw.into_iter().map(build_filter).collect()
    }
}

mod app_access;
