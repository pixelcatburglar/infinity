//! Disposable, transactionally-created fitting fixture and whole-game fingerprints.
//! The fixture uses real SDE types; it does not start a space actor or create banks.
use crate::{db::{Db, Error}, space::World};

#[derive(Clone, Copy)]
pub(super) struct Fixture { pub account: i64, pub owner: i64, pub ship: i64, pub modules: [i64; 2] }

pub(super) async fn fingerprint(db: &Db) -> Result<Vec<(String, String)>, Error> {
    let tables = db.query("SELECT tablename FROM pg_catalog.pg_tables WHERE schemaname = 'game' ORDER BY tablename", &[]).await?;
    let mut result = Vec::new();
    for row in tables {
        let name: String = row.get("tablename");
        let quoted = name.replace('"', "\"\"");
        let sql = format!("SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY row_to_json(t)::text), '')) AS fingerprint FROM game.\"{quoted}\" t");
        let hash = db.query_one(&sql, &[]).await?.get("fingerprint");
        result.push((name, hash));
    }
    Ok(result)
}

pub(super) async fn create(db: &Db, ship_type: i32, module_type: i32) -> Result<Fixture, Error> {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('fitting-fixture-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let owner = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'fitting-fixture-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id");
        let ship = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,1,0,1,true) RETURNING item_id", &[&ship_type,&owner]).await?.get("item_id");
        let mut modules = [0; 2];
        for id in &mut modules {
            *id = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,false) RETURNING item_id", &[&module_type,&owner,&ship,&crate::consts::FLAG_CARGO]).await?.get("item_id");
        }
        Ok(Fixture { account, owner, ship, modules })
    }), |f, ()| async move { Ok(f) }).await
}

pub(super) async fn cleanup(db: &Db, world: &World, f: Fixture) -> Result<(), Error> {
    let rows = db.query("SELECT item_id,type_id,location_id FROM game.items WHERE owner_id=$1 ORDER BY item_id", &[&f.owner]).await?;
    let ids: Vec<i64> = rows.iter().map(|r| r.get("item_id")).collect();
    assert!(ids.contains(&f.ship) && f.modules.iter().all(|id| ids.contains(id)));
    for row in &rows {
        let id: i64 = row.get("item_id");
        let ty: i32 = row.get("type_id");
        if world.sde().types.get(ty).is_some_and(|t| t.category_id == Some(crate::consts::CATEGORY_SHIP)) {
            world.remove_ball(row.get("location_id"), id).await;
            crate::services::combat::forget_ship(id);
        }
    }
    world.unobserve(f.owner);
    world.unregister_pilot(f.owner);
    let flush = world.flush_positions().await;
    let cleaned = db.transaction(|tx, _: &mut ()| {
        let ids = ids.clone();
        Box::pin(async move {
        tx.execute("DELETE FROM game.weapon_banks WHERE ship_id=ANY($1)", &[&ids]).await?;
        tx.execute("DELETE FROM game.module_charges WHERE ship_id=ANY($1)", &[&ids]).await?;
        tx.execute("DELETE FROM game.skill_queue WHERE character_id=$1", &[&f.owner]).await?;
        tx.execute("DELETE FROM game.skills WHERE character_id=$1", &[&f.owner]).await?;
        tx.execute("DELETE FROM game.item_attributes WHERE item_id=ANY($1)", &[&ids]).await?;
        assert_eq!(tx.execute("DELETE FROM game.items WHERE owner_id=$1 AND item_id=ANY($2)", &[&f.owner,&ids]).await?, ids.len() as u64);
        assert_eq!(tx.execute("DELETE FROM game.characters WHERE character_id=$1 AND user_id=$2", &[&f.owner,&f.account]).await?, 1);
        assert_eq!(tx.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&f.account]).await?, 1);
        Ok(())
        })
    }, |(), ()| async move { Ok(()) }).await;
    flush.expect("flush fixture positions");
    cleaned
}
