//! Real current-period payment fixture and receipt regression.
//! Uses the production Lua host, SDE, daily progress registry and LP authority.
//! Applied progress journals and retained package activations remain append-only.
use super::*;
use infinity_core::consts::CORP_HERALDRY;
use crate::{dao::daily_goals::{instances, receipts, rewards}, db::outbox::{self, Outbox}, lua::events::resources::{FactContext, FactOrigin, LoyaltyReason}, services::{daily_goals::payments, standings::{self, LoyaltyMutation}}};
mod caps;
mod races;

const CHILD: &str = "INFINITY_DAILY_PAYMENT_CHILD";
const NAME: &str = "lua::tests::daily_goals::payments::current_payment_replay_and_two_prices";

fn authored(root: &Path, version: &str) {
    source(root, version, false);
    let path = root.join("content/daily_goals/data/daily_goals.json");
    let mut data: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    data["slot_pools"] = serde_json::json!([["damage"], ["jump"], ["mining"]]);
    for goal in data["ordinary"].as_array_mut().unwrap() {
        if goal["key"] == "jump" { goal["target"] = serde_json::json!(20); }
    }
    std::fs::write(path, serde_json::to_vec(&data).unwrap()).unwrap();
}
pub(crate) async fn current_environment(root: &Path) -> (crate::db::Db, Resources) {
    let nonce = crate::handshake::random_u64();
    let a = format!("payment-{nonce}-a");
    let b = format!("payment-{nonce}-b");
    authored(root, &a);
    std::fs::write(root.join("payment-version"), &a).unwrap();
    let registry = crate::progress::Registry::new(vec![], vec![crate::services::daily_goals::progress::registration()]).unwrap();
    let (db, resources) = environment(root, false, registry).await;
    crate::progress::tests::register_fixture(&db, &crate::services::daily_goals::progress::registration()).await;
    let row_a = lua_content::load(&db, "daily_goals", &a).await.unwrap().unwrap();
    authored(root, &b);
    crate::lua::HOST.get().unwrap().reload().await.unwrap();
    let row_b = lua_content::load(&db, "daily_goals", &b).await.unwrap().unwrap();
    assert_ne!(row_a.fingerprint, row_b.fingerprint);
    let now = crate::client::filetime_now();
    let mut bounds = periods::bounds(&db, now).await.unwrap();
    if bounds.day_end - now < infinity_core::consts::FILETIME_MINUTE {
        let seconds = (bounds.day_end - now) / infinity_core::consts::FILETIME_SECOND + 1;
        tokio::time::sleep(std::time::Duration::from_secs(seconds as u64)).await;
        bounds = periods::bounds(&db, crate::client::filetime_now()).await.unwrap();
    }
    let previous = bounds.month_start - FILETIME_DAY;
    assert!(previous > 0);
    db.transaction(|tx, _: &mut ()| {
        let row_a = row_a.clone();
        Box::pin(async move {
            assert!(activations::append_if_changed(tx, &row_a, previous).await?);
            Ok::<_, crate::db::Error>(())
        })
    }, |(), ()| async { Ok(()) }).await.unwrap();
    authored(root, &a);
    crate::lua::HOST.get().unwrap().reload().await.unwrap();
    assert_eq!(activations::list(&db, "daily_goals").await.unwrap().last().unwrap().version, a);
    (db, resources)
}


