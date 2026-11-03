use super::*;
use super::super::fleet_proxy::{capacity, display, eligible, normalize, still_current};
use crate::dao::characters::contacts::Contact;
use crate::services::value_field;
use crate::session::OnlineSession;

fn session(id: i64, corp: i64, alliance: Option<i64>, faction: Option<i64>) -> OnlineSession {
    OnlineSession { character_id: id, station_id: None, system_id: Some(10), corporation_id: Some(corp), alliance_id: alliance, faction_id: faction, fleet_id: None, role: 0, corporation_role: 0 }
}
fn contact(id: i64, standing: f32) -> Contact {
    Contact { contact_id: id, relationship: standing, in_watchlist: false, label_mask: 0 }
}
fn advert(fields: Vec<(&str, Value)>) -> Value { normalize(&Value::dict(fields), 123).unwrap() }

#[test]
fn normalization_ignores_forged_authority_and_checks_limits() {
    let ad = advert(vec![("fleetName", Value::unicode("舰".repeat(40))), ("description", Value::unicode("文".repeat(260))), ("inviteScope", Value::Int(8)), ("fleetID", Value::Int(999)), ("numMembers", Value::Int(1)), ("public_allowedEntities", Value::list([Value::Int(777)])), ("advertJoinLimit", Value::Int(999))]);
    assert_eq!(value_field(&ad, "fleetName").unwrap().as_text().unwrap().chars().count(), 32);
    assert_eq!(value_field(&ad, "description").unwrap().as_text().unwrap().chars().count(), 250);
    assert_eq!(value_field(&ad, "advertJoinLimit"), Some(&Value::Int(256)));
    assert!(value_field(&ad, "fleetID").is_none());
    assert!(value_field(&ad, "numMembers").is_none());
    assert_eq!(value_field(&ad, "public_allowedEntities"), Some(&Value::list([])));
    assert!(normalize(&Value::dict([("inviteScope", Value::Int(32))]), 1).is_none());
    assert!(normalize(&Value::dict([("public_minStanding", Value::Float(f64::NAN))]), 1).is_none());
    assert!(normalize(&Value::dict([("public_allowedEntities", Value::list((1..=1001).map(Value::Int)))]), 1).is_none());
}

#[test]
fn advert_display_uses_authoritative_fields_and_python_sets() {
    let leader = session(1, 10, Some(20), Some(30));
    let ad = advert(vec![("inviteScope", Value::Int(1 | 2 | 4 | 8)), ("public_minStanding", Value::Float(2.0)), ("membergroups_minStanding", Value::Float(2.0)), ("public_allowedEntities", Value::list([Value::Int(999)]))]);
    let result = display(ad, 5, 7, &leader, Some(3.0), Some(4), &[contact(8, 3.0), contact(9, -1.0)], &[6]);
    assert_eq!(value_field(&result, "fleetID"), Some(&Value::Int(5)));
    assert_eq!(value_field(&result, "numMembers"), Some(&Value::Int(7)));
    assert_eq!(value_field(value_field(&result, "leader").unwrap(), "corpID"), Some(&Value::Int(10)));
    let expected = Value::reduce("__builtin__.set", vec![Value::list([Value::Int(8)])], None);
    assert_eq!(value_field(&result, "public_allowedEntities"), Some(&expected));
    let expected = Value::reduce("__builtin__.set", vec![Value::list([Value::Int(6)])], None);
    assert_eq!(value_field(&result, "public_disallowedEntities"), Some(&expected));
    let expected = Value::reduce("__builtin__.set", vec![Value::list([Value::Int(8), Value::Int(10), Value::Int(20), Value::Int(30)])], None);
    assert_eq!(value_field(&result, "membergroups_allowedEntities"), Some(&expected));
}

