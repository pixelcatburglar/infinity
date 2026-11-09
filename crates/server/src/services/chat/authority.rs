//! Channel admission and durable room authority for `xmppchatclient/xmppchatsvc.py`
//! and `xmppchannelsettings.py`. Organization authority comes from current game
//! membership, other session facts from the online pilot registry, local categories
//! from the SDE map, and room metadata, ACLs and special MOTDs from
//! `game.chat_channels`, `game.chat_channel_members` and `game.chat_system_motds`.
//! Occupants and messages are runtime transport state. Restriction deadlines persist
//! and use the durable scheduler. Passwords, corporation/alliance ACL grants,
//! proxy-room ownership and client-preference nullsec blackout are unsupported.
use std::sync::Arc;
use infinity_core::{consts::{ROLE_DIRECTOR, WORMHOLE_SYSTEM_START}, messages::chat as text, session::OnlineSession};
use crate::{chat::{console, Hub}, client::filetime_now, dao::{alliances, chat_channels as dao, characters::lookup}, db::{self, Db, executor::Executor, outbox::{Effect, Outbox}}, sde::Sde};
use super::super::{CallError, notify_error, has_role_or_is_ceo};

/// `inventorycommon/const.py:48-59`: suppressed solar-system ranges.
const SUPPRESSED_SYSTEM_RANGES: [(i64, i64); 4] = [(32_000_000, 32_999_999), (34_000_000, 34_999_999), (35_000_000, 35_000_100), (36_000_000, 36_999_999)];
/// `inventorycommon/const.py:48-59`: last wormhole system ID.
const WORMHOLE_SYSTEM_END: i64 = 31_999_999;
/// `appConst.py:1569`: Zarzakh uses wormhole local.
const ZARZAKH_SYSTEM_ID: i64 = 30_100_000;
/// `appConst.py:608`: factionTriglavian.
const TRIGLAVIAN_FACTION_ID: i64 = 500_026;
/// `xmppchatchannels.py:59`: maximum creation name length.
const CREATE_NAME_LIMIT: usize = 60;
/// `xmppchannelsettings.py:297`: maximum renamed display name length.
const RENAME_LIMIT: usize = 50;
/// `xmppchannelsettings.py:338`: maximum MOTD length.
const MOTD_LIMIT: usize = 4000;

/// `localization/const.py:6-23`: known client languages and their server room names.
const HELP_ROOMS: &[(&str, &str)] = &[("en-us", "help_en-us"), ("de", "help_de"), ("fr", "help_fr"), ("es", "help_es"), ("ru", "help_ru"), ("ja", "help_ja"), ("it", "help_it"), ("ko", "help_ko")];
/// Server default for unknown or omitted client languages.
const DEFAULT_HELP: &str = "help_zh";
/// Server-designated rookie room returned by GetRookieChannel.
pub(crate) const ROOKIE: &str = "help_rookie";

#[derive(Clone, Debug)]
pub(crate) struct Admission { pub affiliation: String, pub role: String, pub motd: String }
#[derive(Clone, Debug)]
pub(crate) struct RoomConfig { pub display_name: String, pub public: bool, pub moderated: bool, pub members_by_default: bool }
#[derive(Clone, Debug)]
pub(crate) struct Affiliation { pub character_id: i64, pub affiliation: String }
#[derive(Clone, Debug)]
pub(crate) struct DiscoRoom { pub name: String, pub display_name: String, pub affiliation: String }

fn forbidden() -> CallError { notify_error(text::FORBIDDEN) }
fn invalid() -> CallError { notify_error(text::INVALID) }
fn name_error(e: db::Error) -> CallError {
    if matches!(&e, db::Error::Postgres(p) if p.code() == Some(&tokio_postgres::error::SqlState::UNIQUE_VIOLATION)) { notify_error(text::NAME_TAKEN) } else { e.into() }
}

pub(crate) fn help(language: Option<&str>) -> &'static str {
    HELP_ROOMS.iter().find(|(id, _)| Some(*id) == language).map_or(DEFAULT_HELP, |(_, room)| *room)
}

fn local_prefix(sde: &Sde, id: i64) -> Option<&'static str> {
    if id <= 0 { return None; }
    let system = sde.map.system(id)?;
    Some(if SUPPRESSED_SYSTEM_RANGES.iter().any(|(start, end)| (*start..=*end).contains(&id)) { "nolocal" } else if id == ZARZAKH_SYSTEM_ID || (WORMHOLE_SYSTEM_START..=WORMHOLE_SYSTEM_END).contains(&id) { "wormhole" } else if system.faction_id == Some(TRIGLAVIAN_FACTION_ID) { "triglavian" } else { "local" })
}

