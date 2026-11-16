//! Ship balls for selected and persistent unpiloted hulls. Destiny consumes the
//! resulting state through `eve/client/script/remote/michelle.py`. Type data
//! comes from the SDE and fittings and condition from game item dogma rows. The
//! slim item name is the persisted `game.items.name` from the same joined
//! appearance read as `dirtTime`; a NULL name falls back to the SDE type name
//! while an empty name is sent unchanged.
//! Prepared piloted and unpiloted hulls publish their condition only when installed.
//! Unpiloted hulls have no pilot or security status; every prepared hull carries
//! its persisted dirt timestamp and player-kill count on the slim item.
//! Piloted preparations also read the persistent GM NPC-ignore flag of the
//! piloting character and install it onto the actor's pilot entry.

use std::sync::Arc;
use infinity_core::consts::dogma::{ATTR_AGILITY, ATTR_MASS, ATTR_MAX_VELOCITY, ATTR_SIGNATURE_RADIUS};
use crate::dao::items::space_ships::FreeShip;
use crate::db::executor::Executor;
use crate::db::Db;
use crate::sde::Sde;
use crate::services::{combat, crimewatch, db_err, fit::{Fit, Key}, identity::Identity, CallError};
use crate::space::{self, now_ms, Ball, MODE_STOP};
use marshal::Value;

/// Server policy: ship radius in metres when the SDE has no radius.
const DEFAULT_SHIP_RADIUS: f64 = 50.0;

/// Collision radius used for both ship placement and its destiny ball.
pub(crate) fn radius(sde: &Sde, type_id: i32) -> Result<f32, CallError> {
    Ok(super::required(sde.types.get(type_id), "ship type")?.radius.unwrap_or(DEFAULT_SHIP_RADIUS) as f32)
}

pub(crate) struct PreparedShip {
    pub ball: Ball,
    /// Persisted GM NPC-ignore flag of the piloting character; false for unpiloted hulls.
    npc_ignored: bool,
    condition: combat::ConditionSeed,
    fit: Fit,
    skin: space::ship_cosmetics::PreparedSkin,
}

impl PreparedShip {
    pub(crate) fn install(self, actor: &mut space::System, add: impl FnOnce(&mut space::System, Ball)) {
        let (id, character, npc_ignored) = (self.ball.id, self.ball.char_id, self.npc_ignored);
        add(actor, self.ball);
        if let Some(character) = character { actor.install_pilot_npc_ignored(character, id, npc_ignored); }
        actor.install_condition(id, self.condition.into_condition(), self.fit);
        if let Some(token) = actor.capture_ball(id) { actor.install_skin(token, self.skin); }
    }

    pub(crate) fn valid_transfer(&self, snapshot: &space::combat::ActorConditionSnapshot) -> bool {
        self.ball.id == snapshot.item_id && snapshot.valid_with_fit(&self.fit, Key::Ship)
    }

    pub(crate) fn install_transfer(mut self, actor: &mut space::System,
        add: impl FnOnce(&mut space::System, Ball), snapshot: space::combat::ActorConditionSnapshot) -> bool {
        let id = self.ball.id;
        if actor.contains(id) || !self.valid_transfer(&snapshot) { return false; }
        self.ball.health = snapshot.fractions_with_fit(&self.fit, Key::Ship);
        let (character, npc_ignored) = (self.ball.char_id, self.npc_ignored);
        add(actor, self.ball);
        if let Some(character) = character { actor.install_pilot_npc_ignored(character, id, npc_ignored); }
        let installed = actor.persistent_generation(id).is_some_and(|generation|
            actor.transfer_combat_state(id, generation, snapshot, self.fit, Key::Ship));
        if installed {
            if let Some(token) = actor.capture_ball(id) { actor.install_skin(token, self.skin); }
        }
        installed
    }
}

pub(super) async fn selected(identity: &Identity, db: &Db, world: &space::World, sde: &Arc<Sde>,
    space_system: Option<i64>, alliance_id: Option<i64>, ship_id: i64, pos: [f64; 3]) -> Result<PreparedShip, CallError> {
    let char_id = identity.char_id;
    let fit = if let Some(system) = space_system {
        Fit::load_in_space(db, world, system, char_id, ship_id).await?
    } else {
        Fit::load(db, sde.clone(), char_id, ship_id).await?
    };
    let security = crimewatch::security_status(db, char_id).await?;
    let condition = combat::prepare_condition_with_fit(db, ship_id, &fit).await?;
    let appearance = super::appearance::prepare(db, sde, ship_id, crate::client::filetime_now()).await?;
    let ball = build(sde, ship_id, appearance.type_id, char_id, identity.corp_id,
        alliance_id.unwrap_or(-1), Some(char_id), pos, &fit, condition.fractions(), Some(security), appearance.name, appearance.dirt_timestamp, appearance.player_kills)?;
    let skin = skin_prepared(db, sde, ship_id, appearance.type_id, Some(char_id)).await?;
    let npc_ignored = crate::dao::characters::npc_control::get(db, char_id).await.map_err(db_err)?
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("selected character missing")))?;
    Ok(PreparedShip { ball, npc_ignored, condition, fit, skin })
}

