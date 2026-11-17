//! Repairable-ship recovery lifecycle persistence in
//! `game.repairable_recovery_groups` and `game.repairable_recovery_items`.
//! The client feature is `repairableShip` (`repairableShipMgr` remote service);
//! the recovery window is `repairableShip/client/repairable_ship_window.py`.
//! Group and member identities share `game.repairable_recovery_groups_group_id_seq`
//! inside `consts::RECOVERY_ID_FIRST..=RECOVERY_ID_LAST` and stay distinct from
//! physical `game.items` identities. Transactions, transition policy, pricing,
//! custody moves, scheduler jobs, notifications and World checks are caller
//! concerns. The client caches service-life and recovery price answers per
//! process: a changed server-side capture can display stale values until the
//! client restarts, and logging out or switching characters does not guarantee
//! a refresh; charges are always priced at the current server value.

use std::collections::BTreeMap;
use infinity_core::consts::{RECOVERY_ID_FIRST, RECOVERY_ID_LAST};
use crate::{db::{self, executor::Executor}, isk::IskAmount};
use tokio_postgres::Row;

macro_rules! recovery_id {
    ($name:ident) => {
        /// In-band identity check only; table membership and ownership are
        /// proven by the row read, never by the range.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub(crate) struct $name(i64);
        impl $name {
            pub(crate) fn new(id: i64) -> Option<Self> {
                (RECOVERY_ID_FIRST..=RECOVERY_ID_LAST).contains(&id).then_some(Self(id))
            }
            pub(crate) fn get(self) -> i64 { self.0 }
        }
    };
}
recovery_id!(RecoveryGroupId);
recovery_id!(RecoveryItemId);

/// Ordinary physical inventory item identity; recovery-band IDs are not items
/// and cannot enter the i64 inventory APIs through this adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct ItemId(i64);
impl ItemId {
    pub(crate) fn new(id: i64) -> Option<Self> {
        (id > 0 && !(RECOVERY_ID_FIRST..=RECOVERY_ID_LAST).contains(&id)).then_some(Self(id))
    }
    pub(crate) fn get(self) -> i64 { self.0 }
}

/// Member lifecycle. Persisted smallint codes keep every active state below
/// every terminal state, which the partial unique index relies on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    AwaitingPayment,
    Delivering,
    Ready,
    Withdrawn,
    Abandoned,
    Expired,
}
impl Status {
    /// States in which the physical item still sits in recovery escrow.
    pub(crate) const ACTIVE: [Status; 3] = [Self::AwaitingPayment, Self::Delivering, Self::Ready];
    fn encode(self) -> i16 { self as i16 }
    fn decode(value: i16) -> Result<Self, db::Error> {
        match value {
            0 => Ok(Self::AwaitingPayment),
            1 => Ok(Self::Delivering),
            2 => Ok(Self::Ready),
            3 => Ok(Self::Withdrawn),
            4 => Ok(Self::Abandoned),
            5 => Ok(Self::Expired),
            other => Err(db::Error::Progress(anyhow::anyhow!("recovery item status {other} is out of range"))),
        }
    }
}

/// The settle_loss lane that produced a recovery group; persisted smallint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SourceKind {
    PilotedShip,
    UnpilotedShip,
    Drone,
}
impl SourceKind {
    fn encode(self) -> i16 { self as i16 }
    fn decode(value: i16) -> Result<Self, db::Error> {
        match value {
            0 => Ok(Self::PilotedShip),
            1 => Ok(Self::UnpilotedShip),
            2 => Ok(Self::Drone),
            other => Err(db::Error::Progress(anyhow::anyhow!("recovery source kind {other} is out of range"))),
        }
    }
}

