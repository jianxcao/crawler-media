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
//! - `facts`：人物/流派这类需要派生 id 的取值解析；
//! - `order`：多项排序键；
//! - `sql`：CTE 组装、分页/计数的两条语句；
//! - `metadata`：页内条目落库快照的水合（观测输入 + 采纳合成，不读现场）。

mod facts;
mod folder;
mod metadata;
mod order;
mod predicates;
mod scope;
mod sql;

use domain::{ProtocolItemPage, ProtocolItemQuery};
use rusqlite::params_from_iter;

use crate::library_query::items::Bindings;
use crate::{Store, StoreError};

impl Store {
    /// 完整合同的一页：服务端筛选、去重、排序后再 `LIMIT`。
    pub fn protocol_item_page(
        &self,
        request: &ProtocolItemQuery,
    ) -> Result<ProtocolItemPage, StoreError> {
        let tx = self.library.unchecked_transaction()?;
        let Some(scope) = scope::Scope::resolve(self, &tx, request)? else {
            tx.commit()?;
            return Ok(empty_page());
        };
        let mut bindings = Bindings::from_values(scope.values.clone());
        // 谓词按支组装：分集支才有 `u` 别名，绑定顺序也就必须与 CTE 出现顺序一致。
        let work = scope
            .granularity
            .wants_work()
            .then(|| {
                predicates::build(
                    &tx,
                    request,
                    &scope,
                    predicates::Branch::Work,
                    &mut bindings,
                )
            })
            .transpose()?;
        let unit = scope
            .granularity
            .wants_unit()
            .then(|| {
                predicates::build(
                    &tx,
                    request,
                    &scope,
                    predicates::Branch::Unit,
                    &mut bindings,
                )
            })
            .transpose()?;
        let plan = sql::plan(request, &scope, work.as_deref(), unit.as_deref(), &bindings);
        let total: i64 = tx.query_row(
            &plan.count_sql,
            params_from_iter(plan.filter_values.iter().cloned()),
            |row| row.get(0),
        )?;
        let items = if total > 0 {
            let mut stmt = tx.prepare(&plan.page_sql)?;
            let rows = stmt.query_map(
                params_from_iter(plan.page_values.iter().cloned()),
                sql::row_to_ref,
            )?;
            rows.collect::<Result<Vec<_>, _>>()?
        } else {
            Vec::new()
        };
        tx.commit()?;
        tracing::debug!(
            user = %request.user_id,
            total,
            returned = items.len(),
            granularity = ?scope.granularity,
            "protocol_item_page"
        );
        Ok(ProtocolItemPage {
            items,
            total: total.max(0) as u64,
        })
    }
}

/// 未知父级、未授权库、空范围都落到这里：空页而不是错误。
fn empty_page() -> ProtocolItemPage {
    ProtocolItemPage {
        items: Vec::new(),
        total: 0,
    }
}