fn matches_system_channel(sde: &Sde, session: &OnlineSession, channel: &str) -> bool {
    let Some((prefix, number)) = channel.split_once('_') else { return false; };
    if number.is_empty() || number.starts_with('0') || !number.bytes().all(|b| b.is_ascii_digit()) { return false; }
    let Ok(id) = number.parse::<i64>() else { return false; };
    if id <= 0 { return false; }
    match prefix {
        "corp" => session.corporation_id == Some(id),
        "alliance" => session.alliance_id == Some(id),
        "faction" => session.faction_id == Some(id),
        "fleet" => session.fleet_id == Some(id),
        _ => session.system_id == Some(id) && local_prefix(sde, id) == Some(prefix),
    }
}

/// Mirrors chatutil/channelnames.py:22-35; nullsec blackout has no server state.
pub(crate) fn system_channels(sde: &Sde, session: &OnlineSession) -> Vec<String> {
    let mut result = Vec::new();
    if let Some(id) = session.system_id {
        if let Some(prefix) = local_prefix(sde, id) { result.push(format!("{prefix}_{id}")); }
    }
    for (prefix, id) in [("corp", session.corporation_id), ("alliance", session.alliance_id), ("faction", session.faction_id), ("fleet", session.fleet_id)] {
        if let Some(id) = id.filter(|id| *id > 0) { result.push(format!("{prefix}_{id}")); }
    }
    result
}
pub(super) fn valid_room(c: &dao::Channel) -> bool {
    if c.owner_id <= 0 { return false; }
    match c.category.as_str() {
        "player" => c.private_low.is_none() && c.private_high.is_none() && c.name.strip_prefix("player_").and_then(|s| s.parse::<i64>().ok()).is_some_and(|id| id > 0 && c.name == format!("player_{id}")),
        "private" => match (c.private_low, c.private_high) { (Some(low), Some(high)) => low > 0 && low < high && (c.owner_id == low || c.owner_id == high) && !c.public && c.name == format!("private_{low}_{high}"), _ => false },
        _ => false,
    }
}
fn participant(c: &dao::Channel, id: i64) -> bool { c.private_low == Some(id) || c.private_high == Some(id) }
fn banned(m: Option<&dao::Member>, now: i64) -> bool {
    m.is_some_and(|m| m.affiliation == dao::Affiliation::Outcast || m.ban_expires_at.is_some_and(|deadline| deadline > now))
}
fn affiliation(c: &dao::Channel, m: Option<&dao::Member>, id: i64, now: i64) -> &'static str {
    if c.owner_id == id { "owner" } else if banned(m, now) { "outcast" } else { match m.map(|m| m.affiliation) { Some(dao::Affiliation::Admin) => "admin", Some(dao::Affiliation::Member) => "member", _ => "none" } }
}
fn permitted(c: &dao::Channel, m: Option<&dao::Member>, id: i64, now: i64) -> bool {
    let aff = affiliation(c, m, id, now);
    aff != "outcast" && (c.category != "private" || participant(c, id)) && (c.category != "player" || aff != "none" || c.public)
}
fn effective_voice(m: Option<&dao::Member>, now: i64) -> dao::Voice {
    match m {
        Some(m) if m.voice == dao::Voice::Muted || m.voice_expires_at.is_some_and(|deadline| deadline > now) => dao::Voice::Muted,
        Some(m) => m.voice,
        None => dao::Voice::Default,
    }
}
fn voice(c: &dao::Channel, m: Option<&dao::Member>, aff: &str, now: i64) -> &'static str {
    if aff == "owner" { return "moderator"; }
    if effective_voice(m, now) == dao::Voice::Muted { return "visitor"; }
    if aff == "admin" { return "moderator"; }
    match effective_voice(m, now) { dao::Voice::Muted => "visitor", dao::Voice::Voiced => "participant", _ if c.moderated && !c.members_by_default && aff == "none" => "visitor", _ => "participant" }
}

