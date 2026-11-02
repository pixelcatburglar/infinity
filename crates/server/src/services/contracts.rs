//! contractProxy: contracts between characters and corporations (eve/client/script/ui/shared/neocom/contracts).
//!
//! * Item exchange: the issuer's items (and any reward) go into escrow; the acceptor pays the
//!   price and hands over the requested items from their hangar at the start station, where
//!   both sides receive theirs.
//! * Auction: the escrowed items go to the highest bid when the contract expires, or at once
//!   to a bid that meets the buyout (the contract's collateral). Only the highest bid holds
//!   its ISK; an outbid bidder is refunded.
//! * Courier: the acceptor puts up the collateral and gets a plastic wrap at the start
//!   station; delivering the wrap to their hangar at the end station hands the goods to the
//!   issuer there and pays out reward and collateral. A failed courier contract pays the
//!   collateral to the issuer and leaves the goods with the acceptor, where the wrap is.
//!   The singleton wrap's negative quantity encodes its contract volume in hundredths.
//!
//! Escrowed items stay in game.items, located at -contractID. An expired or rejected
//! contract waits for its issuer to delete it and take the escrow back; expired auctions
//! with bids settle on their own. Public listings pay broker fees and deposits at creation;
//! exchanges also pay sales tax then, while auctions pay tax on the winning amount at sale.
//! Private listings pay only the minimum broker fee. Deposits are refunded when taken, not
//! on deletion or later courier completion or failure. Auction tax rate and cap and the paid
//! deposit are persisted; legacy listings default to zero. Corporate payments and refunds use
//! the captured wallet division. Wallet journals and faction-warfare fee upgrades are unsupported;
//! corporation custody uses authorized office divisions or station Deliveries.
//! Corporation-assigned contracts are available to current members acting personally;
//! corporate courier completion requires current membership and actual package custody.
//! Contract lifecycle mutations guard the previous persisted status and commit atomically;
//! their notifications are delivered only after commit. Overdue couriers remain in progress
//! until manually failed, and their acceptors may still complete them.
//! Contract and item state comes from game tables; station ancestry comes from the loaded SDE.
//! Skill-based outstanding caps follow the client; listing page sizes use contracts configuration.
//! Auction settlement scans run as durable periodic jobs registered through the services
//! scheduler and persisted in game.jobs, not detached maintenance sleeps. Scan errors
//! propagate to the scheduler for retry.

mod custody;
mod fees;
mod ownership;
mod lifecycle;
mod lists;

use lifecycle::{accept, complete, create, delete_one, place_bid, settle_expired};
use lists::{bidding_on, escrow, in_progress, items_in, list_for_owner, login_info, needing_attention, page_info, search, split_stack};

use crate::dao::contracts::{Contract, query::{self, Filter}, shared};
use crate::dao::items::loading::{Item, ItemQuery};
use super::items::load_items;
use super::market::{lock, round2, station};
use crate::dao::items::transfers;
use super::{db_err, keyval, notify_error, user_error, value_id_list, value_text, Call, CallError, CallResult};
use crate::client::{filetime_now, Client};
use crate::consts::{ACCOUNT_CASH, FILETIME_DAY, FILETIME_MINUTE, FLAG_HANGAR, IX_LOCATION_ID, IX_STACK_SIZE, LOCATION_JUNKYARD, TYPE_PLASTIC_WRAP};
use crate::db::Db;
use crate::db::executor::Executor;
use crate::db::outbox::{dispatch_world, Outbox};
use crate::messages;
use crate::sde::Sde;
use crate::space::World;
use marshal::{DbRowDescriptor, DbType, Value};
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use tracing::info;


// appConst.conType*
const TYPE_ITEM_EXCHANGE: i32 = 1;
const TYPE_AUCTION: i32 = 2;
const TYPE_COURIER: i32 = 3;
/// `eve/common/script/util/contractscommon.py:36`, checked by `createContractWnd.py:1371-1372`.
const MAX_COURIER_VOLUME: f64 = 1_200_000.0;
// contractscommon.CONTYPE_*
const CONTYPE_AUCTION_AND_ITEM_EXCHANGE: i64 = 10;
const CONTYPE_ALL: i64 = 11;

