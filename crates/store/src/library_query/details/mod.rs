use std::collections::BTreeMap;
use domain::{LedgerId, LibraryId, MediaId, MetadataFields, UnitKey};
use rusqlite::{params, OptionalExtension};
use crate::{Store, StoreError};
use crate::library_query::items::{ITEM_COLUMNS, item_from_row};
use crate::library_query::types::{LibraryEpisode, LibraryItemDetail};
use crate::metadata::EffectiveMetadata;

pub mod metadata;
pub mod episodes;

pub use metadata::{AdoptedFilterFacts, AdoptedPerson, library_display_locale};

pub(crate) fn fill_from_adopted(base: &mut MetadataFields, adopted: &EffectiveMetadata) {
    metadata::fill_from_adopted(base, adopted);
}

impl Store {
    /// One item row, by the same column list every card query uses.
    pub(crate) fn library_item_row(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
    ) -> Result<Option<super::types::LibraryItem>, StoreError> {
        let sql = format!(
            "SELECT {ITEM_COLUMNS} FROM library_items i WHERE i.library_id = ?1 AND i.media_id = ?2"
        );
        let mut statement = self.library.prepare(&sql)?;
        let row = statement
            .query_row(params![library_id.to_string(), media_id.to_string()], |row| {
                item_from_row(row)
            })
            .optional()?;
        Ok(row)
    }

    /// Item detail: the real `library_items` columns, adopted metadata for the resolved
    /// locale (Scrape inputs filling the gaps), the first page of files with their coverage.
    pub fn library_item_detail(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        locale: &str,
    ) -> Result<Option<LibraryItemDetail>, StoreError> {
        let Some(item) = self.library_item_row(library_id, media_id)? else {
            return Ok(None);
        };
        let resolved = self.resolved_metadata_locale(library_id, media_id, locale)?;
        let mut metadata = self.metadata_input_fields(library_id, media_id, resolved.as_deref())?;
        let adopted = match resolved.as_deref() {
            Some(locale) => self.get_effective_metadata(media_id, locale)?,
            None => None,
        };
        if let Some(adopted) = &adopted {
            fill_from_adopted(&mut metadata, adopted);
        }
        let file_page = self.library_file_page(library_id, media_id, None, 50)?;
        let file_units = self.file_units(library_id, media_id, &file_page.files)?;
        let seasons = self.library_seasons(library_id, media_id)?;
        Ok(Some(LibraryItemDetail {
            item,
            metadata,
            files: file_page.files,
            next_file_cursor: file_page.next_cursor,
            seasons,
            file_units,
        }))
    }


    /// Scrape inputs of one media: the work-scope fields the Scrape step stored, preferring
    /// the resolved locale and otherwise the deterministic `ORDER BY locale, input_key`.
    pub(crate) fn metadata_input_fields(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        locale: Option<&str>,
    ) -> Result<MetadataFields, StoreError> {
        let preferred = locale.map(str::to_string);
        let mut statement = self.library.prepare(
            "SELECT locale, fields_json FROM library_metadata_inputs WHERE library_id = ?1 AND media_id = ?2 AND entity_scope = 'work' AND parse_state = 'valid' ORDER BY locale, input_key",
        )?;
        let rows = statement
            .query_map(params![library_id.to_string(), media_id.to_string()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let chosen = rows
            .iter()
            .find(|(row_locale, _)| Some(row_locale.as_str()) == preferred.as_deref())
            .or_else(|| rows.first());
        match chosen {
            Some((_, fields_json)) => Ok(serde_json::from_str(fields_json)?),
            None => Ok(MetadataFields::default()),
        }
    }

    pub(crate) fn library_seasons(&self, library_id: LibraryId, media_id: MediaId) -> Result<Vec<u32>, StoreError> {
        let mut statement = self.library.prepare(
            "SELECT season FROM library_units WHERE library_id = ?1 AND media_id = ?2 AND season >= 0 GROUP BY season ORDER BY season",
        )?;
        let rows = statement.query_map(params![library_id.to_string(), media_id.to_string()], |row| row.get::<_, i64>(0))?;
        Ok(rows.map(|row| row.map(|season| season as u32)).collect::<Result<Vec<_>, _>>()?)
    }

    /// Coverage of the listed files (`(-1,-1)` marks a whole-work file), one query.
    pub fn file_units(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        files: &[domain::LibraryFile],
    ) -> Result<Vec<(LedgerId, Vec<UnitKey>)>, StoreError> {
        let mut statement = self.library.prepare(
            "SELECT file_id, season, episode FROM library_file_units WHERE library_id = ?1 AND media_id = ?2 ORDER BY season, episode, file_id",
        )?;
        let rows = statement.query_map(
            params![library_id.to_string(), media_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i32>(1)?,
                    row.get::<_, i32>(2)?,
                ))
            },
        )?;
        let mut by_file: BTreeMap<String, Vec<UnitKey>> = BTreeMap::new();
        for row in rows {
            let (file_id, season, episode) = row?;
            by_file
                .entry(file_id)
                .or_default()
                .push(UnitKey::new(season, episode));
        }
        Ok(files
            .iter()
            .filter_map(|file| {
                by_file
                    .remove(&file.file_id.to_string())
                    .map(|units| (file.file_id, units))
            })
            .collect())
    }
}

