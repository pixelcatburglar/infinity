use super::*;

// serviceConst.py ROLE_PETITIONEE enables support-agent views.
const ROLE_PETITIONEE: i64 = 256;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn support_dispatch_preserves_fixture_state() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let foreign = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = super::client(test_db.clone(), owner.character).await;
        let (mut other, _other_peer) = super::client(test_db.clone(), foreign.character).await;
        client.login.user_id = owner.account;
        other.login.user_id = foreign.account;
        other.session.apply(vec![("userid", Value::Int(foreign.account))]);
        for (selected, role) in [(false, 0), (true, 0), (true, ROLE_PETITIONEE)] {
            client.login.role = role as _;
            client.session.apply(vec![("userid", Value::Int(owner.account)), ("charid", if selected { Value::Int(owner.character) } else { Value::None }), ("role", Value::Int(role))]);
            let login = (client.login.user_id, client.login.user_name.clone(), client.login.role, client.login.client_id, client.login.session_id, client.login.language_id.clone());
            let session: Vec<_> = client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect();
            let sid = client.session.sid;
            let bound = client.bound.clone();
            let pilots = client.world.online_pilots();
            let before_owner = crate::dao::tests::structure_snapshot(&test_db, owner.character).await.unwrap();
            let before_foreign = crate::dao::tests::structure_snapshot(&test_db, foreign.character).await.unwrap();
            let skills = crate::dao::skills::training::current_many(&test_db, &[owner.character, foreign.character]).await.unwrap();
            let owner_queue = crate::dao::skills::queue::list(&test_db, owner.character).await.unwrap();
            let foreign_queue = crate::dao::skills::queue::list(&test_db, foreign.character).await.unwrap();
            let empty = Value::Dict(vec![]);
            let id = Value::Int(1);
            let text = Value::unicode("fixture");
            let create = vec![text.clone(), text.clone(), id.clone(), Value::None, id.clone()];
            let mut full_create = create.clone();
            full_create.extend([text.clone(), text.clone(), Value::list([])]);
            let legacy_kwargs = Value::dict([("chatLog", text.clone()), ("combatLog", text.clone())]);
            let jwt_kwargs = Value::dict([("ticketID", id.clone())]);
            let reports = ["report_inappropriate_language", "report_offensive_mail", "report_macro_use", "report_questionable_transaction", "report_inappropriate_character_name", "report_exploit_abuse", "report_gambling", "report_other", "report_protection_of_minors"];
            for method in reports {
                let mut args = vec![id.clone(), text.clone()];
                if method == "report_inappropriate_language" { args.extend([id.clone(), text.clone(), text.clone()]); }
                let call = Call { service: "ingamereport", method, args: &args, kwargs: &empty, bound: false, bind_params: None };
                assert_refusal(&mut client, &call, infinity_core::messages::support::REPORT_UNAVAILABLE).await;
            }
            let cases = [
                ("MayPetition", vec![id.clone(), id.clone()], &empty),
                ("CreatePetition", full_create, &empty), ("CreatePetition", create, &legacy_kwargs),
                ("UpdatePetitionRating", vec![id.clone(), id.clone(), id.clone(), id.clone(), text.clone()], &empty),
                ("AddPetitionRating", vec![id.clone(), id.clone(), id.clone(), id.clone(), text.clone(), id.clone()], &empty),
                ("PetitioneeChat", vec![id.clone(), text.clone(), text.clone()], &empty),
                ("PetitionerChat", vec![id.clone(), text.clone()], &empty),
                ("DeletePetition", vec![id.clone()], &empty), ("CancelPetition", vec![id.clone()], &empty),
                ("ClosePetition", vec![id.clone()], &empty), ("UnClaimPetition", vec![id.clone()], &empty),
                ("EscalatePetition", vec![id.clone(), id.clone()], &empty), ("ClaimPetition", vec![id.clone()], &empty),
                ("GetZendeskJwtLink", vec![], &jwt_kwargs),
            ];
            for (method, args, kwargs) in cases {
                let call = Call { service: "petitioner", method, args: &args, kwargs, bound: false, bind_params: None };
                assert_refusal(&mut client, &call, infinity_core::messages::support::PETITION_UNAVAILABLE).await;
            }
            for (method, args, expected) in [
                ("GetCategoryHierarchicalInfo", vec![], Value::tuple([Value::Dict(vec![]), Value::Dict(vec![]), Value::Dict(vec![]), Value::Dict(vec![])])),
                ("GetMyPetitionsEx", vec![], Value::list([])), ("GetPetitionMessages", vec![id.clone()], Value::list([])),
                ("MarkAsRead", vec![id.clone()], Value::None), ("HasOpenTickets", vec![], Value::Bool(false)), ("IsSerenity", vec![], Value::Bool(true)),
            ] {
                let call = Call { service: "petitioner", method, args: &args, kwargs: &empty, bound: false, bind_params: None };
                assert!(fixed::root(&call).is_some());
                let value = dispatch(&mut client, &call).await.unwrap();
                assert_eq!(value, expected);
                assert_eq!(marshal::loads(&marshal::dumps(&value)).unwrap(), value);
            }
            for (method, args) in [
                ("GetUserCatalogCountry", vec![]), ("GetCategoryProperties", vec![id.clone()]),
                ("PropertyPopulationInfo", vec![Value::list([]), id.clone()]), ("GetClientPickerInfo", vec![text.clone(), text.clone()]),
                ("GetClaimedPetitions", vec![]), ("GetPetitionQueue", vec![id.clone()]), ("GetQueues", vec![]),
                ("GetCategories", vec![]), ("GetLog", vec![id.clone()]), ("Unknown", vec![]),
            ] {
                let call = Call { service: "petitioner", method, args: &args, kwargs: &empty, bound: false, bind_params: None };
                assert!(fixed::root(&call).is_none());
                assert!(!lua_fixed_claimed(&call));
                assert_eq!(dispatch(&mut client, &call).await.unwrap(), Value::None);
            }
            for (service, method) in [("other", "CreatePetition"), ("ingamereport", "report_unknown")] {
                let call = Call { service, method, args: &[], kwargs: &empty, bound: false, bind_params: None };
                assert!(fixed::root(&call).is_none());
                assert!(!lua_fixed_claimed(&call));
                assert_eq!(dispatch(&mut client, &call).await.unwrap(), Value::None);
            }
            for (method, expected) in [("IsZendeskEnabled", Value::Bool(false)), ("IsZendeskSwapEnabled", Value::Bool(false)), ("GetUnreadMessages", Value::list([]))] {
                let call = Call { service: "petitioner", method, args: &[], kwargs: &empty, bound: false, bind_params: None };
                assert!(fixed::root(&call).is_none());
                assert!(!lua_fixed_claimed(&call));
                assert_eq!(fixed::station(&call).unwrap().unwrap(), expected);
                assert_eq!(dispatch(&mut client, &call).await.unwrap(), expected);
            }
            let call = Call { service: "charUnboundMgr", method: "GetCharacterInfo", args: &[], kwargs: &empty, bound: false, bind_params: None };
            assert_eq!(dispatch(&mut client, &call).await.unwrap(), Value::list([]));
            assert_eq!((client.login.user_id, client.login.user_name.clone(), client.login.role, client.login.client_id, client.login.session_id, client.login.language_id.clone()), login);
            assert_eq!(client.session.attributes().map(|(k, v)| (k.to_owned(), v.clone())).collect::<Vec<_>>(), session);
            assert_eq!(client.session.sid, sid);
            assert_eq!(client.bound, bound);
            assert_eq!(client.world.online_pilots(), pilots);
            assert!(client.outbox.is_empty());
            assert!(other.outbox.is_empty());
            assert_eq!(crate::dao::tests::structure_snapshot(&test_db, owner.character).await.unwrap(), before_owner);
            assert_eq!(crate::dao::tests::structure_snapshot(&test_db, foreign.character).await.unwrap(), before_foreign);
            assert!(crate::dao::skills::training::current_many(&test_db, &[owner.character, foreign.character]).await.unwrap() == skills);
            assert!(crate::dao::skills::queue::list(&test_db, owner.character).await.unwrap() == owner_queue);
            assert!(crate::dao::skills::queue::list(&test_db, foreign.character).await.unwrap() == foreign_queue);
        }
    }).await;
    fixture::cleanup(&db, owner).await.unwrap();
    fixture::cleanup(&db, foreign).await.unwrap();
    result.unwrap();
}

async fn assert_refusal(client: &mut Client, call: &Call<'_>, text: &str) {
    assert!(fixed::root(call).is_some());
    assert!(lua_fixed_claimed(call));
    let Err(CallError::User { msg, dict }) = dispatch(client, call).await else { panic!("expected refusal for {}.{}", call.service, call.method) };
    assert_eq!(msg, infinity_core::messages::MSG_CUSTOM_NOTIFY);
    assert_eq!(dict, vec![("notify".into(), Value::unicode(text))]);
    let wire = crate::client::user_error(&msg, dict);
    assert_eq!(marshal::loads(&marshal::dumps(&wire)).unwrap(), wire);
}
