//! Corporation skill-plan authorization, quota and guarded writes on a real database.
use prost_reflect::DynamicMessage;
use crate::{dao::skills::plans::{self, Owner}, gateway::{message::message, notices::{producer_error, GatewayNotice}, protocol::target_group::Target}, services::{identity::Identity, skill_plans::publication, tests::fixture}, space::World};
use super::{Error, ROLE_SKILL_PLAN_MANAGER};

fn uuid() -> Vec<u8> { [crate::handshake::random_u64().to_be_bytes(), crate::handshake::random_u64().to_be_bytes()].concat() }
async fn corporation(db: &crate::db::Db, ceo: i64) -> i64 {
    db.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('skill-plan-' || nextval('game.corporations_corporation_id_seq')::text, 'SP' || currval('game.corporations_corporation_id_seq')::text, $1,$1,1,1) RETURNING corporation_id", &[&ceo]).await.unwrap().get("corporation_id")
}
async fn assign(db: &crate::db::Db, character: i64, corporation: i64, roles: i64, grantable: i64, title_mask: i64, blocked: bool) {
    db.execute("UPDATE game.characters SET corporation_id=$2, corp_roles=$3, corp_roles_hq=0, corp_roles_base=0, corp_roles_other=0, corp_grantable_roles=$4, corp_grantable_hq=0, corp_grantable_base=0, corp_grantable_other=0, corp_title_mask=$5, corp_block_roles=$6 WHERE character_id=$1", &[&character, &corporation, &roles, &grantable, &title_mask, &blocked]).await.unwrap();
}
fn notice() -> impl Fn(publication::Audience) -> Result<GatewayNotice, crate::services::CallError> + Sync {
    move |audience| {
        let payload = DynamicMessage::new(message("eve_public.Event").unwrap());
        GatewayNotice::new(Target::Corporation(u32::try_from(audience.corporation).unwrap()), &payload).map_err(producer_error)
    }
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to an initialized dedicated test database with SDE"]
async fn writes_require_live_membership_and_global_role_bit() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = std::sync::Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    let writer = fixture::create(&db).await.unwrap();
    let reader = fixture::create(&db).await.unwrap();
    let outsider = fixture::create(&db).await.unwrap();
    let corp = corporation(&db, writer.character).await;
    assign(&db, reader.character, corp, 0, 0, 0, false).await;
    let identity = Identity { char_id: writer.character, ship_id: None, corp_id: corp, station_id: None, system_id: None };
    let notice: std::sync::Arc<publication::Notice> = std::sync::Arc::new(notice());
    // Direct or unblocked title grants of the global bit pass; everything else refuses.
    db.execute("INSERT INTO game.corp_titles(corporation_id,title_id,name,roles) VALUES ($1,1,'planner',$2)", &[&corp, &ROLE_SKILL_PLAN_MANAGER]).await.unwrap();
    let director = infinity_core::consts::ROLE_DIRECTOR;
    for (roles, grantable, mask, blocked, ok) in [
        (0, 0, 0, false, false), (ROLE_SKILL_PLAN_MANAGER, 0, 0, false, true), (0, 0, 1, false, true),
        (0, 0, 1, true, false), (0, ROLE_SKILL_PLAN_MANAGER, 0, false, false), (director, 0, 0, false, false),
    ] {
        assign(&db, writer.character, corp, roles, grantable, mask, blocked).await;
        let attempt = uuid();
        assert_eq!(super::create(&identity, &db, &sde, &world, &attempt, "plan", "", &[], &[], &[0u8; 16], notice.clone()).await.is_ok(), ok, "roles={roles:x}");
        if ok { plans::delete(&db, Owner::Corporation(corp), &attempt).await.unwrap(); }
    }
    assign(&db, writer.character, corp, ROLE_SKILL_PLAN_MANAGER, 0, 0, false).await;
    // Field text rejection precedes malformed UUID rejection at the service boundary.
    let too_long = "x".repeat(super::super::validation::MAX_NAME + 1);
    assert!(matches!(
        super::super::personal::set_field(&identity, &db, &[], plans::Field::Name, &too_long).await,
        Err(Error::Precondition)
    ));
    // Requirements reject the UUID before checking the sequence shape.
    assert!(matches!(
        super::set_requirements(
            &identity, &db, &sde, &world, &[], &[], &[1], notice.clone(),
        ).await,
        Err(Error::Invalid)
    ));
    let category_id: i64 = db.query_one("SELECT key::bigint AS id FROM sde_native.careerpaths ORDER BY key LIMIT 1", &[]).await.unwrap().get("id");
    let mut category = [0u8; 16];
    category[12..].copy_from_slice(&(category_id as u32).to_be_bytes());
    let plan = uuid();
    assert!(super::create(&identity, &db, &sde, &world, &plan, "plan", "d", &[], &[], &category, notice.clone()).await.is_ok());
    assert!(matches!(super::set_category(&identity, &db, &sde, &world, &plan, &[7u8; 16], notice.clone()).await, Err(Error::Precondition)));
    // Member reads see the row; foreign readers get absence either way.
    let reader_id = Identity { char_id: reader.character, corp_id: corp, ..identity };
    let outsider_id = Identity { char_id: outsider.character, corp_id: 0, ..identity };
    assert!(matches!(super::get(&reader_id, &db, &plan, false).await.unwrap().unwrap().owner, Owner::Corporation(_)));
    assert!(matches!(super::get(&outsider_id, &db, &plan, false).await, Err(Error::Missing)));
    assert!(super::get(&outsider_id, &db, &plan, true).await.unwrap().is_none());
    assert!(super::get(&reader_id, &db, &uuid(), true).await.unwrap().is_none());
    assert_eq!(super::all(&reader_id, &db).await.unwrap().len(), 1);
    assert!(super::all(&outsider_id, &db).await.unwrap().is_empty());
    // A departed member lists and reads nothing while the plan rows remain.
    assign(&db, reader.character, 1, 0, 0, 0, false).await;
    assert!(super::all(&reader_id, &db).await.unwrap().is_empty());
    assert!(matches!(super::get(&reader_id, &db, &plan, false).await, Err(Error::Missing)));
    assert_eq!(plans::all(&db, Owner::Corporation(corp)).await.unwrap().len(), 1);
    assign(&db, reader.character, corp, 0, 0, 0, false).await;

    // Field and requirement writes are guarded and do not touch other fields.
    assert!(super::set_field(&identity, &db, &world, &plan, plans::Field::Name, "renamed", notice.clone()).await.is_ok());
    assert!(super::set_field(&identity, &db, &world, &plan, plans::Field::Description, "detail", notice.clone()).await.is_ok());
    let row = super::get(&reader_id, &db, &plan, false).await.unwrap().unwrap();
    assert_eq!((row.name.as_str(), row.description.as_str()), ("renamed", "detail"));
    assert_eq!(plans::get(&db, &plan, Some(Owner::Corporation(corp))).await.unwrap().unwrap().category.as_deref(), Some(category.as_slice()));
    // A personal plan is never writable through the corporation entry.
    let personal_plan = uuid();
    plans::create(&db, &personal_plan, Owner::Character(writer.character), "own", "", &[], &[], None).await.unwrap();
    assert!(matches!(super::set_field(&identity, &db, &world, &personal_plan, plans::Field::Name, "x", notice.clone()).await, Err(Error::Missing)));
    // Revoking the bit turns later writes away while committed rows stand.
    assign(&db, writer.character, corp, 0, 0, 0, false).await;
    assert!(matches!(super::set_field(&identity, &db, &world, &plan, plans::Field::Name, "later", notice.clone()).await, Err(Error::Precondition)));
    // The quota is enforced under the corporation lock.
    for _ in 0..99 { plans::create(&db, &uuid(), Owner::Corporation(corp), "filler", "", &[], &[], Some(&category)).await.unwrap(); }
    assign(&db, writer.character, corp, ROLE_SKILL_PLAN_MANAGER, 0, 0, false).await;
    assert!(matches!(super::create(&identity, &db, &sde, &world, &uuid(), "over", "", &[], &[], &category, notice.clone()).await, Err(Error::Precondition)));
    // Deleting the owner cascades milestones and progress.
    let milestone = uuid();
    crate::dao::skills::plan_milestones::create(&db, &milestone, &plan, "type", 1, None).await.unwrap();
    assert!(super::delete(&identity, &db, &world, &plan, notice.clone()).await.is_ok());
    assert!(crate::dao::skills::plan_milestones::get(&db, &milestone).await.unwrap().is_none());
    db.execute("DELETE FROM game.skill_plans WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_titles WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=ANY($1)", &[&vec![writer.character, reader.character]]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    plans::delete(&db, Owner::Character(writer.character), &personal_plan).await.unwrap();
    for viewer in [writer, reader, outsider] { fixture::cleanup(&db, viewer).await.unwrap(); }
}
