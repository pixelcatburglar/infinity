//! Ship balls for selected and persistent unpiloted hulls. Destiny consumes the
//! resulting state through `eve/client/script/remote/michelle.py`. Type data
//! comes from the SDE and fittings and condition from game item dogma rows.
//! Prepared piloted and unpiloted hulls publish their condition only when installed.
//! Unpiloted hulls have no pilot or security status.

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
    condition: combat::ConditionSeed,
    fit: Fit,
}

impl PreparedShip {
    pub(crate) fn install(self, actor: &mut space::System, add: impl FnOnce(&mut space::System, Ball)) {
        let id = self.ball.id;
        add(actor, self.ball);
        actor.install_condition(id, self.condition.into_condition(), self.fit);
    }

    pub(crate) fn valid_transfer(&self, snapshot: &space::combat::ActorConditionSnapshot) -> bool {
        self.ball.id == snapshot.item_id && snapshot.valid_with_fit(&self.fit, Key::Ship)
    }

    pub(crate) fn install_transfer(mut self, actor: &mut space::System,
        add: impl FnOnce(&mut space::System, Ball), snapshot: space::combat::ActorConditionSnapshot) -> bool {
        let id = self.ball.id;
        if actor.contains(id) || !self.valid_transfer(&snapshot) { return false; }
        self.ball.health = snapshot.fractions_with_fit(&self.fit, Key::Ship);
        add(actor, self.ball);
        actor.persistent_generation(id).is_some_and(|generation|
            actor.transfer_combat_state(id, generation, snapshot, self.fit, Key::Ship))
    }
}

pub(super) async fn selected(identity: &Identity, db: &Db, world: &space::World, sde: &Arc<Sde>,
    space_system: Option<i64>, alliance_id: Option<i64>, ship_id: i64, pos: [f64; 3]) -> Result<PreparedShip, CallError> {
    let type_id = super::required(crate::dao::items::inventory::type_id(db, ship_id).await.map_err(db_err)?, "ship item")?;
    let char_id = identity.char_id;
    let fit = if let Some(system) = space_system {
        Fit::load_in_space(db, world, system, char_id, ship_id).await?
    } else {
        Fit::load(db, sde.clone(), char_id, ship_id).await?
    };
    let security = crimewatch::security_status(db, char_id).await?;
    let condition = combat::prepare_condition_with_fit(db, ship_id, &fit).await?;
    let ball = build(sde, ship_id, type_id, char_id, identity.corp_id,
        alliance_id.unwrap_or(-1), Some(char_id), pos, &fit, condition.fractions(), Some(security))?;
    Ok(PreparedShip { ball, condition, fit })
}

/// Prepare an unpiloted persistent ship without publishing its condition.
pub(crate) async fn unpiloted(ex: &(impl Executor + ?Sized), sde: Arc<Sde>, ship: &FreeShip) -> Result<PreparedShip, CallError> {
    let fit = Fit::load_unpiloted(ex, sde.clone(), ship.item_id).await?;
    let condition = combat::prepare_condition_with_fit(ex, ship.item_id, &fit).await?;
    let ball = build(&sde, ship.item_id, ship.type_id, ship.owner_id, ship.corporation_id,
        ship.alliance_id, None, ship.pos, &fit, condition.fractions(), None)?;
    Ok(PreparedShip { ball, condition, fit })
}

/// Prepare a piloted persistent ship without publishing its condition.
pub(crate) async fn piloted(ex: &(impl Executor + ?Sized), sde: Arc<Sde>, ship: &FreeShip, character: i64, security: f64) -> Result<PreparedShip, CallError> {
    let fit = Fit::load(ex, sde.clone(), character, ship.item_id).await?;
    let condition = combat::prepare_condition_with_fit(ex, ship.item_id, &fit).await?;
    let ball = build(&sde, ship.item_id, ship.type_id, ship.owner_id, ship.corporation_id,
        ship.alliance_id, Some(character), ship.pos, &fit, condition.fractions(), Some(security))?;
    Ok(PreparedShip { ball, condition, fit })
}

fn build(sde: &Sde, id: i64, type_id: i32, owner_id: i64, corp_id: i64, alliance_id: i64,
    char_id: Option<i64>, pos: [f64; 3], fit: &Fit, health: [f64; 3], security: Option<f64>) -> Result<Ball, CallError> {
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
        slim: security.map(|v| vec![("securityStatus", Value::Float(v))]).unwrap_or_default(),
    })
}

#[cfg(test)] mod tests;
