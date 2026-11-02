//! Infinity emulator server: proxy and sol node, gateway, chat, image server and CDN.
//! The handshake mirrors the client's macho networking; state comes from the game
//! database and SDE. Interrupted reloads return to cargo before workers or listeners
//! start. On SIGINT or SIGTERM, scheduler work drains before pilot positions are
//! flushed with bounded waits; existing sessions are not drained.
//! Authenticated account ownership is process-local; replacements wait for
//! prior cleanup. Cross-process session coordination is not supported.

mod cdn;
mod retail;
mod chat;
mod client;
mod conn;
mod config;
mod consts;
mod dao;
mod db;
mod framing;
mod gateway;
mod handshake;
mod images;
mod live_sessions;
mod logging;
mod macho;
mod lua;
mod messages;
mod scheduler;
mod sde;
mod services;
mod session;
mod space;

use anyhow::{Context, Result};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{error, info, warn};

/// Server allocation policy: initial ephemeral client ID.
const FIRST_CLIENT_ID: i64 = 1_000_000_000_000;
/// Server online population counter delta when a session joins.
const ONLINE_JOIN_DELTA: i64 = 1;
/// Server online population counter delta when a session leaves.
const ONLINE_LEAVE_DELTA: i64 = -1;

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
    info!(source = cfg.source(), "configuration loaded");
    config::init_game(cfg.game_config())?;
    let db = db::Db::connect(&cfg.database).await?;
    services::corps::shares::seed(&db).await.map_err(startup_call_error)?;
    services::standings::initialize(&db).await?;
    let sde = Arc::new(sde::Sde::load(&db).await?);
    let types = sde.types.len();
    info!(target: "infinity_server::db", "database ready ({types} types in SDE)");
    services::recover_pending_reloads(&db, &sde).await.map_err(startup_call_error)?;
    if let Err(services::CallError::Internal(e)) = services::unbury_sites(&db, &sde).await {
        tracing::warn!("moving sites out of planets failed: {e:#}");
    }
    let world = space::World::new(db.clone(), sde.clone(), cfg.server.system_capacity);
    let hub = chat::Hub::new(&cfg.chat.domain, db.clone(), world.clone());
    services::trade::recover(&db, &world, sde.clone()).await.map_err(startup_call_error)?;
    lua::start(&cfg, &db, &sde, &world).await?;
    let restored = services::bounty::restore_activations(&db, &world).await.map_err(startup_call_error)?;
    info!(restored, "restored active kill rights");
    services::skills::completion::seed(&db).await.map_err(startup_call_error)?;
    services::industry::completion::seed(&db).await.map_err(startup_call_error)?;
    let (scheduler_stop, scheduler_shutdown) = tokio::sync::watch::channel(false);
    let mut scheduler_task = scheduler::runner::start(db.clone(), world.clone(), services::scheduler_registrations(), cfg.scheduler.clone(), scheduler_shutdown)?;
    scheduler::seed(&db).await?;
    let gateway_task = tokio::spawn(log_exit("public gateway", gateway::serve(cfg.gateway.bind.clone(), gateway::Gateway::new(db.clone(), cfg.cdn_url()))));
    let chat_task = tokio::spawn(log_exit("chat", chat::serve(cfg.chat.bind.clone(), hub.clone())));
    let upstream = images::Upstream::new(&cfg.images.upstream, cfg.images.upstream_timeout_secs);
    let upload_limit = cfg.images.upload_limit_bytes();
    let image_task = tokio::spawn(log_exit("image server", images::serve("images", cfg.images.bind.clone(), cfg.images.dir.clone().into(), upstream.clone(), upload_limit)));
    let cdn_task = tokio::spawn(log_exit("content cdn", images::serve("cdn", cfg.cdn.bind.clone(), cfg.images.dir.clone().into(), upstream, upload_limit)));
    let listener = TcpListener::bind(&cfg.server.bind).await.with_context(|| format!("binding {}", cfg.server.bind))?;
    info!("listening on {} (macho {}, build {}, {}@{})", cfg.server.bind, cfg.server.macho_version, cfg.server.boot_build, cfg.server.boot_codename, cfg.server.boot_region);

    #[cfg(unix)]
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("registering SIGTERM handler")?;
    let shutdown = async {
        #[cfg(unix)]
        {
            tokio::select! {
                result = tokio::signal::ctrl_c() => result.context("handling SIGINT"),
                result = sigterm.recv() => result.context("SIGTERM handler closed").map(|_| ()),
            }
        }
        #[cfg(not(unix))]
        {
            tokio::signal::ctrl_c().await.context("handling SIGINT")
        }
    };
    tokio::pin!(shutdown);
    let next_client_id = Arc::new(AtomicI64::new(FIRST_CLIENT_ID));
    // The biased select polls and registers the signal handler before accepting clients.
    loop {
        let (stream, peer) = tokio::select! {
            biased;
            result = &mut shutdown => {
                result?;
                let _ = scheduler_stop.send(true);
                break;
            }
            result = listener.accept() => result?,
        };
        stream.set_nodelay(true).ok();
        let (cfg, db, sde, ids, hub, world) = (cfg.clone(), db.clone(), sde.clone(), next_client_id.clone(), hub.clone(), world.clone());
        tokio::spawn(async move {
            let client_id = ids.fetch_add(1, Ordering::Relaxed);
            let conn = conn::Conn::new(stream, peer);
            info!("connection from {peer} (clientID {client_id})");
            let (mut conn, login, mut registration) = match handshake::run(conn, &cfg, &db, client_id).await {
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
            if registration.is_cancelled() {
                if let Err(e) = conn.send(&conn::usurped_close()).await {
                    warn!("{peer} usurp close failed: {e:#}");
                }
                drop(conn);
                drop(registration);
                return;
            }
            db.add_online(ONLINE_JOIN_DELTA);
            if let Err(e) = client::Client::new(conn, cfg, db.clone(), sde, hub, world, login).run(&mut registration).await {
                error!("{peer} session ended with error: {e:#}");
            }
            db.add_online(ONLINE_LEAVE_DELTA);
            drop(registration);
        });
    }
    drop(listener);
    gateway_task.abort();
    chat_task.abort();
    image_task.abort();
    cdn_task.abort();
    let _ = tokio::join!(gateway_task, chat_task, image_task, cdn_task);
    match tokio::time::timeout(std::time::Duration::from_secs(cfg.scheduler.drain_secs), &mut scheduler_task).await {
        Ok(Ok(())) => info!("scheduler drained"),
        Ok(Err(error)) => warn!("scheduler runner failed: {error}"),
        Err(_) => {
            warn!("scheduler drain timed out");
            scheduler_task.abort();
            let _ = scheduler_task.await;
        }
    }
    match tokio::time::timeout(
        std::time::Duration::from_secs(cfg.server.shutdown_flush_timeout_secs),
        world.flush_positions(),
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