/// The complete durable columns of the fenced original `game.items` row that a
/// pending loss captured before writing anything. Reconcile compares the live
/// row column-for-column against exactly this shape; it is never updated by a
/// transaction retry.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SourceShape {
    pub item: ItemId,
    pub owner: i64,
    pub type_id: i32,
    pub location: i64,
    pub flag: i32,
    pub quantity: i64,
    pub singleton: bool,
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub z: Option<f64>,
    pub brain_slot: Option<i32>,
    pub jump_clone: Option<i64>,
}
impl SourceShape {
    fn item_id(value: i64) -> Result<ItemId, db::Error> {
        ItemId::new(value).ok_or_else(|| db::Error::Progress(anyhow::anyhow!("recovery row item id {value} is not a physical item")))
    }
    pub(crate) fn from_row(row: &Row) -> Result<Self, db::Error> {
        Ok(Self {
            item: Self::item_id(row.get("item_id"))?, owner: row.get("owner_id"), type_id: row.get("type_id"),
            location: row.get("location_id"), flag: row.get("flag_id"), quantity: row.get("quantity"),
            singleton: row.get("singleton"), x: row.get("x"), y: row.get("y"), z: row.get("z"),
            brain_slot: row.get("brain_slot"), jump_clone: row.get("jump_clone_id"),
        })
    }
    fn from_group_row(row: &Row) -> Result<Self, db::Error> {
        Ok(Self {
            item: Self::item_id(row.get("source_item_id"))?, owner: row.get("source_owner_id"), type_id: row.get("source_type_id"),
            location: row.get("source_location_id"), flag: row.get("source_flag_id"), quantity: row.get("source_quantity"),
            singleton: row.get("source_singleton"), x: row.get("source_x"), y: row.get("source_y"), z: row.get("source_z"),
            brain_slot: row.get("source_brain_slot"), jump_clone: row.get("source_jump_clone_id"),
        })
    }
}

/// One recovery event row from `game.repairable_recovery_groups`; `shape` is
/// the frozen original item row and `position` the sampled lethal position.
#[derive(Clone, Debug)]
pub(crate) struct Group {
    pub id: RecoveryGroupId,
    pub beneficiary: i64,
    pub kind: SourceKind,
    pub shape: SourceShape,
    pub system: i64,
    pub occurred_at: i64,
    pub position: [Option<f64>; 3],
    pub loss_id: Option<i64>,
    pub destination_station: i64,
    pub payment_expires_at: i64,
}
impl Group {
    pub(crate) fn from_row(row: &Row) -> Result<Self, db::Error> {
        let id = RecoveryGroupId::new(row.get("group_id"))
            .ok_or_else(|| db::Error::Progress(anyhow::anyhow!("recovery group id outside its band")))?;
        Ok(Self {
            id,
            beneficiary: row.get("beneficiary_id"),
            kind: SourceKind::decode(row.get("source_kind"))?,
            shape: SourceShape::from_group_row(row)?,
            system: row.get("source_system_id"),
            occurred_at: row.get("occurred_at"),
            position: [row.get("position_x"), row.get("position_y"), row.get("position_z")],
            loss_id: row.get("loss_id"),
            destination_station: row.get("destination_station_id"),
            payment_expires_at: row.get("payment_expires_at"),
        })
    }
}

/// One escrowed member row from `game.repairable_recovery_items`;
/// `last_service_life` is the remaining service life before the loss.
#[derive(Clone, Debug)]
pub(crate) struct RecoveryItem {
    pub id: RecoveryItemId,
    pub group: RecoveryGroupId,
    pub item: ItemId,
    pub original_owner: i64,
    pub type_id: i32,
    pub flag: i32,
    pub quantity: i64,
    pub singleton: bool,
    pub last_service_life: f64,
    pub repair_completed_at: i64,
    pub delivery_completed_at: Option<i64>,
    pub withdraw_expires_at: Option<i64>,
    pub status: Status,
    pub withdrawn_item: Option<ItemId>,
}
impl RecoveryItem {
    pub(crate) fn from_row(row: &Row) -> Result<Self, db::Error> {
        let withdrawn: Option<i64> = row.get("withdrawn_item_id");
        Ok(Self {
            id: RecoveryItemId::new(row.get("member_id"))
                .ok_or_else(|| db::Error::Progress(anyhow::anyhow!("recovery item id outside its band")))?,
            group: RecoveryGroupId::new(row.get("group_id"))
                .ok_or_else(|| db::Error::Progress(anyhow::anyhow!("recovery group id outside its band")))?,
            item: SourceShape::item_id(row.get("item_id"))?,
            original_owner: row.get("original_owner_id"),
            type_id: row.get("type_id"),
            flag: row.get("flag_id"),
            quantity: row.get("quantity"),
            singleton: row.get("singleton"),
            last_service_life: row.get("last_service_life"),
            repair_completed_at: row.get("repair_completed_at"),
            delivery_completed_at: row.get("delivery_completed_at"),
            withdraw_expires_at: row.get("withdraw_expires_at"),
            status: Status::decode(row.get("status"))?,
            withdrawn_item: withdrawn.map(SourceShape::item_id).transpose()?,
        })
    }
}

