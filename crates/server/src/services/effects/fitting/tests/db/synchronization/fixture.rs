//! Shared real-SDE movement fixture and owner notification observations.
use infinity_core::{
    consts::{CATEGORY_MODULE, CATEGORY_SHIP, FLAG_CARGO, FLAG_HANGAR,
        dogma::{ATTR_LOW_SLOTS, ATTR_MED_SLOTS, ATTR_CPU_OUTPUT, ATTR_POWER_OUTPUT}},
};
use crate::{
    client::Client,
    services::{self, CallError, fit::{Fit, Key}, inventory::{access::Container, movement}},
};
use marshal::Value;
use tokio::sync::mpsc::UnboundedReceiver;

/// SDE dogma effect category for modifiers active while a module is online.
const ONLINE_EFFECT_CATEGORY: i32 = 4;

pub(super) struct MovementFixture {
    pub client: Client,
    pub _peer: tokio::net::TcpStream,
    pub rows: super::super::fixture::Fixture,
    pub second_ship: i64,
    pub slot: i32,
    pub requires_online: bool,
    pub events: UnboundedReceiver<(String, Vec<Value>)>,
}

impl MovementFixture {
    pub async fn create(target: i32) -> Self {
        Self::create_for(target, false, false, false).await
    }
    pub async fn create_online_resource() -> Self {
        Self::create_for(ATTR_CPU_OUTPUT, true, false, false).await
    }
    pub async fn create_stat(target: i32) -> Self {
        Self::create_for(target, false, true, false).await
    }
    pub async fn create_passive(target: i32) -> Self {
        Self::create_for(target, false, false, true).await
    }
    async fn create_for(target: i32, resource: bool, stat: bool, passive: bool) -> Self {
        crate::tests::init_config();
        let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
        let sde = crate::sde::Sde::load(&db).await.unwrap();
        let shield = (!resource && !stat && !passive && target == infinity_core::consts::dogma::ATTR_SHIELD_CAPACITY)
            .then(|| crate::services::effects::fitting::tests::shield_extender(&sde));
        let meaningful = |op, v: f64| {
            if stat {
                match op {
                    services::fit::OP_MOD_ADD | services::fit::OP_MOD_SUB | services::fit::OP_POST_PERCENT | services::fit::OP_POST_PERCENT_UNNERFED => v != 0.0,
                    services::fit::OP_PRE_MUL | services::fit::OP_POST_MUL | services::fit::OP_PRE_DIV | services::fit::OP_POST_DIV => v > 0.0 && v != 1.0,
                    _ => false,
                }
            } else {
                match op {
                    services::fit::OP_MOD_ADD | services::fit::OP_POST_PERCENT | services::fit::OP_POST_PERCENT_UNNERFED => v > 0.0,
                    services::fit::OP_PRE_MUL | services::fit::OP_POST_MUL => v > 1.0,
                    _ => false,
                }
            }
        };
        let module = sde.types.iter().find(|ty| {
            if let Some((id, _, _)) = shield { return ty.type_id == id; }
            ty.published == Some(true) && ty.category_id == Some(CATEGORY_MODULE)
                && super::super::super::super::rack(&sde.types, ty.type_id).is_some_and(|(_, attr, _)|
                    if passive { attr == infinity_core::consts::dogma::ATTR_RIG_SLOTS } else { resource || (stat && matches!(attr, ATTR_LOW_SLOTS | ATTR_MED_SLOTS | infinity_core::consts::dogma::ATTR_HI_SLOTS)) || (!stat && attr == if target == infinity_core::consts::dogma::ATTR_ARMOR_HP { ATTR_LOW_SLOTS } else { ATTR_MED_SLOTS }) })
                && super::super::super::super::CAN_FIT_GROUPS.iter().chain(super::super::super::super::CAN_FIT_TYPES.iter()).all(|a| ty.attribute(*a).unwrap_or(0.0) == 0.0)
                && (!stat || !ty.effects().iter().any(|entry| [infinity_core::consts::dogma::EFFECT_TURRET_FITTED, infinity_core::consts::dogma::EFFECT_LAUNCHER_FITTED].contains(&entry.effect_id)))
                && (!stat || ty.effects().iter().any(|entry| i64::from(entry.effect_id) == infinity_core::consts::dogma::EFFECT_ONLINE))
                && (!passive || [super::super::super::super::MAX_GROUP_FITTED, super::super::super::super::MAX_TYPE_FITTED]
                    .iter().all(|a| ty.attribute(*a).unwrap_or(0.0) != 1.0))
                && (!resource || (ty.effects().iter().any(|entry| i64::from(entry.effect_id) == infinity_core::consts::dogma::EFFECT_ONLINE)
                    && [super::super::super::super::MAX_GROUP_FITTED, super::super::super::super::MAX_TYPE_FITTED,
                        super::super::super::super::MAX_GROUP_ONLINE].iter().all(|a| ty.attribute(*a).unwrap_or(0.0) != 1.0)))
                && (!resource || !ty.effects().iter().any(|entry| sde.dogma.effect(entry.effect_id).is_some_and(|effect|
                    effect.effect_category.unwrap_or(0) == 0 && effect.modifiers().iter().any(|m| m.domain == "shipID" && m.modified == target))))
                && (!passive || sde.types.iter().any(|hull| hull.published == Some(true) && hull.category_id == Some(CATEGORY_SHIP)
                    && hull.attribute(infinity_core::consts::dogma::ATTR_RIG_SLOTS).is_some_and(|v| v >= 2.0)
                    && hull.fit_attribute(infinity_core::consts::dogma::ATTR_CAPACITY).unwrap_or(0.0) >= ty.volume.unwrap_or(0.0) * 2.0
                    && hull.fit_attribute(target).is_some_and(|v| v > 0.0)
                    && (hull.fit_attribute(super::super::super::super::RIG_SIZE).unwrap_or(0.0) == 0.0
                        || hull.fit_attribute(super::super::super::super::RIG_SIZE) == ty.fit_attribute(super::super::super::super::RIG_SIZE))
                    && hull.fit_attribute(infinity_core::consts::dogma::ATTR_UPGRADE_CAPACITY).unwrap_or(0.0)
                        >= hull.fit_attribute(infinity_core::consts::dogma::ATTR_UPGRADE_LOAD).unwrap_or(0.0) + 2.0 * ty.fit_attribute(infinity_core::consts::dogma::ATTR_UPGRADE_COST).unwrap_or(0.0)))
                && ty.effects().iter().any(|entry| sde.dogma.effect(entry.effect_id).is_some_and(|effect|
                    (if resource { effect.effect_category == Some(ONLINE_EFFECT_CATEGORY) } else if passive { effect.effect_category.unwrap_or(0) == 0 } else {
                        effect.effect_category.unwrap_or(0) == 0 || (effect.effect_category == Some(ONLINE_EFFECT_CATEGORY)
                            && ty.effects().iter().any(|e| i64::from(e.effect_id) == infinity_core::consts::dogma::EFFECT_ONLINE))
                    }) && effect.modifiers().iter().any(|m| m.func == "ItemModifier" && m.domain == "shipID"
                            && m.modified == target && ty.attribute(m.modifying).is_some_and(|v| meaningful(m.op, v)))))
        }).expect("published unrestricted rack module with supported ship modifier in SDE");
        let requires_online = !module.effects().iter().any(|entry| sde.dogma.effect(entry.effect_id).is_some_and(|effect|
            effect.effect_category.unwrap_or(0) == 0 && effect.modifiers().iter().any(|m|
                m.func == "ItemModifier" && m.domain == "shipID" && m.modified == target
                    && module.attribute(m.modifying).is_some_and(|v| meaningful(m.op, v)))));
        let module_type = module.type_id;
        let (_, slots, slot) = super::super::super::super::rack(&sde.types, module_type).unwrap();
        let hull = sde.types.iter().find(|ty| ty.published == Some(true) && ty.category_id == Some(CATEGORY_SHIP)
            && ty.attribute(slots).is_some_and(|v| v >= 2.0)
            && ty.fit_attribute(infinity_core::consts::dogma::ATTR_CAPACITY).unwrap_or(0.0) >= module.volume.unwrap_or(0.0) * 2.0
            && ty.fit_attribute(target).is_some_and(|v| v > 0.0)
            && (!passive || ((ty.fit_attribute(super::super::super::super::RIG_SIZE).unwrap_or(0.0) == 0.0
                || ty.fit_attribute(super::super::super::super::RIG_SIZE) == module.fit_attribute(super::super::super::super::RIG_SIZE))
                && ty.fit_attribute(infinity_core::consts::dogma::ATTR_UPGRADE_CAPACITY).unwrap_or(0.0)
                    >= ty.fit_attribute(infinity_core::consts::dogma::ATTR_UPGRADE_LOAD).unwrap_or(0.0) + 2.0 * module.fit_attribute(infinity_core::consts::dogma::ATTR_UPGRADE_COST).unwrap_or(0.0))))
            .expect("published hull with two compatible slots and positive target attribute in SDE").type_id;
        let station = sde.map.stations().next().expect("station in SDE").station_id;
        let cleanup_world = crate::space::World::new(db.clone(), std::sync::Arc::new(sde), 1);
        let rows = super::super::fixture::create(&db, hull, module_type).await.unwrap();
        let setup_db = db.clone();
        let result = tokio::spawn(async move {
        let db = setup_db;
        db.execute("UPDATE game.items SET location_id=$2,flag_id=$3 WHERE item_id=$1", &[&rows.ship, &station, &FLAG_HANGAR]).await.unwrap();
        let second_ship = db.query_one("INSERT INTO game.items (type_id,owner_id,location_id,flag_id,quantity,singleton) VALUES ($1,$2,$3,$4,1,true) RETURNING item_id", &[&hull, &rows.owner, &station, &FLAG_HANGAR]).await.unwrap().get("item_id");
        let (mut client, peer) = services::tests::client(db, rows.owner).await;
        client.login.user_id = rows.account;
        client.login.role = infinity_core::consts::GM_ROLES;
        client.session.apply(vec![("stationid", Value::Int(station)), ("shipid", Value::Int(rows.ship))]);
        let (sender, events) = tokio::sync::mpsc::unbounded_channel();
        client.notify_tx = sender;
        let mut skills: Vec<_> = services::skills::requirements::requirements(&client.sde, module_type)
            .map(|(skill, _)| (skill, 5)).collect();
        for attr in infinity_core::consts::dogma::ATTR_REQUIRED_SKILLS {
            if let Some(skill) = client.sde.types.get(module_type).and_then(|ty| ty.attribute(attr)).filter(|v| *v > 0.0) {
                let skill = skill as i32;
                if !skills.iter().any(|&(id, _)| id == skill) { skills.push((skill, 5)); }
            }
        }
        services::skills::gm::apply(&mut client, rows.owner, services::skills::gm::Change::Set(skills)).await.ok().expect("seed required module skills");
        for ship in [rows.ship, second_ship] {
            for attr in [ATTR_CPU_OUTPUT, ATTR_POWER_OUTPUT] {
                assert!(crate::dao::items::dogma::set_override(&client.db, ship, attr, 1_000_000.0).await.unwrap());
            }
        }
        let mut f = Self { client, _peer: peer, rows, second_ship, slot, requires_online, events };
        f.drain();
        f
        }).await;
        match result {
            Ok(f) => f,
            Err(error) => {
                super::super::fixture::cleanup(&db, &cleanup_world, rows).await.expect("clean failed fixture setup");
                std::panic::resume_unwind(error.into_panic())
            }
        }
    }
    pub fn drain(&mut self) -> Vec<(String, Vec<Value>)> {
        let mut result = Vec::new();
        while let Ok(event) = self.events.try_recv() { result.push(event); }
        result
    }
    pub async fn fit(&self, ship: i64) -> Fit {
        Fit::load(&self.client.db, self.client.sde.clone(), self.rows.owner, ship).await.ok().expect("load final fit")
    }
    pub async fn value(&self, ship: i64, attribute: i32) -> f64 {
        self.fit(ship).await.value(Key::Ship, attribute)
    }
    pub async fn move_to(&self, ship: i64, source: i64, flag: i32) -> Result<Option<i64>, CallError> {
        self.move_module(ship, source, flag, self.rows.modules[0]).await
    }
    pub async fn move_module(&self, ship: i64, source: i64, flag: i32, module: i64) -> Result<Option<i64>, CallError> {
        let c = &self.client;
        movement::move_item(&services::identity::Identity::of_client(c), &c.db, &c.world, &c.sde,
            c.session.int("solarsystemid"), c.notify_tx.clone(), &Container::Item(ship), module, Some(source), Some(flag), None,
            |n| services::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await
    }
    pub async fn online(&self, ship: i64) {
        self.set_online(ship, self.rows.modules[0], true).await.ok().expect("online SDE extender");
    }
    pub async fn set_online(&self, ship: i64, module: i64, online: bool) -> services::CallResult {
        let c = &self.client;
        services::effects::online::set_online(&services::identity::Identity::of_client(c), &c.db, &c.world, &c.sde,
            c.notify_tx.clone(), c.login.user_id, ship, module, online,
            |n| services::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await
    }
}

pub(super) fn attribute_rows(events: &[(String, Vec<Value>)], owner: i64, ship: i64, attr: i32) -> Vec<(f64, f64)> {
    let mut result = Vec::new();
    for (name, args) in events {
        if name != "OnModuleAttributeChanges" { continue; }
        let [Value::List(changes)] = args.as_slice() else { continue };
        for value in changes {
            let Value::Tuple(row) = value else { continue };
            if row.get(1) == Some(&Value::Int(owner)) && row.get(2) == Some(&Value::Int(ship)) && row.get(3) == Some(&Value::Int(i64::from(attr))) {
                if let (Some(Value::Float(new)), Some(Value::Float(old))) = (row.get(5), row.get(6)) { result.push((*old, *new)); }
            }
        }
    }
    result
}

pub(super) const CARGO: i32 = FLAG_CARGO;
