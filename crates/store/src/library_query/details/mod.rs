use crate::library_query::items::{ITEM_COLUMNS, item_from_row};
use crate::library_query::types::LibraryItemDetail;
use crate::metadata::EffectiveMetadata;
use crate::metadata::locale::{LocaleFallback, select_locale};
use crate::{Store, StoreError};
use domain::{LedgerId, LibraryId, MediaId, MetadataFields, UnitKey};
use rusqlite::{OptionalExtension, params};
use std::collections::BTreeMap;

pub mod episodes;
pub mod metadata;

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
            .query_row(
                params![library_id.to_string(), media_id.to_string()],
                item_from_row,
            )
            .optional()?;
        Ok(row)
    }

    /// Item detail: the real `library_items` columns, adopted metadata for the resolved
    /// locale (Scrape inputs filling the gaps), the first page of files with their coverage.
    ///
    /// Scrape inputs follow their own chain (requested → Library → instance) so a caller that
    /// asks for `en-US` reads `en-US` inputs even when no adopted metadata exists yet.
    pub fn library_item_detail(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        locale: &str,
    ) -> Result<Option<LibraryItemDetail>, StoreError> {
        self.ensure_library_query_ready(library_id)?;
        let (integrity, op_id): (String, Option<String>) = self
            .library
            .query_row(
                "SELECT l.integrity_state,
                        (SELECT operation_id FROM library_operations WHERE library_id = l.library_id AND kind = 'integrity' AND state IN ('failed', 'succeeded') ORDER BY updated_at_ms DESC LIMIT 1)
                 FROM libraries l WHERE l.library_id = ?1",
                params![library_id.to_string()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .unwrap_or_else(|| ("unknown".into(), None));

        if integrity == "failed" {
            let op_str = op_id.unwrap_or_default();
            return Err(StoreError::Protected(format!(
                "library.integrity: library {} integrity check failed ({op_str})",
                library_id
            )));
        }

        let Some(item) = self.library_item_row(library_id, media_id)? else {
            return Ok(None);
        };
        let resolved = self.resolved_metadata_locale(library_id, media_id, locale)?;
        // NFO 输入自带一条完整的链（请求 → Library → 实例）。媒体还没有任何已采纳元数据时
        // `resolved` 是 `None`，此时若把 `resolved` 交给这条链，调用方请求的语言就会被丢掉，
        // 只有 Library/实例 locale 能命中，按请求语言导入的输入会整片读不到。
        let input_locale = match locale.trim() {
            "" => resolved.as_deref(),
            requested => Some(requested),
        };
        let mut metadata = self.metadata_input_fields(library_id, media_id, input_locale)?;
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

    /// Scrape inputs of one media: the work-scope fields already stored for the locale the
    /// query chain resolves to.
    ///
    /// 绝不用 `rows.first()` 在语言之间任选：只有请求 → Library → 实例这条链上命中的
    /// locale 才可读，外加导入时没有 locale 才会写入的 `''` 兜底桶。
    pub(crate) fn metadata_input_fields(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        locale: Option<&str>,
    ) -> Result<MetadataFields, StoreError> {
        let Some(chosen) = self.metadata_input_locale(library_id, media_id, locale)? else {
            return Ok(MetadataFields::default());
        };
        let fields_json: Option<String> = self
            .library
            .query_row(
                "SELECT fields_json FROM library_metadata_inputs
                 WHERE library_id = ?1 AND media_id = ?2 AND entity_scope = 'work'
                   AND parse_state = 'valid' AND locale = ?3
                 ORDER BY input_key
                 LIMIT 1",
                params![library_id.to_string(), media_id.to_string(), chosen],
                |row| row.get(0),
            )
            .optional()?;
        match fields_json {
            Some(fields_json) => Ok(serde_json::from_str(&fields_json)?),
            None => Ok(MetadataFields::default()),
        }
    }

    /// Pick the NFO input locale along the request → Library → instance chain.
    ///
    /// The unbound bucket (`''`) is an explicit last resort for inputs imported while the
    /// Library had no locale; any other stored locale is never selected implicitly.
    fn metadata_input_locale(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        requested: Option<&str>,
    ) -> Result<Option<String>, StoreError> {
        let mut statement = self.library.prepare(
            "SELECT DISTINCT locale FROM library_metadata_inputs
             WHERE library_id = ?1 AND media_id = ?2 AND entity_scope = 'work'
               AND parse_state = 'valid'
             ORDER BY locale",
        )?;
        let locales = statement
            .query_map(
                params![library_id.to_string(), media_id.to_string()],
                |row| row.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        if locales.is_empty() {
            return Ok(None);
        }
        let chain: Vec<String> = [
            requested.map(str::to_string),
            library_display_locale(self, library_id)?,
            metadata::instance_default_locale(self)?,
        ]
        .into_iter()
        .flatten()
        .collect();
        Ok(select_locale(
            &locales,
            &chain,
            LocaleFallback::UnboundInputOnly,
        ))
    }

    pub(crate) fn library_seasons(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
    ) -> Result<Vec<u32>, StoreError> {
        let mut statement = self.library.prepare(
            "SELECT season FROM library_units WHERE library_id = ?1 AND media_id = ?2 AND season >= 0 GROUP BY season ORDER BY season",
        )?;
        let rows = statement.query_map(
            params![library_id.to_string(), media_id.to_string()],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(rows
            .map(|row| row.map(|season| season as u32))
            .collect::<Result<Vec<_>, _>>()?)
    }

    /// Coverage of the listed files (`(-1,-1)` marks a whole-work file), in bounded
    /// file-id windows. Other files of the same Media are never decoded.
    pub fn file_units(
        &self,
        library_id: LibraryId,
        media_id: MediaId,
        files: &[domain::LibraryFile],
    ) -> Result<Vec<(LedgerId, Vec<UnitKey>)>, StoreError> {
        let mut by_file: BTreeMap<String, Vec<UnitKey>> = BTreeMap::new();
        let ids: std::collections::BTreeSet<_> =
            files.iter().map(|file| file.file_id.to_string()).collect();
        let ids: Vec<_> = ids.into_iter().collect();
        let transaction = if ids.len() > 400 && self.library.is_autocommit() {
            Some(self.library.unchecked_transaction()?)
        } else {
            None
        };
        for chunk in ids.chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            // This WITHOUT ROWID table's primary key is (file_id, season, episode).
            // Without the pin, an unanalyzed database can scan the entire
            // (library_id, media_id, ...) index before testing the requested ids.
            let sql = format!(
                "SELECT file_id, season, episode
                 FROM library_file_units INDEXED BY sqlite_autoindex_library_file_units_1
                 WHERE file_id IN ({placeholders}) AND library_id = ? AND media_id = ?
                 ORDER BY file_id, season, episode"
            );
            let mut bindings = chunk.to_vec();
            bindings.extend([library_id.to_string(), media_id.to_string()]);
            let mut statement = self.library.prepare(&sql)?;
            let rows = statement.query_map(rusqlite::params_from_iter(bindings), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i32>(1)?,
                    row.get::<_, i32>(2)?,
                ))
            })?;
            for row in rows {
                let (file_id, season, episode) = row?;
                by_file
                    .entry(file_id)
                    .or_default()
                    .push(UnitKey::new(season, episode));
            }
        }
        if let Some(transaction) = transaction {
            transaction.commit()?;
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
