//! Bounded entity dimensions; memberships are sought only for selected representatives.
use crate::{Store, StoreError};
use domain::{LibraryId, MediaId, PhysicalSourceId, ProtocolItemRef, TypedItemId, UserId};
use rusqlite::{Connection, OptionalExtension, params};
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolEntityKind {
    Genre,
    Studio,
}
impl ProtocolEntityKind {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Genre => "genre",
            Self::Studio => "studio",
        }
    }
}
pub struct ProtocolEntityQuery {
    pub user_id: UserId,
    pub library_id: Option<LibraryId>,
    pub kind: ProtocolEntityKind,
    pub search: Option<String>,
    pub offset: usize,
    pub limit: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolEntity {
    pub name: String,
    pub wire_id: String,
    pub child_count: u64,
}
pub struct ProtocolEntityPage {
    pub items: Vec<ProtocolEntity>,
    pub total: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolPerson {
    pub wire_id: String,
    pub name: String,
    pub person_type: String,
    pub role: Option<String>,
    pub tmdb_id: Option<String>,
    pub image_url: Option<String>,
}

impl Store {
    pub fn protocol_entities(
        &self,
        request: &ProtocolEntityQuery,
    ) -> Result<ProtocolEntityPage, StoreError> {
        let allowed = authorized(self, Some(request.user_id))?;
        self.with_read(crate::Db::Library,|tx|{
            let mut ids=visible(tx,&allowed)?;
            if let Some(library)=request.library_id{ids.retain(|id|id==&library.to_string());}
            let mut counts=BTreeMap::<String,i64>::new();
            let mut stmt=tx.prepare_cached("SELECT value,SUM(member_count) FROM library_query_counts WHERE library_id IN (SELECT value FROM json_each(?1)) AND surface='protocol_entity' AND dimension=?2 GROUP BY value")?;
            for row in stmt.query_map(params![serde_json::to_string(&ids)?,request.kind.key()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?)))?{let (name,count)=row?;counts.insert(name,count);}
            if ids.len()>1 {
                let mut overlaps=tx.prepare_cached("SELECT name,owner_libraries_json,member_count FROM protocol_entity_overlap_counts WHERE entity_kind=?1")?;
                for row in overlaps.query_map([request.kind.key()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?)))?{
                    let (name,owners,count)=row?;let owners:Vec<String>=serde_json::from_str(&owners)?;
                    let overlap=owners.iter().filter(|lib|ids.contains(lib)).count().saturating_sub(1) as i64;
                    if let Some(total)=counts.get_mut(&name){*total-=overlap*count;}
                }
            }
            let search=request.search.as_deref().unwrap_or("").to_lowercase();
            let items:Vec<_>=counts.into_iter().filter(|(name,count)|*count>0 && name.to_lowercase().contains(&search)).map(|(name,count)|{
                let wire_id=uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_DNS,name.as_bytes()).simple().to_string();
                ProtocolEntity{name,wire_id,child_count:count as u64}
            }).collect();
            Ok(ProtocolEntityPage{total:items.len() as u64,items:items.into_iter().skip(request.offset).take(request.limit).collect()})
        })
    }
    pub fn protocol_entity_representative(
        &self,
        user: Option<UserId>,
        kind: ProtocolEntityKind,
        query: &str,
    ) -> Result<Option<ProtocolItemRef>, StoreError> {
        let allowed = authorized(self, user)?;
        self.with_read(crate::Db::Library,|tx|{
            let ids=serde_json::to_string(&visible(tx,&allowed)?)?;
            let compact=query.replace('-',"");
            let column=if uuid::Uuid::parse_str(query).is_ok(){"wire_id"}else{"name_key"};
            let key=if column=="wire_id"{compact}else{query.to_lowercase()};
            let sql=format!("SELECT e.library_id,e.media_id,i.kind,COALESCE(e.poster_file_id,i.preferred_file_id) FROM library_protocol_entities e JOIN library_items i ON i.library_id=e.library_id AND i.media_id=e.media_id WHERE e.entity_kind=?1 AND e.{column}=?2 AND e.library_id IN (SELECT value FROM json_each(?3)) AND i.available_file_count>0 ORDER BY e.poster_file_id DESC LIMIT 1");
            let row:Option<(String,String,String,Option<String>)>=tx.query_row(&sql,params![kind.key(),key,ids],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
            row.map(|(lib,media,kind,file)|reference(tx,&lib,&media,&kind,file)).transpose()
        })
    }
    pub fn protocol_person(
        &self,
        user: Option<UserId>,
        query: &str,
    ) -> Result<Option<ProtocolPerson>, StoreError> {
        let allowed = authorized(self, user)?;
        self.with_read(crate::Db::Library,|tx|{
            let ids=serde_json::to_string(&visible(tx,&allowed)?)?;
            let compact=query.replace('-',"");
            let (column,key)=if uuid::Uuid::parse_str(query).is_ok(){("wire_id",compact)}else if query.chars().all(|ch|ch.is_ascii_digit()) && !query.is_empty(){("tmdb_id",query.to_string())}else{("name_key",query.to_lowercase())};
            let sql=format!("SELECT p.wire_id,p.display_name,p.role,p.character,NULLIF(p.tmdb_id,''),p.image_url FROM library_protocol_people p JOIN library_items i ON i.library_id=p.library_id AND i.media_id=p.media_id WHERE p.{column}=?1 AND p.library_id IN (SELECT value FROM json_each(?2)) AND i.available_file_count>0 ORDER BY p.image_url DESC LIMIT 1");
            Ok(tx.query_row(&sql,params![key,ids],|r|Ok(ProtocolPerson{wire_id:r.get(0)?,name:r.get(1)?,person_type:r.get(2)?,role:r.get(3)?,tmdb_id:r.get(4)?,image_url:r.get(5)?})).optional()?)
        })
    }
}
pub(super) fn authorized(
    store: &Store,
    user: Option<UserId>,
) -> Result<Option<HashSet<String>>, StoreError> {
    user.map(|user| store.authorized_library_ids(user))
        .transpose()
}
pub(super) fn visible(
    tx: &Connection,
    allowed: &Option<HashSet<String>>,
) -> Result<Vec<String>, StoreError> {
    let rows=tx.prepare_cached("SELECT library_id,lifecycle_state FROM libraries l WHERE NOT EXISTS(SELECT 1 FROM library_removals r WHERE r.library_id=l.library_id AND r.state!='succeeded') ORDER BY library_id")?.query_map([],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
    let mut ids = Vec::new();
    for (id, state) in rows {
        if allowed.as_ref().is_some_and(|a| !a.contains(&id)) {
            continue;
        }
        if state == "reindexing" {
            return Err(StoreError::QueryUnavailable {
                library_id: id.parse()?,
            });
        }
        if matches!(state.as_str(), "frozen" | "draining") {
            continue;
        }
        ids.push(id);
    }
    Ok(ids)
}
pub(super) fn reference(
    tx: &Connection,
    lib: &str,
    media: &str,
    _kind: &str,
    file: Option<String>,
) -> Result<ProtocolItemRef, StoreError> {
    let media: MediaId = media.parse()?;
    let item = TypedItemId::Work(media);
    let preferred_source = file
        .map(|file| {
            let revision = tx.query_row(
                "SELECT file_revision FROM library_files WHERE file_id=?1",
                [&file],
                |r| r.get(0),
            )?;
            Ok::<_, StoreError>(PhysicalSourceId {
                file_id: file.parse()?,
                revision,
            })
        })
        .transpose()?;
    Ok(ProtocolItemRef {
        library_id: lib.parse()?,
        item,
        preferred_source,
    })
}