pub(crate) async fn admit(db: &Db, sde: &Sde, session: &OnlineSession, channel: &str) -> Result<Option<Admission>, CallError> {
    let id = session.character_id;
    if id <= 0 { return Ok(None); }
    if let Some(recipient) = console::recipient(channel) {
        return Ok((recipient == id).then(|| Admission { affiliation: "member".into(), role: "visitor".into(), motd: String::new() }));
    }
    let system_match = if channel.starts_with("corp_") || channel.starts_with("alliance_") {
        let Some(org) = crate::dao::characters::members::organization(db, id).await? else { return Ok(None); };
        let mut actual = session.clone();
        actual.corporation_id = Some(org.corporation_id);
        actual.alliance_id = org.alliance_id;
        matches_system_channel(sde, &actual, channel)
    } else {
        matches_system_channel(sde, session, channel)
    };
    if system_match || channel == DEFAULT_HELP || channel == ROOKIE || HELP_ROOMS.iter().any(|(_, room)| *room == channel) {
        let motd = dao::get_system_motd(db, channel).await?.unwrap_or_default();
        return Ok(Some(Admission { affiliation: "member".into(), role: "participant".into(), motd }));
    }
    let Some(c) = dao::get(db, channel).await? else { return Ok(None); };
    if !valid_room(&c) { return Ok(None); }
    let m = dao::member(db, channel, id).await?;
    let now = filetime_now();
    let aff = affiliation(&c, m.as_ref(), id, now);
    if !permitted(&c, m.as_ref(), id, now) { return Ok(None); }
    let role = voice(&c, m.as_ref(), aff, now);
    Ok(Some(Admission { affiliation: aff.into(), role: role.into(), motd: c.motd }))
}

pub(super) async fn locked(ex: &(impl Executor + ?Sized), name: &str) -> Result<dao::Channel, CallError> {
    dao::get_for_update(ex, name).await?.filter(valid_room).ok_or_else(invalid)
}
/// Requires a player-room manager; callers must already hold the room lock.
pub(super) async fn require_manager(ex: &(impl Executor + ?Sized), name: &str, actor_id: i64) -> Result<(), CallError> {
    if actor_id <= 0 || !dao::may_manage(ex, name, actor_id, filetime_now()).await? { return Err(forbidden()); }
    Ok(())
}
async fn active(ex: &(impl Executor + ?Sized), id: i64) -> Result<(), CallError> {
    if id <= 0 || lookup::find(ex, "", Some(id)).await? != Some(id) { return Err(invalid()); }
    Ok(())
}

pub(super) enum ChatEffect { Invite(i64, String), EvictRoom(String), EvictMember(String, i64), Subject(String, String), Refresh(String) }
crate::progress::leaf_effect!(ChatEffect);
pub(super) async fn dispatch<T>(value: T, outbox: Outbox<ChatEffect>, hub: &Hub) -> Result<T, CallError> {
    for e in outbox.into_entries() {
        match e {
            Effect::Operation(ChatEffect::Invite(id, room)) => hub.invite(id, &room),
            Effect::Operation(ChatEffect::EvictRoom(room)) => hub.evict_room(&room),
            Effect::Operation(ChatEffect::EvictMember(room, id)) => hub.evict_member(&room, id),
            Effect::Operation(ChatEffect::Subject(room, motd)) => hub.broadcast_subject(&room, &motd),
            Effect::Operation(ChatEffect::Refresh(room)) => hub.refresh_permissions(&room).await?,
            Effect::Notification(_) => tracing::warn!("chat dispatcher received an unsupported notification"),
        }
    }
    Ok(value)
}

