//! Server side of the GPS login handshake, mirroring
//! `GPSTransport.HandleClientAuthentication` in carbon/common/script/net/GPS.py,
//! with the client's `evecrypto.placebo` crypto pack (start.ini: cryptoPack = Placebo).
//!
//! Credentials use `game.accounts` via `Db`. Authenticated account ownership is
//! tracked in process-local `live_sessions`; predecessor cleanup completes before
//! the final acknowledgement and `session_init`. Cross-process session coordination
//! is unsupported.

use crate::admission::Permit;
use infinity_wire::conn::Conn;
use crate::db::Db;
use crate::config::Config;
use anyhow::{anyhow, bail, Context, Result};
use marshal::Value;
use std::time::Instant;
use tracing::{info, warn};

/// Client GPS compatibility handshake secret key.
const HANDSHAKE_SECRET_KEY: i64 = 170472;
/// Client GPS password version: the client sends a hash, not plaintext.
const PASSWORD_VERSION: i64 = 2;
/// Client GPS compatibility request tuple length.
const COMPATIBILITY_FIELDS: usize = 6;
/// Server Placebo handshake challenge length in bytes.
const SERVER_CHALLENGE_BYTES: usize = 64;
/// CRC-CCITT as used by Python binascii.crc_hqx.
const CRC_BITS_PER_BYTE: u32 = 8;
/// CRC-CCITT high bit used by binascii.crc_hqx.
const CRC_HIGH_BIT: u16 = 0x8000;
/// CRC-CCITT polynomial used by binascii.crc_hqx.
const CRC_POLYNOMIAL: u16 = 0x1021;
/// Server random identifier policy: discard two high bits to keep IDs positive.
const RANDOM_ID_SHIFT: u32 = 2;
/// Server GPS response: the first logon queue position.
const LOGON_QUEUE_POSITION: i64 = 1;
/// CPython marshal string encoding of the handshake function expression `{}`.
const HANDSHAKE_FUNCTION: &[u8] = b"s\x02\x00\x00\x00{}";

/// What a finished handshake hands to the session layer.
pub struct Login {
    pub user_id: i64,
    pub user_name: String,
    pub role: i64,
    pub client_id: i64,
    pub session_id: i64,
    pub language_id: String,
}

/// `evecrypto.placebo.crypto_hash(*args)`: `str(binascii.crc_hqx(blue.marshal.Save(args), 0))`.
pub fn placebo_hash(args: Vec<Value>) -> Value {
    Value::str(crc_hqx(&marshal::dumps(&Value::Tuple(args)), 0).to_string())
}

/// CRC-CCITT as in `binascii.crc_hqx` (poly 0x1021, MSB first).
fn crc_hqx(data: &[u8], mut crc: u16) -> u16 {
    for &b in data {
        crc ^= (b as u16) << CRC_BITS_PER_BYTE;
        for _ in 0..CRC_BITS_PER_BYTE {
            crc = if crc & CRC_HIGH_BIT != 0 { (crc << 1) ^ CRC_POLYNOMIAL } else { crc << 1 };
        }
    }
    crc
}

fn handshake_tuple(cfg: &Config, user_count: i64) -> Value {
    Value::tuple([
        Value::Int(HANDSHAKE_SECRET_KEY),
        Value::Int(cfg.server.macho_version),
        Value::Int(user_count),
        Value::Float(cfg.server.boot_version),
        Value::Int(cfg.server.boot_build),
        Value::str(format!("{}@{}", cfg.server.boot_codename, cfg.server.boot_region)),
        Value::None, // update info (const.responseUnknown on a real proxy)
    ])
}

fn text(v: Option<&Value>) -> Option<String> {
    v.and_then(|v| v.as_text()).map(|s| s.into_owned())
}

/// State owned by one handshake attempt. Declaration order is the cleanup
/// order on cancellation or timeout: the socket closes, then the registration
/// is released, then the pending-admission slot.
struct State {
    conn: Conn,
    registration: Option<crate::live_sessions::Registration>,
    permit: Permit,
}

