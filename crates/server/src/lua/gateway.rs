//! Scoped gateway Lua calls with one owned, request-bound host operation.
//! Uses the current Lua generation and protobuf text adapter; it exposes no client
//! or general-purpose host capabilities and implements no gameplay operations.

use std::{future::Future, pin::Pin, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};
use mlua::{UserData, UserDataFields, UserDataMethods};
use prost_reflect::{DynamicMessage, ReflectMessage};
use crate::gateway::error::GatewayError;
use super::{content::protocol::gateway as protocol, registry::Registry, vm};

pub(crate) type GatewayOperation = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = Result<DynamicMessage, GatewayError>> + Send + 'static>> + Send + 'static>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GatewayMode { Read, Commit }

#[derive(Clone)]
pub(crate) struct GatewayInvocationIdentity {
    pub user_id: Option<i64>,
    pub character_id: Option<i64>,
    pub correlation_uuid: Option<[u8; 16]>,
}

struct Admission {
    live: AtomicBool,
    admitted: AtomicBool,
    invalid: AtomicBool,
}

struct Token(Arc<Admission>);
impl UserData for Token {}

struct Context {
    state: Arc<Admission>,
    identity: GatewayInvocationIdentity,
    mode: GatewayMode,
}

impl Context {
    fn call(&self, namespace: mlua::Value, mode: GatewayMode) -> mlua::Result<Token> {
        let namespace_valid = matches!(&namespace, mlua::Value::String(value) if value.as_bytes().as_ref() == b"daily_goals");
        if !self.state.live.load(Ordering::Acquire) || !namespace_valid || self.mode != mode
            || self.state.admitted.swap(true, Ordering::AcqRel) {
            self.state.invalid.store(true, Ordering::Release);
            return Err(mlua::Error::runtime("invalid gateway context"));
        }
        Ok(Token(self.state.clone()))
    }
}

impl UserData for Context {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_field_method_get("user_id", |_, this| { if !this.state.live.load(Ordering::Acquire) { return Err(mlua::Error::runtime("gateway context expired")); } Ok(this.identity.user_id) });
        fields.add_field_method_get("character_id", |_, this| { if !this.state.live.load(Ordering::Acquire) { return Err(mlua::Error::runtime("gateway context expired")); } Ok(this.identity.character_id) });
        fields.add_field_method_get("correlation_uuid", |lua, this| { if !this.state.live.load(Ordering::Acquire) { return Err(mlua::Error::runtime("gateway context expired")); } this.identity.correlation_uuid.map(|uuid| lua.create_string(uuid)).transpose() });
    }
    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("content_read", |_, this, namespace: mlua::Value| this.call(namespace, GatewayMode::Read));
        methods.add_method("content_commit", |_, this, namespace: mlua::Value| this.call(namespace, GatewayMode::Commit));
    }
}

struct Guard { state: Arc<Admission>, hook: Arc<Mutex<Option<mlua::Thread>>> }
impl Drop for Guard {
    fn drop(&mut self) {
        self.state.live.store(false, Ordering::Release);
        if let Ok(mut hook) = self.hook.lock() { if let Some(thread) = hook.take() { thread.remove_hook(); } }
    }
}

fn fault() -> GatewayError { GatewayError::Internal(anyhow::anyhow!("gateway Lua invocation failed")) }

/// Pins the current generation. The operation owns every resource it captures.
pub(crate) async fn invoke_gateway(name: &str, request: &DynamicMessage, identity: GatewayInvocationIdentity, mode: GatewayMode, operation: GatewayOperation) -> Result<DynamicMessage, GatewayError> {
    let host = super::HOST.get().ok_or_else(fault)?;
    invoke_in_generation(host.snapshot(), &host.config, name, request, identity, mode, operation).await
}

async fn invoke_in_generation(registry: Arc<Registry>, config: &crate::config::lua::LuaConfig, name: &str, request: &DynamicMessage, identity: GatewayInvocationIdentity, mode: GatewayMode, operation: GatewayOperation) -> Result<DynamicMessage, GatewayError> {
    let (request_desc, response_desc) = protocol::descriptors(name).map_err(|_| fault())?;
    let text = protocol::encode(request, &request_desc).map_err(|_| fault())?;
    let handler = registry.claims.gateway.get(name).ok_or_else(fault)?;
    let deadline = Instant::now() + Duration::from_millis(config.call_budget_ms);
    let invocation = vm::budgeted_invocation(&registry.lua, handler.function.clone(), config, deadline).map_err(|_| fault())?;
    let state = Arc::new(Admission { live: AtomicBool::new(true), admitted: AtomicBool::new(false), invalid: AtomicBool::new(false) });
    let context = registry.lua.create_userdata(Context { state: state.clone(), identity, mode }).map_err(|_| fault())?;
    let guard = Guard { state: state.clone(), hook: invocation.hook_thread };
    let mut result = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline),
        invocation.function.call_async::<mlua::MultiValue>((context, text))).await.map_err(|_| fault())?.map_err(|_| fault())?;
    let valid = match (result.len(), result.pop_front()) {
        (1, Some(mlua::Value::UserData(token))) => token.borrow::<Token>().map(|token| Arc::ptr_eq(&token.0, &state)).unwrap_or(false),
        _ => false,
    };
    if !valid || !state.admitted.load(Ordering::Acquire) || state.invalid.load(Ordering::Acquire) || Instant::now() >= deadline {
        return Err(fault());
    }
    drop(guard);
    let task = tokio::spawn(async move { operation().await });
    let response = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), task)
        .await.map_err(|_| fault())?.map_err(|_| fault())??;
    if response.descriptor() != response_desc { return Err(fault()); }
    Ok(response)
}

#[cfg(test)] mod tests;