pub(crate) async fn destroy(db: &Db, hub: &Hub, session: &OnlineSession, channel: &str) -> Result<(), CallError> {
    let id = session.character_id;
    let channel: Arc<str> = Arc::from(channel);
    db.transaction(|tx, outbox: &mut Outbox<ChatEffect>| { let channel = Arc::clone(&channel); Box::pin(async move {
        let channel = &*channel;
        let c = locked(tx, channel).await?;
        if c.category == "private" {
            if id <= 0 || c.owner_id != id { return Err(forbidden()); }
        } else {
            require_manager(tx, channel, id).await?;
        }
        for member in dao::members(tx, channel).await? {
            if member.voice_expires_at.is_some() { super::expiry::cancel_in_transaction(tx, channel, member.character_id, dao::Restriction::Mute).await?; }
            if member.ban_expires_at.is_some() { super::expiry::cancel_in_transaction(tx, channel, member.character_id, dao::Restriction::Ban).await?; }
        }
        if id <= 0 || !dao::delete(tx, &c.name, id).await? { return Err(forbidden()); }
        outbox.operation(ChatEffect::EvictRoom(c.name)); Ok(())
    }) }, |v, o| dispatch(v, o, hub)).await
}
pub(crate) async fn room_config(db: &Db, session: &OnlineSession, channel: &str) -> Result<RoomConfig, CallError> {
    let id = session.character_id;
    let channel: Arc<str> = Arc::from(channel);
    db.transaction(|tx, _: &mut Outbox| { let channel = Arc::clone(&channel); Box::pin(async move {
        let channel = &*channel;
        let c = locked(tx, channel).await?;
        require_manager(tx, channel, id).await?;
        Ok(RoomConfig { display_name: c.display_name, public: c.public, moderated: c.moderated, members_by_default: c.members_by_default })
    }) }, |value, _| async move { Ok(value) }).await
}
pub(crate) async fn configure(db: &Db, hub: &Hub, session: &OnlineSession, channel: &str, config: RoomConfig) -> Result<(), CallError> {
    let id = session.character_id;
    let channel: Arc<str> = Arc::from(channel);
    let config = Arc::new(config);
    db.transaction(|tx, outbox: &mut Outbox<ChatEffect>| {
        let channel = Arc::clone(&channel);
        let config = Arc::clone(&config);
        Box::pin(async move {
            let channel = &*channel;
            let c = locked(tx, channel).await?;
            require_manager(tx, channel, id).await?;
            if c.display_name != config.display_name && (config.display_name.trim().is_empty() || config.display_name.contains('\0') || config.display_name.chars().count() > RENAME_LIMIT) { return Err(invalid()); }
            if id <= 0 || !dao::configure(tx, channel, id, config.public, config.moderated, config.members_by_default).await? { return Err(forbidden()); }
            if c.display_name != config.display_name && !dao::rename(tx, channel, id, &config.display_name).await.map_err(name_error)? { return Err(forbidden()); }
            outbox.operation(ChatEffect::Refresh(c.name)); Ok(())
        })
    }, |v, o| dispatch(v, o, hub)).await
}
pub(crate) async fn affiliations(db: &Db, session: &OnlineSession, channel: &str) -> Result<Vec<Affiliation>, CallError> {
    let c = dao::get(db, channel).await?.filter(valid_room).ok_or_else(invalid)?;
    let me = session.character_id;
    let member = dao::member(db, channel, me).await?;
    let now = filetime_now();
    if me <= 0 || !permitted(&c, member.as_ref(), me, now) { return Err(forbidden()); }
    let mut result = vec![Affiliation { character_id: c.owner_id, affiliation: "owner".into() }];
    result.extend(dao::members(db, channel).await?.into_iter().map(|m| Affiliation { character_id: m.character_id, affiliation: affiliation(&c, Some(&m), m.character_id, now).into() }));
    Ok(result)
}
pub(crate) async fn set_affiliation(db: &Db, hub: &Hub, session: &OnlineSession, channel: &str, target: i64, affiliation: &str) -> Result<(), CallError> {
    let affiliation = match affiliation {
        "owner" => "owner", "admin" => "admin", "member" => "member", "outcast" => "outcast", "none" => "none",
        _ => return Err(invalid()),
    };
    let id = session.character_id;
    let channel: Arc<str> = Arc::from(channel);
    db.transaction(|tx, outbox: &mut Outbox<ChatEffect>| { let channel = Arc::clone(&channel); Box::pin(async move {
        let channel = &*channel;
        let c = locked(tx, channel).await?; active(tx, target).await?;
        if id <= 0 || c.category != "player" || target == c.owner_id { return Err(forbidden()); }
        require_manager(tx, channel, id).await?;
        let old = dao::member(tx, channel, target).await?;
        let changed = match affiliation {
            "owner" => dao::transfer_owner(tx, channel, id, target).await?,
            "none" => match old.as_ref() { Some(m) if !matches!(m.voice, dao::Voice::Default) || m.voice_expires_at.is_some() || m.ban_expires_at.is_some() => dao::set_member(tx, channel, id, target, dao::Affiliation::None, false, m.voice).await?, Some(_) => dao::delete_member(tx, channel, id, target).await?, None => true },
            _ => { let aff = match affiliation { "admin" => dao::Affiliation::Admin, "member" => dao::Affiliation::Member, "outcast" => dao::Affiliation::Outcast, _ => return Err(invalid()) }; dao::set_member(tx, channel, id, target, aff, old.as_ref().is_some_and(|m| m.invited), old.as_ref().map_or(dao::Voice::Default, |m| m.voice)).await? },
        };
        if !changed { return Err(forbidden()); }
        if affiliation == "owner" {
            super::expiry::cancel_in_transaction(tx, channel, target, dao::Restriction::Ban).await?;
            super::expiry::cancel_in_transaction(tx, channel, target, dao::Restriction::Mute).await?;
        }
        if affiliation == "outcast" { outbox.operation(ChatEffect::EvictMember(c.name.clone(), target)); }
        outbox.operation(ChatEffect::Refresh(c.name)); Ok(())
    }) }, |v, o| dispatch(v, o, hub)).await
}
pub(crate) async fn set_role(db: &Db, hub: &Hub, session: &OnlineSession, channel: &str, target: i64, role: &str) -> Result<(), CallError> {
    if !["participant", "visitor"].contains(&role) { return Err(forbidden()); }
    let muted = role == "visitor";
    let id = session.character_id;
    let channel: Arc<str> = Arc::from(channel);
    db.transaction(|tx, outbox: &mut Outbox<ChatEffect>| { let channel = Arc::clone(&channel); Box::pin(async move {
        let channel = &*channel;
        let c = locked(tx, channel).await?; active(tx, target).await?;
        if id <= 0 || c.category != "player" || target == c.owner_id { return Err(forbidden()); }
        require_manager(tx, channel, id).await?;
        let old = dao::member(tx, channel, target).await?;
        let aff = old.as_ref().map_or(dao::Affiliation::None, |m| m.affiliation);
        if (matches!(aff, dao::Affiliation::None) && !c.public) || matches!(aff, dao::Affiliation::Outcast | dao::Affiliation::Admin) || !dao::set_member(tx, channel, id, target, aff, old.as_ref().is_some_and(|m| m.invited), if muted { dao::Voice::Muted } else { dao::Voice::Voiced }).await? { return Err(forbidden()); }
        outbox.operation(ChatEffect::Refresh(c.name)); Ok(())
    }) }, |v, o| dispatch(v, o, hub)).await
}
pub(crate) async fn invite(db: &Db, hub: &Hub, session: &OnlineSession, channel: &str, target: i64) -> Result<(), CallError> {
    let id = session.character_id;
    let channel: Arc<str> = Arc::from(channel);
    db.transaction(|tx, outbox: &mut Outbox<ChatEffect>| { let channel = Arc::clone(&channel); Box::pin(async move {
        let channel = &*channel;
        let c = locked(tx, channel).await?; active(tx, target).await?;
        if c.category == "private" {
            active(tx, id).await?;
            let actor = dao::member(tx, channel, id).await?;
            if id == target || !participant(&c, target) || !permitted(&c, actor.as_ref(), id, filetime_now()) { return Err(forbidden()); }
        } else {
            require_manager(tx, channel, id).await?;
            let now = filetime_now();
            let old = dao::member(tx, channel, target).await?;
            if target == c.owner_id || banned(old.as_ref(), now) { return Err(forbidden()); }
            if !dao::set_member(tx, channel, id, target, old.as_ref().map_or(dao::Affiliation::Member, |m| if matches!(m.affiliation, dao::Affiliation::None | dao::Affiliation::Outcast) { dao::Affiliation::Member } else { m.affiliation }), true, old.as_ref().map_or(dao::Voice::Default, |m| m.voice)).await? { return Err(forbidden()); }
        }
        outbox.operation(ChatEffect::Invite(target, c.name)); Ok(())
    }) }, |v, o| dispatch(v, o, hub)).await
}
pub(crate) async fn set_subject(db: &Db, hub: &Hub, session: &OnlineSession, channel: &str, motd: &str) -> Result<(), CallError> {
    if motd.contains('\0') || motd.chars().count() > MOTD_LIMIT { return Err(invalid()); }
    let id = session.character_id;
    let channel: Arc<str> = Arc::from(channel);
    let motd: Arc<str> = Arc::from(motd);
    db.transaction(|tx, outbox: &mut Outbox<ChatEffect>| { let channel = Arc::clone(&channel); let motd = Arc::clone(&motd); Box::pin(async move {
        let channel = &*channel;
        let motd = &*motd;
        let c = locked(tx, channel).await?;
        require_manager(tx, channel, id).await?;
        if id <= 0 || !dao::set_motd(tx, channel, id, motd).await? { return Err(forbidden()); }
        outbox.operation(ChatEffect::Subject(c.name, motd.to_owned())); Ok(())
    }) }, |v, o| dispatch(v, o, hub)).await
}
pub(crate) async fn disco_list(db: &Db, session: &OnlineSession) -> Result<Vec<DiscoRoom>, CallError> {
    if session.character_id <= 0 { return Err(forbidden()); }
    let mut rooms = Vec::new();
    let now = filetime_now();
    for c in dao::list_visible(db, session.character_id, now).await? {
        if !valid_room(&c) { continue; }
        let m = dao::member(db, &c.name, session.character_id).await?;
        if !permitted(&c, m.as_ref(), session.character_id, now) { continue; }
        let aff = affiliation(&c, m.as_ref(), session.character_id, now).to_owned();
        rooms.push(DiscoRoom { name: c.name, display_name: c.display_name, affiliation: aff });
    }
    Ok(rooms)
}
pub(crate) async fn disco_info(db: &Db, sde: &Sde, session: &OnlineSession, channel: &str) -> Result<Option<DiscoRoom>, CallError> {
    let Some(a) = admit(db, sde, session, channel).await? else { return Ok(None); };
    let display_name = dao::get(db, channel).await?.map_or_else(|| channel.to_owned(), |c| c.display_name);
    Ok(Some(DiscoRoom { name: channel.into(), display_name, affiliation: a.affiliation }))
}

