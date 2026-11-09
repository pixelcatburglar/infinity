//! Transaction-owned deployment of the inventory Hub into sovereign space.
//! The component definition comes from the SDE export and placement is sampled
//! from the system actor; actor publication follows commit. Fleet, suspect and
//! dungeon deployment gates are not implemented and enabled gates fail closed.
//! `OnItemChange` stack quantities are projected to the client display limit; the
//! deployed and stored quantities stay exact.
use infinity_core::{
    consts::{CATEGORY_SHIP, FLAG_CARGO, GROUP_CONTROL_TOWER, GROUP_STARGATE, GROUP_STATION, GROUP_SOVEREIGNTY_HUB, GROUP_SUN, IX_FLAG, IX_LOCATION_ID, IX_OWNER_ID, IX_SINGLETON, IX_STACK_SIZE, KNOWN_SPACE_SYSTEM_START, OWNER_UNOWNED, WORMHOLE_SYSTEM_START},
    messages,
};
use crate::{
    client::filetime_now,
    dao::{characters::ship, corporations::registry, items::{inventory, loading::ItemQuery, sovereignty as item_sov}, sovereignty::{capture::{self, CaptureRow}, history::{self, HistoryRow}, structures::{self, HubRow, StructureRow}, systems::{self, SystemChange}}},
    db::outbox::{dispatch_world, Outbox},
    lua,
    services::{db_err, identity::Identity, inventory::stacks::split_stack, items::load_items, market, notify_error, CallError},
    space::{self, V3},
};
use marshal::Value;
use super::{CommittedPublication, HubPublication, PlacementProof, Resources, TransitionKind};

#[derive(Clone, Copy)]
pub(crate) struct DeployRequest { pub source_item_id: i64, pub quantity: i32, pub whose_behalf: Option<i64> }
#[derive(Clone, Copy, Debug)]
pub(crate) struct DeployResult { pub item_id: i64 }

fn refused() -> CallError { notify_error(messages::ITEM_NOT_AT_ORIGINAL_LOCATION) }

/// Capture a position once, at acceptance; subsequent ship movement does not revoke it.
pub(super) fn position(proof: &PlacementProof, rules: &crate::sde::sovereignty::DeployDefinition) -> Option<V3> {
    if rules.fixed_star_distance_m <= 0 { return None; }
    let mut stars = proof.obstacles.iter().filter(|o| o.group_id == GROUP_SUN);
    let star = stars.next()?;
    if stars.next().is_some() { return None; }
    let direction = space::sub(proof.position, star.position);
    let length = space::dist(proof.position, star.position);
    if !length.is_finite() || length <= 0.0 { return None; }
    let result = space::add(star.position, space::scale(direction, rules.fixed_star_distance_m as f64 / length));
    if !result.iter().all(|v| v.is_finite()) { return None; }
    for obstacle in &proof.obstacles {
        let limit = match obstacle.group_id {
            GROUP_SOVEREIGNTY_HUB => rules.min_distance_from_own_group_m,
            GROUP_CONTROL_TOWER => rules.min_distance_from_control_tower_m,
            GROUP_STARGATE => rules.min_distance_from_stargates_and_stations_m,
            group if i64::from(group) == GROUP_STATION => rules.min_distance_from_stargates_and_stations_m,
            _ => continue,
        };
        let distance = space::dist(result, obstacle.position);
        if !distance.is_finite() || distance < limit { return None; }
    }
    Some(result)
}

pub(crate) async fn deploy(resources: Resources, identity: Identity, request: DeployRequest) -> Result<DeployResult, CallError> {
    tokio::spawn(async move { deploy_owned(resources, identity, request).await }).await
        .map_err(|e| db_err(anyhow::anyhow!("Hub deployment task failed: {e}")))?
}

