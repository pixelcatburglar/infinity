use super::*;
use crate::services::tests::{self, fixture};

async fn rpc(c: &mut Client, method: &str, text: &str, exact: Value) -> CallResult {
    let args = [Value::unicode(text), exact];
    let kwargs = Value::Dict(vec![]);
    crate::services::dispatch(c, &Call { service: "lookupSvc", method, args: &args, kwargs: &kwargs, bound: false, bind_params: None }).await
}

fn field(row: &Value, key: &str) -> Option<Value> {
    crate::services::value_field(row, key).cloned()
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn lookup_dispatch_searches_active_owners_and_preserves_war_contract() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let active = fixture::create(&db).await.unwrap();
    let deleted = fixture::create(&db).await.unwrap();
    let corp = fixture::create_corporation(&db, active.character, 1).await.unwrap();
    let base = format!("查找{}", active.character);
    let name = format!("{base} Alpha%_Pilot");
    db.execute("UPDATE game.characters SET name=$2 WHERE character_id=$1", &[&active.character, &name]).await.unwrap();
    db.execute("UPDATE game.characters SET name=$2, deleted=true WHERE character_id=$1", &[&deleted.character, &format!("{base} Deleted")]).await.unwrap();
    let corp_name = format!("{base} Corporation");
    db.execute("UPDATE game.corporations SET name=$2 WHERE corporation_id=$1", &[&corp, &corp_name]).await.unwrap();
    let alliance_name = format!("{base} Alliance");
    let alliance: i64 = db.query_one("INSERT INTO game.alliances (name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES ($1, 'lookup-' || nextval('game.alliances_alliance_id_seq')::text, $2,$2,$3,1) RETURNING alliance_id", &[&alliance_name, &corp, &active.character]).await.unwrap().get("alliance_id");
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut c, _peer) = tests::client(test_db.clone(), active.character).await;
        for exact in [Value::Bool(true), Value::Int(1), Value::Int(-3), Value::BigLong(vec![0, 0, 0, 0, 0, 0, 0, 0, 1])] {
            let decoded = marshal::loads(&marshal::dumps(&exact)).unwrap();
            for exact in [exact, decoded] {
                let reply = rpc(&mut c, "LookupEvePlayerCharacters", &format!("  {}  ", name.to_lowercase()), exact.clone()).await.unwrap();
                assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), reply);
                let rows = reply.items().unwrap();
                assert_eq!(rows.len(), 1);
                assert_eq!(field(&rows[0], "characterID"), Some(Value::Int(active.character)));
                assert_eq!(field(&rows[0], "ownerID"), None);
                assert_eq!(rpc(&mut c, "LookupEvePlayerCharacters", &base, exact).await.unwrap(), Value::list([]));
            }
        }
        for exact in [Value::Bool(false), Value::Int(0), Value::BigLong(vec![0; 9])] {
            let decoded = marshal::loads(&marshal::dumps(&exact)).unwrap();
            for exact in [exact, decoded] {
                let reply = rpc(&mut c, "LookupPCOwners", &base, exact).await.unwrap();
                assert_eq!(marshal::loads(&marshal::dumps(&reply)).unwrap(), reply);
                let rows = reply.items().unwrap();
                for (id, type_id) in [(active.character, i64::from(TYPE_CHARACTER)), (corp, TYPE_CORPORATION), (alliance, TYPE_ALLIANCE)] {
                    let row = rows.iter().find(|r| field(r, "ownerID") == Some(Value::Int(id))).unwrap();
                    let group = c.sde.types.get(i32::try_from(type_id).unwrap()).unwrap().group_id.unwrap();
                    assert_eq!(field(row, "typeID"), Some(Value::Int(type_id)));
                    assert_eq!(field(row, "groupID"), Some(Value::Int(i64::from(group))));
                    assert!(field(row, "ownerName").is_some());
                }
                assert!(!rows.iter().any(|r| field(r, "ownerID") == Some(Value::Int(deleted.character))));
            }
        }
        for text in ["%_", "Alpha%_"] {
            let reply = rpc(&mut c, "LookupEvePlayerCharacters", text, Value::Int(0)).await.unwrap();
            assert!(reply.items().unwrap().iter().any(|r| field(r, "characterID") == Some(Value::Int(active.character))));
        }
        for text in ["Alpha__Pilot", "Alpha%%Pilot", "", "   ", "not a matching owner name 92817465"] {
            assert_eq!(rpc(&mut c, "LookupEvePlayerCharacters", text, Value::Int(0)).await.unwrap(), Value::list([]));
        }
        assert_eq!(rpc(&mut c, "LookupPCOwners", &base, Value::Bool(true)).await.unwrap(), Value::list([]));
        let invalid_args = [Value::Int(1), Value::Int(0)];
        let kwargs = Value::Dict(vec![]);
        let invalid = Call { service: "lookupSvc", method: "LookupOwners", args: &invalid_args, kwargs: &kwargs, bound: false, bind_params: None };
        let crate::services::CallError::User { msg, dict } = crate::services::dispatch(&mut c, &invalid).await.unwrap_err() else { panic!("expected lookup refusal") };
        assert_eq!(msg, crate::messages::MSG_CUSTOM_NOTIFY);
        assert_eq!(dict, vec![("notify".into(), Value::unicode(crate::messages::lookup::INVALID))]);
        let war = rpc(&mut c, "LookupWarableCorporationsOrAlliances", &base, Value::Bool(false)).await.unwrap();
        assert_eq!(marshal::loads(&marshal::dumps(&war)).unwrap(), war);
        for id in [corp, alliance] {
            let row = war.items().unwrap().iter().find(|r| field(r, "ownerID") == Some(Value::Int(id))).unwrap();
            assert!(field(row, "name").is_some());
            assert_eq!(field(row, "ownerName"), None);
            assert_eq!(field(row, "warPermit"), Some(Value::Bool(false)));
        }
        assert!(rpc(&mut c, "LookupWarableCorporationsOrAlliances", &base, Value::Int(0)).await.is_err());
        test_db.execute("UPDATE game.corporations SET deleted=true WHERE corporation_id=$1", &[&corp]).await.unwrap();
        test_db.execute("UPDATE game.alliances SET deleted=true WHERE alliance_id=$1", &[&alliance]).await.unwrap();
        let remaining = rpc(&mut c, "LookupPCOwners", &base, Value::Int(0)).await.unwrap();
        for id in [corp, alliance] { assert!(!remaining.items().unwrap().iter().any(|r| field(r, "ownerID") == Some(Value::Int(id)))); }
        let npc = c.sde.characters.npc_characters().find_map(|r| {
            r.name_id.and_then(|id| c.sde.localization.chinese(id)).or(r.name.as_deref()).filter(|s| !s.is_empty()).map(|name| (r.character_id, name.to_owned()))
        }).expect("named NPC character in SDE");
        let generic = rpc(&mut c, "LookupOwners", &npc.1, Value::Bool(true)).await.unwrap();
        let row = generic.items().unwrap().iter().find(|r| field(r, "ownerID") == Some(Value::Int(npc.0))).unwrap();
        let group = c.sde.types.get(TYPE_CHARACTER).unwrap().group_id.unwrap();
        assert_eq!(field(row, "typeID"), Some(Value::Int(i64::from(TYPE_CHARACTER))));
        assert_eq!(field(row, "groupID"), Some(Value::Int(i64::from(group))));
        let pc = rpc(&mut c, "LookupPCOwners", &npc.1, Value::Bool(true)).await.unwrap();
        assert!(!pc.items().unwrap().iter().any(|r| field(r, "ownerID") == Some(Value::Int(npc.0))));
        let npc_corp = c.sde.npc_corporations.iter().find_map(|r| r.name.as_deref().filter(|s| !s.is_empty()).map(|name| (r.corporation_id, name.to_owned()))).expect("named NPC corporation in SDE");
        let faction = c.sde.factions.iter().find_map(|r| r.name.as_deref().filter(|s| !s.is_empty()).map(|name| (r.faction_id, name.to_owned()))).expect("named faction in SDE");
        for ((id, name), type_id) in [(npc_corp, TYPE_CORPORATION), (faction, TYPE_FACTION)] {
            let generic = rpc(&mut c, "LookupOwners", &name, Value::Int(1)).await.unwrap();
            let row = generic.items().unwrap().iter().find(|r| field(r, "ownerID") == Some(Value::Int(id))).unwrap();
            let group = c.sde.types.get(i32::try_from(type_id).unwrap()).unwrap().group_id.unwrap();
            assert_eq!(field(row, "typeID"), Some(Value::Int(type_id)));
            assert_eq!(field(row, "groupID"), Some(Value::Int(i64::from(group))));
            let pc = rpc(&mut c, "LookupPCOwners", &name, Value::Int(1)).await.unwrap();
            assert!(!pc.items().unwrap().iter().any(|r| field(r, "ownerID") == Some(Value::Int(id))));
        }
    }).await;
    db.execute("DELETE FROM game.alliances WHERE alliance_id=$1", &[&alliance]).await.unwrap();
    fixture::cleanup_corporation(&db, corp, &[active.character], 0).await.unwrap();
    fixture::cleanup(&db, active).await.unwrap();
    fixture::cleanup(&db, deleted).await.unwrap();
    result.unwrap();
}