// appConst.conStatus*
const STATUS_OUTSTANDING: i32 = 0;
const STATUS_IN_PROGRESS: i32 = 1;
const STATUS_FINISHED: i32 = 4;
const STATUS_REJECTED: i32 = 6;
const STATUS_FAILED: i32 = 7;
const STATUS_DELETED: i32 = 8;

// appConst.conAvail*
const AVAIL_PUBLIC: i64 = 0;
const AVAIL_MYSELF: i64 = 1;
const AVAIL_MY_CORP: i64 = 2;

const SKILL_CONTRACTING: i32 = 25235;
const SKILL_ADVANCED_CONTRACTING: i32 = 73912;
/// contractscommon.NUM_CONTRACTS_BY_SKILL and NUM_CONTRACTS_BY_ADVSKILL.
const NUM_CONTRACTS_BY_SKILL: [i64; 6] = [1, 5, 9, 13, 17, 21];
const NUM_CONTRACTS_BY_ADVSKILL: [i64; 6] = [0, 4, 8, 12, 16, 20];
/// contractscommon.NUM_CONTRACTS_BY_SKILL_CORP and MAX_NUM_CONTRACTS.
const NUM_CONTRACTS_BY_SKILL_CORP: [i64; 6] = [10, 20, 30, 40, 50, 60];
const MAX_INTERNAL_CONTRACTS: i64 = 500;
/// inventorycommon.const.typeCorporationContracting.
const SKILL_CORPORATION_CONTRACTING: i32 = 25233;

/// Attribute of a client object: an old-style instance, a new-style one (contracts.ContractStruct
/// pickles as a NewObj with its __dict__ as state), or a dict.
pub(super) fn field<'a>(v: &'a Value, name: &str) -> Option<&'a Value> {
    let found = match v {
        Value::Instance(_, state) => state.get(name),
        Value::Reduce { data, dict, .. } | Value::NewObj { data, dict, .. } => data
            .items()
            .and_then(|parts| parts.iter().find_map(|p| p.get(name)))
            .or_else(|| dict.iter().find(|(k, _)| k.as_text().is_some_and(|t| t == name)).map(|(_, v)| v)),
        _ => v.get(name),
    };
    found.filter(|v| !v.is_none())
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        Some(Value::Bool(b)) => *b,
        Some(v) => v.as_int().is_some_and(|i| i != 0),
        None => false,
    }
}

/// typeIDs from a list, tuple, set or the keys of a dict.
fn id_list(v: &Value) -> Vec<i32> {
    match v {
        Value::Dict(pairs) => pairs.iter().filter_map(|(k, _)| k.as_int()).map(|i| i as i32).collect(),
        _ => v.items().unwrap_or(&[]).iter().filter_map(|i| i.as_int()).map(|i| i as i32).collect(),
    }
}


impl Contract {
    fn expired(&self, now: i64) -> bool {
        self.expires <= now
    }

    fn overdue(&self, now: i64) -> bool {
        self.status == STATUS_IN_PROGRESS && self.accepted.saturating_add(i64::from(self.num_days).saturating_mul(FILETIME_DAY)) <= now
    }

    /// May `char_id` (of `corp_id`) take this contract?
    async fn available_to(&self, ex: &(impl Executor + ?Sized), char_id: i64, corp_id: i64) -> Result<bool, CallError> {
        Ok(self.assignee == 0 || self.assignee == char_id
            || (self.assignee == corp_id && super::corps::membership(ex, char_id).await?.0 == corp_id))
    }