/// A group row joined with its member rows.
#[derive(Clone, Debug)]
pub(crate) struct Selection {
    pub group: Group,
    pub items: Vec<RecoveryItem>,
}

/// The pending loss facts a caller captured inside the item fence: the fenced
/// original row shape plus the loss facts that must not drift across retries.
/// `sampled_position` is the interpolated lethal position kept as a loss fact;
/// it is not the last durable position.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LossSource {
    pub kind: SourceKind,
    pub beneficiary: i64,
    pub shape: SourceShape,
    pub system: i64,
    pub lethal_at: i64,
    pub sampled_position: [f64; 3],
}

/// The persisted state for one original item ID: its recovery group, the
/// live physical row and the group's members, enough for the caller to tell
/// committed custody, original-live and drift apart.
#[derive(Debug)]
pub(crate) struct SourceProof {
    pub group: Option<Group>,
    pub physical: Option<SourceShape>,
    pub members: Vec<RecoveryItem>,
}

/// Everything `insert_group` needs; `loss_id` is the piloted loss's canonical
/// LossSettled identity, `None` for non-player sources.
#[derive(Debug)]
pub(crate) struct NewGroup {
    pub loss: LossSource,
    pub loss_id: Option<i64>,
    pub destination_station: i64,
    pub payment_expires_at: i64,
}

/// A member recorded before its physical row moves into escrow custody.
#[derive(Debug)]
pub(crate) struct NewItem {
    pub group: RecoveryGroupId,
    pub item: ItemId,
    pub original_owner: i64,
    pub type_id: i32,
    pub flag: i32,
    pub quantity: i64,
    pub singleton: bool,
    pub last_service_life: f64,
    pub repair_completed_at: i64,
}

/// One member's recorded payment; per-member amounts stay exact numerics.
#[derive(Debug)]
pub(crate) struct Payment {
    pub member: RecoveryItemId,
    pub amount: IskAmount,
}

const GROUP_COLUMNS: &str = "g.group_id, g.beneficiary_id, g.source_kind, g.source_item_id, g.source_owner_id, \
    g.source_type_id, g.source_location_id, g.source_flag_id, g.source_quantity, g.source_singleton, \
    g.source_x, g.source_y, g.source_z, g.source_brain_slot, g.source_jump_clone_id, g.source_system_id, \
    g.occurred_at, g.position_x, g.position_y, g.position_z, g.loss_id, g.destination_station_id, g.payment_expires_at";
const MEMBER_COLUMNS: &str = "m.member_id, m.group_id, m.item_id, m.original_owner_id, m.type_id, m.flag_id, \
    m.quantity, m.singleton, m.last_service_life, m.repair_completed_at, \
    m.delivery_completed_at, m.withdraw_expires_at, m.status, m.withdrawn_item_id";
const SHAPE_COLUMNS: &str = "item_id, owner_id, type_id, location_id, flag_id, quantity, singleton, x, y, z, brain_slot, jump_clone_id";

/// One owned joined snapshot of every group that still has an active member.
pub(crate) async fn list_owned(ex: &(impl Executor + ?Sized), character: i64) -> Result<Vec<Selection>, db::Error> {
    let rows = ex.query(
        &format!("SELECT {GROUP_COLUMNS}, {MEMBER_COLUMNS} FROM game.repairable_recovery_groups g \
            JOIN game.repairable_recovery_items m ON m.group_id = g.group_id AND m.status <= $2 \
            WHERE g.beneficiary_id = $1 ORDER BY g.group_id, m.member_id"),
        &[&character, &Status::Ready.encode()],
    ).await?;
    let mut selections: Vec<Selection> = Vec::new();
    for row in &rows {
        let group = Group::from_row(row)?;
        if selections.last().is_none_or(|last| last.group.id != group.id) {
            selections.push(Selection { group, items: Vec::new() });
        }
        selections.last_mut().expect("selection just pushed").items.push(RecoveryItem::from_row(row)?);
    }
    Ok(selections)
}

