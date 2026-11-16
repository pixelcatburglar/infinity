//! Isolated database rows and actor balls for effect regressions.
use crate::{db::Db, space::{Ball, WarpState, MODE_STOP}};
#[cfg(feature = "tests-effects")] use crate::db::Error;

#[cfg(feature = "tests-effects")]
#[allow(clippy::too_many_arguments)]
pub(super) async fn item(db: &Db, type_id: i32, owner: i64, location: i64, flag: i32, quantity: i64, singleton: bool, online: bool) -> Result<i64, Error> {
    Ok(db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,$5,$6,$7) RETURNING item_id",
        &[&type_id, &owner, &location, &flag, &quantity, &singleton, &online]).await?.get("item_id"))
}

#[cfg(feature = "tests-effects")]
pub(super) async fn position(db: &Db, item: i64, system: i64, pos: [f64; 3]) -> Result<(), Error> {
    db.execute("UPDATE game.items SET location_id=$2,x=$3,y=$4,z=$5 WHERE item_id=$1", &[&item,&system,&pos[0],&pos[1],&pos[2]]).await?;
    Ok(())
}

#[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-ships", feature = "tests-slash"))]
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
#[cfg(feature = "tests-effects")]
pub(in crate::services::effects) async fn publish_fit(db: &Db, world: &std::sync::Arc<crate::space::World>, system: i64, owner: i64, ship: i64) {
    let fit = crate::services::fit::Fit::load_in_space(db, world, system, owner, ship).await.expect("load changed fit");
    assert!(crate::services::tests::fixture::publish_fitting(world, system, owner, ship, fit).await);
}

#[cfg(feature = "tests-effects")]
pub(in crate::services::effects) async fn capture_cycle(
    mut cycle: crate::services::effects::lifecycle::Cycle,
) -> crate::services::effects::lifecycle::Cycle {
    let world = cycle.cx.world.clone();
    let system = cycle.cx.system_id;
    world.with(system, move |s| {
        if cycle.source_membership.is_none() {
            cycle.source_membership = s.combat_membership_generation(cycle.cx.ship_id);
        }
        assert!(crate::services::effects::lifecycle::capture_cycle_inputs(s, &mut cycle)
            .expect("current resident cycle"));
        cycle
    }).await.expect("system actor stopped")
}

#[cfg(feature = "tests-effects")]
pub(in crate::services::effects) async fn set_cycle_online(
    cycle: &crate::services::effects::lifecycle::Cycle, account: i64, online: bool,
) {
    let cx = &cycle.cx;
    let ball = cx.world.ball(cx.system_id, cx.ship_id).await.expect("source ball");
    let identity = crate::services::identity::Identity { char_id: cx.char_id,
        ship_id: Some(cx.ship_id), corp_id: ball.corp_id, station_id: None,
        system_id: Some(cx.system_id) };
    crate::services::effects::online::set_online(&identity, &cx.db, &cx.world,
        cx.world.sde(), cx.notify.clone(), account, cx.ship_id, cycle.modules[0].id, online,
        |notification| crate::services::effects::online::dispatch_notification(
            notification, &cx.world, &cx.notify, Some(cx.char_id)))
        .await.expect("committed online mutation");
    cx.world.with(cx.system_id, |_| ()).await.expect("publication barrier");
}