    fn keyval(&self, sde: &Sde) -> Value {
        let int = Value::Int;
        let start = sde.map.station(self.start);
        let end = sde.map.station(self.end);
        keyval([
            ("contractID", int(self.id)),
            ("type", int(self.kind as i64)),
            ("issuerID", int(self.issuer)),
            ("issuerCorpID", int(self.issuer_corp)),
            ("issuerAllianceID", Value::None),
            ("issuerWalletKey", int(self.issuer_wallet_key as i64)),
            ("forCorp", Value::Bool(self.for_corp)),
            ("availability", int(if self.assignee == 0 { AVAIL_PUBLIC } else if super::corps::is_player_corp(self.assignee) { AVAIL_MY_CORP } else { AVAIL_MYSELF })),
            ("assigneeID", int(self.assignee)),
            ("acceptorID", int(self.acceptor)),
            ("acceptorWalletKey", int(self.acceptor_wallet_key as i64)),
            ("dateIssued", int(self.issued)),
            ("dateExpired", int(self.expires)),
            ("dateAccepted", int(self.accepted)),
            ("dateCompleted", int(self.completed)),
            ("numDays", int(self.num_days as i64)),
            ("expires", int(self.accepted.saturating_add(i64::from(self.num_days).saturating_mul(FILETIME_DAY)))),
            ("startStationID", int(self.start)),
            ("startSolarSystemID", int(start.and_then(|s| s.solar_system_id).unwrap_or(0))),
            ("startRegionID", int(start.and_then(|s| s.region_id).unwrap_or(0))),
            ("endStationID", int(self.end)),
            ("endSolarSystemID", int(end.and_then(|s| s.solar_system_id).unwrap_or(0))),
            ("endRegionID", int(end.and_then(|s| s.region_id).unwrap_or(0))),
            ("price", Value::Float(self.price)),
            ("reward", Value::Float(self.reward)),
            ("collateral", Value::Float(self.collateral)),
            ("title", Value::unicode(self.title.as_str())),
            ("description", Value::unicode(self.description.as_str())),
            ("status", int(self.status as i64)),
            ("volume", Value::Float(self.volume)),
            ("crateID", int(0)),
        ])
    }
}


async fn contract(db: &(impl Executor + ?Sized), id: i64) -> Result<Contract, CallError> {
    query::list(db, Filter::Id(id)).await.map_err(db_err)?.pop().ok_or_else(|| user_error(messages::MSG_CON_CONTRACT_NOT_FOUND, vec![]))
}

async fn set_status(db: &(impl Executor + ?Sized), k: &mut Contract, status: i32, completed: i64) -> Result<(), CallError> {
    if !shared::set_status(db, k.id, k.status, status, completed, k.acceptor, k.acceptor_wallet_key).await.map_err(db_err)? {
        return Err(user_error(messages::MSG_CON_CONTRACT_NOT_OUTSTANDING, vec![]));
    }
    k.status = status;
    k.completed = completed;
    Ok(())
}

/// contractID -> the contract's item KeyVals.
async fn items_of(db: &Db, ids: &[i64]) -> Result<HashMap<i64, Vec<Value>>, CallError> {
    let rows = shared::items(db, ids).await.map_err(db_err)?;
    let mut out: HashMap<i64, Vec<Value>> = HashMap::new();
    for r in &rows {
        let contract_id = r.contract_id;
        out.entry(contract_id).or_default().push(keyval([
            ("contractID", Value::Int(contract_id)),
            ("itemID", Value::Int(r.item_id.unwrap_or(0))),
            ("itemTypeID", Value::Int(r.type_id)),
            ("quantity", Value::Int(r.quantity)),
            ("inCrate", Value::Bool(r.in_crate)),
            ("parentID", Value::Int(0)),
            ("productivityLevel", Value::None),
            ("materialLevel", Value::None),
            ("copy", Value::Int(0)),
            ("licensedProductionRunsRemaining", Value::None),
            ("damage", Value::Float(0.0)),
            ("flagID", Value::Int(FLAG_HANGAR as i64)),
        ]));
    }
    Ok(out)
}

/// contractID -> bid KeyVals, highest first.
async fn bids_of(db: &Db, ids: &[i64]) -> Result<HashMap<i64, Vec<Value>>, CallError> {
    let rows = shared::bids(db, ids).await.map_err(db_err)?;
    let mut out: HashMap<i64, Vec<Value>> = HashMap::new();
    for r in &rows {
        let contract_id = r.contract_id;
        out.entry(contract_id).or_default().push(keyval([
            ("bidID", Value::Int(r.bid_id)),
            ("contractID", Value::Int(contract_id)),
            ("bidderID", Value::Int(r.bidder_id)),
            ("amount", Value::Float(r.amount)),
            ("dateBid", Value::Int(r.date)),
        ]));
    }
    Ok(out)
}