/// Owned group and member facts for one requested selection map. A requested
/// group with an empty member list still returns its group row so the caller
/// can authenticate it; requested members only surface under their true group.
/// `lock` takes `FOR UPDATE` in a fixed group-then-member order.
pub(crate) async fn load_selection(
    ex: &(impl Executor + ?Sized), character: i64,
    groups: &BTreeMap<RecoveryGroupId, Vec<RecoveryItemId>>, lock: bool,
) -> Result<Vec<Selection>, db::Error> {
    let group_ids: Vec<i64> = groups.keys().map(|id| id.get()).collect();
    let member_ids: Vec<i64> = groups.values().flatten().map(|id| id.get()).collect();
    let lock = if lock { " FOR UPDATE" } else { "" };
    let group_rows = ex.query(
        &format!("SELECT {GROUP_COLUMNS} FROM game.repairable_recovery_groups g \
            WHERE group_id = ANY($1) AND beneficiary_id = $2 ORDER BY group_id{lock}"),
        &[&group_ids, &character],
    ).await?;
    let member_rows = ex.query(
        &format!("SELECT {MEMBER_COLUMNS} FROM game.repairable_recovery_items m \
            WHERE member_id = ANY($1) AND group_id = ANY($2) ORDER BY member_id{lock}"),
        &[&member_ids, &group_ids],
    ).await?;
    let mut selections: Vec<Selection> = group_rows.iter()
        .map(|row| Group::from_row(row).map(|group| Selection { group, items: Vec::new() }))
        .collect::<Result<_, db::Error>>()?;
    for row in &member_rows {
        let member = RecoveryItem::from_row(row)?;
        if let Some(selection) = selections.iter_mut().find(|selection| selection.group.id == member.group) {
            selection.items.push(member);
        }
    }
    Ok(selections)
}

/// The recovery group, its members and the live physical row for one loss
/// source; the DAO reports facts and never judges World state.
pub(crate) async fn source_proof(ex: &(impl Executor + ?Sized), source: &LossSource) -> Result<SourceProof, db::Error> {
    let group = ex.query_opt(
        &format!("SELECT {GROUP_COLUMNS} FROM game.repairable_recovery_groups g WHERE source_item_id = $1"),
        &[&source.shape.item.get()],
    ).await?.as_ref().map(Group::from_row).transpose()?;
    let members = match &group {
        Some(group) => members_of(ex, group.id).await?,
        None => Vec::new(),
    };
    let physical = ex.query_opt(
        &format!("SELECT {SHAPE_COLUMNS}, (brain_activated_at IS NULL AND brain_expires_at IS NULL AND brain_effect_ids IS NULL) AS brain_clear FROM game.items WHERE item_id = $1"),
        &[&source.shape.item.get()],
    ).await?.as_ref().map(|row| {
        if !row.get::<_, bool>("brain_clear") {
            return Err(db::Error::Progress(anyhow::anyhow!("item contains brain state")));
        }
        SourceShape::from_row(row)
    }).transpose()?;
    Ok(SourceProof { group, physical, members })
}


/// Mark delivered members `ready` once their persisted delivery time has
/// passed; a late scheduler run still transitions them inside the window.
/// Returns the unordered transitioned IDs.
pub(crate) async fn mark_ready(
    ex: &(impl Executor + ?Sized), members: &[RecoveryItemId], now: i64,
) -> Result<Vec<RecoveryItemId>, db::Error> {
    let ids: Vec<i64> = members.iter().map(|id| id.get()).collect();
    let rows = ex.query(
        "UPDATE game.repairable_recovery_items SET status = $2 \
         WHERE member_id = ANY($1) AND status = $3 AND delivery_completed_at <= $4 \
         RETURNING member_id",
        &[&ids, &Status::Ready.encode(), &Status::Delivering.encode(), &now],
    ).await?;
    rows.iter().map(|row| RecoveryItemId::new(row.get("member_id"))
        .ok_or_else(|| db::Error::Progress(anyhow::anyhow!("recovery item id outside its band")))).collect()
}

/// Guarded terminal transition for a batch of members: `abandoned` and
/// `expired` take no physical item, `withdrawn` records each member's new
/// inventory ID before the old escrow row is deleted. Only members still in
/// one of `from` transition; the returned IDs are unordered.
pub(crate) async fn terminalize(
    ex: &(impl Executor + ?Sized), members: &[(RecoveryItemId, Option<ItemId>)],
    from: &[Status], to: Status, finished_at: i64,
) -> Result<Vec<RecoveryItemId>, db::Error> {
    let ids: Vec<i64> = members.iter().map(|(id, _)| id.get()).collect();
    let withdrawn: Vec<Option<i64>> = members.iter().map(|(_, item)| item.map(|item| item.get())).collect();
    let from: Vec<i16> = from.iter().map(|status| status.encode()).collect();
    let rows = ex.query(
        "UPDATE game.repairable_recovery_items m \
         SET status = $3, finished_at = $4, withdrawn_item_id = COALESCE(u.withdrawn, m.withdrawn_item_id) \
         FROM unnest($1::bigint[], $2::bigint[]) AS u(member_id, withdrawn) \
         WHERE m.member_id = u.member_id AND m.status = ANY($5::smallint[]) \
         RETURNING m.member_id",
        &[&ids, &withdrawn, &to.encode(), &finished_at, &from],
    ).await?;
    rows.iter().map(|row| RecoveryItemId::new(row.get("member_id"))
        .ok_or_else(|| db::Error::Progress(anyhow::anyhow!("recovery item id outside its band")))).collect()
}

