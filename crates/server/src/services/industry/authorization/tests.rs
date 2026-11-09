use super::*;

fn location(item_id: i64, flag_id: i32, can_take: bool) -> Location {
    Location { item_id, type_id: 50, owner_id: 60, flag_id, can_view: true, can_take }
}

#[test]
fn location_wire_shapes_resolve_only_canonical_custody() {
    let allowed = location(101, FLAG_CORP_SAG1, true);
    let view_only = location(101, FLAG_CORP_SAG1 + 1, false);
    let locations = [allowed.clone(), view_only.clone()];
    let wire = allowed.value();
    assert!(matches!(wire, Value::Reduce { .. }));
    let selected = select_location(Some(&wire), &locations, true).ok().unwrap();
    assert_eq!((selected.item_id, selected.type_id, selected.owner_id, selected.flag_id), (101, 50, 60, FLAG_CORP_SAG1));
    let forged = Value::instance("industry.job.Location", [
        ("itemID", Value::Int(101)), ("flagID", Value::Int(i64::from(view_only.flag_id))),
        ("typeID", Value::Int(999)), ("ownerID", Value::Int(999)),
        ("canView", Value::Bool(true)), ("canTake", Value::Bool(true)),
    ]);
    assert!(select_location(Some(&forged), &locations, true).is_err());
    let selected = select_location(Some(&forged), &locations, false).ok().unwrap();
    assert_eq!((selected.type_id, selected.owner_id, selected.can_take), (50, 60, false));
    for (item, flag) in [(102, i64::from(FLAG_CORP_SAG1)), (101, i64::from(FLAG_CORP_SAG1 + 2)), (101, i64::from(i32::MAX) + 1)] {
        let request = Value::instance("industry.job.Location", [("itemID", Value::Int(item)), ("flagID", Value::Int(flag))]);
        assert!(select_location(Some(&request), &locations, false).is_err());
    }
    assert!(select_location(None, &locations, false).is_err());
    for malformed in [Value::None, Value::Int(1), Value::dict([("itemID", Value::Int(101))]), Value::instance("industry.job.Location", [("itemID", Value::Int(101)), ("flagID", Value::Bool(true))])] {
        assert!(select_location(Some(&malformed), &locations, false).is_err());
    }
    let personal = location(103, FLAG_HANGAR, true);
    assert_eq!(select_location(None, &[personal], true).ok().unwrap().item_id, 103);
}