/// `KeyVal(contract, items, bids)` per contract (GetContract, SearchContracts).
async fn bundles(db: &Db, sde: &Sde, list: &[Contract]) -> Result<Vec<Value>, CallError> {
    let ids: Vec<i64> = list.iter().map(|k| k.id).collect();
    let (mut items, mut bids) = (items_of(db, &ids).await?, bids_of(db, &ids).await?);
    Ok(list
        .iter()
        .map(|k| {
            keyval([
                ("contract", k.keyval(sde)),
                ("items", Value::List(items.remove(&k.id).unwrap_or_default())),
                ("bids", Value::List(bids.remove(&k.id).unwrap_or_default())),
            ])
        })
        .collect())
}

/// `KeyVal(contracts, items={contractID: items})` (the my-contracts lists).
async fn listing(db: &Db, sde: &Sde, list: &[Contract]) -> CallResult {
    let ids: Vec<i64> = list.iter().map(|k| k.id).collect();
    let items = items_of(db, &ids).await?;
    Ok(keyval([
        ("contracts", Value::list(list.iter().map(|k| k.keyval(sde)))),
        ("items", Value::Dict(items.into_iter().map(|(id, v)| (Value::Int(id), Value::List(v))).collect())),
    ]))
}




async fn escrowed_ids(db: &(impl Executor + ?Sized), contract_id: i64) -> Result<Vec<i64>, CallError> {
    shared::escrowed_ids(db, contract_id).await.map_err(db_err)
}


/// Lose the escrowed items (a courier package that no longer exists).
async fn destroy_escrow(db: &(impl Executor + ?Sized), contract_id: i64) -> Result<(), CallError> {
    let ids = escrowed_ids(db, contract_id).await?;
    shared::destroy_escrow(db, &ids).await.map_err(db_err)?;
    Ok(())
}


async fn max_outstanding(db: &(impl Executor + ?Sized), char_id: i64, for_corp: bool) -> Result<i64, CallError> {
    let rows = crate::dao::skills::levels::levels_of(db, char_id, &[SKILL_CONTRACTING, SKILL_ADVANCED_CONTRACTING, SKILL_CORPORATION_CONTRACTING]).await.map_err(db_err)?;
    let level = |skill: i32| rows.iter().find(|r| r.type_id == skill).map(|r| r.level.clamp(0, 5) as usize).unwrap_or(0);
    Ok(if for_corp { NUM_CONTRACTS_BY_SKILL_CORP[level(SKILL_CORPORATION_CONTRACTING)] } else { NUM_CONTRACTS_BY_SKILL[level(SKILL_CONTRACTING)] + NUM_CONTRACTS_BY_ADVSKILL[level(SKILL_ADVANCED_CONTRACTING)] })
}

async fn outstanding_counts(db: &(impl Executor + ?Sized), char_id: i64, corp_id: i64) -> Result<shared::OutstandingCounts, CallError> {
    shared::outstanding_counts(db, char_id, corp_id, STATUS_OUTSTANDING).await.map_err(db_err)
}

// ---------------------------------------------------------------- lifecycle


/// Execute one scheduler-driven auction settlement scan.
pub(crate) fn settle_auctions(db: Db, world: Arc<World>, _: crate::scheduler::Job) -> std::pin::Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
    Box::pin(async move {
        settle_expired(&db, world.sde(), &world).await.map_err(crate::startup_call_error)
    })
}


