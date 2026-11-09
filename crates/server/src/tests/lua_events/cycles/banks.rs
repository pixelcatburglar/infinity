//! Real weapon-bank cycle events through dogmaIM and the shared Lua observer.
//! The fixtures use SDE dogma and game items; client behavior follows dogmaIM.
//! Mining does not expand weapon banks and is covered separately.

use std::{sync::{Arc, Mutex}, time::Duration};
use marshal::Value;
use infinity_core::{consts::{CATEGORY_CHARGE, CATEGORY_MODULE, CATEGORY_SHIP, FLAG_HI_SLOT_FIRST, GROUPABLE_GROUP_IDS, dogma::{ATTR_CAPACITOR_CAPACITY, ATTR_RECHARGE_RATE, ATTR_SCAN_RESOLUTION}}};
use crate::{client::Client, dao::items::{charges, dogma}, services::{self, combat, effects::{registry::{self, EffectAttribute}, EffectKind}, Call}, space::{Ball, WarpState, MODE_STOP}};

/// SDE type Thermodynamics, used by dogmaIM overload admission.
const THERMODYNAMICS: i32 = 28164;

#[derive(Default)]
struct Keys { account: Option<i64>, character: Option<i64>, ship: Option<i64>, target: Option<i64>, guns: Vec<i64> }

fn ball(id: i64, type_id: i32, owner: i64, pilot: Option<i64>) -> Ball {
    Ball { id, type_id, owner_id: owner, corp_id: 1, alliance_id: -1, char_id: pilot,
        signature_radius: None, mode: MODE_STOP, radius: 100.0, mass: 1e6, max_vel: 0.0,
        agility: 1.0, speed_fraction: 0.0, pos: [0.0; 3], vel: [0.0; 3], old_pos: [0.0; 3],
        old_vel: [0.0; 3], segment_acceleration: None, goto: [0.0; 3], follow_id: 0,
        follow_range: 0.0, free: false, global: false, massive: false, interactive: false,
        warp: WarpState::default(), last_ms: 0, interpolation_anchor_ms: 0,
        effect_stamp: 0, cloak: 0, cloak_until: 0, health: [1.0; 3], launcher: 0, slim: Vec::new() }
}

async fn call(client: &mut Client, method: &str, args: &[Value]) {
    let kwargs = Value::Dict(vec![]);
    let call = Call { service: "dogmaIM", method, args, kwargs: &kwargs, bound: false, bind_params: None };
    services::dispatch(client, &call).await.unwrap_or_else(|error| panic!("dogmaIM {method} failed: {error:?}"));
}