#[tokio::test]
#[ignore = "requires INFINITY_TEST_DATABASE_URL pointing to a dedicated database with SDE"]
async fn current_membership_roles_divisions_and_payer() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let baseline = super::super::tests::database_fingerprint(&db).await;
    let sde = std::sync::Arc::new(Sde::load(&db).await.unwrap());
    let station = sde.map.stations().next().expect("SDE station").station_id;
    let office_type = sde.types.iter().next().expect("SDE type").type_id;
    let account: i64 = db.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('industry-auth-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await.unwrap().get("user_id");
    let actor: i64 = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'industry-auth-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let ceo: i64 = db.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'industry-auth-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await.unwrap().get("character_id");
    let corp: i64 = db.query_one("INSERT INTO game.corporations (name, ticker, ceo_id, creator_id, station_id, created) VALUES ('industry-auth-' || nextval('game.corporations_corporation_id_seq')::text, 'IA' || nextval('game.corporations_corporation_id_seq')::text, $1, $1, $2, 0) RETURNING corporation_id", &[&ceo, &station]).await.unwrap().get("corporation_id");
    db.execute("UPDATE game.characters SET corporation_id = $1 WHERE character_id = ANY($2)", &[&corp, &vec![actor, ceo]]).await.unwrap();
    db.execute("UPDATE game.characters SET corp_roles=$2 WHERE character_id=$1", &[&ceo, &ROLE_DIRECTOR]).await.unwrap();
    let office = crate::dao::items::transfers::insert(&db, office_type, corp, station, FLAG_HANGAR, 1).await.unwrap();
    crate::dao::offices::insert(&db, office, corp, station, 1).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let db = test_db;
        let facility = Facility { id: station, type_id: i64::from(office_type), owner: 1, system: 1, security: 0.0, factory: true, laboratory: true };
        let required = ROLE_FACTORY_MANAGER | ROLE_FACTORY_MANUFACTURING;
        for (roles, allowed) in [(0, false), (ROLE_FACTORY_MANAGER, false), (ROLE_FACTORY_MANUFACTURING, false), (required, true)] {
            db.execute("UPDATE game.characters SET corp_roles = $2 WHERE character_id = $1", &[&actor, &roles]).await.unwrap();
            assert_eq!(owner(&db, actor, corp, Some(MANUFACTURING)).await.is_ok(), allowed);
        }
        for (roles, allowed) in [(ROLE_HANGAR_CAN_TAKE_1, false), (ROLE_FACTORY_MANAGER, false), (ROLE_FACTORY_MANAGER | ROLE_HANGAR_CAN_TAKE_1, true)] {
            db.execute("UPDATE game.characters SET corp_roles=$2::bigint & ~$3::bigint,corp_roles_hq=$2::bigint & $3::bigint WHERE character_id=$1", &[&actor, &roles, &ROLE_HANGAR_CAN_TAKE_1]).await.unwrap();
            match locations(&db, &sde, actor, corp, &facility).await {
                Ok(found) => {
                    assert_eq!(found.len(), 7);
                    assert_eq!(found[0].item_id, office);
                    assert_eq!(found[0].can_take, allowed);
                    assert!(!found[1].can_take);
                }
                Err(_) => assert_eq!(roles, ROLE_HANGAR_CAN_TAKE_1),
            }
        }
        db.execute("UPDATE game.characters SET corp_roles = $2, corp_roles_hq = 0 WHERE character_id = $1", &[&actor, &ROLE_DIRECTOR]).await.unwrap();
        assert!(owner(&db, actor, corp, Some(COPYING)).await.is_ok());
        assert!(locations(&db, &sde, actor, corp, &facility).await.ok().unwrap().iter().all(|l| l.can_take));
        assert!(owner(&db, ceo, corp, Some(MANUFACTURING)).await.is_ok());
        assert!(locations(&db, &sde, ceo, corp, &facility).await.ok().unwrap().iter().all(|l| l.can_take));
        db.execute("INSERT INTO game.corp_titles (corporation_id, title_id, roles) VALUES ($1, 1, $2)", &[&corp, &required]).await.unwrap();
        db.execute("UPDATE game.characters SET corp_roles = 0, corp_title_mask = 1 WHERE character_id = $1", &[&actor]).await.unwrap();
        assert!(owner(&db, actor, corp, Some(MANUFACTURING)).await.is_ok());
        let account_value = |owner, key| Value::tuple([Value::Int(owner), Value::Int(key)]);
        assert_eq!(payer(&db, actor, None).await.ok().unwrap(), (actor, ACCOUNT_CASH));
        assert_eq!(payer(&db, actor, Some(&account_value(actor, i64::from(ACCOUNT_CASH)))).await.ok().unwrap(), (actor, ACCOUNT_CASH));
        assert!(payer(&db, actor, Some(&account_value(actor, i64::from(ACCOUNT_CASH + 1)))).await.is_err());
        assert!(payer(&db, actor, Some(&account_value(corp, i64::from(ACCOUNT_CASH + 1)))).await.is_err());
        let wallet_role = 134_217_728_i64 << 1;
        db.execute("UPDATE game.characters SET corp_roles = $2 WHERE character_id = $1", &[&actor, &wallet_role]).await.unwrap();
        assert_eq!(payer(&db, actor, Some(&account_value(corp, i64::from(ACCOUNT_CASH + 1)))).await.ok().unwrap(), (corp, ACCOUNT_CASH + 1));
        for bad in [account_value(corp + 1, i64::from(ACCOUNT_CASH + 1)), account_value(corp, i64::from(ACCOUNT_CASH + 7)), account_value(corp, i64::from(i32::MAX) + 1), Value::tuple([Value::Int(corp)]), Value::None] {
            assert!(payer(&db, actor, Some(&bad)).await.is_err());
        }
        db.execute("UPDATE game.characters SET corporation_id = 1 WHERE character_id = $1", &[&actor]).await.unwrap();
        assert!(owner(&db, actor, corp, None).await.is_err());
        assert!(payer(&db, actor, Some(&account_value(corp, i64::from(ACCOUNT_CASH + 1)))).await.is_err());
    }).await;
    db.execute("DELETE FROM game.offices WHERE office_id = $1", &[&office]).await.unwrap();
    db.execute("DELETE FROM game.items WHERE item_id = $1", &[&office]).await.unwrap();
    db.execute("DELETE FROM game.corp_titles WHERE corporation_id = $1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id = $1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE user_id = $1", &[&account]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&account]).await.unwrap();
    assert_eq!(super::super::tests::database_fingerprint(&db).await, baseline);
    result.unwrap();
}
