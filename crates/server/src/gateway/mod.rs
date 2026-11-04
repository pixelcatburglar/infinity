//! Public gateway: the gRPC (HTTP/2, plaintext) service the client's native
//! `eve_public_grpc_client` talks to for newer features (wallet, map, corps,
//! goals, campaigns...). Point the client at it with
//! `EVE_PUBLIC_GATEWAY_ADDRESS=host:port` (publicGateway/grpc/connectionConfig.py).
//!
//! Services (eve_public/gateway.proto), all bidirectional streams:
//! * `Requests.Send`: `eve_public.Request` -> `eve_public.Response`, correlated by uuid
//! * `Notices.Consume`: server pushes `eve_public.Notice`
//! * `Events.Publish`: client telemetry, accepted and dropped
//!
//! Payloads are `google.protobuf.Any`; they are decoded with the client's own
//! descriptors (extracted by tools/extract_protos.py) through prost-reflect.
//! Responses use embedded protobuf descriptors, server CDN content and DAO data;
//! these handlers do not read World. Unclaimed requests retain default responses;
//! valid Events are decoded and dropped, notice delivery is not repaired, and
//! career content is not complete.

mod achievements;
mod career_goals;
mod character_titles;
mod characters;
mod content;
mod context;
mod error;
mod events;
mod fallback;
mod handlers;
mod insurgency;
mod message;
mod plex;
mod protocol;
mod requests;
mod skill_plans;
mod space_content;
mod transport;

#[cfg(test)] mod tests;

use crate::{config::Config, db::Db, space::World};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing::{debug, info};
use message::pool;
use protocol::Notice;
use transport::connection;

/// Who sent a request.
#[derive(Clone, Debug, Default)]
pub struct Caller {
    pub user_id: Option<i64>,
    pub character_id: Option<i64>,
}

// ---- shared state: notice streams for server pushes ----

type NoticeTx = mpsc::UnboundedSender<Notice>;

pub struct Gateway {
    pub db: Db,
    pub world: Arc<World>,
    pub config: Arc<Config>,
    /// application instance uuid -> notice stream
    notice_streams: Mutex<HashMap<Vec<u8>, NoticeTx>>,
}
impl Gateway {
    pub fn new(db: Db, world: Arc<World>, config: Arc<Config>) -> Arc<Gateway> {
        Arc::new(Gateway { db, world, config, notice_streams: Mutex::new(HashMap::new()) })
    }
}



pub async fn serve(bind: String, gw: Arc<Gateway>) -> Result<()> {
    handlers::validate()?;
    anyhow::ensure!(message::message("eve_public.Event").is_some(), "missing event descriptor eve_public.Event");
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