pub(super) struct PaymentFixture {
    pub(super) db: crate::db::Db,
    pub(super) resources: Resources,
    pub(super) owner: crate::services::tests::fixture::Fixture,
    pub(super) identity: Identity,
    pub(super) notices: tokio::sync::mpsc::UnboundedReceiver<(String, Vec<marshal::Value>)>,
}
impl PaymentFixture {
    pub(super) async fn fresh_profile(db: crate::db::Db, resources: Resources) -> Self {
        let owner = crate::services::tests::fixture::create(&db).await.unwrap();
        let identity = identity(owner.character);
        let (sender, notices) = tokio::sync::mpsc::unbounded_channel();
        resources.world.register_pilot(owner.character, sender);
        resources.world.set_online_session(owner.character, Some(infinity_core::session::OnlineSession {
            character_id: owner.character, user_id: Some(owner.account), session_id: 1, ship_id: None,
            station_id: None, system_id: None, corporation_id: None, alliance_id: None,
            faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
        }));
        Self { db, resources, owner, identity, notices }
    }
    pub(super) async fn ensure_current(&self, at: i64) -> Vec<crate::services::daily_goals::GoalView> {
        queries::current(&self.resources, &self.identity, self.owner.account, at).await.unwrap()
    }
    pub(super) async fn balance(&self) -> i64 {
        standings::lp_balance(&self.db, self.owner.character, CORP_HERALDRY).await.unwrap()
    }
    pub(super) async fn fund(&self, amount: i64, at: i64) {
        let owner = self.owner.character;
        let world = self.resources.world.clone();
        let _market = crate::services::market::lock().await;
        self.db.transaction::<_, _, crate::services::CallError, Outbox, _, _, _>(|tx, outbox| Box::pin(async move {
            standings::add_lp(tx, outbox, owner, CORP_HERALDRY, amount, LoyaltyMutation {
                reason: LoyaltyReason::GmAdjustment, occurred_at: at,
                context: FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: None, station_id: None, origin: FactOrigin::Gm },
            }).await
        }), |(), outbox| async move { outbox::dispatch_world((), outbox, &world) }).await.unwrap();
    }
    /// Journal a fixture jump, not proof that a space actor performed one.
    pub(super) async fn publish_jump(&self, occurred_at: i64) -> crate::progress::DeliveryKey {
        use crate::{dao::progress::{self as journal, JournalEvent}, lua::events::GameEvent, progress};
        let event = GameEvent::Jumped { char_id: self.owner.character, ship_id: 1, from_system_id: 2, to_system_id: 3 };
        let occurrence = crate::dao::map_telemetry::next_occurrence(&self.db).await.unwrap();
        let source = crate::db::outbox::SourceKey::new("Jumped", occurrence);
        let row = JournalEvent {
            source_namespace: "business".into(), source_key: source.as_str().into(),
            schema_version: progress::codec::SCHEMA_VERSION, kind: event.kind().into(),
            occurred_at, recorded_at: crate::client::filetime_now(), payload: progress::codec::encode(&event).unwrap(),
            context: "{}".into(), origin: "server".into(), ordinal: 0, subject_character_id: Some(self.owner.character),
        };
        let inserted = journal::insert_batch(&self.db, &[row]).await.unwrap();
        assert_eq!(inserted.len(), 1);
        assert!(inserted[0].2, "fixture jump source must be unique");
        progress::DeliveryKey { consumer_key: crate::services::daily_goals::progress::registration().consumer_key.into(), event_id: inserted[0].1.clone() }
    }
    pub(super) async fn drain(&self, key: &crate::progress::DeliveryKey) {
        use crate::{dao::progress::{self as journal, DeliveryState}, progress::{self, ApplyError, ApplyOutcome, ClaimMode, ContentProgressResources, PrepareOutcome}};
        let registration = crate::services::daily_goals::progress::registration();
        assert_eq!(key.consumer_key, registration.consumer_key);
        loop {
            let resources = ContentProgressResources {
                db: self.db.clone(), sde: self.resources.sde.clone(), world: self.resources.world.clone(),
                policies: self.resources.policies.clone(),
            };
            let plan = match crate::services::daily_goals::progress::prepare::delivery(resources, key.clone()).await.unwrap() {
                PrepareOutcome::Ready(plan) => plan,
                PrepareOutcome::AlreadyApplied => {
                    assert!(matches!(journal::delivery(&self.db, key).await.unwrap(), DeliveryState::Applied));
                    return;
                }
            };
            let world = self.resources.world.clone();
            let sde = self.resources.sde.clone();
            let result = self.db.transaction::<_, _, ApplyError, Outbox, _, _, _>(|tx, outbox| {
                let key = key.clone();
                let plan = plan.clone();
                let sde = sde.clone();
                Box::pin(async move {
                    progress::apply_once_tx(tx, outbox, &sde, &registration, key, Some(plan.as_ref()), ClaimMode::Blocking).await
                })
            }, |value, outbox| async move { outbox::dispatch_world(value, outbox, &world).map_err(ApplyError::from) }).await;
            match result {
                Ok(ApplyOutcome::Applied | ApplyOutcome::AlreadyApplied) => return,
                Ok(ApplyOutcome::NotClaimed) => panic!("blocking daily delivery was not claimed"),
                Err(ApplyError::Reprepare) => continue,
                Err(error) => panic!("daily delivery failed: {error:?}"),
            }
        }
    }
    pub(super) async fn cleanup(&self) {
        loop {
            let pending = crate::dao::progress::pending_for_character(&self.db, crate::services::daily_goals::progress::registration().consumer_key, self.owner.character, 64).await.unwrap();
            if pending.is_empty() { break; }
            for key in pending { self.drain(&key).await; }
        }
        self.resources.world.unregister_pilot_generation(self.owner.character, None);
        let f = self.owner;
        // Keep applied and foreign journal records; the service fixture's cleanup deletes them.
        self.db.transaction(|tx, _: &mut ()| Box::pin(async move {
            tx.execute("DELETE FROM game.daily_goal_admin_audit WHERE subject_id=$1 OR actor_id=$1", &[&f.character]).await?;
            tx.execute("DELETE FROM game.daily_goal_payments WHERE character_id=$1", &[&f.character]).await?;
            tx.execute("DELETE FROM game.daily_goal_rewards WHERE goal_id IN (SELECT goal_id FROM game.daily_goal_instances WHERE character_id=$1)", &[&f.character]).await?;
            tx.execute("DELETE FROM game.daily_goal_instances WHERE character_id=$1", &[&f.character]).await?;
            tx.execute("DELETE FROM game.daily_goal_periods WHERE character_id=$1", &[&f.character]).await?;
            tx.execute("DELETE FROM game.lp_balances WHERE char_id=$1", &[&f.character]).await?;
            tx.execute("DELETE FROM game.account_clone_grade_audit WHERE user_id=$1 OR actor_user_id=$1", &[&f.account]).await?;
            tx.execute("DELETE FROM game.jobs WHERE job_id=$1", &[&format!("subscription.clone_grade.expire:{}", f.account)]).await?;
            tx.execute("DELETE FROM game.agent_missions WHERE character_id=$1", &[&f.character]).await?;
            tx.execute("DELETE FROM game.epic_arc_missions WHERE character_id=$1", &[&f.character]).await?;
            tx.execute("DELETE FROM game.skill_queue WHERE character_id=$1", &[&f.character]).await?;
            tx.execute("DELETE FROM game.skills WHERE character_id=$1", &[&f.character]).await?;
            tx.execute("DELETE FROM game.items WHERE owner_id=$1", &[&f.character]).await?;
            tx.execute("DELETE FROM game.store_purchases WHERE user_id=$1", &[&f.account]).await?;
            tx.execute("DELETE FROM game.wallet_journal WHERE account_id IN (SELECT account_id FROM game.wallet_accounts WHERE owner_id=$1)", &[&f.character]).await?;
            tx.execute("DELETE FROM game.wallet_accounts WHERE owner_id=$1", &[&f.character]).await?;
            tx.execute("DELETE FROM game.characters WHERE character_id=$1", &[&f.character]).await?;
            tx.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&f.account]).await?;
            Ok::<_, crate::db::Error>(())
        }), |(), ()| async { Ok(()) }).await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires dedicated initialized SDE database and externally serialized Lua publication"]