/// Insert one recovery group; `source_item_id` uniqueness makes a retried
/// loss a database conflict the caller reconciles instead of duplicating.
pub(crate) async fn insert_group(ex: &(impl Executor + ?Sized), group: &NewGroup) -> Result<RecoveryGroupId, db::Error> {
    let loss = &group.loss;
    let shape = &loss.shape;
    let id: i64 = ex.query_one(
        "INSERT INTO game.repairable_recovery_groups (\
            beneficiary_id, source_kind, source_item_id, source_owner_id, source_type_id, \
            source_location_id, source_flag_id, source_quantity, source_singleton, \
            source_x, source_y, source_z, source_brain_slot, source_jump_clone_id, \
            source_system_id, occurred_at, position_x, position_y, position_z, \
            loss_id, destination_station_id, payment_expires_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22) \
         RETURNING group_id",
        &[&loss.beneficiary, &loss.kind.encode(), &shape.item.get(), &shape.owner, &shape.type_id,
            &shape.location, &shape.flag, &shape.quantity, &shape.singleton,
            &shape.x, &shape.y, &shape.z, &shape.brain_slot, &shape.jump_clone,
            &loss.system, &loss.lethal_at, &loss.sampled_position[0], &loss.sampled_position[1], &loss.sampled_position[2],
            &group.loss_id, &group.destination_station, &group.payment_expires_at],
    ).await?.get("group_id");
    RecoveryGroupId::new(id).ok_or_else(|| db::Error::Progress(anyhow::anyhow!("recovery group id {id} outside its band")))
}

/// Insert the escrowed members of a group in one statement; the returned IDs
/// are unordered.
pub(crate) async fn insert_items(ex: &(impl Executor + ?Sized), items: &[NewItem]) -> Result<Vec<RecoveryItemId>, db::Error> {
    let len = items.len();
    let mut groups = Vec::with_capacity(len);
    let mut item_ids = Vec::with_capacity(len);
    let mut owners = Vec::with_capacity(len);
    let mut types = Vec::with_capacity(len);
    let mut flags = Vec::with_capacity(len);
    let mut quantities = Vec::with_capacity(len);
    let mut singletons = Vec::with_capacity(len);
    let mut lives = Vec::with_capacity(len);
    let mut repairs = Vec::with_capacity(len);
    for item in items {
        groups.push(item.group.get());
        item_ids.push(item.item.get());
        owners.push(item.original_owner);
        types.push(item.type_id);
        flags.push(item.flag);
        quantities.push(item.quantity);
        singletons.push(item.singleton);
        lives.push(item.last_service_life);
        repairs.push(item.repair_completed_at);
    }
    let statuses = vec![Status::AwaitingPayment.encode(); len];
    let rows = ex.query(
        "INSERT INTO game.repairable_recovery_items (\
            group_id, item_id, original_owner_id, type_id, flag_id, quantity, singleton, \
            last_service_life, repair_completed_at, status) \
         SELECT * FROM unnest(\
            $1::bigint[], $2::bigint[], $3::bigint[], $4::integer[], $5::integer[], \
            $6::bigint[], $7::boolean[], $8::float8[], $9::bigint[], $10::smallint[]) AS u(\
            group_id, item_id, original_owner_id, type_id, flag_id, quantity, singleton, \
            last_service_life, repair_completed_at, status) \
         RETURNING member_id",
        &[&groups, &item_ids, &owners, &types, &flags, &quantities, &singletons, &lives, &repairs, &statuses],
    ).await?;
    rows.iter().map(|row| RecoveryItemId::new(row.get("member_id"))
        .ok_or_else(|| db::Error::Progress(anyhow::anyhow!("recovery item id outside its band")))).collect()
}