pub(super) async fn create_player(db: &Db, id: i64, name: &str) -> Result<String, CallError> {
    if name.trim().is_empty() || name.contains('\0') || name.chars().count() > CREATE_NAME_LIMIT { return Err(invalid()); }
    let name: Arc<str> = Arc::from(name);
    db.transaction(|tx, _: &mut Outbox| { let name = Arc::clone(&name); Box::pin(async move { active(tx, id).await?; Ok(dao::create_player(tx, id, &name).await.map_err(name_error)?.name) }) }, |v, _| async move { Ok(v) }).await
}
pub(super) async fn create_private(db: &Db, id: i64, target: i64) -> Result<String, CallError> {
    if id == target { return Err(invalid()); }
    db.transaction(|tx, _: &mut Outbox| Box::pin(async move { active(tx, id).await?; active(tx, target).await?; let c = dao::create_private(tx, id, target).await?; if !valid_room(&c) { return Err(invalid()); } Ok(c.name) }), |v, _| async move { Ok(v) }).await
}
pub(super) async fn system_motd(db: &Db, hub: &Hub, session: &OnlineSession, channel: &str, motd: &str) -> Result<(), CallError> {
    if motd.contains('\0') || motd.chars().count() > MOTD_LIMIT || session.character_id <= 0 { return Err(invalid()); }
    let corp = session.corporation_id.filter(|id| *id > 0).ok_or_else(forbidden)?;
    let allowed_corp = channel == format!("corp_{corp}");
    let alliance = session.alliance_id.filter(|id| *id > 0);
    let allowed_alliance = alliance.is_some_and(|id| channel == format!("alliance_{id}"));
    if !allowed_corp && !allowed_alliance { return Err(forbidden()); }
    let id = session.character_id;
    let channel: Arc<str> = Arc::from(channel);
    let motd: Arc<str> = Arc::from(motd);
    db.transaction(|tx, outbox: &mut Outbox<ChatEffect>| { let channel = Arc::clone(&channel); let motd = Arc::clone(&motd); Box::pin(async move {
        let channel = &*channel;
        let motd = &*motd;
        let alliances: Vec<i64> = if allowed_alliance { alliance.into_iter().collect() } else { Vec::new() };
        if !crate::dao::corporations::registry::lock_admission(tx, &alliances, &[corp], &[id]).await? { return Err(forbidden()); }
        if !has_role_or_is_ceo(tx, id, corp, ROLE_DIRECTOR).await? || (allowed_alliance && alliances::executor_corp(tx, alliance.unwrap()).await? != Some(corp)) { return Err(forbidden()); }
        dao::set_system_motd(tx, channel, motd).await?;
        outbox.operation(ChatEffect::Subject(channel.to_owned(), motd.to_owned())); Ok(())
    }) }, |v, o| dispatch(v, o, hub)).await
}
