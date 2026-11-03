//! Invocation-scoped client context. Client actions are sent to the invocation's
//! request pump and applied to the real client, never through a borrowed pointer.
//! Session and bind data use the same client state as native service handlers.

use std::sync::{Arc, Mutex, atomic::{AtomicBool, AtomicI64, Ordering}};
use mlua::{Lua, MultiValue, Thread, UserData, UserDataFields, UserDataMethods};
use tokio::sync::{mpsc, oneshot};
use crate::{client::Client, consts::GM_ROLES, lua::value::{from_lua, WireValue}, services::CallError};
use marshal::Value;

struct Context {
    live: Arc<AtomicBool>,
    tx: mpsc::UnboundedSender<Request>,
    user_id: i64,
    char_id: Arc<AtomicI64>,
    role: i64,
}

struct Session { live: Arc<AtomicBool>, tx: mpsc::UnboundedSender<Request> }

enum Request {
    Get(String, oneshot::Sender<Value>),
    Notify(String, Vec<Value>, oneshot::Sender<()>),
    Change(Vec<(String, Value)>, oneshot::Sender<()>),
    Bind(String, Value, oneshot::Sender<Value>),
    Mutation(Mutation, oneshot::Sender<Result<Option<i64>, CallError>>),
}

pub(super) enum Mutation {
    Wallet { character: i64, amount: f64, credit: bool },
    Create { owner: i64, location: i64, flag: i32, type_id: i32, quantity: i32 },
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
    let mut cause: &(dyn std::error::Error + 'static) = &error;
    loop {
        if let Some(user) = cause.downcast_ref::<UserError>() {
            return CallError::User { msg: user.msg.clone(), dict: user.dict.clone() };
        }
        match cause.source() {
            Some(next) => cause = next,
            None => return CallError::Internal(error.into()),
        }
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
        Mutation::Delete { item } => { inventory::stacks::trash_items(c, &[item]).await?; Ok(None) }
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
                    Box::pin(async move { items::deposit(tx, &sde, outbox, owner, location, flag, type_id, quantity, 0).await })
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

struct InvocationGuard { live: Arc<AtomicBool>, hook_thread: Arc<Mutex<Option<Thread>>> }
impl Drop for InvocationGuard {
    fn drop(&mut self) {
        self.live.store(false, Ordering::Release);
        if let Some(thread) = self.hook_thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            thread.remove_hook();
        }
    }
}

pub async fn invoke(c: &mut Client, lua: &Lua, invocation: super::super::vm::Invocation, args: Vec<mlua::Value>, deadline: tokio::time::Instant) -> Result<MultiValue, CallError> {
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
    let _guard = InvocationGuard { live, hook_thread: invocation.hook_thread };
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