/// Move unpaid members to `delivering`: record `paid_at` and the exact amount,
/// then set delivery to `max(paid_at, repair_completed_at) + delivery` and the
/// withdrawal window to `delivery + withdrawal`, all as FILETIME intervals.
/// Only members still `awaiting_payment` move; the returned IDs are unordered.
pub(crate) async fn start_delivery(
    ex: &(impl Executor + ?Sized), paid_at: i64, delivery: i64, withdrawal: i64, payments: &[Payment],
) -> Result<Vec<RecoveryItemId>, db::Error> {
    let ids: Vec<i64> = payments.iter().map(|payment| payment.member.get()).collect();
    let amounts: Vec<String> = payments.iter().map(|payment| payment.amount.decimal()).collect();
    let rows = ex.query(
        "UPDATE game.repairable_recovery_items m \
         SET status = $3, paid_at = $4, paid_amount = u.amount::numeric, \
             delivery_completed_at = GREATEST($4, m.repair_completed_at) + $5, \
             withdraw_expires_at = GREATEST($4, m.repair_completed_at) + $5 + $6 \
         FROM unnest($1::bigint[], $2::text[]) AS u(member_id, amount) \
         WHERE m.member_id = u.member_id AND m.status = $7 \
         RETURNING m.member_id",
        &[&ids, &amounts, &Status::Delivering.encode(), &paid_at, &delivery, &withdrawal, &Status::AwaitingPayment.encode()],
    ).await?;
    rows.iter().map(|row| RecoveryItemId::new(row.get("member_id"))
        .ok_or_else(|| db::Error::Progress(anyhow::anyhow!("recovery item id outside its band")))).collect()
}

/// One group row by identity; beneficiary and ownership checks are caller concerns.
pub(crate) async fn get_group(ex: &(impl Executor + ?Sized), id: RecoveryGroupId) -> Result<Option<Group>, db::Error> {
    ex.query_opt(
        &format!("SELECT {GROUP_COLUMNS} FROM game.repairable_recovery_groups g WHERE group_id = $1"),
        &[&id.get()],
    ).await?.as_ref().map(Group::from_row).transpose()
}

/// One member row by identity; group and beneficiary checks are caller concerns.
pub(crate) async fn get_member(ex: &(impl Executor + ?Sized), id: RecoveryItemId) -> Result<Option<RecoveryItem>, db::Error> {
    ex.query_opt(
        &format!("SELECT {MEMBER_COLUMNS} FROM game.repairable_recovery_items m WHERE member_id = $1"),
        &[&id.get()],
    ).await?.as_ref().map(RecoveryItem::from_row).transpose()
}

/// Every member of one group in member order.
pub(crate) async fn members_of(ex: &(impl Executor + ?Sized), group: RecoveryGroupId) -> Result<Vec<RecoveryItem>, db::Error> {
    ex.query(
        &format!("SELECT {MEMBER_COLUMNS} FROM game.repairable_recovery_items m WHERE group_id = $1 ORDER BY member_id"),
        &[&group.get()],
    ).await?.iter().map(RecoveryItem::from_row).collect()
}

/// The beneficiary of the active member that still escrows one physical item
/// row; `None` means no active recovery member claims it.
pub(crate) async fn escrow_beneficiary(ex: &(impl Executor + ?Sized), item: ItemId) -> Result<Option<i64>, db::Error> {
    Ok(ex.query_opt(
        "SELECT g.beneficiary_id FROM game.repairable_recovery_items m \
         JOIN game.repairable_recovery_groups g ON g.group_id = m.group_id \
         JOIN game.items i ON i.item_id = m.item_id AND i.owner_id = $3 AND i.location_id = g.group_id \
             AND i.type_id = m.type_id AND i.quantity = m.quantity AND i.singleton = m.singleton \
             AND i.flag_id = m.flag_id AND i.brain_slot IS NULL AND i.brain_activated_at IS NULL \
             AND i.brain_expires_at IS NULL AND i.brain_effect_ids IS NULL AND i.jump_clone_id IS NULL \
         WHERE m.item_id = $1 AND m.status < $2",
        &[&item.get(), &Status::Withdrawn.encode(), &infinity_core::consts::NPC_CORP_INTERBUS],
    ).await?.map(|row| row.get("beneficiary_id")))
}

#[cfg(all(test, feature = "tests-storage"))] mod tests;