/// Prepare an unpiloted persistent ship without publishing its condition.
pub(crate) async fn unpiloted(ex: &(impl Executor + ?Sized), sde: Arc<Sde>, ship: &FreeShip) -> Result<PreparedShip, CallError> {
    let fit = Fit::load_unpiloted(ex, sde.clone(), ship.item_id).await?;
    let condition = combat::prepare_condition_with_fit(ex, ship.item_id, &fit).await?;
    let appearance = super::appearance::prepare(ex, &sde, ship.item_id, crate::client::filetime_now()).await?;
    let ball = build(&sde, ship.item_id, ship.type_id, ship.owner_id, ship.corporation_id,
        ship.alliance_id, None, ship.pos, &fit, condition.fractions(), None, appearance.name, appearance.dirt_timestamp, appearance.player_kills)?;
    let skin = skin_prepared(ex, &sde, ship.item_id, ship.type_id, None).await?;
    Ok(PreparedShip { ball, npc_ignored: false, condition, fit, skin })
}

/// Prepare a piloted persistent ship without publishing its condition.
pub(crate) async fn piloted(ex: &(impl Executor + ?Sized), sde: Arc<Sde>, ship: &FreeShip, character: i64, security: f64) -> Result<PreparedShip, CallError> {
    let fit = Fit::load(ex, sde.clone(), character, ship.item_id).await?;
    let condition = combat::prepare_condition_with_fit(ex, ship.item_id, &fit).await?;
    let appearance = super::appearance::prepare(ex, &sde, ship.item_id, crate::client::filetime_now()).await?;
    let ball = build(&sde, ship.item_id, ship.type_id, ship.owner_id, ship.corporation_id,
        ship.alliance_id, Some(character), ship.pos, &fit, condition.fractions(), Some(security), appearance.name, appearance.dirt_timestamp, appearance.player_kills)?;
    let skin = skin_prepared(ex, &sde, ship.item_id, ship.type_id, Some(character)).await?;
    let npc_ignored = crate::dao::characters::npc_control::get(ex, character).await.map_err(db_err)?
        .ok_or_else(|| CallError::Internal(anyhow::anyhow!("piloted ship owner character missing")))?;
    Ok(PreparedShip { ball, npc_ignored, condition, fit, skin })
}
/// Prepare the ship's SKIN projection once for this admission: the effective
/// application and its license deadline from the game tables. A hull that has
/// no live application gets the plain no_skin projection.
async fn skin_prepared(ex: &(impl Executor + ?Sized), sde: &Sde, ship_id: i64, type_id: i32, character_id: Option<i64>) -> Result<space::ship_cosmetics::PreparedSkin, CallError> {
    let fact = space::ship_cosmetics::Ship { ship_id, type_id, character_id, is_npc: false };
    crate::services::ship_cosmetics_mgr::state::prepare(ex, sde, &[fact], crate::client::filetime_now()).await?
        .into_iter().next().ok_or_else(|| CallError::Internal(anyhow::anyhow!("missing prepared ship SKIN")))
}

fn build(sde: &Sde, id: i64, type_id: i32, owner_id: i64, corp_id: i64, alliance_id: i64,
    char_id: Option<i64>, pos: [f64; 3], fit: &Fit, health: [f64; 3], security: Option<f64>, name: Option<String>, dirt_timestamp: i64, player_kills: i64) -> Result<Ball, CallError> {
    if !pos.iter().all(|v| v.is_finite()) {
        return Err(CallError::Internal(anyhow::anyhow!("non-finite ship position")));
    }
    let r = super::required(sde.types.get(type_id), "ship type")?;
    let fitted = |attr: i32, base: f64| match fit.value(Key::Ship, attr) {
        v if v > 0.0 => v,
        _ => base,
    };
    let mass = fitted(ATTR_MASS, r.attribute(ATTR_MASS).or(r.mass).unwrap_or(1_000_000.0));
    let radius = radius(sde, type_id)?;
    Ok(Ball {
        id, type_id, owner_id, corp_id, alliance_id, char_id,
        mode: MODE_STOP,
        radius,
        signature_radius: Some(fitted(ATTR_SIGNATURE_RADIUS, radius as f64)),
        mass,
        max_vel: fitted(ATTR_MAX_VELOCITY, r.attribute(ATTR_MAX_VELOCITY).unwrap_or(200.0)) as f32,
        agility: fitted(ATTR_AGILITY, r.attribute(ATTR_AGILITY).unwrap_or(3.0)) as f32,
        speed_fraction: 0.0,
        pos,
        vel: [0.0; 3],
        old_pos: pos, old_vel: [0.0; 3], segment_acceleration: None,
        goto: [0.0; 3],
        follow_id: 0,
        follow_range: 0.0,
        free: true,
        global: false,
        massive: true,
        interactive: char_id.is_some(),
        warp: space::WarpState::default(),
        last_ms: now_ms(),
        interpolation_anchor_ms: 0,
        effect_stamp: 0,
        cloak: 0,
        cloak_until: 0,
        health,
        launcher: 0,
        slim: {
            let mut slim = Vec::with_capacity(3 + usize::from(security.is_some()));
            if let Some(v) = security { slim.push(("securityStatus", Value::Float(v))); }
            slim.push(("name", Value::unicode(name.or_else(|| r.name.clone()).unwrap_or_default())));
            slim.push(("dirtTime", super::appearance::dirt_timestamp_value(dirt_timestamp)));
            slim.push(("kills", Value::Int(player_kills)));
            slim
        },
    })
}

#[cfg(all(test, feature = "tests-ships"))] mod tests;
