//! Persistent references are inspected in one repeatable-read, read-only snapshot.
use std::collections::{BTreeMap, BTreeSet};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use super::{DbPreflight, Decoded, index};

// Names correspond to game_schema.sql; notification and wallet event types are
// protocol enums, not invTypes. Nullable columns and arrays retain their semantics.
const COLUMNS: &[(&str,&str,&str,bool)] = &[
    ("types","crate_rewards","crate_type_id",false), ("types","items","type_id",false),
    ("types","dynamic_items","source_type_id",false), ("types","dynamic_items","mutator_type_id",false), ("types","dynamic_items","result_type_id",false),
    ("types","skills","type_id",false), ("types","character_expert_systems","type_id",false), ("types","skill_history","skill_type_id",false), ("types","skill_queue","type_id",false),
    ("types","market_orders","type_id",false), ("types","market_order_history","type_id",false), ("types","market_transactions","type_id",false),
    ("types","contract_items","type_id",false), ("types","industry_jobs","blueprint_type_id",false), ("types","industry_jobs","product_type_id",false), ("types","industry_jobs","optional_type_id",false),
    ("types","killmails","victim_ship_type",false), ("types","killmails","final_ship_type",false), ("types","killmails","final_weapon_type",false),
    ("types","insurance","type_id",false), ("types","bookmarks","type_id",false), ("types","lp_offers","type_id",false), ("types","skill_plan_milestones","type_id",false),
    ("types","retail_prices","type_id",false), ("types","redeem_tokens","type_id",false), ("types","login_reward_retro_grants","type_id",false),
    ("types","fittings","ship_type_id",false), ("types","fittings","fit_types",true), ("types","module_charges","type_id",false), ("types","pending_reloads","type_id",false),
    ("types","exploration_sites","wh_type",false), ("types","asteroids","type_id",false), ("types","character_mining_ledger","type_id",false), ("types","character_mining_ledger","source_ship_type_id",false),
    ("dogmaattributes","item_attributes","attribute_id",false), ("dogmaattributes","dynamic_item_modifiers","attribute_id",false), ("dogmaeffects","items","brain_effect_ids",true),
];
fn group(row: &Value) -> Option<i64> { row.get("groupID").and_then(Value::as_i64) }
fn category(groups: &BTreeMap<i64,&Value>, row: &Value) -> Option<i64> { groups.get(&group(row)?).and_then(|r| r.get("categoryID")).and_then(Value::as_i64) }

pub(super) async fn preflight(client: &mut tokio_postgres::Client, decoded: &Decoded) -> Result<DbPreflight> {
    let new=index(&decoded.native)?;
    let tx=client.build_transaction().isolation_level(tokio_postgres::IsolationLevel::RepeatableRead).read_only(true).start().await.context("starting read-only database preflight")?;
    let mut old_rows=super::FamilyRows::new();
    for family in ["types","groups","dogmaattributes","dogmaeffects"] {
        let sql=format!("SELECT key::text AS key, data::text AS data FROM sde_native.{family}");
        let mut records=Vec::new();
        for row in tx.query(&sql,&[]).await.with_context(|| format!("reading old {family} snapshot"))? {
            let key:String=row.get("key"); let data:String=row.get("data"); let mut value:Value=serde_json::from_str(&data)?;
            let id:i64=key.parse().with_context(|| format!("invalid old {family} key {key}"))?;
            value.as_object_mut().with_context(|| format!("invalid old {family}/{key}"))?.insert("_key".into(),Value::from(id)); records.push(value);
        }
        old_rows.insert(family.into(),records);
    }
    let old=index(&old_rows)?;
    let mut changed:BTreeMap<&str,BTreeSet<i64>>=BTreeMap::new();
    for family in ["types","dogmaattributes","dogmaeffects"] {
        let previous=old.get(family).context("missing old family")?; let current=new.get(family).context("missing new family")?;
        let mut ids=BTreeSet::new();
        for (&id,&row) in previous { match current.get(&id) { None => {ids.insert(id);}, Some(next) if family=="types" && (group(row)!=group(next) || category(&old["groups"],row)!=category(&new["groups"],next)) => {ids.insert(id);}, _=>() } }
        changed.insert(family,ids);
    }
    let mut result=DbPreflight {checked_references:0,checked_columns:Vec::new()};
    for &(family,table,column,array) in COLUMNS {
        let expr=if array {format!("unnest({column})")} else {column.to_owned()};
        let sql=format!("SELECT DISTINCT {expr} AS id FROM game.{table}");
        let mut refs=BTreeSet::new();
        for row in tx.query(&sql,&[]).await.with_context(|| format!("checking game.{table}.{column}"))? { if let Some(id)=row.get::<_,Option<i32>>("id") { refs.insert(i64::from(id)); } }
        result.checked_references += refs.len() as u64;
        result.checked_columns.push(format!("game.{table}.{column}"));
        if let Some(id)=refs.intersection(&changed[family]).next() { bail!("persistent-reference game.{table}.{column}: {family} ID {id} was removed or reclassified; no mutation performed"); }
    }
    for row in tx.query("SELECT required_items::text AS required_items FROM game.lp_offers", &[]).await.context("checking game.lp_offers.required_items")? {
        let text: String = row.get("required_items");
        let entries: Value = serde_json::from_str(&text)?;
        for item in entries.as_array().context("invalid game.lp_offers.required_items")? {
            let id = item.get("type_id").and_then(Value::as_i64).context("invalid game.lp_offers.required_items type_id")?;
            result.checked_references += 1;
            if changed["types"].contains(&id) { bail!("persistent-reference game.lp_offers.required_items: types ID {id} was removed or reclassified; no mutation performed"); }
        }
    }
    result.checked_columns.push("game.lp_offers.required_items".into());
    // Opaque persisted payloads (notably killmail blobs) cannot be exhaustively
    // inspected as static references. Require a separate migration decision.
    if changed.values().any(|ids| !ids.is_empty()) {
        bail!("persistent-reference migration required: removed or reclassified IDs {changed:?}; opaque killmail and generic persisted payload references are unverified; no mutation performed");
    }
    tx.commit().await.context("ending read-only database preflight")?;
    Ok(result)
}