#[test]
fn eligibility_uses_real_affiliations_contacts_blocks_and_security() {
    let leader = session(1, 10, Some(20), Some(30));
    let applicant = session(2, 11, Some(20), Some(31));
    let ad = advert(vec![("inviteScope", Value::Int(2 | 8)), ("membergroups_minStanding", Value::Float(2.0)), ("membergroups_minSecurity", Value::Float(1.0)), ("public_minStanding", Value::Float(5.0))]);
    let contacts = [contact(2, -1.0), contact(20, 3.0)];
    assert_eq!(eligible(&ad, &leader, &applicant, &contacts, &[], &[], Some(1.0)), Some(3.0));
    assert!(eligible(&ad, &leader, &applicant, &contacts, &[20], &[], Some(1.0)).is_none());
    assert!(eligible(&ad, &leader, &applicant, &contacts, &[], &[1], Some(1.0)).is_none());
    assert!(eligible(&ad, &leader, &applicant, &contacts, &[], &[], Some(0.9)).is_none());
    assert!(eligible(&ad, &leader, &applicant, &contacts, &[], &[], None).is_none());
    let outsider = session(3, 12, None, None);
    assert!(eligible(&ad, &leader, &outsider, &contacts, &[], &[], Some(5.0)).is_none());
    assert_eq!(eligible(&ad, &leader, &outsider, &[contact(3, 5.0)], &[], &[], Some(5.0)), Some(5.0));
    let open = advert(vec![("inviteScope", Value::Int(16))]);
    assert!(eligible(&open, &leader, &outsider, &[], &[], &[], Some(0.0)).is_none());
    assert!(eligible(&open, &leader, &outsider, &[contact(3, 0.1)], &[], &[], Some(0.0)).is_some());
}

