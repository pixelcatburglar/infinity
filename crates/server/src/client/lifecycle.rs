//! Per-connection session ownership: one request child, one supervisor, and
//! the single ordinary cleanup they guarantee together.
//!
//! `Client::run` constructs the session synchronously: the caller's registry
//! guard, the online counter, the sender attachment, and one
//! `Arc<tokio::sync::Mutex<Session>>` shared by exactly two tasks. The request
//! child locks the session once and runs `Client::run_loop` to its end; the
//! supervisor joins it, becomes the session's only owner, and runs
//! `Client::finish` exactly once before the session's fields drop in
//! declaration order: client, online counter, sender, registration.
//!
//! Dropping `SessionRun` never aborts: it marks the registration
//! `CallerDropped`, the child exits at the existing cancellation points once
//! an in-flight request returns, and the detached supervisor still cleans up.
//! A caller that needs a task constructs `SessionRun` first and moves it in; a
//! request panic reaches the caller as the supervisor's join error after
//! cleanup ran. Committed work is not rolled back. A failed system actor marks
//! this exact session and waits for the same ordinary cleanup. No tables.

use std::{future::Future, pin::Pin, sync::Arc, task::{Context, Poll}};

use anyhow::Result;
use tokio::task::JoinHandle;
use crate::client::Client;
use crate::db::Db;
use crate::live_sessions::{CallerGuard, Registration, SenderGuard};

/// Server online population counter delta when a session joins.
const ONLINE_JOIN_DELTA: i64 = 1;
/// Server online population counter delta when a session leaves.
const ONLINE_LEAVE_DELTA: i64 = -1;

/// The session both lifecycle tasks share; field order is its cleanup order.
struct Session {
    client: Client,
    _online: OnlineGuard,
    _sender: Option<SenderGuard>,
    registration: Registration,
}

/// One session's share of the online counter: `+1`/`-1`, each exactly once.
struct OnlineGuard {
    db: Db,
}

impl OnlineGuard {
    fn new(db: Db) -> Self {
        db.add_online(ONLINE_JOIN_DELTA);
        OnlineGuard { db }
    }
}

impl Drop for OnlineGuard {
    fn drop(&mut self) {
        self.db.add_online(ONLINE_LEAVE_DELTA);
    }
}

/// The owned future of one session. `caller` is declared before `supervisor`
/// so an unpolled drop still marks the registration `CallerDropped` first; the
/// supervisor handle then detaches and finishes cleanup on its own.
pub(crate) struct SessionRun {
    caller: CallerGuard,
    supervisor: JoinHandle<Result<()>>,
}

impl SessionRun {
    pub(super) fn new(mut client: Client, registration: Registration) -> Self {
        let caller = registration.caller_guard();
        client.failure = Some(registration.failure_target(&caller));
        let online = OnlineGuard::new(client.db.clone());
        let sender = registration.attach_sender(client.notify_tx.clone());
        let session = Arc::new(tokio::sync::Mutex::new(Session { client, _online: online, _sender: sender, registration }));
        let child = tokio::spawn({
            let session = session.clone();
            // One async lock for the whole loop; a panic releases guard and
            // Arc but leaves `Client` for the supervisor.
            async move {
                let session = &mut *session.lock().await;
                session.client.run_loop(&mut session.registration).await
            }
        });
        let supervisor = tokio::spawn(async move {
            let result = match child.await {
                Ok(result) => result,
                Err(join) => Err(anyhow::Error::new(join)),
            };
            // Join consumed the child's Arc: this is the only owner left.
            let mut session = Arc::into_inner(session)
                .expect("session supervisor joins the only other owner")
                .into_inner();
            session.client.finish(&session.registration, result).await
        });
        SessionRun { caller, supervisor }
    }
}

impl Future for SessionRun {
    type Output = Result<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match Pin::new(&mut this.supervisor).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(joined) => {
                this.caller.disarm();
                Poll::Ready(joined.unwrap_or_else(|join| Err(anyhow::Error::new(join))))
            }
        }
    }
}
