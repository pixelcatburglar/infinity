//! Invocation-scoped client context. Client actions are sent to the invocation's
//! request pump and applied to the real client, never through a borrowed pointer.
//! Session and bind data use the same client state as native service handlers.

use std::sync::{Arc, Mutex, atomic::{AtomicBool, AtomicI64, Ordering}};
use mlua::{Lua, MultiValue, Thread, UserData, UserDataFields, UserDataMethods};
use tokio::sync::{mpsc, oneshot};
use infinity_core::consts::GM_ROLES;
use crate::{client::Client, lua::value::{from_lua, WireValue}, services::CallError};
use marshal::Value;

struct Context {
    live: Arc<AtomicBool>,
    tx: mpsc::UnboundedSender<Request>,
    user_id: i64,
    char_id: Arc<AtomicI64>,
    role: i64,
}

struct Session { live: Arc<AtomicBool>, tx: mpsc::UnboundedSender<Request> }

pub(crate) struct AdaptionInvocation<'a> {
    pub host: &'a Arc<crate::lua::reload::Host>,
    pub pin: crate::lua::content::PinnedPolicy,
    pub request: crate::services::ship_adaption::model::Request,
    pub identity: crate::services::identity::Identity,
    pub user: i64,
}

enum Request {
    Get(String, oneshot::Sender<Value>),
    Notify(String, Vec<Value>, oneshot::Sender<()>),
    Change(Vec<(String, Value)>, oneshot::Sender<()>),
    Bind(String, Value, oneshot::Sender<Value>),
    Mutation(Mutation, oneshot::Sender<Result<Option<i64>, CallError>>),
    ContentSnapshot(ContentKind, oneshot::Sender<Result<ContentSnapshot, CallError>>),
    ContentCommit(ContentKind, Option<i64>, oneshot::Sender<Result<Value, CallError>>),
}

#[derive(Clone, Copy)]
enum ContentKind { Sovereignty, ShipAdaption }
enum ContentSnapshot { Wire(Value), Revision(i64) }
impl ContentKind {
    fn parse(namespace: &str) -> mlua::Result<Self> {
        match namespace {
            "sovereignty" => Ok(Self::Sovereignty),
            "ship_adaption" => Ok(Self::ShipAdaption),
            _ => Err(mlua::Error::runtime("unknown content namespace")),
        }
    }
}

pub(super) enum Mutation {
    Wallet { character: i64, amount: crate::isk::IskAmount, credit: bool },
    Create { owner: i64, location: i64, flag: i32, type_id: i32, quantity: i64 },
    Move { container: crate::services::inventory::access::Container, item: i64, flag: Option<i32>, quantity: Option<i64> },
    Delete { item: i64 },
    Mail(crate::services::mail::delivery::SendMail),
    Standing { character: i64, from: i64, value: f64 },
}

pub(super) async fn mutate(ctx: mlua::AnyUserData, operation: Mutation) -> mlua::Result<Option<i64>> {
    let sender = {
        let ctx = ctx.borrow::<Context>()?;
        valid(&ctx.live)?;
        ctx.tx.clone()
    };
    let (tx, rx) = oneshot::channel();
    sender.send(Request::Mutation(operation, tx)).map_err(|_| mlua::Error::runtime("client context has expired"))?;
    rx.await.map_err(|_| mlua::Error::runtime("client context has expired"))?.map_err(lua_error)
}

pub(super) fn lua_error(error: CallError) -> mlua::Error {
    match error {
        CallError::User { msg, dict } => mlua::Error::external(UserError { msg, dict }),
        CallError::Internal(error) => mlua::Error::runtime(format!("{error:#}")),
    }
}

#[derive(Debug)]
struct UserError { msg: String, dict: Vec<(String, Value)> }
impl std::fmt::Display for UserError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.msg) }
}
impl std::error::Error for UserError {}

fn call_error(error: mlua::Error) -> CallError {
    if let Some(user) = error.downcast_ref::<UserError>() {
        CallError::User { msg: user.msg.clone(), dict: user.dict.clone() }
    } else {
        CallError::Internal(error.into())
    }
}

fn valid(live: &AtomicBool) -> mlua::Result<()> {
    if live.load(Ordering::Acquire) { Ok(()) } else { Err(mlua::Error::runtime("client context has expired")) }
}

impl UserData for Session {
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_async_method("get", |_, this, key: String| async move {
            valid(&this.live)?;
            let (tx, rx) = oneshot::channel();
            this.tx.send(Request::Get(key, tx)).map_err(|_| mlua::Error::runtime("client context has expired"))?;
            Ok(WireValue(rx.await.map_err(|_| mlua::Error::runtime("client context has expired"))?))
        });
    }
}

