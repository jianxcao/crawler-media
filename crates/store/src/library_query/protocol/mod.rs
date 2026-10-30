//! 协议分页：把客户端的条目查询翻成一次短读事务内的 SQL 分页。
//!
//! 事务内的固定顺序是：解析授权与实体范围 → 组装谓词 → 算总数 → 取当前页 → 提交。
//! 授权、去重、排序、`LIMIT` 全在同一事务内完成，所以"筛选后的第 51 项"
//! 与"首页返回什么"看到的是同一份快照；页内条目只带逻辑 ID 与可播放来源，
//! 元数据合成是 Task 7/8 的产物，不在这里读现场 NFO、cache 或网络。
//!
//! 子模块分工：
//! - `scope`：授权库集合、实体范围与分集粒度；
//! - `predicates`：合同字段 → SQL 子句；
//! - `request` / `membership`：规范化分类键与范围内精确成员；
//! - `order`：多项排序键；
//! - `sql`：范围内 Work/Unit 行输入；
//! - `count` / `page` / `source`：精确数量、索引或通用分页、页内物理来源；
//! - `metadata`：页内条目落库快照的水合（观测输入 + 采纳合成，不读现场）。

mod count;
mod entities;
mod entity_counts;
mod hints;
mod seasons;
pub use entities::{
    ProtocolEntity, ProtocolEntityKind, ProtocolEntityPage, ProtocolEntityQuery, ProtocolPerson,
};
pub use entity_counts::ProtocolCounts;
pub use seasons::ProtocolSeason;
mod folder;
mod latest;
mod membership;
pub(crate) use latest::select_latest;
pub(crate) mod metadata;
mod order;
mod page;
mod predicates;
mod request;
mod scope;
mod source;
mod sql;

use domain::{LibraryId, ProtocolItemPage, ProtocolItemQuery};
use rusqlite::Connection;
use std::collections::HashSet;

use crate::library_query::items::Bindings;
use crate::{Store, StoreError};

impl Store {
    /// 完整合同的一页：服务端筛选、去重、排序后再 `LIMIT`。
    pub fn protocol_item_page(
        &self,
        request: &ProtocolItemQuery,
    ) -> Result<ProtocolItemPage, StoreError> {
        self.protocol_item_page_inner(request)
    }

    pub fn protocol_item_page_with_stats(
        &self,
        request: &ProtocolItemQuery,
    ) -> Result<(ProtocolItemPage, super::items::QueryWorkStats), StoreError> {
        let (page, mut stats) = self.with_stats(crate::stats::StatsScope::Both, || {
            self.protocol_item_page_inner(request)
        });
        let page = page?;
        stats.rows_returned = page.items.len() as u64;
        Ok((page, stats))
    }

    fn protocol_item_page_inner(
        &self,
        request: &ProtocolItemQuery,
    ) -> Result<ProtocolItemPage, StoreError> {
        let authorized = self
            .authorized_library_ids(request.user_id)?
            .into_iter()
            .map(|id| id.parse())
            .collect::<Result<HashSet<LibraryId>, _>>()?;
        self.with_read(crate::Db::Library, |conn| {
            select_protocol(conn, &authorized, request)
        })
    }
}

pub(crate) fn select_protocol(
    tx: &Connection,
    authorized: &HashSet<LibraryId>,
    request: &ProtocolItemQuery,
) -> Result<ProtocolItemPage, StoreError> {
    select_protocol_inner(tx, authorized, request, true)
}

fn select_protocol_inner(
    tx: &Connection,
    authorized: &HashSet<LibraryId>,
    request: &ProtocolItemQuery,
    exact_total: bool,
) -> Result<ProtocolItemPage, StoreError> {
    let normalized = request::normalize(request)?;
    let request = &normalized.original;
    let Some(scope) = scope::Scope::resolve(tx, authorized, request)? else {
        return Ok(empty_page());
    };
    let mut bindings = Bindings::from_values(scope.values.clone());
    let membership = membership::exact_predicates(&normalized, &scope, &mut bindings)?;
    let exact = membership.predicate.as_str();
    // 谓词按支组装：分集支才有 `u` 别名，绑定顺序也就必须与 CTE 出现顺序一致。
    let work = scope
        .granularity
        .wants_work()
        .then(|| predicates::build(request, exact, predicates::Branch::Work, &mut bindings))
        .transpose()?;
    let unit = scope
        .granularity
        .wants_unit()
        .then(|| predicates::build(request, exact, predicates::Branch::Unit, &mut bindings))
        .transpose()?;
    let plan = page::classify(&normalized, &scope);
    let search = request
        .search_term
        .as_deref()
        .is_some_and(|v| !v.trim().is_empty());
    let scoped = sql::scoped_cte(&scope, search && scope.granularity.wants_work());
    // A derived membership set (person identities) has to sit between `scoped` — which
    // it reads — and `rows`, which reads it. `rows` is the only CTE fragment both the
    // count and the page interpolate, so it carries the extra CTE.
    let mut rows = sql::rows_ctes(&scope, work.as_deref(), unit.as_deref());
    if let Some(extra) = &membership.ctes {
        rows = format!("{extra}, {rows}");
    }
    let values = bindings.values().collect::<Vec<_>>();
    let total = if exact_total {
        count::count(tx, &plan, &scope, &scoped, &rows, &values)?
    } else {
        0
    };
    let items = page::read(
        tx,
        request,
        &scope,
        &plan,
        &scoped,
        &rows,
        work.as_deref(),
        unit.as_deref(),
        &values,
    )?;
    tracing::debug!(
        user = %request.user_id,
        total,
        returned = items.len(),
        granularity = ?scope.granularity,
        "protocol_item_page"
    );
    let total = if exact_total {
        total
    } else {
        items.len() as u64
    };
    Ok(ProtocolItemPage { items, total })
}

/// 未知父级、未授权库、空范围都落到这里：空页而不是错误。
fn empty_page() -> ProtocolItemPage {
    ProtocolItemPage {
        items: Vec::new(),
        total: 0,
    }
}

#[cfg(test)]
mod query_model_faults;
