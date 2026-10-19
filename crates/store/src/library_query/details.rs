use domain::{LedgerId, LibraryId, MediaId, MetadataFields, UnitKey};
use rusqlite::params;

use super::types::{BrowsePage, LibraryItemDetail, LibraryUnit, UnitPage};
use super::super::{Store, StoreError};

impl Store {
    pub fn library_unit_page(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        season: Option<u32>,
        page: &BrowsePage,
    ) -> Result<UnitPage, StoreError> {
        let lib_id_str = library_id.to_string();
        let media_id_str = media_id.to_string();

        let (total_sql, rows_sql, params_vec): (String, String, Vec<rusqlite::types::Value>) = match season {
            Some(s) => (
                "SELECT COUNT(*) FROM library_units WHERE library_id = ?1 AND media_id = ?2 AND season = ?3"
                    .into(),
                "SELECT season, episode, file_count, available_file_count, preferred_file_id
                 FROM library_units
                 WHERE library_id = ?1 AND media_id = ?2 AND season = ?3
                 ORDER BY season ASC, episode ASC
                 LIMIT ?4 OFFSET ?5"
                    .into(),
                vec![
                    lib_id_str.clone().into(),
                    media_id_str.clone().into(),
                    (s as i64).into(),
                ],
            ),
            None => (
                "SELECT COUNT(*) FROM library_units WHERE library_id = ?1 AND media_id = ?2".into(),
                "SELECT season, episode, file_count, available_file_count, preferred_file_id
                 FROM library_units
                 WHERE library_id = ?1 AND media_id = ?2
                 ORDER BY season ASC, episode ASC
                 LIMIT ?3 OFFSET ?4"
                    .into(),
                vec![lib_id_str.clone().into(), media_id_str.clone().into()],
            ),
        };

        let total: i64 = self.library.query_row(
            &total_sql,
            rusqlite::params_from_iter(params_vec.iter().cloned()),
            |r| r.get(0),
        )?;

        let (offset, limit) = match page {
            BrowsePage::Offset { start, limit } => (*start, *limit),
            BrowsePage::Cursor { .. } => (0, 50),
        };

        let mut query_params = params_vec;
        query_params.push((limit as i64).into());
        query_params.push((offset as i64).into());

        let mut stmt = self.library.prepare(&rows_sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(query_params), |r| {
            let s: i32 = r.get(0)?;
            let ep: i32 = r.get(1)?;
            let fc: i64 = r.get(2)?;
            let afc: i64 = r.get(3)?;
            let pref_str: Option<String> = r.get(4)?;
            let pref_id = pref_str.and_then(|id| id.parse::<LedgerId>().ok());

            Ok(LibraryUnit {
                library_id,
                media_id,
                key: UnitKey::new(s, ep),
                file_count: fc as usize,
                available_file_count: afc as usize,
                preferred_file_id: pref_id,
            })
        })?;

        let units = rows.collect::<Result<Vec<_>, _>>()?;

        Ok(UnitPage {
            units,
            total: total as u64,
            next_cursor: None,
        })
    }

    pub fn library_item_detail(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        locale: &str,
    ) -> Result<Option<LibraryItemDetail>, StoreError> {
        let summary_opt = self.get_library_item(library_id, media_id)?;
        let Some(summary) = summary_opt else {
            return Ok(None);
        };

        let item = super::types::LibraryItem {
            library_id,
            media_id,
            kind: summary.kind,
            title: summary.title.clone(),
            title_sort: summary.title.clone(),
            year: None,
            file_count: summary.file_count,
            available_file_count: summary.available_file_count,
            missing_file_count: summary.missing_file_count,
            unknown_file_count: summary.unknown_file_count,
            distinct_episode_count: summary.distinct_episode_count,
            distinct_season_count: summary.distinct_season_count,
            size_bytes: summary.size_bytes,
            preferred_file_id: summary.preferred_file_id,
            has_still: false,
            rating: None,
            studio: None,
            studio_logo: None,
        };

        // Fetch effective metadata or default
        let metadata = match self.get_effective_metadata(media_id, locale)? {
            Some(eff) => {
                let mut f = MetadataFields::default();
                f.title = domain::FieldValue::Value(eff.title);
                if let Some(ot) = eff.original_title {
                    f.original_title = domain::FieldValue::Value(ot);
                }
                if let Some(ov) = eff.overview {
                    f.overview = domain::FieldValue::Value(ov);
                }
                if let Some(y) = eff.year {
                    f.year = domain::FieldValue::Value(y);
                }
                if let Some(r) = eff.rating {
                    f.rating = domain::FieldValue::Value(r);
                }
                if let Some(rt) = eff.runtime_minutes {
                    f.runtime_minutes = domain::FieldValue::Value(rt);
                }
                if let Some(l) = eff.language {
                    f.language = domain::FieldValue::Value(l);
                }
                f
            }
            None => MetadataFields::default(),
        };

        // First page of files (limit 50)
        let file_page = self.library_file_page(library_id, media_id, None, 50)?;

        Ok(Some(LibraryItemDetail {
            item,
            metadata,
            files: file_page.files,
            next_file_cursor: file_page.next_cursor,
        }))
    }
}