impl UserData for Context {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("user_id", |_, this| { valid(&this.live)?; Ok(this.user_id) });
        fields.add_field_method_get("char_id", |_, this| { valid(&this.live)?; let id = this.char_id.load(Ordering::Acquire); Ok((id != 0).then_some(id)) });
        fields.add_field_method_get("role", |_, this| { valid(&this.live)?; Ok(this.role) });
        fields.add_field_method_get("session", |_, this| {
            valid(&this.live)?;
            Ok(Session { live: Arc::clone(&this.live), tx: this.tx.clone() })
        });
    }
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("is_gm", |_, this, ()| { valid(&this.live)?; Ok(this.role & GM_ROLES != 0) });
        methods.add_async_method("content_snapshot", |lua, this, namespace: String| async move {
            valid(&this.live)?;
            let kind = ContentKind::parse(&namespace)?;
            let (tx, rx) = oneshot::channel();
            this.tx.send(Request::ContentSnapshot(kind, tx)).map_err(|_| mlua::Error::runtime("client context has expired"))?;
            match rx.await.map_err(|_| mlua::Error::runtime("client context has expired"))?.map_err(lua_error)? {
                ContentSnapshot::Wire(value) => Ok(mlua::Value::UserData(lua.create_userdata(WireValue(value))?)),
                ContentSnapshot::Revision(value) => Ok(mlua::Value::Integer(value)),
            }
        });
        // A read-only namespace may use commit to deliver its original request's
        // response; the bound invocation, never the script, determines the operation.
        methods.add_async_method("content_commit", |_, this, (namespace, revision): (String, mlua::Value)| async move {
            valid(&this.live)?;
            let kind = ContentKind::parse(&namespace)?;
            let revision = match revision { mlua::Value::Integer(value) if value >= 0 => Some(value), _ => None };
            let (tx, rx) = oneshot::channel();
            this.tx.send(Request::ContentCommit(kind, revision, tx)).map_err(|_| mlua::Error::runtime("client context has expired"))?;
            Ok(WireValue(rx.await.map_err(|_| mlua::Error::runtime("client context has expired"))?.map_err(lua_error)?))
        });
        methods.add_async_method("notify", |_, this, (event, args): (String, mlua::Variadic<mlua::Value>)| async move {
            valid(&this.live)?;
            let args = args.into_iter().map(from_lua).collect::<mlua::Result<Vec<_>>>()?;
            let (tx, rx) = oneshot::channel();
            this.tx.send(Request::Notify(event, args, tx)).map_err(|_| mlua::Error::runtime("client context has expired"))?;
            rx.await.map_err(|_| mlua::Error::runtime("client context has expired"))
        });
        methods.add_method("user_error", |_, this, (msg, dict): (String, mlua::Table)| {
            valid(&this.live)?;
            let dict = dict.pairs::<String, mlua::Value>().map(|entry| {
                let (key, value) = entry?;
                Ok((key, from_lua(value)?))
            }).collect::<mlua::Result<Vec<_>>>()?;
            Err::<(), _>(mlua::Error::external(UserError { msg, dict }))
        });
        methods.add_async_method("change_session", |_, this, updates: mlua::Table| async move {
            valid(&this.live)?;
            let updates = updates.pairs::<String, mlua::Value>().map(|entry| {
                let (key, value) = entry?;
                Ok((key, from_lua(value)?))
            }).collect::<mlua::Result<Vec<_>>>()?;
            let (tx, rx) = oneshot::channel();
            this.tx.send(Request::Change(updates, tx)).map_err(|_| mlua::Error::runtime("client context has expired"))?;
            rx.await.map_err(|_| mlua::Error::runtime("client context has expired"))
        });
        methods.add_async_method("bind", |_, this, (service, params): (String, mlua::Value)| async move {
            valid(&this.live)?;
            let params = from_lua(params)?;
            let (tx, rx) = oneshot::channel();
            this.tx.send(Request::Bind(service, params, tx)).map_err(|_| mlua::Error::runtime("client context has expired"))?;
            Ok(WireValue(rx.await.map_err(|_| mlua::Error::runtime("client context has expired"))?))
        });
    }
}