/// Send a not-accepting-connections refusal with the given reason code.
async fn refuse(conn: &mut Conn, reason_code: &str) -> Result<Option<Login>> {
    conn.send(&infinity_wire::conn::not_accepting_close(reason_code)).await?;
    Ok(None)
}

/// Run the handshake, answering status probes (`(None, 'QC')`) on the same
/// socket, all inside one `deadline` from accept time. A returned triple
/// contains the owned connection, authenticated identity and process-local
/// ownership guard. If cancellation is observed after waiting, no final ack is
/// sent; the caller closes the socket before dropping the guard. The guard may
/// also be cancelled after the ack is sent.
pub(crate) async fn run(conn: Conn, cfg: &Config, db: &Db, client_id: i64, permit: Permit, deadline: Instant) -> Result<Option<(Conn, Login, crate::live_sessions::Registration)>> {
    let mut state = State { conn, registration: None, permit };
    match tokio::time::timeout_at(deadline.into(), inner(&mut state, cfg, db, client_id)).await {
        Ok(Ok(Some(login))) => {
            let State { conn, registration, .. } = state;
            Ok(Some((conn, login, registration.expect("login implies registration"))))
        }
        Ok(result) => result.map(|_| None),
        Err(_) => {
            warn!("handshake from {} timed out", state.conn.peer);
            Ok(None)
        }
    }
}