pub async fn dispatch(c: &mut Client, call: &Call<'_>) -> Option<CallResult> {
    if call.service != "contractProxy" {
        return None;
    }
    let int = |i: usize| call.args.get(i).and_then(|v| v.as_int()).unwrap_or(0);
    let for_corp = |i: usize| truthy(call.args.get(i));
    Some(match call.method {
        "CreateContract" => match call.args.first() {
            Some(info) => create(c, info).await,
            None => Err(notify_error(messages::CONTRACT_CREATION_REQUEST_MISSING)),
        },
        "GetContract" => match contract(&c.db, int(0)).await {
            Ok(k) => bundles(&c.db, &c.sde, &[k]).await.map(|mut b| b.pop().unwrap_or(Value::None)),
            Err(e) => Err(e),
        },
        "AcceptContract" => accept(c, int(0), for_corp(1)).await,
        "CompleteContract" => complete(c, int(0), int(1) as i32).await,
        "DeleteContract" => {
            let _trading = lock().await;
            delete_one(c, int(0)).await.map(|_| Value::Bool(true))
        }
        // DeleteMultipleContracts([contractID]) -> (deleted, failed)
        "DeleteMultipleContracts" => {
            let ids = value_id_list(call.args.first());
            let _trading = lock().await;
            let (mut deleted, mut failed) = (vec![], vec![]);
            for id in ids {
                match delete_one(c, id).await {
                    Ok(()) => deleted.push(Value::Int(id)),
                    Err(CallError::User { .. }) => failed.push(Value::Int(id)),
                    Err(e) => return Some(Err(e)),
                }
            }
            Ok(Value::tuple([Value::List(deleted), Value::List(failed)]))
        }
        "PlaceBid" => place_bid(c, int(0), call.args.get(1).and_then(|v| v.as_float()).unwrap_or(0.0), for_corp(2)).await,
        // FinishAuction(contractID, isIssuer): expired auctions settle on their own; settle now if due.
        "FinishAuction" => {
            let r = settle_expired(&c.db, &c.sde, &c.world).await;
            r.map(|_| Value::Bool(true))
        }
        "SearchContracts" => search(c, call.kwargs).await,
        "GetContractListForOwner" => list_for_owner(c, call).await,
        "GetMyBids" => match lists::selected_owner(c, for_corp(0)).await {
            Ok(Some(owner)) => match bidding_on(&c.db, owner).await { Ok(list) => listing(&c.db, &c.sde, &list).await, Err(e) => Err(e) },
            Ok(None) => listing(&c.db, &c.sde, &[]).await,
            Err(e) => Err(e),
        },
        "GetMyExpiredContractList" => match lists::selected_owner(c, for_corp(0)).await {
            Ok(Some(owner)) => match needing_attention(&c.db, owner).await { Ok(list) => listing(&c.db, &c.sde, &list).await, Err(e) => Err(e) },
            Ok(None) => listing(&c.db, &c.sde, &[]).await,
            Err(e) => Err(e),
        },
        // GetMyCurrentContractList(isAccepted, forCorp)
        "GetMyCurrentContractList" => {
            let owner = match lists::selected_owner(c, for_corp(1)).await {
                Ok(Some(owner)) => owner,
                Ok(None) => return Some(listing(&c.db, &c.sde, &[]).await),
                Err(e) => return Some(Err(e)),
            };
            let list = if for_corp(0) {
                in_progress(&c.db, owner).await
            } else {
                query::list(&c.db, Filter::Issuer(owner, STATUS_OUTSTANDING)).await.map_err(db_err)
            };
            match list {
                Ok(list) => listing(&c.db, &c.sde, &list).await,
                Err(e) => Err(e),
            }
        }
        "CollectMyPageInfo" => page_info(c).await,
        "NumOutstandingContracts" => lists::counts(c).await,
        "GetLoginInfo" => login_info(c).await,
        "GetMyContractEscrow" => escrow(c).await,
        // GetItemsInDockableLocation(stationID, forCorp)
        "GetItemsInDockableLocation" => items_in(c, int(0), None, for_corp(1), FLAG_HANGAR).await,
        "GetItemsInContainer" => items_in(c, int(0), Some(int(1)), for_corp(2), int(3) as i32).await,
        "GetNumItemsInContainers" => lists::container_counts(c, int(0), &value_id_list(call.args.get(1)), for_corp(2), int(3) as i32).await,
        "SplitStack" => split_stack(c, int(0), int(1), int(2), for_corp(3), int(4) as i32).await,
        // GetCourierContractFromItemID(wrapItemID) -> the contract, or None
        "GetCourierContractFromItemID" => {
            match query::list(&c.db, Filter::Package(int(0), STATUS_IN_PROGRESS)).await.map_err(db_err) {
                Ok(mut list) => Ok(list.pop().map(|k| k.keyval(&c.sde)).unwrap_or(Value::None)),
                Err(e) => Err(e),
            }
        }
        "DeleteNotification" | "DeleteContractNotification" => Ok(Value::None),
        _ => return None,
    })
}

