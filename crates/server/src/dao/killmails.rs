//! Killmail, immutable war affiliation snapshots and retail prices in `game.killmails` and `game.retail_prices`.
//! This DAO has no direct client mirror or wire contract; the owning service handles those.
//! Transactions, security policy and notifications are not handled here.

use crate::db::{self, executor::Executor};
use tokio_postgres::Row;

pub(crate) mod wars;

pub(crate) struct Price {
    pub type_id: i32,
    pub average: f64,
}

impl Price {
    fn from_row(r: &Row) -> Self {
        Self { type_id: r.get("type_id"), average: r.get("average") }
    }
}

pub(crate) async fn prices(ex: &(impl Executor + ?Sized), types: &[i32]) -> Result<Vec<Price>, db::Error> {
    let rows = ex.query("SELECT type_id, average FROM game.retail_prices WHERE type_id = ANY($1)", &[&types]).await?;
    Ok(rows.iter().map(Price::from_row).collect())
}
pub(crate) async fn victim_exists(ex: &(impl Executor + ?Sized), ship: i64) -> Result<bool, db::Error> {
    Ok(ex.query_opt("SELECT 1 FROM game.killmails WHERE victim_item_id=$1", &[&ship]).await?.is_some())
}


pub(crate) struct NewKillmail<'a> {
    pub now: i64,
    pub system: i64,
    pub victim: i64,
    pub victim_item_id: Option<i64>,
    pub victim_corp: i64,
    pub victim_alliance: Option<i64>,
    pub ship_type: i32,
    pub damage_taken: f64,
    pub final_char: Option<i64>,
    pub final_corp: Option<i64>,
    pub final_alliance: Option<i64>,
    pub final_ship_type: i32,
    pub final_weapon_type: i32,
    pub final_damage: f64,
    pub final_sec: f64,
    pub isk_lost: f64,
    pub involved: &'a [i64],
    pub blob: &'a str,
}

struct KillId {
    kill_id: i64,
}

impl KillId {
    fn from_row(r: &Row) -> Self {
        Self { kill_id: r.get("kill_id") }
    }
}

pub(crate) async fn insert(ex: &(impl Executor + ?Sized), k: &NewKillmail<'_>) -> Result<i64, db::Error> {
    let row = ex.query_one(
        "INSERT INTO game.killmails (kill_time, solar_system_id, victim_id, victim_corp_id, victim_ship_type, damage_taken, final_char_id,
                                         final_corp_id, final_ship_type, final_weapon_type, final_damage, final_sec_status, isk_lost,
                                         attacker_ids, blob, victim_alliance_id, final_alliance_id, victim_item_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18) RETURNING kill_id",
        &[
            &k.now, &k.system, &k.victim, &k.victim_corp, &k.ship_type, &k.damage_taken, &k.final_char,
            &k.final_corp, &k.final_ship_type, &k.final_weapon_type, &k.final_damage, &k.final_sec,
            &k.isk_lost, &k.involved, &k.blob, &k.victim_alliance, &k.final_alliance, &k.victim_item_id,
        ],
    ).await?;
    Ok(KillId::from_row(&row).kill_id)
}

pub(crate) enum Filter {
    Kills,
    Losses,
    Id,
}

pub(crate) struct Killmail {
    pub kill_id: i64,
    pub kill_time: i64,
    pub solar_system_id: i64,
    pub victim_id: i64,
    pub victim_corp_id: i64,
    pub victim_alliance_id: Option<i64>,
    pub victim_ship_type: i32,
    pub damage_taken: f64,
    pub final_char_id: Option<i64>,
    pub final_corp_id: Option<i64>,
    pub final_alliance_id: Option<i64>,
    pub final_ship_type: i32,
    pub final_weapon_type: i32,
    pub final_damage: f64,
    pub final_sec_status: f64,
    pub isk_lost: f64,
    pub blob: String,
}

impl Killmail {
    pub(super) fn from_row(r: &Row) -> Self {
        Self {
            kill_id: r.get("kill_id"),
            kill_time: r.get("kill_time"),
            solar_system_id: r.get("solar_system_id"),
            victim_id: r.get("victim_id"),
            victim_corp_id: r.get("victim_corp_id"),
            victim_alliance_id: r.get("victim_alliance_id"),
            victim_ship_type: r.get("victim_ship_type"),
            damage_taken: r.get("damage_taken"),
            final_char_id: r.get("final_char_id"),
            final_corp_id: r.get("final_corp_id"),
            final_alliance_id: r.get("final_alliance_id"),
            final_ship_type: r.get("final_ship_type"),
            final_weapon_type: r.get("final_weapon_type"),
            final_damage: r.get("final_damage"),
            final_sec_status: r.get("final_sec_status"),
            isk_lost: r.get("isk_lost"),
            blob: r.get("blob"),
        }
    }
}

pub(crate) async fn list(ex: &(impl Executor + ?Sized), filter: Filter, key: i64, before: Option<i64>, num: i64) -> Result<Vec<Killmail>, db::Error> {
    let condition = match filter {
        Filter::Kills => "$1 = ANY(attacker_ids)",
        Filter::Losses => "victim_id = $1",
        Filter::Id => "kill_id = $1",
    };
    let rows = ex.query(
        &format!("SELECT kill_id, kill_time, solar_system_id, victim_id, victim_corp_id, victim_ship_type, damage_taken, final_char_id,
                        final_corp_id, final_ship_type, final_weapon_type, final_damage, final_sec_status, isk_lost, blob,
                        victim_alliance_id, final_alliance_id
                 FROM game.killmails WHERE {condition} AND ($2::bigint IS NULL OR kill_id < $2) ORDER BY kill_id DESC LIMIT $3"),
        &[&key, &before, &num],
    ).await?;
    Ok(rows.iter().map(Killmail::from_row).collect())
}