async fn inner(state: &mut State, cfg: &Config, db: &Db, client_id: i64) -> Result<Option<Login>> {
    loop {
        state.conn.send(&handshake_tuple(cfg, db.online_count())).await?;

        // Step 1: client compatibility tuple.
        let Some(step1) = state.conn.recv().await? else { return Ok(None) };
        let s1 = step1.items().ok_or_else(|| anyhow!("step1 is not a tuple: {step1:?}"))?;
        if s1.len() != COMPATIBILITY_FIELDS || s1[0].as_int() != Some(HANDSHAKE_SECRET_KEY) {
            bail!("bad compatibility handshake: {step1:?}");
        }
        if s1[1].as_int() != Some(cfg.server.macho_version) || s1[4].as_int() != Some(cfg.server.boot_build) {
            warn!("client reports macho {:?} build {:?}; server is {} / {}", s1[1], s1[4], cfg.server.macho_version, cfg.server.boot_build);
        }

        // Step 2: optional VIP key, then queue check or crypto context.
        let Some(mut step2) = state.conn.recv().await? else { return Ok(None) };
        if is_marker(&step2, "VK") {
            let Some(next) = state.conn.recv().await? else { return Ok(None) };
            step2 = next;
        }
        if is_marker(&step2, "QC") {
            info!("status probe from {}", state.conn.peer);
            state.conn.send(&Value::Int(LOGON_QUEUE_POSITION)).await?; // logon queue position
            continue;
        }
        // (publicKeyVersion, cryptoRequest) -> placebo context needs nothing.
        state.conn.send(&Value::str("OK CC")).await?;

        // Step 3: (clientChallenge, request dict).
        let Some(step3) = state.conn.recv().await? else { return Ok(None) };
        let s3 = step3.items().ok_or_else(|| anyhow!("step3 is not a tuple"))?;
        let (client_challenge, request) = match s3 {
            [c, r @ Value::Dict(_)] => (c.clone(), r.clone()),
            _ => bail!("bad secure handshake: {step3:?}"),
        };
        state.conn.send(&Value::Int(PASSWORD_VERSION)).await?;

        let user_name = text(request.get("user_name")).unwrap_or_default();
        let password_hash = request.get("user_password_hash").and_then(|v| v.as_bytes()).map(|b| b.to_vec());
        let language_id = text(request.get("user_languageid")).unwrap_or_else(|| "EN".into());

        let response = Value::dict([
            ("challenge_responsehash", placebo_hash(vec![client_challenge])),
            ("macho_version", Value::Int(cfg.server.macho_version)),
            ("boot_version", Value::Float(cfg.server.boot_version)),
            ("boot_build", Value::Int(cfg.server.boot_build)),
            ("boot_codename", Value::str(&cfg.server.boot_codename)),
            ("boot_region", Value::str(&cfg.server.boot_region)),
            ("cluster_usercount", Value::Int(db.online_count())),
            ("proxy_nodeid", Value::Int(cfg.server.node_id)),
            ("user_logonqueueposition", Value::Int(LOGON_QUEUE_POSITION)),
            ("config_vals", cfg.global_config()),
        ]);
        // Placebo Sign(x) == (x, 0). The "handshake function" is CPython-marshalled
        // and eval()'d by the client; the string "{}" evaluates to an empty dict.
        let handshake_func = Value::str(HANDSHAKE_FUNCTION);
        let server_challenge = Value::str(vec![0u8; SERVER_CHALLENGE_BYTES]);
        state.conn.send(&Value::tuple([server_challenge, Value::tuple([handshake_func, Value::Int(0)]), Value::Dict(vec![]), response]))
            .await?;

        if user_name.is_empty() || password_hash.is_none() {
            bail!("login without credentials");
        }

        // Step 4: (hash(serverChallenge), funcOutput, funcResult).
        let Some(step4) = state.conn.recv().await? else { return Ok(None) };
        if let Some(out) = step4.items().and_then(|s| s.get(1)).and_then(|v| v.as_text()) {
            if !out.is_empty() {
                warn!("handshake function output: {out}");
            }
        }

        // A planned shutdown inside its rejection window refuses new logins here:
        // the single login-admission point after step 4, before any DB lookup.
        if crate::shutdown::rejecting(cfg.server.shutdown_reject_secs) {
            return refuse(&mut state.conn, infinity_core::messages::ACL_SHUTTINGDOWN).await;
        }
        // A source in credential-failure cooldown is refused without a database
        // lookup; this policy check is the single admission point for logins.
        if state.permit.blocked(&cfg.server) {
            return refuse(&mut state.conn, infinity_core::messages::ACL_NOTACCEPTING).await;
        }
        let account = db.login(&user_name, password_hash.as_deref().unwrap(), &cfg.development).await.context("account lookup")?;
        let Some(account) = account else {
            warn!("login rejected for {user_name:?}: invalid credentials or registration disabled");
            state.permit.record_failure(&cfg.server);
            return Ok(None);
        };
        let session_id = rand_i64();
        let login = Login { user_id: account.user_id, user_name, role: account.role, client_id, session_id, language_id };
        let Some(registration) = crate::live_sessions::register(login.user_id, cfg.server.login_takeover) else {
            return refuse(&mut state.conn, infinity_core::messages::ACL_NOTACCEPTING).await;
        };
        let registration = state.registration.insert(registration);
        registration.wait_for_predecessor().await;
        if registration.is_cancelled() {
            return Ok(Some(login));
        }
        let session_init = Value::dict([
            ("userid", Value::Int(account.user_id)),
            ("userType", Value::Int(account.user_type)),
            ("role", Value::Int(account.role)),
            ("languageID", Value::str(&login.language_id)),
            ("address", Value::str(state.conn.peer.ip().to_string())),
            ("inDetention", Value::None),
            ("maxSessionTime", Value::None),
            ("countryCode", Value::None),
        ]);
        let ack = Value::dict([
            ("sessionID", Value::Int(session_id)),
            ("user_clientid", Value::Int(client_id)),
            ("session_init", session_init),
            ("access_token", Value::None),
            ("computer_hash", Value::None),
            ("live_updates", Value::list([])),
            ("client_hash", Value::None),
        ]);
        if let Err(e) = state.conn.send(&ack).await {
            // Field order drops the socket before releasing the registration.
            return Err(e);
        }
        info!("user {:?} (id {}) logged in from {}", login.user_name, login.user_id, state.conn.peer);
        return Ok(Some(login));
    }
}

fn is_marker(v: &Value, m: &str) -> bool {
    matches!(v.items(), Some([Value::None, tag, ..]) if tag.as_text().is_some_and(|t| t == m))
}

/// Returns 64 random bits.
pub fn random_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
    h.finish()
}

pub fn rand_i64() -> i64 {
    (random_u64() >> RANDOM_ID_SHIFT) as i64
}

#[cfg(all(test, feature = "tests-runtime"))] mod tests;