pub(super) async fn run(ctx: &super::super::Context) {
    let cursor = ctx.events().await.len();
    let keys = Arc::new(Mutex::new(Keys::default()));
    let saved = keys.clone();
    let r = ctx.resources.clone();
    let result = tokio::spawn(async move {
        let sde = &r.sde;
        let size = sde.dogma.attributes_by_name("chargeSize").next().map(|a| a.attribute_id);
        let (gun, ammo, effect_name, effect_id, overload, duration) = sde.types.iter()
            .filter(|m| m.published == Some(true) && m.category_id == Some(CATEGORY_MODULE)
                && m.group_id.is_some_and(|g| GROUPABLE_GROUP_IDS.contains(&i64::from(g))))
            .find_map(|m| {
                let (effect, name) = m.effects().iter().find_map(|e| {
                    let effect = registry::resolve(sde, e.effect_id)?;
                    let row = sde.dogma.effect(e.effect_id)?;
                    let name = row.name.as_ref()?;
                    (effect.kind == EffectKind::Turret && effect.guid.as_deref().is_some_and(|g| !g.is_empty())
                        && sde.dogma.effects_by_name(name).count() == 1).then_some((effect, name.clone()))
                })?;
                let duration = m.attribute(effect.attribute(EffectAttribute::Duration)?)?;
                if !duration.is_finite() || duration < 5_000.0 { return None; }
                let overload = m.effects().iter().find_map(|e| sde.dogma.effect(e.effect_id)
                    .filter(|e| e.effect_category == Some(crate::services::effects::heat::rpc::OVERLOAD_CATEGORY)).map(|_| e.effect_id))?;
                let groups: Vec<_> = ["chargeGroup1", "chargeGroup2", "chargeGroup3", "chargeGroup4", "chargeGroup5"].iter()
                    .flat_map(|name| sde.dogma.attributes_by_name(name)).filter_map(|a| m.attribute(a.attribute_id)).collect();
                let module_size = size.and_then(|id| m.attribute(id));
                let capacity = m.capacity.or_else(|| m.attribute(infinity_core::consts::dogma::ATTR_CAPACITY))?;
                let charge = sde.types.iter().find(|c| c.published == Some(true) && c.category_id == Some(CATEGORY_CHARGE)
                    && c.group_id.is_some_and(|g| groups.contains(&f64::from(g)))
                    && (module_size.is_none_or(|v| v <= 0.0) || size.and_then(|id| c.attribute(id)) == module_size)
                    && c.volume.is_some_and(|v| v.is_finite() && v > 0.0 && capacity >= 3.0 * v))?;
                Some((m.type_id, charge.type_id, name, effect.id, overload, duration as u64))
            }).expect("groupable overloadable SDE turret with compatible ammunition and long cycle");
        let heat_capacity = sde.dogma.attributes_by_name("heatCapacityHi").next().expect("high rack heat capacity").attribute_id;
        let ship_type = sde.types.iter().find(|t| t.published == Some(true) && t.category_id == Some(CATEGORY_SHIP)
            && [ATTR_CAPACITOR_CAPACITY, ATTR_RECHARGE_RATE, ATTR_SCAN_RESOLUTION, heat_capacity].iter()
                .all(|a| t.attribute(*a).is_some_and(|v| v.is_finite() && v > 0.0))).expect("heat-capable ship").type_id;
        let db = &r.db;
        let account: i64 = db.query_one("INSERT INTO game.accounts (user_name,password_hash,role) VALUES ('event-bank-' || nextval('game.accounts_user_id_seq')::text,$1,0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
        saved.lock().unwrap().account = Some(account);
        let character: i64 = db.query_one("INSERT INTO game.characters (user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,'event-bank-' || nextval('game.characters_character_id_seq')::text,1,1,1,1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
        saved.lock().unwrap().character = Some(character);
        let rank = sde.types.get(THERMODYNAMICS)
            .and_then(|t| t.fit_attribute(infinity_core::consts::dogma::ATTR_SKILL_TIME_CONSTANT))
            .expect("Thermodynamics rank");
        let (mut low, mut high) = (0_i64, 1_i64 << 40);
        while low < high {
            let mid = low + (high - low) / 2;
            if crate::services::skills::formulas::level_from_points(mid, rank) < 5 { low = mid + 1; } else { high = mid; }
        }
        assert_eq!(crate::services::skills::formulas::level_from_points(low, rank), 5);
        crate::dao::skills::training::set_level(db, character, THERMODYNAMICS, 5, low).await.unwrap();
        let ship: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES ($1,$2,1,0,1,true,0,0,0) RETURNING item_id", &[&ship_type, &character]).await.unwrap().get("item_id");
        saved.lock().unwrap().ship = Some(ship);
        let target: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,x,y,z) VALUES ($1,$2,1,0,1,true,0,0,0) RETURNING item_id", &[&ship_type, &character]).await.unwrap().get("item_id");
        saved.lock().unwrap().target = Some(target);
        let mut guns = Vec::new();
        for i in 0..3 {
            let flag = FLAG_HI_SLOT_FIRST + i;
            let id: i64 = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton,online) VALUES ($1,$2,$3,$4,1,true,true) RETURNING item_id", &[&gun, &character, &ship, &flag]).await.unwrap().get("item_id");
            saved.lock().unwrap().guns.push(id);
            guns.push(id);
            db.execute("INSERT INTO game.module_charges (ship_id,flag_id,type_id,quantity) VALUES ($1,$2,$3,3)", &[&ship, &flag, &ammo]).await.unwrap();
        }
        dogma::set_override(db, ship, ATTR_CAPACITOR_CAPACITY, 1_000_000.0).await.unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
        let (stream, address) = listener.accept().await.unwrap();
        let cfg = Arc::new(crate::config::Config::default());
        let hub = crate::chat::Hub::new(&cfg.chat.domain, db.clone(), r.world.clone());
        let login = crate::handshake::Login { user_id: account, user_name: "event-bank".into(), role: 0, client_id: character, session_id: 1, language_id: "EN".into() };
        let mut client = Client::new(crate::conn::Conn::new(stream, address), cfg, db.clone(), sde.clone(), hub, r.world.clone(), login);
        client.session.apply(vec![("charid", Value::Int(character)), ("shipid", Value::Int(ship)), ("solarsystemid", Value::Int(1))]);
        r.world.register_pilot(character, client.notify_tx.clone());
        let row = crate::dao::items::space_ships::FreeShip { item_id: ship, type_id: ship_type, owner_id: character, corporation_id: 1, alliance_id: -1, pos: [0.0; 3] };
        let mut prepared = crate::services::ship::balls::piloted(db, sde.clone(), &row, character, 0.0).await.unwrap();
        prepared.ball = ball(ship, ship_type, character, Some(character));
        r.world.system(1).await.unwrap();
        r.world.with(1, move |s| prepared.install(s, |s, ball| s.add_ship(ball))).await.unwrap();
        let row = crate::dao::items::space_ships::FreeShip { item_id: target, type_id: ship_type, owner_id: character, corporation_id: 1, alliance_id: -1, pos: [0.0; 3] };
        let mut prepared = crate::services::ship::balls::unpiloted(db, sde.clone(), &row).await.unwrap();
        prepared.ball = ball(target, ship_type, character, None);
        r.world.with(1, move |s| prepared.install(s, |s, ball| s.add_persistent_ship(ball))).await.unwrap();
        call(&mut client, "LinkWeapons", &[Value::Int(ship), Value::Int(guns[0]), Value::Int(guns[1])]).await;
        call(&mut client, "LinkWeapons", &[Value::Int(ship), Value::Int(guns[0]), Value::Int(guns[2])]).await;
        call(&mut client, "AddTarget", &[Value::Int(target)]).await;
        tokio::time::timeout(Duration::from_secs(90), async {
            while !combat::targeting_charges::targets_of(character).contains(&target) { assert!(r.world.after(1, 100).await); }
        }).await.expect("target lock");
        call(&mut client, "Activate", &[Value::Int(guns[1]), Value::str(&effect_name), Value::Int(target), Value::Int(2)]).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let mut ready = true;
                for i in 0..3 { ready &= combat::targeting_charges::loaded(db, ship, FLAG_HI_SLOT_FIRST + i).await.unwrap() == Some((ammo, 2)); }
                if ready { break; }
                assert!(r.world.after(1, 100).await);
            }
        }).await.expect("bank volley");
        call(&mut client, "UnlinkModule", &[Value::Int(ship), Value::Int(guns[0])]).await;
        assert_eq!(charges::banks(db, ship).await.unwrap().len(), 1);
        super::idle(&r.world, 1, guns[0], duration + 15_000).await;
        call(&mut client, "UnlinkAllModules", &[Value::Int(ship)]).await;
        call(&mut client, "Overload", &[Value::Int(guns[0]), Value::Int(i64::from(overload))]).await;
        assert!(crate::services::effects::heat::overloaded_modules().contains(&guns[0]));
        // The keyed trigger changes only this fixture hull in the real charge transaction.
        // Begin has already loaded Fit; end captures the real heat source before apply.
        let name = format!("event_bank_fault_{ship}");
        db.execute(&format!("CREATE FUNCTION game.{name}() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.ship_id = {ship} AND NEW.flag_id = {} THEN UPDATE game.items SET location_id = 0 WHERE item_id = {ship}; END IF; RETURN NEW; END $$", FLAG_HI_SLOT_FIRST), &[]).await.unwrap();
        db.execute(&format!("CREATE TRIGGER {name} AFTER UPDATE OF quantity ON game.module_charges FOR EACH ROW EXECUTE FUNCTION game.{name}()"), &[]).await.unwrap();
        call(&mut client, "Activate", &[Value::Int(guns[0]), Value::str(&effect_name), Value::Int(target), Value::Int(2)]).await;
        super::idle(&r.world, 1, guns[0], duration * 2 + 15_000).await;
        assert_eq!(db.query_one("SELECT location_id FROM game.items WHERE item_id=$1", &[&ship]).await.unwrap().get::<_, i64>("location_id"), 0);
        assert_eq!(combat::targeting_charges::loaded(db, ship, FLAG_HI_SLOT_FIRST).await.unwrap(), Some((ammo, 1)));
        (character, guns, effect_id)
    }).await;
    let (account, character, ship, target, guns) = { let k = keys.lock().unwrap(); (k.account, k.character, k.ship, k.target, k.guns.clone()) };
    let db = &ctx.resources.db;
    if let Some(character) = character {
        let owners: Vec<i64> = account.into_iter().chain(std::iter::once(character)).collect();
        db.transaction(|tx, _: &mut ()| {
            let owners = owners.clone();
            Box::pin(async move { crate::services::tests::fixture::cleanup_progress(tx, &owners, &[character]).await })
        }, |(), ()| async move { Ok(()) }).await.unwrap();
    }
    if let Some(ship) = ship {
        let name = format!("event_bank_fault_{ship}");
        db.execute(&format!("DROP TRIGGER IF EXISTS {name} ON game.module_charges"), &[]).await.unwrap();
        db.execute(&format!("DROP FUNCTION IF EXISTS game.{name}()"), &[]).await.unwrap();
        db.execute("UPDATE game.items SET location_id=1 WHERE item_id=$1", &[&ship]).await.unwrap();
        if let Some(character) = character {
            combat::reset_pilot(&ctx.resources.world, character, Some((1, ship)));
            ctx.resources.world.unobserve(character);
            ctx.resources.world.unregister_pilot_generation(character, None);
        }
        ctx.resources.world.remove_ball(1, ship).await;
        if let Some(target) = target { ctx.resources.world.remove_ball(1, target).await; combat::forget_ship(target); }
        combat::forget_ship(ship);
        db.execute("DELETE FROM game.weapon_banks WHERE ship_id=$1", &[&ship]).await.unwrap();
        db.execute("DELETE FROM game.module_charges WHERE ship_id=$1", &[&ship]).await.unwrap();
    }
    let mut items = guns.clone();
    if let Some(ship) = ship { items.push(ship); }
    if let Some(target) = target { items.push(target); }
    db.execute("DELETE FROM game.item_attributes WHERE item_id=ANY($1)", &[&items]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id=ANY($1)", &[&items]).await.unwrap();
    if let Some(character) = character {
        db.execute("DELETE FROM game.skills WHERE character_id=$1", &[&character]).await.unwrap();
        db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&character]).await.unwrap();
    }
    if let Some(account) = account { db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap(); }
    let (character, guns, effect) = result.unwrap();
    let events = ctx.events().await;
    let actual: Vec<_> = events[cursor..].iter().filter(|e| e["kind"] == "ModuleCycleEnded" && e["char_id"] == character).collect();
    assert_eq!(actual.len(), 4, "{actual:?}");
    for (event, module) in actual.iter().zip([guns[0], guns[1], guns[2], guns[0]]) {
        assert_eq!(event["module_item_id"], module);
        assert_eq!(event["effect_id"], effect);
        assert_eq!(event["outcome"], "completed");
    }
}