async fn apply_mutation(c: &mut Client, operation: Mutation) -> Result<Option<i64>, CallError> {
    use crate::{db::outbox::{dispatch_client, dispatch_world, Outbox}, services::{inventory, items, market, standings, wallets}};
    match operation {
        Mutation::Move { container, item, flag, quantity } => inventory::movement::move_item(&crate::services::identity::Identity::of_client(c), &c.db, &c.world, &c.sde, c.session.int("solarsystemid"), c.notify_tx.clone(), &container, item, None, flag, quantity, |n| crate::services::effects::online::dispatch_notification(n, &c.world, &c.notify_tx, c.session.int("charid"))).await,
        Mutation::Delete { item } => {
            let location = inventory::trashing::resolve_location(c, item).await?;
            inventory::trashing::trash_items(c, &[item], location).await?;
            Ok(None)
        }
        Mutation::Mail(input) => {
            let id = crate::services::mail::delivery::send_mail(&c.db, &c.world, c.session.int("charid").unwrap_or(0), c.session.int("corpid"), input).await?;
            Ok(Some(id))
        }
        Mutation::Wallet { character, amount, credit } => {
            let _lock = market::lock().await;
            let world = c.world.clone();
            c.db.transaction(
                |tx, outbox: &mut Outbox| Box::pin(async move {
                    if credit { wallets::credit(tx, outbox, character, amount).await }
                    else { wallets::debit(tx, outbox, character, amount).await }
                }),
                |value, outbox| async move { dispatch_world(value, outbox, &world) },
            ).await?;
            Ok(None)
        }
        Mutation::Create { owner, location, flag, type_id, quantity } => {
            let _lock = market::lock().await;
            let db = c.db.clone();
            let sde = c.sde.clone();
            db.transaction(
                |tx, outbox: &mut Outbox| {
                    let sde = sde.clone();
                    Box::pin(async move { items::deposit(tx, &sde, outbox, &crate::services::inventory::destination::Destination::owned(owner, location, flag), type_id, quantity, 0).await.map(|_| ()) })
                },
                |value, outbox| async move { dispatch_client(value, outbox, c) },
            ).await?;
            Ok(None)
        }
        Mutation::Standing { character, from, value } => {
            let world = c.world.clone();
            c.db.transaction(
                |tx, outbox: &mut Outbox| Box::pin(async move {
                    standings::set(tx, character, from, value).await?;
                    standings::notify_set(outbox, character, from, value);
                    Ok(())
                }),
                |value, outbox| async move { dispatch_world(value, outbox, &world) },
            ).await?;
            Ok(None)
        }
    }
}

async fn run_adaption(c: &mut Client, invocation: AdaptionInvocation<'_>, snapshot: crate::dao::ship_adaption::Snapshot, live: &AtomicBool, deadline: tokio::time::Instant) -> Result<Value, CallError> {
    use crate::{lua::content::{adaption, protocol::{self, Identity, Response}}, services::{db_err, ship_adaption::{model::Request, operations, policy::{self, Input}, wire}}};
    let AdaptionInvocation { host, pin, request, identity, user } = invocation;
    let definitions = pin.adaption().ok_or_else(|| db_err(anyhow::anyhow!("missing adaptation catalogue")))?.clone();
    let character = identity.char_id;
    if c.login.user_id != user || c.session.int("charid") != Some(character) {
        return Err(operations::refusal(&definitions, adaption::INVALID));
    }
    let wire_identity = || Identity { user, character };
    match request {
        Request::Tree(tree) => {
            if !definitions.published_tree(tree) { return Err(operations::refusal(&definitions, adaption::INVALID)); }
            let text = protocol::encode_content_text(Response::Tree { identity: wire_identity(), tree }).map_err(db_err)?;
            c.notify("OnTreeResponsed", vec![Value::str(text)]);
            return Ok(Value::None);
        }
        Request::Missions => {
            let text = protocol::encode_content_text(Response::Mission { identity: wire_identity() }).map_err(db_err)?;
            c.notify("OnMissionResponsed", vec![Value::str(text)]);
            return Ok(Value::None);
        }
        Request::User => {
            let text = wire::user(&definitions, &snapshot, user, character)?;
            c.notify("OnUserResponsed", vec![Value::str(text)]);
            return Ok(Value::None);
        }
        Request::Inject { item, quantity } => {
            let row = crate::services::items::load_items(&c.db, &c.sde, crate::dao::items::loading::ItemQuery::Id(item)).await.map_err(db_err)?.pop()
                .ok_or_else(|| operations::refusal(&definitions, adaption::ITEM))?;
            let credit = policy::conversion(&c.sde, &definitions, row.type_id, quantity);
            let input = Input { definitions: definitions.clone(), item_type: Some(row.type_id), quantity, credit, issuer: 0, chips: Arc::new(Vec::new()) };
            let plan = policy::plan(&pin, input, deadline.into_std())?.ok_or_else(|| operations::refusal(&definitions, adaption::ITEM))?;
            return submit_adaption(c, host.clone(), pin, identity, user, snapshot.revision(), plan, Some(item), live, deadline).await;
        }
        Request::Exchange { issuer, chips } => {
            let input = Input { definitions: definitions.clone(), item_type: None, quantity: 0, credit: None, issuer, chips: Arc::new(chips) };
            let plan = policy::plan(&pin, input, deadline.into_std())?.ok_or_else(|| operations::refusal(&definitions, adaption::CHIPS))?;
            return submit_adaption(c, host.clone(), pin, identity, user, snapshot.revision(), plan, None, live, deadline).await;
        }
    }
}

