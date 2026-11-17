//! Contract persistence for contractProxy (eve/client/script/ui/shared/neocom/contracts).
//! Reads game contract escrow and item counts; callers select the financial owner.

use crate::db::{executor::Executor, Error};

/// Escrowed quantity grouped by type: the SQL float8 sum of `contract_items.quantity`,
/// a display aggregate for valuation, not the authoritative stack count.
pub(crate) struct EscrowItem {
    pub type_id: i32,
    pub quantity: f64,
}

pub(crate) struct Escrow {
    pub isk: f64,
    pub items: Vec<EscrowItem>,
}

/// One joined report row; `type_id`/`quantity` are null only on the LEFT JOIN's
/// empty-items row, which still carries `isk`.
struct EscrowRow {
    isk: f64,
    type_id: Option<i32>,
    quantity: Option<f64>,
}
impl EscrowRow {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { isk: row.get("isk"), type_id: row.get("type_id"), quantity: row.get("quantity") }
    }
}

pub(crate) async fn escrow(ex: &(impl Executor + ?Sized), me: i64, auction: i32, outstanding: i32, in_progress: i32) -> Result<Escrow, Error> {
    let rows = ex.query(
        "WITH isk AS (SELECT (SELECT COALESCE(sum(reward), 0) FROM game.contracts WHERE (CASE WHEN for_corp THEN issuer_corp_id ELSE issuer_id END) = $1 AND type <> $2 AND status IN ($3, $4))
                  + (SELECT COALESCE(sum(b.amount), 0) FROM game.contract_bids b JOIN game.contracts k USING (contract_id)
                     WHERE b.bidder_id = $1 AND k.status = $3
                       AND b.amount = (SELECT max(amount) FROM game.contract_bids WHERE contract_id = b.contract_id)) AS isk),
              quantities AS (SELECT ci.type_id, sum(ci.quantity::float8) AS quantity FROM game.contract_items ci
                  JOIN game.contracts k USING (contract_id)
                  WHERE (CASE WHEN k.for_corp THEN k.issuer_corp_id ELSE k.issuer_id END) = $1 AND ci.in_crate AND k.status IN ($3, $4) GROUP BY ci.type_id)
         SELECT isk.isk, quantities.type_id, quantities.quantity FROM isk LEFT JOIN quantities ON true",
        &[&me, &auction, &outstanding, &in_progress],
    ).await?;
    let mut escrow = Escrow { isk: 0.0, items: Vec::new() };
    for row in rows.iter().map(EscrowRow::from_row) {
        escrow.isk = row.isk;
        if let (Some(type_id), Some(quantity)) = (row.type_id, row.quantity) {
            escrow.items.push(EscrowItem { type_id, quantity });
        }
    }
    Ok(escrow)
}

pub(crate) struct ContainerCount {
    pub location_id: i64,
    pub count: i64,
}
impl ContainerCount {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self { location_id: row.get("location_id"), count: row.get("count") }
    }
}

pub(crate) async fn container_counts(ex: &(impl Executor + ?Sized), ids: &[i64], owner: i64) -> Result<Vec<ContainerCount>, Error> {
    Ok(ex.query("SELECT location_id, count(*) FROM game.items WHERE location_id = ANY($1) AND owner_id = $2 GROUP BY 1", &[&ids, &owner]).await?.iter().map(ContainerCount::from_row).collect())
}
