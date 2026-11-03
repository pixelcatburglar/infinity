use crate::{services::{self, Call}, session::OnlineSession};
use marshal::Value;

fn guest(id: i64, station: i64, corporation: Option<i64>, alliance: Option<i64>, faction: Option<i64>) -> OnlineSession {
    OnlineSession { character_id: id, station_id: Some(station), system_id: None, corporation_id: corporation,
        alliance_id: alliance, faction_id: faction, fleet_id: None, role: 0, corporation_role: 0 }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn dispatch_get_guests_returns_live_real_and_server_owned_pilots() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let fixture = &services::tests::fixture::create;
    let caller = fixture(&db).await.unwrap();
    let real = fixture(&db).await.unwrap();
    let simulated = fixture(&db).await.unwrap();
    let offline = fixture(&db).await.unwrap();
    let simulated_owner = crate::dao::accounts::simulated::account_id(&db).await.unwrap();
    assert_eq!(db.execute("UPDATE game.characters SET user_id=$1 WHERE character_id=$2", &[&simulated_owner, &simulated.character]).await.unwrap(), 1);
    let owner = db.query_one("SELECT user_id FROM game.characters WHERE character_id=$1", &[&simulated.character]).await.unwrap();
    assert_eq!(owner.get::<_, i64>("user_id"), simulated_owner);
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = services::tests::client(test_db, caller.character).await;
        let station = 10;
        client.session.apply(vec![("stationid", Value::Int(station))]);
        let (self_tx, _self_rx) = tokio::sync::mpsc::unbounded_channel();
        let (real_tx, mut real_rx) = tokio::sync::mpsc::unbounded_channel();
        let (sim_tx, mut sim_rx) = tokio::sync::mpsc::unbounded_channel();
        let (other_tx, _other_rx) = tokio::sync::mpsc::unbounded_channel();
        for (s, tx) in [
            (guest(caller.character, station, Some(1), None, None), self_tx),
            (guest(real.character, station, Some(2), Some(3), None), real_tx),
            (guest(simulated.character, station, None, None, Some(4)), sim_tx),
            (guest(offline.character, 20, Some(1), None, None), other_tx),
        ] {
            client.world.register_pilot(s.character_id, tx);
            client.world.set_online_session(s.character_id, Some(s));
        }
        let kwargs = Value::Dict(vec![]);
        let call = Call { service: "station", method: "GetGuests", args: &[], kwargs: &kwargs, bound: false, bind_params: None };
        let response = services::dispatch(&mut client, &call).await.unwrap();
        let expected = [
            Value::tuple([Value::Int(caller.character), Value::Int(1), Value::None, Value::None]),
            Value::tuple([Value::Int(real.character), Value::Int(2), Value::Int(3), Value::None]),
            Value::tuple([Value::Int(simulated.character), Value::None, Value::None, Value::Int(4)]),
        ];
        if let Value::List(rows) = response {
            assert_eq!(rows.len(), expected.len());
            for record in &expected { assert_eq!(rows.iter().filter(|row| *row == record).count(), 1); }
        } else {
            panic!("expected tuple list");
        }
        for (service, method, args) in [
            ("beyonce", "CmdDock", vec![Value::Int(station)]),
            ("ship", "Undock", vec![]),
        ] {
            let failed = Call { service, method, args: &args, kwargs: &kwargs, bound: false, bind_params: None };
            assert!(services::dispatch(&mut client, &failed).await.is_err());
            assert_eq!(client.session.int("stationid"), Some(station));
            let roster = services::dispatch(&mut client, &call).await.unwrap();
            if let Value::List(rows) = roster {
                assert_eq!(rows.len(), expected.len());
                for record in &expected { assert_eq!(rows.iter().filter(|row| *row == record).count(), 1); }
            } else {
                panic!("expected tuple list");
            }
            assert!(matches!(real_rx.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
            assert!(matches!(sim_rx.try_recv(), Err(tokio::sync::mpsc::error::TryRecvError::Empty)));
        }
        client.world.unregister_pilot(caller.character);
        client.world.unregister_pilot(offline.character);
        let reply = services::dispatch(&mut client, &call).await.unwrap();
        if let Value::List(rows) = reply { assert_eq!(rows.len(), 2); } else { panic!("expected tuple list"); }
        client.session.apply(vec![("stationid", Value::None)]);
        assert_eq!(services::dispatch(&mut client, &call).await.unwrap(), Value::list([]));
    }).await;
    for f in [caller, real, simulated, offline] { services::tests::fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}
