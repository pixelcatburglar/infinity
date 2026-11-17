//! Real slash dispatch for the agent bulk-standing commands and their history RPC.
use super::*;
use crate::dao::characters::standings as dao;
use crate::services::tests::fixture;

async fn history(client: &mut Client, from: i64, to: i64) -> CallResult {
    let args = [Value::Int(from), Value::Int(to)];
    let kwargs = Value::Dict(vec![]);
    let call = Call {
        service: "standingMgr", method: "GetStandingTransactions",
        args: &args, kwargs: &kwargs, bound: false, bind_params: None,
    };
    crate::services::dispatch(client, &call).await
}

fn field<'a>(record: &'a Value, name: &str) -> &'a Value {
    let Value::Instance(_, state) = record else { panic!("expected KeyVal record") };
    state.get(name).expect("history field")
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn agent_standings_bulk_command_journals_only_union_owners() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let f = fixture::create(&db).await.unwrap();
    let gm = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = crate::services::tests::client(test_db.clone(), gm.character).await;
        client.login.role = GM_ROLES;
        // GetStandingTransactions only serves the caller's own character.
        let (mut target, _target_peer) = crate::services::tests::client(test_db.clone(), f.character).await;
        let sde = client.sde.clone();
        let corps = sde.characters.agent_corporation_ids().to_vec();
        assert!(corps.len() >= 2, "dedicated SDE must expose at least two agent corporations");
        // Same owner union the service writes: agent corporations plus their positive factions.
        let mut owners = corps.clone();
        owners.extend(corps.iter().filter_map(|&id| sde.npc_corporations.get(id).and_then(|corp| corp.faction_id)).filter(|&id| id > 0));
        owners.sort_unstable();
        owners.dedup();
        let negative = corps[0];
        let full = owners.iter().copied().find(|&id| id != negative).expect("union has a second owner");
        let private = sde.characters.npc_characters().find(|row| row.is_agent && !row.deleted).expect("live agent").character_id;
        let outsider = sde.npc_corporations.iter().map(|row| row.corporation_id).find(|id| !owners.contains(id)).expect("non-union NPC corporation");
        let mut rx = support::register_online(
            &client.world,
            infinity_core::session::OnlineSession::from_session(&target.session).unwrap(),
        );
        for (from, value, character) in [(negative, -5.0, f.character), (full, 10.0, f.character), (private, -4.0, f.character), (outsider, -3.0, f.character), (gm.character, 2.0, f.character), (negative, -6.0, gm.character)] {
            dao::set(&test_db, character, from, value).await.unwrap();
        }
        support::rpc(&mut client, &format!("/maxagentstandings {}", f.character)).await.unwrap();
        assert_eq!(dao::standing(&test_db, f.character, negative).await.unwrap(), Some(10.0));
        for (from, unchanged) in [(private, -4.0), (outsider, -3.0), (gm.character, 2.0)] {
            assert_eq!(dao::standing(&test_db, f.character, from).await.unwrap(), Some(unchanged));
        }
        assert_eq!(dao::standing(&test_db, gm.character, negative).await.unwrap(), Some(-6.0));
        for owner in &owners {
            let expected = if *owner == full { 0 } else { 1 };
            assert_eq!(dao::history::list(&test_db, f.character, *owner).await.unwrap().len(), expected, "owner {owner}");
        }
        let mut notified = Vec::new();
        while let Ok((event, notice)) = rx.try_recv() {
            assert_eq!(event, "OnStandingSet");
            notified.push(notice);
        }
        let expected: Vec<Vec<Value>> = owners.iter().filter(|&&id| id != full)
            .map(|&id| vec![Value::Int(id), Value::Int(f.character), Value::Float(10.0)]).collect();
        assert_eq!(notified, expected);
        let Ok(Value::List(rows)) = history(&mut target, negative, f.character).await else { panic!("expected history list") };
        assert_eq!(rows.len(), 1);
        assert_eq!(field(&rows[0], "msg").as_text().unwrap(), messages::standings::REASON_GM_COMMAND);
        assert_eq!(field(&rows[0], "modification").as_float().unwrap(), 1.5);
        assert_eq!(field(&rows[0], "eventTypeID"), &Value::Int(84));
        assert_eq!(field(&rows[0], "int_1"), &Value::Int(gm.character));
        support::rpc(&mut client, &format!("/fullstandings {}", f.character)).await.unwrap();
        assert_eq!(dao::history::list(&test_db, f.character, negative).await.unwrap().len(), 1);
        assert!(rx.try_recv().is_err());
        // The target session has no GM role and cannot read another character's journal.
        assert!(support::rpc(&mut target, &format!("/maxagentstandings {}", f.character)).await.is_err());
        assert!(history(&mut target, negative, gm.character).await.is_err());
        client.world.unregister_pilot_generation(f.character, None);
    }).await;
    db.execute("DELETE FROM game.standings WHERE char_id = ANY($1)", &[&vec![f.character, gm.character]]).await.unwrap();
    fixture::cleanup(&db, f).await.unwrap();
    fixture::cleanup(&db, gm).await.unwrap();
    result.expect("bulk standings test task failed");
}