async fn submit_adaption(c: &Client, host: Arc<crate::lua::reload::Host>, pin: crate::lua::content::PinnedPolicy, identity: crate::services::identity::Identity, user: i64, expected: i64, plan: crate::services::ship_adaption::policy::PreparedPlan, item: Option<i64>, live: &AtomicBool, deadline: tokio::time::Instant) -> Result<Value, CallError> {
    use crate::{lua::content::{adaption, AdmissionError}, services::{market, ship_adaption::operations}};
    let db = c.db.clone();
    let world = c.world.clone();
    let sender = c.notify_tx.clone();
    let sde = c.sde.clone();
    let (tx, rx) = oneshot::channel();
    if !live.load(Ordering::Acquire) || tokio::time::Instant::now() >= deadline {
        return Err(CallError::Internal(super::super::vm::budget_exceeded().into()));
    }
    tokio::spawn(async move {
        let _market = market::lock().await;
        let result = match host.admit(&pin) {
            Ok(admitted) => match plan.admit(Arc::new(admitted)) {
                Ok(Some(plan)) => operations::commit(&db, &world, &sender, sde, identity, user, expected, Arc::new(plan), item).await,
                Ok(None) => Err(operations::refusal(pin.adaption().expect("validated adaptation package"), adaption::CHIPS)),
                Err(error) => Err(error),
            },
            Err(AdmissionError::Stale) => Err(operations::refusal(pin.adaption().expect("validated adaptation package"), adaption::CONFLICT)),
            Err(AdmissionError::Infrastructure(error)) => Err(error),
        };
        if let Err(Err(error)) = tx.send(result) { tracing::warn!("adaptation owned operation failed after caller departed: {error:?}"); }
    });
    rx.await.map_err(|_| CallError::Internal(anyhow::anyhow!("adaptation owned operation terminated")))??;
    Ok(Value::None)
}

struct InvocationGuard { live: Arc<AtomicBool>, hook_thread: Arc<Mutex<Option<Thread>>> }
impl Drop for InvocationGuard {
    fn drop(&mut self) {
        self.live.store(false, Ordering::Release);
        if let Some(thread) = self.hook_thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            thread.remove_hook();
        }
    }
}