#[test]
fn stale_leader_withdrawal_and_capacity_refuse_snapshot() {
    let ad = advert(vec![("inviteScope", Value::Int(8)), ("advertJoinLimit", Value::Int(2))]);
    let mut f = Fleet::new(1, vec![]);
    f.members.insert(1, Member::new(NONE_ID, NONE_ID, ROLE_LEADER, JOB_NONE, 0));
    f.advert = Some(ad.clone());
    assert!(capacity(&f, &ad));
    f.members.get_mut(&1).unwrap().reconnect = Some(ReconnectState { generation: 1, deadline: Instant::now(), offered_session: None });
    assert!(!still_current(&f, 1, 1, &ad));
    f.members.get_mut(&1).unwrap().reconnect = None;
    assert!(still_current(&f, 1, 1, &ad));
    f.members.insert(2, Member::new(1, 1, ROLE_MEMBER, JOB_NONE, 0));
    assert!(!capacity(&f, &ad));
    assert!(!still_current(&f, 1, 1, &ad));
    f.members.remove(&2);
    f.members.get_mut(&1).unwrap().role = ROLE_MEMBER;
    f.members.insert(2, Member::new(NONE_ID, NONE_ID, ROLE_LEADER, JOB_NONE, 0));
    assert!(!still_current(&f, 1, 1, &ad));
    f.advert = None;
    assert!(!still_current(&f, 2, 2, &ad));
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external listener flock"]
async fn leader_transfer_withdraws_or_refreshes_advert() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = std::sync::Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = World::new(db, sde, 1);
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(2, tx);
    world.set_online_session(2, Some(session(2, 12, Some(20), Some(30))));
    let mut f = Fleet::new(1, vec![]);
    f.options.registered = true;
    f.join_requests.insert(3, Value::None);
    f.advert = Some(advert(vec![("updateOnBossChange", Value::Bool(false))]));
    super::super::fleet_proxy::leader_changed(&world, &mut f, 2);
    assert!(f.advert.is_none());
    assert!(!f.options.registered);
    assert!(f.join_requests.is_empty());
    f.options.registered = true;
    f.advert = Some(advert(vec![("updateOnBossChange", Value::Bool(true))]));
    super::super::fleet_proxy::leader_changed(&world, &mut f, 2);
    let ad = f.advert.as_ref().unwrap();
    assert_eq!(value_field(value_field(ad, "leader").unwrap(), "charID"), Some(&Value::Int(2)));
    assert_eq!(value_field(ad, "solarSystemID"), Some(&Value::Int(10)));
    assert!(f.options.registered);
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external listener flock"]
async fn application_becomes_normal_invitation_without_membership() {
    use crate::{conn::Conn, handshake::Login};
    use std::sync::Arc;
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let now = crate::client::filetime_now();
    let account = -now;
    let leader = 1_500_000_000 + now % 100_000_000;
    let applicant = leader + 1;
    let name = format!("finder-test-{now}");
    let password: Vec<u8> = Vec::new();
    db.execute("INSERT INTO game.accounts (user_id,user_name,password_hash,role) VALUES ($1,$2,$3,0)", &[&account, &name, &password]).await.unwrap();
    for id in [leader, applicant] {
        let character_name = format!("{name}-{id}");
        db.execute("INSERT INTO game.characters (character_id,user_id,name,race_id,bloodline_id,gender_id,corporation_id,balance) VALUES ($1,$2,$3,1,1,1,$4,0)", &[&id, &account, &character_name, &account]).await.unwrap();
    }
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    let (leader_tx, mut leader_rx) = tokio::sync::mpsc::unbounded_channel();
    let (applicant_tx, mut applicant_rx) = tokio::sync::mpsc::unbounded_channel();
    world.register_pilot(leader, leader_tx); world.register_pilot(applicant, applicant_tx);
    world.set_online_session(leader, Some(session(leader, account, None, None)));
    world.set_online_session(applicant, Some(session(applicant, account, None, None)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let _peer = tokio::net::TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let mut client = Client::new(Conn::new(stream, address), Arc::new(crate::config::Config::default()), db.clone(), sde,
        crate::chat::Hub::new("localhost", db.clone(), world.clone()), world.clone(),
        Login { user_id: account, user_name: name, role: 0, client_id: 1, session_id: 1, language_id: "EN".into() });
    client.session.apply(vec![("charid", Value::Int(applicant)), ("corpid", Value::Int(account)), ("solarsystemid2", Value::Int(10))]);
    let fleet_id = next_id();
    let mut fleet = Fleet::new(leader, vec![Wing { id: 10, name: String::new(), squads: vec![Squad { id: 11, name: String::new() }] }]);
    fleet.members.insert(leader, Member::new(NONE_ID, NONE_ID, ROLE_LEADER, JOB_NONE, 0));
    fleet.advert = Some(advert(vec![("inviteScope", Value::Int(8)), ("joinNeedsApproval", Value::Bool(true))]));
    { let mut st = state(); st.fleets.insert(fleet_id, fleet); st.of.insert(leader, fleet_id); }
    let args = [Value::Int(fleet_id), Value::Bool(true)];
    let kwargs = Value::None;
    let call = Call { service: "fleetProxy", method: "ApplyToJoinFleet", args: &args, kwargs: &kwargs, bound: false, bind_params: None };
    assert_eq!(super::super::fleet_proxy::dispatch(&mut client, &call).await.unwrap().unwrap(), Value::Bool(true));
    let request = leader_rx.try_recv().unwrap();
    assert_eq!(request.0, "OnFleetJoinRequest");
    assert_eq!(value_field(&request.1[0], "charID"), Some(&Value::Int(applicant)));
    assert_eq!(leader_rx.try_recv().unwrap().0, "OnJoinRequestUpdate");
    client.session.apply(vec![("charid", Value::Int(leader))]);
    super::super::fleet_object_handler::invite(&mut client, fleet_id, applicant, None, None, None).await.unwrap();
    let invitation = applicant_rx.try_recv().unwrap();
    assert_eq!(invitation.0, "OnFleetInvite");
    assert_eq!(&invitation.1[..2], &[Value::Int(fleet_id), Value::Int(leader)]);
    { let mut st = state(); let f = st.fleets.remove(&fleet_id).unwrap(); assert_eq!(f.invites.get(&applicant), Some(&(10, 11, ROLE_MEMBER))); assert!(!f.members.contains_key(&applicant)); assert!(f.join_requests.is_empty()); st.of.remove(&leader); }
    db.execute("DELETE FROM game.characters WHERE user_id=$1", &[&account]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
}
