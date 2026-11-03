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
//! these handlers do not read World. Unclaimed requests retain default responses,
//! Events are drained, notice delivery is not repaired, and career content is
//! not complete.

mod achievements;
mod career_goals;
mod character_titles;
mod characters;
mod content;
mod fallback;
mod handlers;
mod insurgency;
mod message;
mod plex;
mod protocol;
mod requests;
mod resources;
mod skill_plans;
mod space_content;
mod transport;

use crate::db::Db;
use anyhow::Result;
use prost_reflect::DynamicMessage;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tracing::{debug, info};
use message::{pool, pack, UUID_LEN};
use protocol::{Notice, TargetGroup, now_timestamp};
use transport::connection;

/// Who sent a request.
pub struct Caller {
    pub user_id: Option<i64>,
    pub character_id: Option<i64>,
}

// ---- shared state: notice streams for server pushes ----

type NoticeTx = mpsc::UnboundedSender<Notice>;

#[derive(Default)]
pub struct Gateway {
    pub db: Option<Db>,
    /// Base URL of the content CDN, e.g. `http://127.0.0.1` (see crate::cdn).
    pub cdn_url: String,
    /// application instance uuid -> notice stream
    notice_streams: Mutex<HashMap<Vec<u8>, NoticeTx>>,
    /// application instance uuid -> (user, character) last seen on its requests
    identities: Mutex<HashMap<Vec<u8>, (Option<i64>, Option<i64>)>>,
}

impl Gateway {
    pub fn new(db: Db, cdn_url: String) -> Arc<Gateway> {
        Arc::new(Gateway { db: Some(db), cdn_url, ..Default::default() })
    }

    /// Push a notice to every client playing `character_id`.
    #[allow(dead_code)]
    pub fn notify_character(&self, character_id: i64, payload: &DynamicMessage) {
        let ids = self.identities.lock().unwrap();
        let streams = self.notice_streams.lock().unwrap();
        for (app, (_, ch)) in ids.iter() {
            if *ch == Some(character_id) {
                if let Some(tx) = streams.get(app) {
                    let _ = tx.send(Notice {
                        dispatched: Some(now_timestamp()),
                        uuid: crate::handshake::rand_i64().to_le_bytes().repeat(UUID_LEN / std::mem::size_of::<i64>()),
                        internal_origin: "infinity".into(),
                        tenant: String::new(),
                        payload: Some(pack(payload)),
                        target_group: Some(TargetGroup { character: Some(character_id as u32), ..Default::default() }),
                    });
                }
            }
        }
    }
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
