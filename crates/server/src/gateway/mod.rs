//! Public gateway: the gRPC (HTTP/2, plaintext) service the client's native
//! `eve_public_grpc_client` talks to for newer features (wallet, map, corps,
//! goals, campaigns...). Point the client at it with
//! `EVE_PUBLIC_GATEWAY_ADDRESS=host:port` (publicGateway/grpc/connectionConfig.py).
//!
//! Services (eve_public/gateway.proto), all bidirectional streams:
//! * `Requests.Send`: `eve_public.Request` -> `eve_public.Response`, correlated by uuid
//! * `Notices.Consume`: server pushes `eve_public.Notice`
//! * `Events.Publish`: client telemetry, decoded when known and unclaimed
//!
//! Payloads are `google.protobuf.Any`; they are decoded with the client's own
//! descriptors (extracted by tools/extract_protos.py) through prost-reflect.
//! Responses use embedded protobuf descriptors, server CDN content and DAO data;
//! notice delivery uses World to resolve current pilots and has no durable replay.
//! Descriptors are validated before database startup. Unclaimed requests retain
//! default responses; valid Events are unclaimed, and career content is not complete.

mod achievements;
mod career_goals;
mod character_titles;
mod characters;
mod content;
mod context;
pub(crate) mod daily_goals;
pub(crate) mod error;
mod events;
mod fallback;
pub(crate) mod handlers;
mod insurgency;
pub(crate) mod message;
pub(crate) mod notices;
mod plex;
pub(crate) mod protocol;
mod requests;
pub(crate) mod ship_cosmetics;
mod skill_plans;
mod space_content;
mod transport;

#[cfg(test)] pub(crate) mod tests;

use crate::{config::Config, db::Db, space::World};
use anyhow::Result;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{debug, info};
use message::pool;
use transport::connection;

/// Who sent a request.
#[derive(Clone, Debug, Default)]
pub struct Caller {
    pub user_id: Option<i64>,
    pub character_id: Option<i64>,
}

pub struct Gateway {
    pub db: Db,
    pub world: Arc<World>,
    pub config: Arc<Config>,
    pub(crate) notice_hub: notices::NoticeHub,
}
impl Gateway {
    pub fn new(db: Db, world: Arc<World>, config: Arc<Config>) -> Arc<Gateway> {
        Arc::new(Gateway { db, world: world.clone(), config, notice_hub: world.notice_hub() })
    }
}



pub(crate) fn validate() -> Result<()> {
    handlers::validate()?;
    daily_goals::validate()?;
    anyhow::ensure!(message::message("eve_public.Event").is_some(), "missing event descriptor eve_public.Event");
    Ok(())
}

pub async fn serve(bind: String, gw: Arc<Gateway>) -> Result<()> {
    let listener = TcpListener::bind(&bind).await?;
    info!("public gateway (gRPC) listening on {bind}; {} message types loaded", pool().all_messages().count());
    loop {
        let (sock, peer) = listener.accept().await?;
        let gw = gw.clone();
        tokio::spawn(async move {
            if let Err(e) = connection(sock, gw).await {
                debug!("gateway connection {peer} ended: {e:#}");
            }
        });
    }
}
