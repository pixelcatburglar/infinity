//! Isolated database rows and actor balls for effect regressions.
use crate::{db::{Db, Error}, space::{Ball, WarpState, MODE_STOP}};

#[derive(Clone, Copy)]
pub(in crate::services) struct Fixture { pub account: i64, pub character: i64, pub ship: i64, pub module: i64 }

pub(in crate::services) async fn create(db: &Db, ship_type: i32, module_type: i32, flag: i32) -> Result<Fixture, Error> {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('effects-fixture-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let character = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'effects-fixture-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id");
        let ship = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES ($1,$2,1,0,1,true,0,0,0) RETURNING item_id", &[&ship_type,&character]).await?.get("item_id");
        let module = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true) RETURNING item_id", &[&module_type,&character,&ship,&flag]).await?.get("item_id");
        Ok(Fixture { account, character, ship, module })
    }), |fixture, ()| async move { Ok(fixture) }).await
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn item(db: &Db, type_id: i32, owner: i64, location: i64, flag: i32, quantity: i64, singleton: bool, online: bool) -> Result<i64, Error> {
    Ok(db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING item_id",
        &[&type_id, &owner, &location, &flag, &quantity, &singleton, &online]).await?.get("item_id"))
}

pub(super) async fn position(db: &Db, item: i64, system: i64, pos: [f64; 3]) -> Result<(), Error> {
    db.execute("UPDATE game.items SET location_id=$2,x=$3,y=$4,z=$5 WHERE item_id=$1", &[&item,&system,&pos[0],&pos[1],&pos[2]]).await?;
    Ok(())
}

pub(in crate::services) async fn cleanup(db: &Db, fixture: Fixture) -> Result<(), Error> {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        tx.execute("DELETE FROM game.module_charges WHERE ship_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.item_attributes WHERE item_id IN (SELECT item_id FROM game.items WHERE owner_id=$1)", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.items WHERE owner_id=$1", &[&fixture.character]).await?;
        crate::services::tests::fixture::cleanup_progress(tx, &[fixture.account, fixture.character], &[fixture.character]).await?;
        tx.execute("DELETE FROM game.skill_history WHERE character_id=$1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.skill_queue WHERE character_id=$1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.skills WHERE character_id=$1", &[&fixture.character]).await?;
        tx.execute("DELETE FROM game.characters WHERE character_id=$1 AND user_id=$2", &[&fixture.character,&fixture.account]).await?;
        tx.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&fixture.account]).await?;
        Ok(())
    }), |(), ()| async move { Ok(()) }).await
}

pub(super) async fn fingerprint(db: &Db) -> Result<Vec<String>, Error> {
    let mut result = Vec::new();
    for sql in [
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY user_id), '')) AS fingerprint FROM game.accounts t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY character_id), '')) AS fingerprint FROM game.characters t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY item_id), '')) AS fingerprint FROM game.items t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY item_id, attribute_id), '')) AS fingerprint FROM game.item_attributes t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY ship_id, flag_id), '')) AS fingerprint FROM game.module_charges t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY character_id, type_id), '')) AS fingerprint FROM game.skills t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY history_id), '')) AS fingerprint FROM game.skill_history t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY character_id, position), '')) AS fingerprint FROM game.skill_queue t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY event_id), '')) AS fingerprint FROM game.progress_events t",
        "SELECT md5(COALESCE(string_agg(row_to_json(t)::text, '' ORDER BY consumer_key, event_id), '')) AS fingerprint FROM game.progress_deliveries t",
    ] { result.push(db.query_one(sql, &[]).await?.get("fingerprint")); }
    Ok(result)
}
pub(in crate::services) async fn admit(db: &Db, world: &std::sync::Arc<crate::space::World>, system: i64, ball: Ball) {
    let row = crate::dao::items::space_ships::FreeShip { item_id: ball.id, type_id: ball.type_id,
        owner_id: ball.owner_id, corporation_id: ball.corp_id, alliance_id: ball.alliance_id, pos: ball.pos };
    let mut prepared = match ball.char_id {
        Some(pilot) => crate::services::ship::balls::piloted(db, world.sde().clone(), &row, pilot, 0.0).await.expect("prepare pilot ship"),
        None => crate::services::ship::balls::unpiloted(db, world.sde().clone(), &row).await.expect("prepare free ship"),
    };
    prepared.ball = ball;
    world.system(system).await.expect("load system");
    world.with(system, move |s| prepared.install(s, |s, b| {
        if b.char_id.is_some() { s.add_ship(b); } else { s.add_persistent_ship(b); }
    })).await.expect("install prepared ship");
}
pub(super) async fn publish_fit(db: &Db, world: &std::sync::Arc<crate::space::World>, system: i64, owner: i64, ship: i64) {
    let (generation, revision) = world.with(system, move |s| Some((s.combat_membership_generation(ship)?, s.combat_fit_revision(ship)?)))
        .await.flatten().expect("resident fit generation");
    let fit = crate::services::fit::Fit::load_in_space(db, world, system, owner, ship).await.expect("load changed fit");
    let projection = fit.projection_revision();
    assert!(world.with(system, move |s| s.replace_fitted_cache(ship, generation, revision, fit,
        crate::services::fit::Key::Ship, projection)).await.expect("publish changed fit"));
}


pub(super) async fn admit_npc(world: &std::sync::Arc<crate::space::World>, system: i64, ball: Ball) {
    let id = ball.id;
    world.add_ship(system, ball).await.expect("add transient target");
    assert!(world.with(system, move |s| s.add_npc(id, crate::space::NpcBrain::new(None,
        crate::space::NpcConfig { fly_range: 1.0, aggro_range: 0.0, rate_of_fire_ms: 1e9,
            scan_resolution: 1.0, repairs: (0.0, 0.0, 1000.0) },
        std::sync::Arc::new(|_, _| Some(Box::pin(async {})))))).await.expect("admit transient target"));
}


pub(in crate::services) fn ball(id: i64, type_id: i32, owner: i64, pilot: Option<i64>, x: f64) -> Ball {
    Ball { id, type_id, owner_id: owner, corp_id: 1, alliance_id: -1, char_id: pilot,
        signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1e6, max_vel: 0.0,
        agility: 1.0, speed_fraction: 0.0, pos: [x, 0.0, 0.0], vel: [0.0; 3], old_pos: [x, 0.0, 0.0],
        old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3], follow_id: 0,
        follow_range: 0.0, free: false, global: false, massive: false, interactive: false,
        warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }
}
