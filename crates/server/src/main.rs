//! Infinity emulator server: proxy and sol node, gateway, chat, image server and CDN.
//! The handshake mirrors the client's macho networking; state comes from the game
//! database and SDE. Interrupted reloads return to cargo before workers and
//! listeners start. A signal or a due planned shutdown drains the scheduler and
//! flushes positions with bounded waits; existing sessions are not drained.
//! Authenticated account ownership is process-local; replacements wait for
//! prior cleanup. Cross-process session coordination is not supported.

mod admission;
mod build_identity;
mod chat;
mod client;
mod config;
mod dao;
mod db;
mod gateway;
mod handshake;
mod hash;
mod histogram;
mod images;
mod isk;
mod npc_security;
mod live_sessions;
mod logging;
mod lua;
mod panic_report;
mod poison;
mod progress;
mod runtime_watchdog;
mod scheduler;
mod sde;
mod services;
mod shutdown;
mod space;

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{error, info, warn};

/// Server allocation policy: initial ephemeral client ID.
const FIRST_CLIENT_ID: i64 = 1_000_000_000_000;

fn startup_call_error(error: services::CallError) -> anyhow::Error {
    match error {
        services::CallError::Internal(error) => error,
        services::CallError::User { msg, .. } => anyhow::anyhow!(msg),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = Arc::new(config::Config::load()?);
    let _logging_guards = logging::init(&cfg.logging)?;
    panic_report::install();
    build_identity::log_startup();
    info!(source = cfg.source(), "configuration loaded");
    config::init_game(cfg.game_config())?;
    gateway::validate()?;
    let db = db::Db::connect(&cfg.database).await?
        .with_progress_registry(progress::Registry::new(vec![progress::lua_observer], vec![services::skill_plans::progress::registration(), services::daily_goals::progress::registration(), services::sovereignty::indices::progress::registration()])?);
    progress::migration::migrate(&db).await?;
    let db = progress::reconcile(db).await?;
    services::corps::shares::seed(&db).await.map_err(startup_call_error)?;
    let sde = Arc::new(sde::Sde::load(&db).await?);
    services::standings::initialize(&sde);
    services::corps::initialize(&db, &sde).await.map_err(startup_call_error)?;
    services::skill_plans::validation::audit(&db, &sde).await?;
    let types = sde.types.len();
    info!(target: "infinity_server::db", "database ready ({types} types in SDE)");
    services::recover_pending_reloads(&db, &sde).await.map_err(startup_call_error)?;
    if let Err(services::CallError::Internal(e)) = services::unbury_sites(&db, &sde).await {
        tracing::warn!("moving sites out of planets failed: {e:#}");
    }
    let world = space::World::new(db.clone(), sde.clone(), cfg.server.system_capacity);
    let hub = chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    services::trade::recover(&db, &world, sde.clone()).await.map_err(startup_call_error)?;
    lua::initialize_content(&cfg.lua, &db, &sde).await?;
    lua::start(&cfg, &db, &sde, &world, &hub).await?;
    if cfg.lua.enabled {
        lua::content_policy("sovereignty").map_err(startup_call_error)?;
    }
    // Resolve the development-index capability outside every market, database
    // and actor lock; the world's slot and loaded actors receive the result.
    let policies = lua::retained_policies();
    let capability = match &policies {
        Ok(policies) => {
            let resources = progress::ContentProgressResources { db: db.clone(), sde: sde.clone(), world: world.clone(), policies: policies.clone() };
            services::sovereignty::indices::query::initialize(&resources).await
        }
        Err(services::CallError::Internal(error)) if error.downcast_ref::<lua::reload::HostUnavailable>().is_some() =>
            services::sovereignty::indices::model::IndexCapability::Unavailable(services::sovereignty::indices::model::IndexUnavailable::HostUnavailable),
        Err(error) => services::sovereignty::indices::model::IndexCapability::Invalid(format!("{error:?}").into()),
    };
    space::sovereignty::indices::install_policy(&world, capability);
    let sovereignty_resources = services::sovereignty::Resources { db: db.clone(), sde: sde.clone(), world: world.clone() };
    services::sovereignty::recovery::recover(&sovereignty_resources).await.map_err(startup_call_error)?;
    space::sovereignty::open(&world);
    let restored = services::bounty::restore_activations(&db, &world).await.map_err(startup_call_error)?;
    info!(restored, "restored active kill rights");
    services::skills::completion::seed(&db).await.map_err(startup_call_error)?;
    services::skill_plans::recovery::seed(&db).await.map_err(startup_call_error)?;
    services::industry::completion::seed(&db).await.map_err(startup_call_error)?;
    services::subscriptions::deadlines::recover(&db).await.map_err(startup_call_error)?;
    services::ship_cosmetics_mgr::expiry::recover(&db, &world).await.map_err(startup_call_error)?;
    let (scheduler_stop, scheduler_shutdown) = tokio::sync::watch::channel(false);
    let mut registrations = services::scheduler_registrations();
    registrations.push(services::sovereignty::jobs::registration());
    registrations.push(services::sovereignty::indices::jobs::registration());
    registrations.push(scheduler::periodic(progress::DRAIN_KIND, progress::drain, cfg.scheduler.poll_secs));
    registrations.push(scheduler::periodic(services::daily_goals::rollover::KIND, services::daily_goals::rollover::run, cfg.scheduler.poll_secs));
    let mut scheduler_task = scheduler::runner::start(db.clone(), world.clone(), registrations, cfg.scheduler.clone(), scheduler_shutdown)?;
    scheduler::seed(&db).await?;
    progress::seed(&db).await?;
    if cfg.lua.enabled {
        let lua_policies = lua::retained_policies().map_err(startup_call_error)?;
        services::sovereignty::indices::jobs::seed(&db, &lua_policies).await.map_err(startup_call_error)?;
    }
    services::daily_goals::rollover::seed(&db).await?;
    let watchdog_task = tokio::spawn(runtime_watchdog::probe());
    let gateway_task = tokio::spawn(log_exit("public gateway", gateway::serve(cfg.gateway.bind.clone(), gateway::Gateway::new(db.clone(), world.clone(), cfg.clone()))));
    let chat_task = tokio::spawn(log_exit("chat", chat::serve(cfg.chat.bind.clone(), hub.clone(), std::time::Duration::from_secs(cfg.server.client_write_timeout_secs))));
    let upstream = images::Upstream::new(&cfg.images.upstream, cfg.images.upstream_timeout_secs);
    let upload_limit = cfg.images.upload_limit_bytes();
    let image_task = tokio::spawn(log_exit("image server", images::serve(images::ListenerKind::Images, cfg.images.bind.clone(), cfg.images.dir.clone().into(), upstream.clone(), upload_limit)));
    let cdn_task = tokio::spawn(log_exit("content cdn", images::serve(images::ListenerKind::Cdn, cfg.cdn.bind.clone(), cfg.images.dir.clone().into(), upstream, upload_limit)));
    let listener = TcpListener::bind(&cfg.server.bind).await.with_context(|| format!("binding {}", cfg.server.bind))?;
    info!("listening on {} (macho {}, build {}, {}@{})", cfg.server.bind, cfg.server.macho_version, cfg.server.boot_build, cfg.server.boot_codename, cfg.server.boot_region);
    db::timing::startup_complete();

    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("registering SIGTERM handler")?;
    let shutdown = async {
        #[cfg(unix)]
        {
            tokio::select! {
                result = tokio::signal::ctrl_c() => result.context("handling SIGINT"),
                result = sigterm.recv() => result.context("SIGTERM handler closed").map(|_| ()),
                () = shutdown::wait() => Ok(()),
            }
        }
        #[cfg(not(unix))]
        {
            tokio::select! {
                result = tokio::signal::ctrl_c() => result.context("handling SIGINT"),
                () = shutdown::wait() => Ok(()),
            }
        }
    };
    tokio::pin!(shutdown);
    let next_client_id = Arc::new(AtomicI64::new(FIRST_CLIENT_ID));
    let gate = admission::Admission::new(cfg.server.max_pending_handshakes, cfg.server.max_pending_handshakes_per_address);
    // The biased select polls and registers the signal handler before accepting clients.
    loop {
        let (stream, peer) = tokio::select! {
            biased;
            result = &mut shutdown => {
                result?;
                space::sovereignty::close(&world);
                let _ = scheduler_stop.send(true);
                break;
            }
            result = listener.accept() => result?,
        };
        let accepted_at = std::time::Instant::now();
        let Some(permit) = gate.try_acquire(peer.ip()) else { continue };
        let Ok(handshake_deadline) = admission::deadline(accepted_at, cfg.server.handshake_timeout_secs) else { continue };
        stream.set_nodelay(true).ok();
        let (cfg, db, sde, ids, hub, world) = (cfg.clone(), db.clone(), sde.clone(), next_client_id.clone(), hub.clone(), world.clone());
        tokio::spawn(async move {
            let client_id = ids.fetch_add(1, Ordering::Relaxed);
            let conn = infinity_wire::conn::Conn::new(stream, peer)
                .with_write_timeout(std::time::Duration::from_secs(cfg.server.client_write_timeout_secs));
            info!("connection from {peer} (clientID {client_id})");
            let (mut conn, login, registration) = match handshake::run(conn, &cfg, &db, client_id, permit, handshake_deadline).await {
                Ok(Some(triple)) => triple,
                Ok(None) => {
                    info!("{peer} closed during handshake");
                    return;
                }
                Err(e) => {
                    error!("{peer} handshake failed: {e:#}");
                    return;
                }
            };
            if registration.is_replaced() {
                if let Err(e) = conn.send(&infinity_wire::conn::usurped_close()).await {
                    warn!("{peer} usurp close failed: {e:#}");
                }
                drop(conn);
                drop(registration);
                return;
            }
            let run = client::Client::new(conn, cfg, db, sde, hub, world, login).run(registration);
            if let Err(e) = run.await {
                error!("{peer} session ended with error: {e:#}");
            }
        });
    }
    drop(listener);
    gateway_task.abort();
    chat_task.abort();
    image_task.abort();
    cdn_task.abort();
    watchdog_task.abort();
    let _ = tokio::join!(gateway_task, chat_task, image_task, cdn_task, watchdog_task);
    match tokio::time::timeout(std::time::Duration::from_secs(cfg.scheduler.drain_secs), &mut scheduler_task).await {
        Ok(Ok(())) => info!("scheduler drained"),
        Ok(Err(error)) => warn!("scheduler runner failed: {error}"),
        Err(_) => {
            warn!("scheduler drain timed out");
            scheduler_task.abort();
            let _ = scheduler_task.await;
        }
    }
    services::sovereignty::recovery::recover(&sovereignty_resources).await.map_err(startup_call_error)?;
    match tokio::time::timeout(
        std::time::Duration::from_secs(cfg.server.shutdown_flush_timeout_secs),
        async {
            world.quiesce_loaded().await;
            world.flush_positions().await
        },
    ).await {
        Ok(Ok(positions_written)) => info!(positions_written, "position snapshot shutdown flush"),
        Ok(Err(e)) => {
            warn!("position snapshot shutdown flush failed: {e:#}");
            info!(outcome = "failed", "position snapshot shutdown flush");
        }
        Err(_) => {
            warn!("position snapshot shutdown flush timed out");
            info!(outcome = "timed out", "position snapshot shutdown flush");
        }
    }
    Ok(())
}


async fn log_exit(what: &'static str, f: impl std::future::Future<Output = Result<()>>) {
    if let Err(e) = f.await {
        error!("{what} stopped: {e:#}");
    }
}

#[cfg(test)]
mod tests;
