//! Exploration site, wormhole, scan and probe persistence, including site position repair.
//! The owning services support `eve/client/script/parklife`; this DAO has no wire contract.
//! Uses `game.exploration_sites`, `game.exploration_scanned`, `game.exploration_systems`,
//! `game.agent_missions`, `game.asteroids`, `game.items`, `game.module_charges`
//! and `game.retail_prices`. Simulation, policy, transactions and notifications remain with callers.
use crate::db::{executor::Executor, Error};
use tokio_postgres::Row;

struct SiteId { site_id: i64 }
impl SiteId { fn from_row(r: &Row) -> Self { Self { site_id: r.get("site_id") } } }
struct ModuleFlag { flag_id: i32 }
impl ModuleFlag { fn from_row(r: &Row) -> Self { Self { flag_id: r.get("flag_id") } } }
struct Quantity { quantity: i64 }
impl Quantity { fn from_row(r: &Row) -> Self { Self { quantity: r.get("quantity") } } }
struct ItemId { item_id: i64 }
impl ItemId { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id") } } }

const SITE_COLUMNS: &str = "site_id, system_id, target, kind, strength_attr, ease, dungeon_id, name_id, archetype, faction, difficulty,
    x, y, z, overlay_x, overlay_y, overlay_z, deviation, in_space, containers, hacked,
    wh_type, wh_dest_system, wh_dest_class, wh_other, wh_born, wh_life_minutes, wh_mass_max, wh_mass_used, wh_jump_mass, wh_nebula";

pub(crate) const EXPLORATION_SITES_TABLE: &str = "game.exploration_sites";
pub(crate) const AGENT_MISSIONS_TABLE: &str = "game.agent_missions";

pub(crate) struct RepairRow { pub(crate) id: String, pub(crate) system: i64, pub(crate) old: [f64; 3] }
impl RepairRow { fn from_row(r: &Row) -> Self { Self { id: r.get("id"), system: r.get("system_id"), old: [r.get("x"), r.get("y"), r.get("z")] } } }
pub(crate) async fn repair_rows(ex: &(impl Executor + ?Sized), exploration: bool) -> Result<Vec<RepairRow>, Error> {
    let sql = if exploration {
        "SELECT site_id::text AS id, system_id, x, y, z FROM game.exploration_sites"
    } else {
        "SELECT (character_id, agent_id)::text AS id, system_id, x, y, z FROM game.agent_missions"
    };
    Ok(ex.query(sql, &[]).await?.iter().map(RepairRow::from_row).collect())
}
pub(crate) async fn move_exploration(ex: &(impl Executor + ?Sized), site: i64, delta: [f64; 3]) -> Result<(), Error> {
    ex.execute("UPDATE game.exploration_sites SET x = x + $2, y = y + $3, z = z + $4,
                                overlay_x = overlay_x + $2, overlay_y = overlay_y + $3, overlay_z = overlay_z + $4 WHERE site_id = $1", &[&site, &delta[0], &delta[1], &delta[2]]).await?;
    ex.execute("UPDATE game.asteroids SET x = x + $2, y = y + $3, z = z + $4 WHERE field_id = $1", &[&site, &delta[0], &delta[1], &delta[2]]).await?; Ok(())
}
pub(crate) async fn move_mission(ex: &(impl Executor + ?Sized), id: &str, delta: [f64; 3]) -> Result<(), Error> { ex.execute("UPDATE game.agent_missions SET x = x + $2, y = y + $3, z = z + $4 WHERE (character_id, agent_id)::text = $1", &[&id, &delta[0], &delta[1], &delta[2]]).await?; Ok(()) }

pub(crate) struct Wormhole { pub(crate) type_id: i32, pub(crate) dest_system: i64, pub(crate) dest_class: i64, pub(crate) other: i64, pub(crate) born: i64, pub(crate) life_minutes: f64, pub(crate) mass_max: f64, pub(crate) mass_used: f64, pub(crate) jump_mass: f64, pub(crate) nebula: i64 }
pub(crate) struct Site { pub(crate) id: i64, pub(crate) system: i64, pub(crate) target: String, pub(crate) kind: i16, pub(crate) strength_attr: i64, pub(crate) ease: f64, pub(crate) dungeon_id: Option<i64>, pub(crate) name_id: Option<i64>, pub(crate) archetype: Option<i64>, pub(crate) faction: Option<i64>, pub(crate) difficulty: i64, pub(crate) pos: [f64; 3], pub(crate) overlay_pos: [f64; 3], pub(crate) deviation: f64, pub(crate) in_space: bool, pub(crate) containers: Option<Vec<i64>>, pub(crate) hacked: Vec<i64>, pub(crate) wormhole: Option<Wormhole> }

pub(crate) struct SiteWrite<'a> { pub(crate) id: i64, pub(crate) system: i64, pub(crate) target: &'a str, pub(crate) kind: i16, pub(crate) strength_attr: i64, pub(crate) ease: f64, pub(crate) dungeon_id: Option<i64>, pub(crate) name_id: Option<i64>, pub(crate) archetype: Option<i64>, pub(crate) faction: Option<i64>, pub(crate) difficulty: i64, pub(crate) pos: [f64; 3], pub(crate) overlay_pos: [f64; 3], pub(crate) deviation: f64, pub(crate) in_space: bool, pub(crate) containers: Option<&'a [i64]>, pub(crate) hacked: &'a [i64], pub(crate) wormhole: Option<Wormhole> }
impl Site {
    fn from_row(r: &Row) -> Self {
        let wormhole = r.get::<_, Option<i32>>("wh_type").map(|type_id| Wormhole {
            type_id, dest_system: r.get("wh_dest_system"), dest_class: r.get("wh_dest_class"), other: r.get("wh_other"), born: r.get("wh_born"), life_minutes: r.get("wh_life_minutes"), mass_max: r.get("wh_mass_max"), mass_used: r.get("wh_mass_used"), jump_mass: r.get("wh_jump_mass"), nebula: r.get::<_, Option<i64>>("wh_nebula").unwrap_or(0),
        });
        Self { id: r.get("site_id"), system: r.get("system_id"), target: r.get("target"), kind: r.get("kind"), strength_attr: r.get("strength_attr"), ease: r.get("ease"), dungeon_id: r.get("dungeon_id"), name_id: r.get("name_id"), archetype: r.get("archetype"), faction: r.get("faction"), difficulty: r.get("difficulty"), pos: [r.get("x"), r.get("y"), r.get("z")], overlay_pos: [r.get("overlay_x"), r.get("overlay_y"), r.get("overlay_z")], deviation: r.get("deviation"), in_space: r.get("in_space"), containers: r.get("containers"), hacked: r.get("hacked"), wormhole }
    }
}
pub(crate) async fn sites(ex: &(impl Executor + ?Sized), system: i64) -> Result<Vec<Site>, Error> { Ok(ex.query(&format!("SELECT {SITE_COLUMNS} FROM game.exploration_sites WHERE system_id = $1"), &[&system]).await?.iter().map(Site::from_row).collect()) }
pub(crate) async fn lock_site(ex: &(impl Executor + ?Sized), id: i64) -> Result<Option<Site>, Error> { Ok(ex.query_opt(&format!("SELECT {SITE_COLUMNS} FROM game.exploration_sites WHERE site_id = $1 FOR UPDATE"), &[&id]).await?.as_ref().map(Site::from_row)) }
pub(crate) async fn save_site(ex: &(impl Executor + ?Sized), s: &SiteWrite<'_>) -> Result<(), Error> {
    let w = s.wormhole.as_ref();
    ex.execute(&format!("INSERT INTO game.exploration_sites ({SITE_COLUMNS})
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21,
                         $22, $23, $24, $25, $26, $27, $28, $29, $30, $31)
                 ON CONFLICT (site_id) DO UPDATE SET in_space = EXCLUDED.in_space, containers = EXCLUDED.containers,
                     wh_mass_used = EXCLUDED.wh_mass_used"), &[
        &s.id, &s.system, &s.target, &s.kind, &s.strength_attr, &s.ease, &s.dungeon_id, &s.name_id, &s.archetype, &s.faction, &s.difficulty,
        &s.pos[0], &s.pos[1], &s.pos[2], &s.overlay_pos[0], &s.overlay_pos[1], &s.overlay_pos[2], &s.deviation, &s.in_space, &s.containers, &s.hacked,
        &w.map(|w| w.type_id), &w.map(|w| w.dest_system), &w.map(|w| w.dest_class), &w.map(|w| w.other), &w.map(|w| w.born), &w.map(|w| w.life_minutes), &w.map(|w| w.mass_max), &w.map(|w| w.mass_used), &w.map(|w| w.jump_mass), &w.map(|w| w.nebula),
    ]).await?; Ok(())
}
/// Claim one current site container without overwriting another completed hack.
pub(crate) async fn claim_hacked_container(ex: &(impl Executor + ?Sized), site_id: i64, container_id: i64) -> Result<Option<Site>, Error> {
    Ok(ex.query_opt(&format!("UPDATE game.exploration_sites SET hacked = array_append(hacked, $2)
        WHERE site_id = $1 AND $2 = ANY(containers) AND NOT ($2 = ANY(hacked))
        RETURNING {SITE_COLUMNS}"), &[&site_id, &container_id]).await?.as_ref().map(Site::from_row))
}
pub(crate) async fn delete_site(ex: &(impl Executor + ?Sized), site_id: i64) -> Result<(), Error> {
    ex.execute("DELETE FROM game.exploration_sites WHERE site_id = $1", &[&site_id]).await?;
    ex.execute("DELETE FROM game.exploration_scanned WHERE site_id = $1", &[&site_id]).await?; Ok(())
}
pub(crate) async fn delete_sites(ex: &(impl Executor + ?Sized), ids: &mut Vec<i64>) -> Result<(), Error> {
    ids.sort_unstable();
    ids.dedup();
    for &id in ids.iter() {
        delete_site(ex, id).await?;
    }
    Ok(())
}
pub(crate) async fn is_scanned(ex: &(impl Executor + ?Sized), char_id: i64, site_id: i64) -> Result<bool, Error> { Ok(ex.query_opt("SELECT 1 FROM game.exploration_scanned WHERE character_id = $1 AND site_id = $2", &[&char_id, &site_id]).await?.is_some()) }
pub(crate) async fn mark_scanned(ex: &(impl Executor + ?Sized), char_id: i64, site_id: i64) -> Result<bool, Error> { Ok(ex.execute("INSERT INTO game.exploration_scanned (character_id, site_id) VALUES ($1, $2) ON CONFLICT DO NOTHING", &[&char_id, &site_id]).await? == 1) }
pub(crate) async fn next_site_id(ex: &(impl Executor + ?Sized)) -> Result<i64, Error> { Ok(SiteId::from_row(&ex.query_one("SELECT GREATEST(nextval('game.exploration_site_seq'), COALESCE((SELECT max(site_id) + 1 FROM game.exploration_sites), 0)) AS site_id", &[]).await?).site_id) }
pub(crate) async fn delete_expired(ex: &(impl Executor + ?Sized), now: i64) -> Result<(), Error> { ex.execute("DELETE FROM game.exploration_sites WHERE wh_born IS NOT NULL AND wh_born + (wh_life_minutes * 60 * 10000000)::bigint < $1", &[&now]).await?; Ok(()) }
pub(crate) async fn is_generated(ex: &(impl Executor + ?Sized), system: i64) -> Result<bool, Error> { Ok(ex.query_opt("SELECT 1 FROM game.exploration_systems WHERE system_id = $1", &[&system]).await?.is_some()) }
pub(crate) async fn mark_generated(ex: &(impl Executor + ?Sized), system: i64, now: i64) -> Result<(), Error> { ex.execute("INSERT INTO game.exploration_systems (system_id, generated_at) VALUES ($1, $2) ON CONFLICT DO NOTHING", &[&system, &now]).await?; Ok(()) }
pub(crate) async fn module_flag(ex: &(impl Executor + ?Sized), module: i64, ship: i64) -> Result<Option<i32>, Error> { Ok(ex.query_opt("SELECT flag_id FROM game.items WHERE item_id = $1 AND location_id = $2", &[&module, &ship]).await?.as_ref().map(|r| ModuleFlag::from_row(r).flag_id)) }
pub(crate) async fn consume_probes(ex: &(impl Executor + ?Sized), ship: i64, flag: i32, type_id: i32, have: i64, take: i64) -> Result<Option<i64>, Error> {
    if take == have { Ok(ex.query_opt("DELETE FROM game.module_charges WHERE ship_id = $1 AND flag_id = $2 AND type_id = $3 AND quantity = $4 RETURNING quantity", &[&ship, &flag, &type_id, &have]).await?.map(|_| 0)) }
    else { Ok(ex.query_opt("UPDATE game.module_charges SET quantity = quantity - $4 WHERE ship_id = $1 AND flag_id = $2 AND type_id = $3 AND quantity >= $4 RETURNING quantity", &[&ship, &flag, &type_id, &take]).await?.as_ref().map(|r| Quantity::from_row(r).quantity)) }
}
pub(crate) async fn delete_container(ex: &(impl Executor + ?Sized), id: i64) -> Result<(), Error> { ex.execute("DELETE FROM game.items WHERE location_id = $1 OR item_id = $1", &[&id]).await?; Ok(()) }
pub(crate) async fn insert_container(ex: &(impl Executor + ?Sized), type_id: i32, system: i64, pos: [f64; 3]) -> Result<i64, Error> { Ok(ItemId::from_row(&ex.query_one("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity, singleton, x, y, z)
                 VALUES ($1, 1, $2, 0, 1, true, $3, $4, $5) RETURNING item_id", &[&type_id, &system, &pos[0], &pos[1], &pos[2]]).await?).item_id) }
pub(crate) struct Loot { pub(crate) type_id: i32, pub(crate) average: f64 }
impl Loot { fn from_row(r: &Row) -> Self { Self { type_id: r.get("type_id"), average: r.get("average") } } }
pub(crate) async fn loot(ex: &(impl Executor + ?Sized), value: f64) -> Result<Vec<Loot>, Error> { Ok(ex.query("SELECT type_id, average FROM game.retail_prices WHERE average BETWEEN 1000 AND $1", &[&value]).await?.iter().map(Loot::from_row).collect()) }
pub(crate) async fn insert_loot(ex: &(impl Executor + ?Sized), type_id: i32, id: i64, quantity: i64) -> Result<(), Error> { ex.execute("INSERT INTO game.items (type_id, owner_id, location_id, flag_id, quantity) VALUES ($1, 1, $2, 0, $3)", &[&type_id, &id, &quantity]).await?; Ok(()) }
pub(crate) struct Container { pub(crate) item_id: i64, pub(crate) type_id: i32, pub(crate) pos: [f64; 3] }
impl Container { fn from_row(r: &Row) -> Self { Self { item_id: r.get("item_id"), type_id: r.get("type_id"), pos: [r.get("x"), r.get("y"), r.get("z")] } } }
pub(crate) async fn containers(ex: &(impl Executor + ?Sized), ids: &[i64]) -> Result<Vec<Container>, Error> { Ok(ex.query("SELECT item_id, type_id, x, y, z FROM game.items WHERE item_id = ANY($1) AND x IS NOT NULL", &[&ids]).await?.iter().map(Container::from_row).collect()) }
pub(crate) async fn set_container_owner(ex: &(impl Executor + ?Sized), container: i64, char_id: i64) -> Result<(), Error> { ex.execute("WITH changed AS (UPDATE game.items SET owner_id = $2 WHERE location_id = $1 RETURNING item_id, owner_id, singleton) DELETE FROM game.ship_skin_applications a USING changed c WHERE a.ship_item_id = c.item_id AND (a.character_id <> c.owner_id OR NOT c.singleton)", &[&container, &char_id]).await?; Ok(()) }
pub(crate) struct WormholeLocation { pub(crate) site_id: i64, pub(crate) system_id: i64 }
impl WormholeLocation { fn from_row(r: &Row) -> Self { Self { site_id: r.get("site_id"), system_id: r.get("system_id") } } }
pub(crate) async fn random_wormhole(ex: &(impl Executor + ?Sized)) -> Result<Option<WormholeLocation>, Error> { Ok(ex.query_opt("SELECT site_id, system_id FROM game.exploration_sites WHERE wh_type IS NOT NULL ORDER BY random() LIMIT 1", &[]).await?.as_ref().map(WormholeLocation::from_row)) }
