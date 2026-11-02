//! Mailing-list RPC integration regressions using real dispatch and database fixtures.
mod delivery;
mod lua;
mod concurrency;
mod boundaries;
mod acl;
mod capacity;
mod organizations;
mod regressions;
mod welcome_atomicity;
use super::*;
use crate::{client::Client, db::Db, services::{Call, CallError}};
use crate::services::tests::fixture;

async fn rpc(c: &mut Client, service: &str, method: &str, args: &[Value]) -> Result<Value, CallError> {
    let kwargs = Value::None;
    crate::services::dispatch(c, &Call { service, method, args, kwargs: &kwargs, bound: false, bind_params: None }).await
}
async fn list(c: &mut Client, method: &str, args: &[Value]) -> Result<Value, CallError> {
    rpc(c, "mailingListsMgr", method, args).await
}
fn batch(ids: &[i64]) -> Value { Value::list(ids.iter().copied().map(Value::Int)) }
fn wire(value: &Value) { assert_eq!(marshal::loads(&marshal::dumps(value)).unwrap(), *value); }

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn manager_membership_welcome_and_tombstone_dispatch() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let guest = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut a, _a) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut b, _b) = crate::services::tests::client(test_db.clone(), guest.character).await;
        let name = format!("list {}", owner.character);
        let id = list(&mut a, "Create", &[Value::unicode(&name), Value::Int(1), Value::Int(1), Value::Int(0)]).await.unwrap().as_int().unwrap();
        let arg = Value::Int(id);
        let joined = list(&mut a, "GetJoinedLists", &[]).await.unwrap();
        wire(&joined);
        let owner_view = match &joined { Value::Dict(rows) => &rows.iter().find(|(k, _)| *k == arg).unwrap().1, _ => panic!("expected dictionary") };
        assert_eq!(crate::services::value_field(owner_view, "isOwner"), Some(&Value::Bool(true)));
        let owner_join = list(&mut a, "Join", &[Value::unicode(&name)]).await.unwrap();
        assert_eq!(crate::services::value_field(&owner_join, "isOwner"), Some(&Value::Bool(true)));
        wire(&owner_join);
        assert!(list(&mut a, "Leave", &[arg.clone()]).await.is_err());
        for method in ["KickMembers", "SetMembersMuted", "SetMembersOperator", "SetMembersClear"] {
            assert!(list(&mut a, method, &[arg.clone(), batch(&[owner.character])]).await.is_err());
            assert_eq!(list(&mut a, method, &[arg.clone(), batch(&[])]).await.unwrap(), Value::None);
        }
        assert_eq!(list(&mut a, "GetWelcomeMail", &[arg.clone()]).await.unwrap(), Value::list([]));
        assert_eq!(list(&mut a, "SaveWelcomeMail", &[arg.clone(), Value::unicode("welcome"), Value::unicode("")]).await.unwrap(), Value::None);
        assert!(crate::dao::mail::statuses(&test_db, owner.character).await.unwrap().is_empty());
        let welcome = list(&mut a, "GetWelcomeMail", &[arg.clone()]).await.unwrap(); wire(&welcome);
        assert!(list(&mut b, "Join", &[Value::unicode(&name)]).await.is_ok());
        assert_eq!(crate::dao::mail::statuses(&test_db, guest.character).await.unwrap().len(), 1);
        list(&mut b, "Join", &[Value::unicode(&name)]).await.unwrap();
        assert_eq!(crate::dao::mail::statuses(&test_db, guest.character).await.unwrap().len(), 1);
        for method in ["GetMembers", "GetSettings", "GetWelcomeMail"] { assert!(list(&mut b, method, &[arg.clone()]).await.is_err()); }
        let members = list(&mut a, "GetMembers", &[arg.clone()]).await.unwrap(); wire(&members);
        assert!(matches!(&members, Value::Dict(rows) if rows.contains(&(Value::Int(owner.character), Value::Int(3))) && rows.contains(&(Value::Int(guest.character), Value::Int(1)))));
        let settings = list(&mut a, "GetSettings", &[arg.clone()]).await.unwrap(); wire(&settings);
        assert!(list(&mut a, "SetMembersMuted", &[arg.clone(), batch(&[guest.character, owner.character])]).await.is_err());
        assert_eq!(crate::dao::mailing_lists::membership(&test_db, id, guest.character).await.unwrap(), Some(1));
        assert_eq!(list(&mut a, "SetMembersOperator", &[arg.clone(), batch(&[guest.character])]).await.unwrap(), Value::None);
        assert_eq!(list(&mut b, "SetMembersClear", &[arg.clone(), batch(&[guest.character])]).await.unwrap(), Value::None);
        assert_eq!(list(&mut a, "SetMembersMuted", &[arg.clone(), batch(&[guest.character])]).await.unwrap(), Value::None);
        assert_eq!(list(&mut a, "SendWelcomeMail", &[arg.clone(), Value::unicode("again"), Value::unicode("body")]).await.unwrap(), Value::None);
        assert_eq!(crate::dao::mail::statuses(&test_db, guest.character).await.unwrap().len(), 2);
        assert_eq!(crate::dao::mail::statuses(&test_db, owner.character).await.unwrap().len(), 1);
        assert_eq!(list(&mut a, "ClearWelcomeMail", &[arg.clone()]).await.unwrap(), Value::None);
        assert_eq!(list(&mut a, "SetDefaultAccess", &[arg.clone(), Value::Int(1), Value::Int(1), Value::Int(0)]).await.unwrap(), Value::None);
        assert_eq!(list(&mut a, "SetEntitiesAccess", &[arg.clone(), Value::Dict(vec![(Value::Int(guest.character), Value::Int(0))])]).await.unwrap(), Value::None);
        assert_eq!(list(&mut a, "ClearEntityAccess", &[arg.clone(), Value::Int(guest.character)]).await.unwrap(), Value::None);
        assert_eq!(list(&mut a, "KickMembers", &[arg.clone(), batch(&[guest.character])]).await.unwrap(), Value::None);
        assert!(list(&mut b, "Join", &[Value::unicode(&name)]).await.is_err());
        list(&mut a, "ClearEntityAccess", &[arg.clone(), Value::Int(guest.character)]).await.unwrap();
        list(&mut b, "Join", &[Value::unicode(&name)]).await.unwrap();
        assert_eq!(crate::dao::mail::statuses(&test_db, guest.character).await.unwrap().len(), 2);
        list(&mut b, "Leave", &[arg.clone()]).await.unwrap();
        list(&mut b, "Join", &[Value::unicode(&name)]).await.unwrap();
        list(&mut a, "Delete", &[arg.clone()]).await.unwrap();
        assert_eq!(list(&mut a, "GetJoinedLists", &[]).await.unwrap(), Value::Dict(vec![]));
        let info = list(&mut a, "GetInfo", &[arg.clone()]).await.unwrap(); wire(&info);
        assert_eq!(crate::services::value_field(&info, "displayName"), Some(&Value::unicode(&name)));
        let replacement = list(&mut a, "Create", &[Value::unicode(&name), Value::Int(1), Value::Int(1), Value::Int(0)]).await.unwrap().as_int().unwrap();
        assert_ne!(replacement, id);
    }).await;
    crate::dao::mailing_lists::tests::cleanup(&db, owner.character, &[owner.character, guest.character]).await.unwrap();
    for f in [owner, guest] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}
