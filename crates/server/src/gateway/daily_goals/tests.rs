//! Request decoding and production Lua gateway integration.
use super::*;
use prost_reflect::{ReflectMessage, Value};
use crate::gateway::{message::{child, pack, set}, protocol::{AuthoritativeContext, Identifier32, Request, UserIdentifier}, requests::handle_request, Gateway};

fn request(name: &str) -> DynamicMessage { DynamicMessage::new(message(name).unwrap()) }
fn with_goal(name: &str, bytes: Vec<u8>) -> DynamicMessage {
    let mut req = request(name);
    let mut goal = child(&req.descriptor(), "goal").unwrap();
    set(&mut goal, "uuid", Value::Bytes(bytes.into())).unwrap();
    set(&mut req, "goal", Value::Message(goal)).unwrap();
    req
}

#[test]
fn goal_identifiers_and_proto3_false_are_decoded_without_coercion() {
    let name = "eve_public.dailygoal.api.RedeemRequest";
    assert!(goal_id(&request(name)).is_err());
    assert!(goal_id(&with_goal(name, vec![1; UUID_LEN - 1])).is_err());
    let req = with_goal(name, vec![0; UUID_LEN]);
    assert_eq!(goal_id(&req).unwrap(), "00000000-0000-0000-0000-000000000000");
    assert!(matches!(decode(name, &req).unwrap(), Operation::Redeem(_, false)));
}

fn envelope(payload: &DynamicMessage, user: i64, character: i64, correlation: Vec<u8>) -> Request {
    Request {
        issued: None, correlation_uuid: correlation, external_origin: String::new(),
        application_instance_uuid: vec![9; UUID_LEN],
        authoritative_context: Some(AuthoritativeContext {
            tenant: String::new(), authenticated_user: Some(UserIdentifier { sequential: i32::try_from(user).unwrap() }),
            active_character: Some(Identifier32 { sequential: u32::try_from(character).unwrap() }), identity: None,
        }),
        payload: Some(pack(payload)),
    }
}