async fn deploy_owned(resources: Resources, identity: Identity, request: DeployRequest) -> Result<DeployResult, CallError> {
    let character = identity.char_id;
    if character <= 0 || request.source_item_id <= 0 || request.quantity != 1 || request.whose_behalf.is_some_and(|id| id != character) { return Err(refused()); }
    let definition = resources.sde.sovereignty.hub();
    let rules = &definition.deploy;
    // ponytail: these optional component gates have no producer; reject definitions
    // enabling them until their authoritative world state is supplied.
    if rules.requires_fleet || rules.launch_for_own_corp || rules.disallow_if_cyno_blocked || rules.disallow_in_warp_disrupted_dungeon || rules.flag_as_suspect_when_deployed
        || rules.min_distance_from_ess_m.is_some() || rules.min_distance_from_moon_beacon_m.is_some() || rules.min_distance_from_wormhole_m.is_some() || rules.max_distance_from_control_tower_m.is_some() || rules.max_per_solar_system != 1 { return Err(refused()); }
    let current = ship::current(&resources.db, character).await.map_err(db_err)?.ok_or_else(refused)?;
    let system_id = current.system.ok_or_else(refused)?;
    let ship_id = current.ship.ok_or_else(refused)?;
    if current.station.is_some() || identity.ship_id != Some(ship_id) || (rules.requires_alliance && current.alliance.is_none()) { return Err(refused()); }
    let map = resources.sde.map.system(system_id).ok_or_else(refused)?;
    let security = map.security.filter(|s| s.is_finite()).ok_or_else(refused)?;
    if !(KNOWN_SPACE_SYSTEM_START..WORMHOLE_SYSTEM_START).contains(&system_id) || map.faction_id.is_some() || security > 0.0 || security > rules.max_security_level { return Err(refused()); }
    let pin = lua::content_policy("sovereignty")?;
    let _market = market::lock().await;
    if !crate::space::sovereignty::is_ready(&resources.world) { return Err(refused()); }
    let admission = lua::admit_content(&pin)?;
    let sde = resources.sde.clone();
    let actor_sde = sde.clone();
    let proof = resources.world.with(system_id, move |system| system.sovereignty_placement(ship_id, &actor_sde)).await.flatten().ok_or_else(refused)?;
    if proof.character_id != character || proof.ship_id != ship_id { return Err(refused()); }
    let position = position(&proof, rules).ok_or_else(refused)?;
    let now = filetime_now();
    let checkpoint = i64::try_from(admission.policy().checkpoint_interval_ms()?).map_err(db_err)?;
    let frozen = std::sync::Arc::new((admission, current));
    let world = resources.world.clone();
    resources.db.transaction(|tx, outbox: &mut Outbox| {
        let sde = sde.clone();
        let frozen = frozen.clone();
        Box::pin(async move {
            let (admission, current) = &*frozen;
            let alliances: Vec<_> = current.alliance.into_iter().collect();
            if !registry::lock_admission(tx, &alliances, &[current.corporation], &[character]).await.map_err(db_err)? { return Err(refused()); }
            let actual = ship::current(tx, character).await.map_err(db_err)?.ok_or_else(refused)?;
            if actual.corporation != current.corporation || actual.alliance != current.alliance || actual.station.is_some() || actual.system != Some(system_id) || actual.ship != Some(ship_id) { return Err(refused()); }
            let authority = crate::services::corps::permissions::load(tx, character).await?.ok_or_else(refused)?;
            if !authority.live || authority.corporation != current.corporation { return Err(refused()); }
            let system = systems::lock(tx, system_id).await.map_err(db_err)?;
            if system.hub_item_id.is_some() {
                let existing = structures::snapshots(tx, Some(system_id), None).await.map_err(db_err)?;
                if let [hub] = existing.as_slice() {
                    if hub.structure.source_item_id == request.source_item_id && hub.structure.deployer_character_id == character { return Ok(DeployResult { item_id: hub.structure.item_id }); }
                }
                return Err(refused());
            }
            if structures::for_system(tx, system_id).await.map_err(db_err)?.is_some() || capture::for_system(tx, system_id).await.map_err(db_err)?.is_some() { return Err(refused()); }
            let candidates = inventory::descendants_and_ancestors(tx, &[request.source_item_id], &[ship_id]).await.map_err(db_err)?;
            if candidates.len() != 2 || candidates.binary_search(&request.source_item_id).is_err() || candidates.binary_search(&ship_id).is_err() { return Err(refused()); }
            let requests: Vec<_> = candidates.iter().map(|&item_id| crate::dao::items::parent_locks::LockRequest {
                item_id, strength: crate::dao::items::parent_locks::LockStrength::Update,
            }).collect();
            let _locks = crate::services::inventory::destination::lock_items(tx, &sde, &requests, &[]).await?;
            if inventory::descendants_and_ancestors(tx, &[request.source_item_id], &[ship_id]).await.map_err(db_err)? != candidates
                || !crate::dao::trades::reserved(tx, &[request.source_item_id, ship_id]).await.map_err(db_err)?.is_empty() { return Err(refused()); }
            let hull = load_items(tx, &sde, ItemQuery::Id(ship_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(refused)?;
            if hull.owner_id != character || hull.location_id != system_id || hull.category_id != i64::from(CATEGORY_SHIP) || !hull.singleton { return Err(refused()); }
            let source = load_items(tx, &sde, ItemQuery::Id(request.source_item_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(refused)?;
            let def = sde.sovereignty.hub();
            if source.type_id != def.type_id || source.owner_id != character || source.location_id != ship_id || source.flag_id != FLAG_CARGO || source.quantity < 1 || (source.singleton && source.quantity != 1) { return Err(refused()); }
            let item = if source.quantity > 1 {
                let id = split_stack(tx, &source, 1, character, ship_id, FLAG_CARGO, false).await?;
                let left = crate::dao::items::loading::Item { quantity: source.quantity - 1, ..source.clone() };
                outbox.notify_pilot(character, "OnItemChange", vec![left.row(), crate::services::inventory::notifications::item_change_dict([(IX_STACK_SIZE, Value::Int(i64::from(source.quantity)))]), Value::None]);
                load_items(tx, &sde, ItemQuery::Id(id)).await.map_err(db_err)?.into_iter().next().ok_or_else(refused)?
            } else { source.clone() };
            if !item_sov::place(tx, &item, system_id, def.deploy.default_owner, position).await.map_err(db_err)? { return Err(refused()); }
            let placed = load_items(tx, &sde, ItemQuery::Id(item.item_id)).await.map_err(db_err)?.into_iter().next().ok_or_else(refused)?;
            let changes = vec![(Value::Int(IX_LOCATION_ID), Value::Int(if item.item_id == source.item_id { ship_id } else { 0 })), (Value::Int(IX_FLAG), Value::Int(i64::from(FLAG_CARGO))), (Value::Int(IX_OWNER_ID), Value::Int(character)), (Value::Int(IX_SINGLETON), Value::Bool(item.singleton))];
            outbox.notify_pilot(character, "OnItemChange", vec![placed.row(), Value::Dict(changes), Value::None]);
            let ns = admission.policy().namespace().to_owned();
            let version = admission.policy().version().to_owned();
            let structure = StructureRow { item_id: item.item_id, system_id, source_item_id: source.item_id, deployer_character_id: character, deployer_corporation_id: current.corporation, deployed_at: now, content_namespace: ns.clone(), content_version: version.clone(), self_destruct_due_at: None, self_destruct_actor: None };
            if !structures::insert(tx, &structure).await.map_err(db_err)? { return Err(refused()); }
            let updated = systems::replace(tx, &system, &SystemChange { hub_item_id: Some(item.item_id), claim_corporation_id: None, alliance_id: None, claim_time: None, ownership_epoch: system.ownership_epoch }).await.map_err(db_err)?.ok_or_else(refused)?;
            let target = CaptureRow { target_item_id: item.item_id, system_id, ownership_epoch: updated.ownership_epoch, controlling_team: None, scoring_team: None, defender_team: OWNER_UNOWNED, score: 0.0, rate: 0.0, blocked: false, score_sim_time: now, checkpoint_wall_time: now, revision: 0, capture_time_seconds: def.capture.capture_time_seconds, defensive_regen_rate: def.capture.defensive_regen_rate, checkpoint_interval_ms: checkpoint, content_namespace: ns.clone(), content_version: version.clone() };
            if !capture::insert(tx, &target).await.map_err(db_err)? { return Err(refused()); }
            let history = HistoryRow { history_id: 0, system_id, hub_item_id: item.item_id, old_corporation_id: None, new_corporation_id: None, old_alliance_id: None, new_alliance_id: None, old_epoch: system.ownership_epoch, new_epoch: updated.ownership_epoch, reason: TransitionKind::Deployed.as_str().to_owned(), at: now, actor_id: Some(character), operation_key: format!("deploy:{system_id}:{}", updated.revision), content_namespace: ns, content_version: version };
            if history::insert(tx, &history).await.map_err(db_err)?.is_none() { return Err(refused()); }
            let hub = HubRow { system: updated.clone(), structure, type_id: item.type_id, owner_id: def.deploy.default_owner, x: Some(position[0]), y: Some(position[1]), z: Some(position[2]) };
            outbox.sovereignty(CommittedPublication { hubs: vec![HubPublication { system: updated, hub: Some(hub), capture: Some(target) }], ..Default::default() });
            Ok(DeployResult { item_id: item.item_id })
        })
    }, |result, outbox| async move { dispatch_world(result, outbox, &world) }).await
}