pub(crate) async fn invoke(c: &mut Client, lua: &Lua, invocation: super::super::vm::Invocation, args: Vec<mlua::Value>, deadline: tokio::time::Instant, query: Option<(i64, crate::services::sovereignty::queries::Query)>, mut adaption: Option<AdaptionInvocation<'_>>) -> Result<MultiValue, CallError> {
    let live = Arc::new(AtomicBool::new(true));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let char_id = Arc::new(AtomicI64::new(c.session.int("charid").unwrap_or(0)));
    let ctx = Context { live: live.clone(), tx, user_id: c.login.user_id, char_id: Arc::clone(&char_id), role: c.login.role };
    let ctx = lua.create_userdata(ctx).map_err(|e| CallError::Internal(e.into()))?;
    let mut values = MultiValue::from_vec(args);
    values.push_front(mlua::Value::UserData(ctx));
    let call = invocation.function.call_async::<MultiValue>(values);
    tokio::pin!(call);
    // Expire the context and remove the hook before mlua terminates and recycles the coroutine.
    let _guard = InvocationGuard { live: live.clone(), hook_thread: invocation.hook_thread };
    let mut prepared = None;
    loop {
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline) => return Err(CallError::Internal(super::super::vm::budget_exceeded().into())),
            result = &mut call => return result.map_err(call_error),
            request = rx.recv() => match request {
                Some(Request::Get(key, tx)) => { let _ = tx.send(c.session.get(&key).clone()); }
                Some(Request::Notify(event, args, tx)) => { c.notify(&event, args); let _ = tx.send(()); }
                Some(Request::Change(updates, tx)) => {
                    c.change_session(updates.iter().map(|(key, value)| (key.as_str(), value.clone())).collect());
                    char_id.store(c.session.int("charid").unwrap_or(0), Ordering::Release);
                    let _ = tx.send(());
                }
                Some(Request::Bind(service, params, tx)) => { let _ = tx.send(c.bind(&service, params)); }
                Some(Request::ContentSnapshot(ContentKind::Sovereignty, tx)) => {
                    let result = match query {
                        Some((character, query)) => {
                            use crate::services::sovereignty::queries::Query;
                            let alliance = match query {
                                Query::AllianceStructures { alliance_id } | Query::CapitalSystemInfo { alliance_id } => Some(alliance_id),
                                _ => None,
                            };
                            if let Some(alliance) = alliance {
                                match crate::dao::characters::members::organization(&c.db, character).await.map_err(crate::services::db_err) {
                                    Ok(Some(org)) if org.alliance_id == Some(alliance) => crate::services::sovereignty::queries::read(&c.db, &c.sde, query).await,
                                    Ok(_) => Err(CallError::Internal(anyhow::anyhow!("sovereignty alliance membership required"))),
                                    Err(error) => Err(error),
                                }
                            } else {
                                crate::services::sovereignty::queries::read(&c.db, &c.sde, query).await
                            }
                        }
                        None => Err(CallError::Internal(anyhow::anyhow!("sovereignty snapshot unavailable for this invocation"))),
                    };
                    let _ = tx.send(result.map(ContentSnapshot::Wire));
                }
                Some(Request::ContentSnapshot(ContentKind::ShipAdaption, tx)) => {
                    let result = if let Some(invocation) = adaption.take() {
                        let definitions = invocation.pin.adaption().expect("validated adaptation package");
                        let character = invocation.identity.char_id;
                        if c.login.user_id != invocation.user || c.session.int("charid") != Some(character) {
                            Err(crate::services::ship_adaption::operations::refusal(definitions, crate::lua::content::adaption::INVALID))
                        } else {
                            match crate::dao::characters::selection::owns(&c.db, character, invocation.user).await.map_err(crate::services::db_err) {
                                Ok(true) => match crate::dao::ship_adaption::snapshot(&c.db, character).await.map_err(crate::services::db_err) {
                                    Ok(snapshot) => { let revision = snapshot.revision(); prepared = Some((invocation, snapshot)); Ok(revision) }
                                    Err(error) => Err(error),
                                },
                                Ok(false) => Err(crate::services::ship_adaption::operations::refusal(definitions, crate::lua::content::adaption::INVALID)),
                                Err(error) => Err(error),
                            }
                        }
                    } else { Err(CallError::Internal(anyhow::anyhow!("adaptation snapshot unavailable for this invocation"))) };
                    let _ = tx.send(result.map(ContentSnapshot::Revision));
                }
                Some(Request::ContentCommit(ContentKind::ShipAdaption, revision, tx)) => {
                    let result = match prepared.take() {
                        Some((invocation, snapshot)) if revision == Some(snapshot.revision()) => tokio::select! {
                            result = run_adaption(c, invocation, snapshot, &live, deadline) => result,
                            _ = tokio::time::sleep_until(deadline) => return Err(CallError::Internal(super::super::vm::budget_exceeded().into())),
                        },
                        Some((invocation, _)) => Err(crate::services::ship_adaption::operations::refusal(invocation.pin.adaption().expect("validated adaptation package"), crate::lua::content::adaption::CONFLICT)),
                        None => Err(CallError::Internal(anyhow::anyhow!("adaptation request unavailable for this invocation"))),
                    };
                    let _ = tx.send(result);
                }
                Some(Request::ContentCommit(ContentKind::Sovereignty, _, tx)) => {
                    let _ = tx.send(Err(CallError::Internal(anyhow::anyhow!("sovereignty content has no commit operation"))));
                }
                Some(Request::Mutation(operation, tx)) => {
                    let result = apply_mutation(c, operation).await;
                    if tokio::time::Instant::now() >= deadline {
                        return Err(CallError::Internal(super::super::vm::budget_exceeded().into()));
                    }
                    let _ = tx.send(result);
                }
                None => return Err(CallError::Internal(anyhow::anyhow!("client context channel closed"))),
            }
        }
    }
}

#[cfg(test)]
mod tests;