#[tokio::test]
#[ignore = "requires dedicated initialized SDE database and externally serialized Lua publication"]
async fn actual_gateway_routes_and_qa_preflight() {
    const CHILD: &str = "INFINITY_DAILY_GATEWAY_CHILD";
    const NAME: &str = "gateway::daily_goals::tests::actual_gateway_routes_and_qa_preflight";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", NAME, "--ignored", "--nocapture"]).env(CHILD, "1").status().unwrap();
        assert!(status.success());
        return;
    }
    use crate::lua::tests::daily_goals::{environment, reload, source};
    use crate::services::tests::fixture;
    let root = std::env::temp_dir().join(format!("daily-gateway-{}-{}", std::process::id(), crate::handshake::random_u64()));
    std::fs::create_dir(&root).unwrap();
    // This child starts with no host, so an authenticated online caller proves
    // that a supported reservation cannot fall through to a default response.
    let unavailable_gateway = crate::gateway::tests::gateway().await;
    let unavailable_owner = fixture::create(&unavailable_gateway.db).await.unwrap();
    let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
    unavailable_gateway.world.register_pilot(unavailable_owner.character, sender);
    unavailable_gateway.world.set_online_session(unavailable_owner.character, Some(infinity_core::session::OnlineSession {
        character_id: unavailable_owner.character, user_id: Some(unavailable_owner.account), session_id: 1,
        ship_id: None, station_id: None, system_id: None, corporation_id: None,
        alliance_id: None, faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
    }));
    let empty_current = request("eve_public.dailygoal.api.GetAllCurrentRequest");
    assert_eq!(handle_request(&unavailable_gateway, envelope(&empty_current, unavailable_owner.account, unavailable_owner.character, vec![1; UUID_LEN]), None).await.status_code, 500);
    unavailable_gateway.world.unregister_pilot_generation(unavailable_owner.character, None);
    fixture::cleanup(&unavailable_gateway.db, unavailable_owner).await.unwrap();
    let version = format!("gateway-{}", crate::handshake::random_u64());
    source(&root, &version, false);
    let registry = crate::progress::Registry::new(vec![], vec![daily::progress::registration()]).unwrap();
    let (db, resources) = environment(&root, false, registry).await;
    let owner = fixture::create(&db).await.unwrap();
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    resources.world.register_pilot(owner.character, tx);
    resources.world.set_online_session(owner.character, Some(infinity_core::session::OnlineSession {
        character_id: owner.character, user_id: Some(owner.account), session_id: 1, ship_id: None,
        station_id: None, system_id: None, corporation_id: None, alliance_id: None,
        faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
    }));
    let package = crate::dao::lua_content::load(&db, "daily_goals", &version).await.unwrap().unwrap();
    let second = format!("{version}-other");
    source(&root, &second, false);
    reload().await;
    let bounds = crate::dao::daily_goals::periods::bounds(&db, crate::client::filetime_now()).await.unwrap();
    db.transaction(|tx, _: &mut ()| {
        let package = package.clone();
        Box::pin(async move {
            crate::dao::lua_content::activations::append_if_changed(tx, &package, bounds.month_start - infinity_core::consts::FILETIME_DAY).await?;
            Ok::<_, crate::db::Error>(())
        })
    }, |(), ()| async { Ok(()) }).await.unwrap();
    source(&root, &version, false);
    reload().await;
    let gateway = Gateway::new(db.clone(), resources.world.clone(), Arc::new(crate::config::Config::default()));
    let result = crate::tests::catch_scenario(async {
        let lease = gateway.notice_hub.lease([9; UUID_LEN]);
        let mut notices = gateway.notice_hub.register([9; UUID_LEN], 32);
        let current = request("eve_public.dailygoal.api.GetAllCurrentRequest");
        let response = handle_request(&gateway, envelope(&current, owner.account, owner.character, vec![1; UUID_LEN]), Some(&lease)).await;
        assert_eq!(response.status_code, 200);
        let frame = notices.rx.try_recv().expect("new assignment notice");
        let (_, notice) = crate::gateway::tests::decode_notice(&frame);
        assert_eq!(notice.descriptor().full_name(), "eve_public.dailygoal.api.CurrentGoalsNotice");
        let any = response.payload.unwrap();
        let parsed = DynamicMessage::decode(message("eve_public.dailygoal.api.GetAllCurrentResponse").unwrap(), any.value.as_slice()).unwrap();
        let goals = parsed.get_field_by_name("goals").unwrap();
        let goals = goals.as_list().unwrap();
        assert_eq!(goals.len(), 13);
        let first = goals[0].as_message().unwrap();
        let id = first.get_field_by_name("id").unwrap();
        let uuid = id.as_message().unwrap().get_field_by_name("uuid").unwrap();
        let id = uuid.as_bytes().unwrap().to_vec();
        let get = with_goal("eve_public.dailygoal.api.GetRequest", id.clone());
        let other = fixture::create(&db).await.unwrap();
        let (other_tx, _other_rx) = tokio::sync::mpsc::unbounded_channel();
        resources.world.register_pilot(other.character, other_tx);
        resources.world.set_online_session(other.character, Some(infinity_core::session::OnlineSession {
            character_id: other.character, user_id: Some(other.account), session_id: 2, ship_id: None,
            station_id: None, system_id: None, corporation_id: None, alliance_id: None,
            faction_id: None, fleet_id: None, role: 0, corporation_role: 0,
        }));
        assert_eq!(handle_request(&gateway, envelope(&get, other.account, other.character, vec![2; UUID_LEN]), None).await.status_code, 404);
        resources.world.unregister_pilot_generation(other.character, None);
        fixture::cleanup(&db, other).await.unwrap();
        assert_eq!(handle_request(&gateway, envelope(&get, owner.account, owner.character, vec![2; UUID_LEN]), None).await.status_code, 200);
        let history = request("eve_public.dailygoal.api.GetAllWithRewardsRequest");
        assert_eq!(handle_request(&gateway, envelope(&history, owner.account, owner.character, vec![3; UUID_LEN]), None).await.status_code, 200);
        let mut bad_page = history.clone();
        let mut page = child(&bad_page.descriptor(), "page").unwrap();
        set(&mut page, "size", Value::U32(25)).unwrap();
        set(&mut page, "token", Value::String("invalid-token".into())).unwrap();
        set(&mut bad_page, "page", Value::Message(page)).unwrap();
        assert_eq!(handle_request(&gateway, envelope(&bad_page, owner.account, owner.character, vec![3; UUID_LEN]), None).await.status_code, 400);
        let invalid_goal = with_goal("eve_public.dailygoal.api.GetRequest", vec![1]);
        assert_eq!(handle_request(&gateway, envelope(&invalid_goal, owner.account, owner.character, vec![4; UUID_LEN]), None).await.status_code, 400);
        let missing = with_goal("eve_public.dailygoal.api.GetRequest", vec![0; UUID_LEN]);
        assert_eq!(handle_request(&gateway, envelope(&missing, owner.account, owner.character, vec![4; UUID_LEN]), None).await.status_code, 404);
        // Replace only gateway callbacks with an error: QA preflight must stop before Lua.
        source(&root, &format!("{version}-preflight"), false);
        let path = root.join("daily_goals.lua");
        let script = std::fs::read_to_string(&path).unwrap().replace("return context:content_commit(\"daily_goals\")", "error('gateway must not be invoked')");
        std::fs::write(&path, script).unwrap();
        reload().await;
        let admin = request(ADMIN_PROGRESS);
        assert_eq!(handle_request(&gateway, envelope(&admin, owner.account, owner.character, vec![1]), None).await.status_code, 400);
        assert_eq!(handle_request(&gateway, envelope(&admin, owner.account, owner.character, vec![5; UUID_LEN]), None).await.status_code, 403);
        source(&root, &version, false);
        reload().await;
        db.execute("UPDATE game.accounts SET role=$2 WHERE user_id=$1", &[&owner.account, &infinity_core::consts::ROLE_QA]).await.unwrap();
        let mut config = crate::config::Config::default();
        config.game.daily_goals.qa_account_ids.push(owner.account);
        let qa_gateway = Gateway::new(db.clone(), resources.world.clone(), Arc::new(config));
        let mut admin = with_goal(ADMIN_PROGRESS, id);
        let mut character = child(&admin.descriptor(), "character").unwrap();
        set(&mut character, "sequential", Value::U32(u32::try_from(owner.character).unwrap())).unwrap();
        set(&mut admin, "character", Value::Message(character)).unwrap();
        set(&mut admin, "progress", Value::U64(1)).unwrap();
        let mut before = crate::dao::daily_goals::instances::period(&db, owner.character, "day", bounds.day_start).await.unwrap();
        before.sort_by(|a, b| a.goal_id.cmp(&b.goal_id));
        let counts_sql = "SELECT (SELECT count(*) FROM game.daily_goal_admin_audit WHERE subject_id=$1 OR actor_id=$1) AS audits, (SELECT count(*) FROM game.daily_goal_payments WHERE character_id=$1) AS payments, (SELECT count(*) FROM game.daily_goal_rewards WHERE goal_id IN (SELECT goal_id FROM game.daily_goal_instances WHERE character_id=$1)) AS rewards";
        let counts = db.query_one(counts_sql, &[&owner.character]).await.unwrap();
        let before_counts: (i64, i64, i64) = (counts.get("audits"), counts.get("payments"), counts.get("rewards"));
        for (index, body) in [
            "context:content_commit('daily_goals'); return 'forged'",
            "context:content_commit('daily_goals'); error('after admission')",
            "context:content_commit('daily_goals'); return context:content_commit('daily_goals')",
            "local t = context:content_commit('daily_goals'); pcall(function() context:content_commit('daily_goals') end); return t",
            "return context:content_read('daily_goals')",
            "return context:content_commit('wrong')",
            "return nil",
        ].iter().enumerate() {
            source(&root, &format!("{version}-rejected-{index}"), false);
            let path = root.join("daily_goals.lua");
            let script = std::fs::read_to_string(&path).unwrap().replace("return context:content_commit(\"daily_goals\")", body);
            std::fs::write(&path, script).unwrap();
            reload().await;
            let rejected_uuid = [20 + index as u8; UUID_LEN];
            assert_eq!(handle_request(&qa_gateway, envelope(&admin, owner.account, owner.character, rejected_uuid.to_vec()), None).await.status_code, 500);
            assert!(crate::dao::daily_goals::receipts::audit(&db, &crate::uuid_text::format(&rejected_uuid)).await.unwrap().is_none());
            let mut after = crate::dao::daily_goals::instances::period(&db, owner.character, "day", bounds.day_start).await.unwrap();
            after.sort_by(|a, b| a.goal_id.cmp(&b.goal_id));
            assert_eq!(after, before);
            let counts = db.query_one(counts_sql, &[&owner.character]).await.unwrap();
            let after_counts: (i64, i64, i64) = (counts.get("audits"), counts.get("payments"), counts.get("rewards"));
            assert_eq!(after_counts, before_counts);
        }
        source(&root, &version, false);
        reload().await;
        let correlation = [7; UUID_LEN];
        let response = handle_request(&qa_gateway, envelope(&admin, owner.account, owner.character, correlation.to_vec()), None).await;
        assert_eq!(response.status_code, 200);
        assert_eq!(response.correlation_uuid, correlation);
        let audit = crate::dao::daily_goals::receipts::audit(&db, &crate::uuid_text::format(&correlation)).await.unwrap().unwrap();
        assert_eq!(audit.actor_id, owner.character);
        assert!(crate::dao::daily_goals::receipts::audit(&db, &crate::uuid_text::format(&[9; UUID_LEN])).await.unwrap().is_none());
        let rows = crate::dao::daily_goals::instances::period(&db, owner.character, "day", bounds.day_start).await.unwrap();
        let mut committed = rows.clone();
        committed.sort_by(|a, b| a.goal_id.cmp(&b.goal_id));
        assert_ne!(committed, before);
        assert_eq!(handle_request(&qa_gateway, envelope(&admin, owner.account, owner.character, correlation.to_vec()), None).await.status_code, 200);
        let mut replayed = crate::dao::daily_goals::instances::period(&db, owner.character, "day", bounds.day_start).await.unwrap();
        replayed.sort_by(|a, b| a.goal_id.cmp(&b.goal_id));
        assert_eq!(replayed, committed);
        let ordinary: Vec<_> = rows.iter().filter(|row| row.category == "ordinary").collect();
        assert!(ordinary.len() >= 2);
        let first_id = crate::uuid_text::parse(&ordinary[0].goal_id, crate::uuid_text::Case::Either).unwrap();
        let second_id = crate::uuid_text::parse(&ordinary[1].goal_id, crate::uuid_text::Case::Either).unwrap();
        let mut finish = with_goal(ADMIN_PROGRESS, first_id.to_vec());
        let mut character = child(&finish.descriptor(), "character").unwrap();
        set(&mut character, "sequential", Value::U32(u32::try_from(owner.character).unwrap())).unwrap();
        set(&mut finish, "character", Value::Message(character)).unwrap();
        set(&mut finish, "progress", Value::U64(ordinary[0].target)).unwrap();
        assert_eq!(handle_request(&qa_gateway, envelope(&finish, owner.account, owner.character, vec![8; UUID_LEN]), None).await.status_code, 200);
        let redeem = with_goal("eve_public.dailygoal.api.RedeemRequest", first_id.to_vec());
        for _ in 0..2 {
            assert_eq!(handle_request(&gateway, envelope(&redeem, owner.account, owner.character, vec![10; UUID_LEN]), None).await.status_code, 200);
        }
        use crate::{db::outbox::{self, Outbox}, lua::events::resources::{FactContext, FactOrigin, LoyaltyReason}, services::standings::{self, LoyaltyMutation}};
        let now = crate::client::filetime_now();
        let world = resources.world.clone();
        let _market = crate::services::market::lock().await;
        db.transaction::<_, _, crate::services::CallError, Outbox, _, _, _>(|tx, outbox| Box::pin(async move {
            standings::add_lp(tx, outbox, owner.character, infinity_core::consts::CORP_HERALDRY, 10_000, LoyaltyMutation {
                reason: LoyaltyReason::GmAdjustment, occurred_at: now,
                context: FactContext { corporation_id: None, alliance_id: None, faction_id: None, ship_id: None, ship_type_id: None, system_id: None, station_id: None, origin: FactOrigin::Gm },
            }).await
        }), |(), outbox| async move { outbox::dispatch_world((), outbox, &world) }).await.unwrap();
        drop(_market);
        let pay = with_goal("eve_public.dailygoal.api.PayForCompletionRequest", second_id.to_vec());
        for _ in 0..2 {
            assert_eq!(handle_request(&gateway, envelope(&pay, owner.account, owner.character, vec![11; UUID_LEN]), None).await.status_code, 200);
        }
    }).await;
    resources.world.unregister_pilot_generation(owner.character, None);
    for sql in [
        "DELETE FROM game.daily_goal_admin_audit WHERE subject_id=$1 OR actor_id=$1",
        "DELETE FROM game.daily_goal_payments WHERE character_id=$1",
        "DELETE FROM game.daily_goal_rewards WHERE goal_id IN (SELECT goal_id FROM game.daily_goal_instances WHERE character_id=$1)",
        "DELETE FROM game.daily_goal_instances WHERE character_id=$1",
        "DELETE FROM game.daily_goal_periods WHERE character_id=$1",
    ] { db.execute(sql, &[&owner.character]).await.unwrap(); }
    fixture::cleanup(&db, owner).await.unwrap();
    std::fs::remove_dir_all(root).unwrap();
    if let Err(panic) = result { std::panic::resume_unwind(panic); }
}