async fn current_payment_replay_and_two_prices() {
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--ignored", "--nocapture"]).env(CHILD, "1").status().unwrap();
        assert!(status.success(), "payment child failed");
        return;
    }
    let nonce = crate::handshake::random_u64();
    let root = std::env::temp_dir().join(format!("daily-payment-{nonce}"));
    let mut fixture = None;
    let result = crate::tests::catch_scenario(async {
        let (db, resources) = current_environment(&root).await;
        let a = std::fs::read_to_string(root.join("payment-version")).unwrap();
        let bounds = periods::bounds(&db, crate::client::filetime_now()).await.unwrap();
        fixture = Some(PaymentFixture::fresh_profile(db, resources).await);
        let f = fixture.as_mut().unwrap();
        let at = crate::client::filetime_now();
        let views = f.ensure_current(at).await;
        assert_eq!(views.len(), 13);
        assert_eq!(views.iter().filter(|v| v.goal.kind == "day" && v.goal.category == "ordinary").count(), 4);
        assert_eq!(views.iter().filter(|v| v.goal.kind == "day" && v.goal.category == "bonus").count(), 1);
        assert_eq!(views.iter().filter(|v| v.goal.slot_key.starts_with("base:")).count(), 4);
        assert_eq!(views.iter().filter(|v| v.goal.slot_key.starts_with("omega:")).count(), 4);
        assert!(views.iter().all(|v| v.goal.version == a));
        let targets: Vec<_> = views.iter().filter(|v| v.goal.kind == "day" && v.goal.category == "ordinary").take(2).map(|v| v.goal.goal_id.clone()).collect();
        assert_ne!(targets[0], targets[1]);
        f.fund(7000, at).await;
        assert_eq!(f.balance().await, 7000);
        payments::pay_for_completion(&f.resources, &f.identity, f.owner.account, &targets[0], at).await.unwrap();
        assert_eq!(f.balance().await, 4500);
        let first = instances::get(&f.db, f.owner.character, &targets[0]).await.unwrap().unwrap();
        assert_eq!(first.progress, first.target);
        assert!(first.paid && first.completed_at.is_some());
        let receipt = receipts::payment(&f.db, f.owner.character, &targets[0]).await.unwrap().unwrap();
        assert_eq!((receipt.ordinal, receipt.amount), (1, 2500));
        payments::pay_for_completion(&f.resources, &f.identity, f.owner.account, &targets[0], at).await.unwrap();
        assert_eq!(f.balance().await, 4500);
        assert_eq!(receipts::payment(&f.db, f.owner.character, &targets[0]).await.unwrap().unwrap(), receipt);
        assert_eq!(receipts::payment_count(&f.db, f.owner.character, first.period_start).await.unwrap(), 1);
        payments::pay_for_completion(&f.resources, &f.identity, f.owner.account, &targets[1], at).await.unwrap();
        assert_eq!(f.balance().await, 0);
        let second = instances::get(&f.db, f.owner.character, &targets[1]).await.unwrap().unwrap();
        assert_eq!(second.progress, second.target);
        assert!(second.paid && second.completed_at.is_some());
        let receipt2 = receipts::payment(&f.db, f.owner.character, &targets[1]).await.unwrap().unwrap();
        assert_eq!((receipt2.ordinal, receipt2.amount), (2, 4500));
        assert_eq!(receipts::payment_count(&f.db, f.owner.character, first.period_start).await.unwrap(), 2);
        let day = instances::period(&f.db, f.owner.character, "day", first.period_start).await.unwrap();
        let bonus = day.iter().find(|g| g.category == "bonus").unwrap();
        assert_eq!(bonus.progress, bonus.target);
        assert!(bonus.completed_at.is_some());
        let month = instances::period(&f.db, f.owner.character, "month", bounds.month_start).await.unwrap();
        assert_eq!(month.len(), 8);
        assert!(month.iter().all(|g| g.progress == 1));
        for id in targets.iter().chain(std::iter::once(&bonus.goal_id)) {
            let assets = rewards::list(&f.db, f.owner.character, id, false).await.unwrap();
            assert!(!assets.is_empty() && assets.iter().all(|r| r.earned && r.claimed_at.is_none()));
        }
        let pending = crate::dao::progress::pending_for_character(&f.db, crate::services::daily_goals::progress::registration().consumer_key, f.owner.character, 1).await.unwrap();
        assert!(pending.is_empty(), "completion facts must not create pending daily deliveries");
        assert!(f.notices.try_recv().is_ok());
    }).await;
    let cleanup = crate::tests::catch_scenario(async {
        if let Some(f) = &fixture { f.cleanup().await; }
    }).await;
    let restore = crate::tests::catch_scenario(async {
        if crate::lua::HOST.get().is_some() {
            let a = std::fs::read_to_string(root.join("payment-version")).unwrap();
            authored(&root, &a);
            crate::lua::HOST.get().unwrap().reload().await.unwrap();
        }
    }).await;
    let removed = std::fs::remove_dir_all(&root);
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
    if let Err(panic) = cleanup { std::panic::resume_unwind(panic); }
    if let Err(panic) = restore { std::panic::resume_unwind(panic); }
    removed.unwrap();
}