#[cfg(any(feature = "tests-effects", feature = "tests-combat"))]
pub(in crate::services) async fn gm_value(
    client: &mut crate::client::Client, item: i64, attribute: i32, value: f64,
) {
    client.login.role = infinity_core::consts::GM_ROLES;
    let args = [marshal::Value::str(format!("/dogma {item} {attribute} {value}"))];
    let kwargs = marshal::Value::Dict(vec![]);
    let call = crate::services::Call { service: "slash", method: "SlashCmd",
        args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    crate::services::slash::dispatch(client, &call).await.expect("slash dispatch")
        .expect("committed dogma change");
}

#[cfg(any(feature = "tests-effects", feature = "tests-combat"))]
pub(in crate::services) fn due_fit(
    mut fit: crate::services::fit::Fit, sde: &crate::sde::Sde,
    read: impl Fn(&crate::services::fit::Fit) -> f64,
) -> (crate::services::fit::Fit, f64, f64) {
    use crate::services::fit::character::{BrainInput, CharacterInputs};
    use crate::dao::{items::brain::BrainItem, skills::expert_systems::ExpertSystem};
    use infinity_core::consts::{CATEGORY_IMPLANT, GROUP_BOOSTER, FLAG_BOOSTER};
    let empty = CharacterInputs { at: 1, ..Default::default() };
    assert_eq!(fit.character_inputs().at, 0, "requires a new cold evaluation");
    let boosters = sde.types.iter().filter(|ty| ty.category_id == Some(CATEGORY_IMPLANT)
        && ty.group_id == Some(GROUP_BOOSTER)).map(|ty| CharacterInputs { at: 1, brain: vec![BrainInput {
            item: BrainItem { item_id: -1, type_id: ty.type_id, flag_id: FLAG_BOOSTER,
                slot: None, activated_at: Some(1), expires_at: Some(2),
                effect_ids: Some(ty.effects().iter().map(|effect| effect.effect_id).collect()) },
            overrides: Vec::new(),
        }], ..Default::default() });
    let experts = sde.expert_systems.iter().map(|definition| CharacterInputs {
        at: 1, installations: vec![ExpertSystem { character_id: 0,
            type_id: definition.type_id, installed_at: 1, expires_at: 2,
        }], ..Default::default() });
    for inputs in boosters.chain(experts) {
        fit.apply_character_inputs(&inputs);
        let active = read(&fit);
        fit.apply_character_inputs(&empty);
        let expired = read(&fit);
        if active.is_finite() && expired.is_finite() && active > 0.0 && expired > 0.0
            && (active - expired).abs() > active.abs().max(expired.abs()) * 1e-8 {
            fit.apply_character_inputs(&inputs);
            assert_eq!(fit.next_character_expiry(), Some(2));
            assert_eq!(read(&fit), active);
            return (fit, active, expired);
        }
    }
    panic!("SDE has no deadline input affecting the selected real consumer");
}

#[cfg(feature = "tests-effects")]
static CYCLE_EVENTS: std::sync::Mutex<Vec<(i64, crate::lua::events::ModuleCycleOutcome)>> = std::sync::Mutex::new(Vec::new());
#[cfg(feature = "tests-effects")]
fn record_cycle(event: &crate::lua::events::GameEvent) {
    if let crate::lua::events::GameEvent::ModuleCycleEnded { module_item_id, outcome, .. } = event {
        CYCLE_EVENTS.lock().unwrap().push((*module_item_id, *outcome));
    }
}
#[cfg(feature = "tests-effects")]
pub(in crate::services) fn cycle_observer(db: crate::db::Db) -> crate::db::Db {
    assert!(db.progress_registry().durable().is_empty(), "isolated fixture registry");
    db.with_progress_registry(crate::progress::Registry::new(vec![record_cycle], vec![]).unwrap())
}
#[cfg(feature = "tests-effects")]
pub(in crate::services) fn completed_cycles(module: i64) -> usize {
    CYCLE_EVENTS.lock().unwrap().iter().filter(|(id, outcome)| *id == module
        && *outcome == crate::lua::events::ModuleCycleOutcome::Completed).count()
}
#[cfg(feature = "tests-effects")]
pub(in crate::services) fn clear_cycles(module: i64) {
    CYCLE_EVENTS.lock().unwrap().retain(|(id, _)| *id != module);
}


#[cfg(feature = "tests-effects")]
pub(super) async fn admit_npc(world: &std::sync::Arc<crate::space::World>, system: i64, ball: Ball) {
    let id = ball.id;
    world.add_ship(system, ball).await.expect("add transient target");
    assert!(world.with(system, move |s| s.add_npc(id, crate::space::NpcBrain::new(None,
        crate::space::NpcConfig { fly_range: 1.0, aggro_range: 0.0, rate_of_fire_ms: 1e9,
            scan_resolution: 1.0, repairs: (0.0, 0.0, 1000.0) },
        std::sync::Arc::new(|_, _| Some(Box::pin(async {})))))).await.expect("admit transient target"));
}


#[cfg(any(feature = "tests-effects", feature = "tests-combat", feature = "tests-ships", feature = "tests-slash"))]
pub(in crate::services) fn ball(id: i64, type_id: i32, owner: i64, pilot: Option<i64>, x: f64) -> Ball {
    Ball { id, type_id, owner_id: owner, corp_id: 1, alliance_id: -1, char_id: pilot,
        signature_radius: None, mode: MODE_STOP, radius: 1.0, mass: 1e6, max_vel: 0.0,
        agility: 1.0, speed_fraction: 0.0, pos: [x, 0.0, 0.0], vel: [0.0; 3], old_pos: [x, 0.0, 0.0],
        old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3], follow_id: 0,
        follow_range: 0.0, free: false, global: false, massive: false, interactive: false,
        warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }
}
