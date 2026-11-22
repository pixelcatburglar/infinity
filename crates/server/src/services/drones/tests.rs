#[cfg(any(feature = "tests-combat", feature = "tests-social", feature = "tests-economy", feature = "tests-effects", feature = "tests-ships", feature = "tests-space", feature = "tests-services", feature = "tests-slash"))] pub(crate) mod fixture;
#[cfg(feature = "tests-combat")] mod launch_combat;
#[cfg(any(feature = "tests-combat", feature = "tests-economy", feature = "tests-ships", feature = "tests-effects"))] pub(in crate::services) mod resources;
#[cfg(feature = "tests-combat")] pub(in crate::services) mod weapon_admission;
#[cfg(feature = "tests-combat")] mod handoff;
#[cfg(feature = "tests-combat")] mod salvage;
#[cfg(feature = "tests-combat")] mod ewar;

/// Salvage drone SDE selections and launch scaffolding shared by the combat
/// and effects salvage tests.
#[cfg(any(feature = "tests-combat", feature = "tests-effects"))]
pub(crate) mod shared {
    use anyhow::{anyhow, ensure, Context, Result};
    use marshal::Value;
    use infinity_core::consts::{*, dogma::*};
    use crate::{dao::items::creation, sde::Sde,
        services::{drones::worker, effects::{registry::{self, EffectAttribute}, tests::fixture as effect_fixture, EffectKind}, fit::{Fit, Key}}};
    use super::fixture::Fixture;

    /// A published drone whose spec picks the salvage effect, a wreck type
    /// carrying access difficulty, and a published Salvaged Materials type.
    pub(crate) fn salvage_types(sde: &std::sync::Arc<Sde>) -> (i32, i32, i32, i32, i32) {
        let (ty, duration, range) = sde.types.iter().find_map(|t| {
            if !(t.published == Some(true) && t.category_id == Some(CATEGORY_DRONE)
                && t.attribute(ATTR_DRONE_BANDWIDTH_USED).is_some_and(|v| v > 0.0)) { return None; }
            let effect = worker::spec(&Fit::for_space_type(sde.clone(), t.type_id, Vec::new()), Key::Ship, t.type_id)
                .ok().and_then(|spec| spec.effect_id).and_then(|id| registry::resolve(sde, id))
                .filter(|e| e.kind == EffectKind::Salvage)?;
            Some((t.type_id, effect.attribute(EffectAttribute::Duration).unwrap_or(ATTR_RATE_OF_FIRE),
                effect.attribute(EffectAttribute::Range).unwrap_or(ATTR_MAX_RANGE)))
        }).expect("published salvage drone");
        let wreck = sde.types.iter().find(|t| t.group_id == Some(GROUP_WRECK)
            && crate::services::effects::salvage::recovery::difficulty(sde, t.type_id).is_some())
            .expect("wreck with access difficulty").type_id;
        let material = sde.types.iter().find(|t| t.group_id == Some(GROUP_SALVAGED_MATERIALS) && t.published == Some(true)).expect("published material").type_id;
        (ty, duration, range, wreck, material)
    }

    pub(crate) async fn wreck(f: &Fixture, type_id: i32, owner: i64, x: f64) -> Result<i64> {
        let id = creation::positioned(&f.db, type_id, owner, f.system, [x, 0.0, 0.0]).await?;
        f.world.add_ship(f.system, effect_fixture::ball(id, type_id, owner, None, x)).await?;
        Ok(id)
    }

    /// A transient actor-owned wreck root with its own reserved ids and loot;
    /// nothing reaches `game.items`.
    #[cfg(feature = "tests-effects")]
    pub(crate) async fn transient_wreck(f: &Fixture, type_id: i32, owner: i64, x: f64, loot: Vec<(i32, i64)>) -> Result<i64> {
        let radius = f.sde.types.get(type_id).and_then(|t| t.radius).unwrap_or(100.0) as f32;
        f.world.with(f.system, move |s|
            s.npc_wreck_spawn(type_id, owner, 1, [x, 0.0, 0.0], radius, None, loot))
        .await.context("system stopped")?.context("reserved ids exhausted")
    }

    pub(crate) fn launch_args(id: i64) -> [Value; 3] { [Value::list([Value::tuple([Value::Int(id), Value::Int(1)])]), Value::None, Value::Bool(false)] }

    pub(crate) async fn launch_salvager(f: &mut Fixture, ty: i32, duration: i32, range: i32, bonus: f64, cycle_ms: f64) -> Result<i64> {
        let id = f.drone(ty, 1, true).await;
        f.set(f.ship, ATTR_DRONE_BANDWIDTH, 1_000_000.0).await;
        f.set(f.ship, ATTR_DRONE_CAPACITY, 1_000_000.0).await;
        for (attr, value) in [(ATTR_MAX_VELOCITY, 800.0), (ATTR_MAX_TARGET_RANGE, 1_000_000.0),
            (ATTR_SCAN_RESOLUTION, 1_000_000.0), (range, 500.0), (ATTR_ACCESS_DIFFICULTY_BONUS, bonus), (duration, cycle_ms)] {
            f.set(id, attr, value).await;
        }
        let response = f.call("ship", "LaunchDrones", &launch_args(id)).await.map_err(|e| anyhow!("launch: {e:?}"))?;
        ensure!(response == Value::Dict(vec![(Value::Int(id), Value::list([Value::Int(id)]))]), "launch refused: {response:?}");
        Ok(id)
    }

    pub(crate) async fn wait_until<T: Send + 'static>(f: &Fixture, what: &str, probe: impl Fn(&mut crate::space::System) -> Option<T> + Send + Sync + 'static) -> Result<T> {
        let probe = std::sync::Arc::new(probe);
        tokio::time::timeout(std::time::Duration::from_secs(90), async {
            loop {
                let probe = probe.clone();
                if let Some(value) = f.world.with(f.system, move |s| probe.as_ref()(s)).await.context("system stopped")? { return Ok::<_, anyhow::Error>(value); }
                ensure!(f.world.after(f.system, 100).await, "system stopped");
            }
        }).await.map_err(|_| anyhow!("{what} timed out"))?
    }
}
