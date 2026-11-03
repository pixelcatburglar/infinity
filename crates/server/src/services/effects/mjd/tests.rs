use super::*;

#[test]
fn absent_type_row_uses_client_distance_without_evaluating_fit() {
    assert_eq!(resolve_range(None, || panic!("fit must not be evaluated")), Some(100_000.0));
}

#[test]
fn present_type_row_uses_fitted_range() {
    assert_eq!(resolve_range(Some(120_000.0), || 150_000.0), Some(150_000.0));
    assert_eq!(resolve_range(Some(0.0), || 25_000.0), Some(25_000.0));
}

#[test]
fn present_type_row_rejects_invalid_fitted_range() {
    for value in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(resolve_range(Some(100_000.0), || value), None);
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL with SDE; run serially"]
async fn stale_generation_does_not_complete_jump_or_move_ship() {
    use crate::consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_HI_SLOT_FIRST, dogma::{ATTR_WARP_SCRAMBLE_STATUS, EFFECT_MICRO_JUMP_DRIVE}};
    use crate::services::combat::ActiveModule;
    use crate::services::effects::{lifecycle::{ActivationOwner, Module}, registry};
    use crate::space::{Ball, WarpState, World, MODE_STOP};
    use std::sync::atomic::AtomicBool;

    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let hull = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_SHIP)).unwrap().type_id;
    let module_type = sde.types.iter().find(|t| t.category_id == Some(CATEGORY_MODULE)
        && t.effects().iter().any(|e| e.effect_id == EFFECT_MICRO_JUMP_DRIVE)).unwrap().type_id;
    let effect = registry::resolve(&sde, EFFECT_MICRO_JUMP_DRIVE).unwrap();
    let (account, character, ship, module): (i64, i64, i64, i64) = db.transaction(
        |tx, _: &mut ()| Box::pin(async move {
            let account = tx.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('effects-fixture-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
            let character = tx.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'effects-fixture-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await?.get("character_id");
            let ship = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES ($1,$2,1,0,1,true,0,0,0) RETURNING item_id", &[&hull, &character]).await?.get("item_id");
            let module = tx.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true) RETURNING item_id", &[&module_type, &character, &ship, &FLAG_HI_SLOT_FIRST]).await?.get("item_id");
            Ok::<_, crate::db::Error>((account, character, ship, module))
        }), |fixture, ()| async move { Ok::<_, crate::db::Error>(fixture) },
    ).await.unwrap();
    let world = World::new(db.clone(), sde.clone(), 1);
    let test_world = world.clone();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        crate::dao::items::dogma::set_override(&test_db, module, ATTR_MJD_JUMP_RANGE, CLIENT_MICRO_JUMP_DISTANCE).await.unwrap();
        crate::dao::items::dogma::set_override(&test_db, ship, ATTR_WARP_SCRAMBLE_STATUS, 0.0).await.unwrap();
        let (notify, _) = tokio::sync::mpsc::unbounded_channel();
        test_world.register_pilot(character, notify.clone());
        test_world.add_ship(1, Ball { id: ship, type_id: hull, owner_id: character, corp_id: 1, alliance_id: -1,
            char_id: Some(character), signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1e6, max_vel: 0.0,
            agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3],
            old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3], follow_id: 0, follow_range: 0.0,
            free: false, global: false, massive: false, interactive: false, warp: WarpState::default(),
            warp_factor: 0, last_ms: 0, interpolation_anchor_ms: 0, effect_stamp: 0, cloak: 0,
            cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }).await.unwrap();
        let cx = Ctx { db: test_db.clone(), world: test_world.clone(), notify, char_id: character, ship_id: ship, system_id: 1 };
        let generation = Arc::new(AtomicBool::new(false));
        {
            let mut st = combat::state();
            st.active.insert(module, ActiveModule { char_id: character, stop: generation.clone(), forced_inactive: false });
            st.activation_generation.insert(module, generation.clone());
        }
        let mut cycle = Cycle { cx: cx.clone(), modules: vec![Module { id: module, type_id: module_type, flag: FLAG_HI_SLOT_FIRST }],
            effect, target: None, repeat: 1, stop: generation.clone(),
            owner: ActivationOwner { master: module, generation: generation.clone(), armed: true },
            started: 0, charge_type: None, cycles: 0, duration_ms: 1.0, nominal_start: 0, other: None, repair_amount: 0.0 };
        let fit = Fit::load(&test_db, sde, character, ship).await.ok().unwrap();
        assert_eq!(start(&cx, &fit, &mut cycle).await.ok().unwrap(), CycleOutcome::Continue);
        combat::state().active.get_mut(&module).unwrap().stop = Arc::new(AtomicBool::new(false));
        assert!(!generation.load(Ordering::Relaxed));
        assert_eq!(end(&cx, &fit, &mut cycle).await.ok().unwrap(), CycleOutcome::Stop("micro jump completed"));
        assert!(test_world.after(1, 1200).await);
        assert_eq!(test_world.ball(1, ship).await.unwrap().pos, [0.0; 3]);
    }).await;
    {
        let mut st = combat::state();
        st.active.remove(&module);
        st.activation_generation.remove(&module);
    }
    world.remove_ball(1, ship).await;
    combat::forget_ship(ship);
    world.unregister_pilot(character);
    let flush = world.flush_positions().await;
    drop(world);
    db.execute("DELETE FROM game.item_attributes WHERE item_id = ANY($1)", &[&&[module, ship][..]]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = $1 AND owner_id = $2 AND location_id = $3", &[&module, &character, &ship]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = $1 AND owner_id = $2", &[&ship, &character]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id = $1 AND user_id = $2", &[&character, &account]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
    flush.unwrap();
    result.unwrap();
}
