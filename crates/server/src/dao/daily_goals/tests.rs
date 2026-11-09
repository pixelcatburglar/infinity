use super::{instances::{self, Cursor, GoalRow, ProgressUpdate}, periods::{self, PeriodRow}, receipts::{self, AuditRow, PaymentRow}, rewards::{self, RewardRow}};
use crate::{db::Db, isk::IskAmount, lua::events::combat::BinaryAmount};

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn utc_boundaries_include_leap_month_and_exact_midnight() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let feb = crate::client::filetime_from_unix_ms(1_709_164_800_000);
    let a = periods::bounds(&db, feb).await.unwrap();
    let b = periods::bounds(&db, a.day_end).await.unwrap();
    assert_eq!(a.day_start, feb);
    assert_eq!(a.day_end, b.day_start);
    assert_eq!(a.month_end, b.month_start);
    let c = periods::bounds(&db, a.day_start - 1).await.unwrap();
    assert_eq!(c.day_end, a.day_start);
    assert_eq!(c.month_end, a.month_end);
    let december = crate::client::filetime_from_unix_ms(1_735_689_600_000);
    let year = periods::bounds(&db, december).await.unwrap();
    let previous = periods::bounds(&db, december - 1).await.unwrap();
    assert_eq!(year.month_start, december);
    assert_eq!(previous.month_end, december);
    let epoch = infinity_core::consts::FILETIME_UNIX_EPOCH;
    let before_epoch = periods::bounds(&db, epoch - 1).await.unwrap();
    assert_eq!(before_epoch.day_end, epoch);
    assert_eq!(before_epoch.month_end, epoch);
    assert!(periods::bounds(&db, i64::MIN).await.is_err());
    assert!(periods::bounds(&db, i64::MAX).await.is_err());
    assert!(periods::bounds(&db, i64::MIN + 40 * infinity_core::consts::FILETIME_DAY).await.is_err());
    let ancient = db.query_one("SELECT (extract(epoch FROM TIMESTAMPTZ '4712-01-15 12:00:00+00 BC') * 1000)::bigint AS unix_ms", &[]).await.unwrap();
    let ancient = crate::client::filetime_from_unix_ms(ancient.try_get("unix_ms").unwrap());
    let ancient_bounds = periods::bounds(&db, ancient).await.unwrap();
    assert!(ancient_bounds.day_start <= ancient && ancient < ancient_bounds.day_end);
    assert!(ancient_bounds.month_start <= ancient && ancient < ancient_bounds.month_end);
    assert!(periods::bounds(&db, i64::MAX - 40 * infinity_core::consts::FILETIME_DAY).await.is_ok());
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn frozen_rows_guards_history_and_receipts() {
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = crate::sde::Sde::load(&db).await.expect("daily reward database test requires a loaded SDE");
    let item_type = sde.types.iter().find(|t| t.type_id > 0 && t.published == Some(true)).unwrap().type_id;
    let seq: i64 = db.query_one("SELECT nextval('game.accounts_user_id_seq') AS id", &[]).await.unwrap().get("id");
    let account = crate::dao::accounts::authentication::register(&db, &format!("daily-fixture-{seq}"), b"fixture", 0, 0).await.unwrap().user_id;
    let owner: i64 = db.query_one("INSERT INTO game.characters(user_id,name,race_id,bloodline_id,gender_id,corporation_id) VALUES ($1,$2,1,1,1,1) RETURNING character_id", &[&account,&format!("daily-fixture-{seq}")]).await.unwrap().get("character_id");
    let namespace = format!("daily-fixture-{seq}");
    db.execute("INSERT INTO game.lua_content_versions(namespace,version,schema_version,compiler_identity,registration,source,data,fingerprint) VALUES ($1,'test',1,'test',$2,$2,$2,$3)", &[&namespace,&Vec::<u8>::new(),&vec![0u8;32]]).await.unwrap();
    let p = PeriodRow { character_id:owner, kind:"day".into(), period_start:1, period_end:2, assigned_at:1, generation:0, namespace:namespace.clone(), version:"test".into(), revision:0 };
    assert!(periods::insert(&db,&p).await.unwrap());
    assert!(!periods::insert(&db,&p).await.unwrap());
    let zero = BinaryAmount { coefficient:"0".into(), exponent:0 };
    let mut ids = Vec::new();
    for n in 1..=31 {
        let id = format!("{:08x}-0000-0000-0000-{n:012x}", seq as u32);
        let g = GoalRow { goal_id:id.clone(), character_id:owner, kind:"day".into(), period_start:1, period_end:2, slot_key:n.to_string(), generation:0, namespace:namespace.clone(), version:"test".into(), category:"ordinary".into(), definition:"{}".into(), target:u64::MAX, progress:0, remainder:zero.clone(), completed_at:None, earned_omega:false, paid:false, voided_at:None, has_unclaimed:false, revision:0 };
        assert!(instances::insert(&db,&g).await.unwrap());
        assert!(!instances::insert(&db,&g).await.unwrap());
        if n == 1 {
            let mut collision = g.clone();
            collision.goal_id = format!("{:08x}-0000-0000-0001-000000000001", seq as u32);
            assert!(!instances::insert(&db, &collision).await.unwrap());
            let mut invalid = g.clone();
            invalid.goal_id = collision.goal_id.clone(); invalid.slot_key = "fraction".into();
            for (coefficient, exponent) in [("1".to_string(), 0), ("3".into(), -1), ("02".into(), -2), ("2".into(), -2), ("0".into(), -1), ("9".repeat(651), -1074), ("1".into(), -1075)] {
                invalid.remainder = BinaryAmount { coefficient, exponent };
                assert!(instances::insert(&db, &invalid).await.is_err());
            }
            invalid.remainder = BinaryAmount { coefficient:"3".into(), exponent:-2 };
            assert!(instances::insert(&db, &invalid).await.unwrap());
            assert_eq!(instances::get(&db,owner,&invalid.goal_id).await.unwrap().unwrap().remainder, invalid.remainder);
            db.execute("DELETE FROM game.daily_goal_instances WHERE goal_id=$1::text::uuid", &[&invalid.goal_id]).await.unwrap();
            invalid.remainder = BinaryAmount { coefficient:"1".into(), exponent:-1 };
            invalid.progress = invalid.target; invalid.completed_at = Some(1);
            assert!(instances::insert(&db, &invalid).await.is_err());
            let bad = ProgressUpdate { progress:g.target, remainder:invalid.remainder.clone(), completed_at:Some(1), earned_omega:false, paid:false, has_unclaimed:false };
            assert!(instances::update_progress(&db,owner,&id,0,&bad).await.is_err());
            assert!(db.execute("UPDATE game.daily_goal_instances SET remainder_coefficient='1',remainder_exponent=0 WHERE goal_id=$1::text::uuid", &[&id]).await.is_err());
            assert!(db.execute("UPDATE game.daily_goal_instances SET remainder_coefficient=$2,remainder_exponent=-1074 WHERE goal_id=$1::text::uuid", &[&id,&"9".repeat(651)]).await.is_err());
        }
        let update = ProgressUpdate { progress:u64::MAX, remainder:zero.clone(), completed_at:Some(n), earned_omega:true, paid:false, has_unclaimed:true };
        assert!(instances::update_progress(&db,owner,&id,0,&update).await.unwrap());
        assert!(!instances::update_progress(&db,owner,&id,0,&update).await.unwrap());
        assert_eq!(instances::get(&db,owner,&id).await.unwrap().unwrap().progress,u64::MAX);
        assert!(instances::get(&db,owner+1,&id).await.unwrap().is_none());
        ids.push(id);
    }
    assert_eq!(instances::lock_sorted(&db,owner,&ids[..2]).await.unwrap().len(), 2);
    assert!(instances::lock_sorted(&db,owner,&[ids[1].clone(), ids[0].clone()]).await.is_err());
    assert!(instances::lock_sorted(&db,owner,&[ids[0].clone(), ids[0].clone()]).await.is_err());
    assert!(instances::lock_sorted(&db,owner+1,&ids[..1]).await.is_err());
    assert!(instances::lock_sorted(&db,owner,&[format!("{:08x}-0000-0000-0000-ffffffffffff", seq as u32)]).await.is_err());
    assert!(instances::lock_sorted(&db,owner,&[ids[0].replace('-', "")]).await.is_err());
    assert!(instances::lock_sorted(&db,owner,&[]).await.unwrap().is_empty());
    assert!(db.execute("UPDATE game.daily_goal_instances SET target=18446744073709551616 WHERE goal_id=$1::text::uuid", &[&ids[0]]).await.is_err());
    assert!(db.execute("UPDATE game.daily_goal_instances SET progress=0 WHERE goal_id=$1::text::uuid", &[&ids[0]]).await.is_err());
    assert!(db.execute("UPDATE game.daily_goal_instances SET remainder_coefficient='02' WHERE goal_id=$1::text::uuid", &[&ids[0]]).await.is_err());
    assert!(instances::history(&db,owner,None,None,0,25).await.is_err());
    assert!(instances::history(&db,owner,None,None,-1,25).await.is_err());
    assert!(instances::history(&db,owner,None,None,26,25).await.is_err());
    assert!(instances::history(&db,owner,None,None,1,0).await.is_err());
    let first = instances::history(&db,owner,None,None,25,25).await.unwrap();
    assert_eq!(first.len(),25);
    let high = Cursor { completed_at:first[0].completed_at, goal_id:first[0].goal_id.clone() };
    let last = Cursor { completed_at:first[24].completed_at, goal_id:first[24].goal_id.clone() };
    assert!(instances::set_unclaimed(&db,owner,&first[5].goal_id,1,false).await.unwrap());
    let rest = instances::history(&db,owner,Some(&high),Some(&last),25,25).await.unwrap();
    assert_eq!(rest.len(),6);
    let wide = IskAmount::parse_decimal("9999999999999999999999999999999999.99").unwrap();
    assert!(wide.split_precision().is_none());
    let mut reward = RewardRow { goal_id:ids[0].clone(), reward_index:0, asset_id:format!("{:08x}-0000-0001-0000-000000000001", seq as u32), unit:"isk".into(), amount_isk:Some(wide), amount_units:None, issuer_id:None, type_id:None, omega_required:false, earned:false, claimed_at:None, location_id:None, item_id:None, plex_reference:None, revision:0 };
    assert!(rewards::insert(&db,owner,&reward).await.is_err());
    assert!(IskAmount::parse_decimal("10000000000000000000000000000000000.00").is_err());
    for amount in ["0.00", "-0.01", "9223372036854775808.00"] {
        reward.amount_isk = Some(IskAmount::parse_decimal(amount).unwrap());
        assert!(rewards::insert(&db,owner,&reward).await.is_err(), "{amount}");
    }
    reward.amount_isk = Some(IskAmount::parse_decimal("9223372036854775807.99").unwrap());
    assert!(instances::history(&db,owner+1,None,None,25,25).await.unwrap().is_empty());
    assert!(!rewards::insert(&db,owner+1,&reward).await.unwrap());
    assert!(rewards::insert(&db,owner,&reward).await.unwrap());
    assert!(rewards::for_goals(&db,owner+1,&ids).await.unwrap().is_empty());
    assert_eq!(rewards::for_goals(&db,owner,&ids).await.unwrap(), vec![reward.clone()]);
    let mut wrong = reward.clone(); wrong.reward_index = 1; wrong.asset_id = format!("{:08x}-0000-0001-0000-000000000002", seq as u32); wrong.unit = "lp".into();
    assert!(rewards::insert(&db,owner,&wrong).await.is_err());
    let mut duplicate_asset = reward.clone(); duplicate_asset.reward_index = 1;
    assert!(!rewards::insert(&db,owner,&duplicate_asset).await.unwrap());
    assert!(rewards::list(&db,owner+1,&ids[0],true).await.unwrap().is_empty());
    assert!(!rewards::earn(&db,owner+1,&ids[0],0,0).await.unwrap());
    assert!(rewards::earn(&db,owner,&ids[0],0,0).await.unwrap());
    assert!(!rewards::claim(&db,owner+1,&ids[0],0,1,2,None,None,None).await.unwrap());
    assert!(rewards::claim(&db,owner,&ids[0],0,1,2,None,None,None).await.unwrap());
    assert!(!rewards::claim(&db,owner,&ids[0],0,1,2,None,None,None).await.unwrap());
    assert_eq!(rewards::list(&db,owner,&ids[0],false).await.unwrap()[0].amount_isk,reward.amount_isk);
    for amount in ["9223372036854775808.00", "9999999999999999999999999999999999.99", "0.00", "-0.01"] {
        assert!(db.execute("UPDATE game.daily_goal_rewards SET amount_isk=$3::text::numeric WHERE goal_id=$1::text::uuid AND reward_index=$2", &[&reward.goal_id,&reward.reward_index,&amount]).await.is_err(), "{amount}");
    }
    let mut fractional = reward.clone();
    fractional.goal_id = ids[2].clone();
    fractional.asset_id = format!("{:08x}-0000-0001-0001-000000000002", seq as u32);
    fractional.amount_isk = Some(IskAmount::parse_decimal("1.01").unwrap());
    assert!(rewards::insert(&db,owner,&fractional).await.unwrap());
    assert_eq!(rewards::list(&db,owner,&ids[2],false).await.unwrap()[0].amount_isk, fractional.amount_isk);
    assert!(db.execute("UPDATE game.daily_goal_rewards SET amount_isk=0.99 WHERE goal_id=$1::text::uuid AND reward_index=$2", &[&fractional.goal_id,&fractional.reward_index]).await.is_ok());
    assert_eq!(rewards::list(&db,owner,&ids[2],false).await.unwrap()[0].amount_isk, Some(IskAmount::parse_decimal("0.99").unwrap()));
    let pay = PaymentRow { goal_id:ids[0].clone(), character_id:owner, day_start:1, ordinal:1, issuer_id:1, amount:i64::MAX, paid_at:2 };
    for (index, (unit, max)) in [("lp", i64::MAX as u64), ("sp", u32::MAX as u64), ("item", i32::MAX as u64), ("plex", i64::MAX as u64 / infinity_core::consts::PLEX_HUNDREDTHS as u64)].into_iter().enumerate() {
        let mut r = reward.clone();
        r.reward_index = index as i32 + 2;
        r.asset_id = format!("{:08x}-0000-0001-0000-{:012x}", seq as u32, index + 3);
        r.unit = unit.into(); r.amount_isk = None; r.amount_units = Some(max);
        r.issuer_id = (unit == "lp").then_some(1); r.type_id = (unit == "item").then_some(item_type);
        assert!(rewards::insert(&db,owner,&r).await.unwrap());
        assert!(db.execute("UPDATE game.daily_goal_rewards SET amount_units=$3::text::numeric WHERE goal_id=$1::text::uuid AND reward_index=$2", &[&r.goal_id,&r.reward_index,&(max + 1).to_string()]).await.is_err());
        assert!(db.execute("UPDATE game.daily_goal_rewards SET amount_units=1.5 WHERE goal_id=$1::text::uuid AND reward_index=$2", &[&r.goal_id,&r.reward_index]).await.is_err());
        r.amount_units = Some(max + 1);
        assert!(rewards::insert(&db,owner,&r).await.is_err());
    }
    let plex_index = 5;
    assert!(rewards::earn(&db, owner, &ids[0], plex_index, 0).await.unwrap());
    let reference = crate::dao::accounts::plex::reference(&db).await.unwrap();
    assert!(rewards::claim(&db, owner, &ids[0], plex_index, 1, 2, None, None, Some(reference)).await.unwrap());
    let claimed = rewards::list(&db, owner, &ids[0], false).await.unwrap();
    assert_eq!(claimed.iter().find(|r| r.reward_index == plex_index).unwrap().plex_reference, Some(reference));
    let mut other_plex = reward.clone();
    other_plex.goal_id = ids[1].clone();
    other_plex.asset_id = format!("{:08x}-0000-0001-0001-000000000003", seq as u32);
    other_plex.unit = "plex".into(); other_plex.amount_isk = None; other_plex.amount_units = Some(1);
    assert!(rewards::insert(&db, owner, &other_plex).await.unwrap());
    assert!(rewards::earn(&db, owner, &ids[1], 0, 0).await.unwrap());
    assert!(rewards::claim(&db, owner, &ids[1], 0, 1, 2, None, None, Some(reference)).await.is_err());
    let mut invalid_pay = pay.clone(); invalid_pay.amount = 0;
    assert!(receipts::insert_payment(&db,&invalid_pay).await.is_err());
    invalid_pay.amount = -1;
    assert!(receipts::insert_payment(&db,&invalid_pay).await.is_err());
    assert!(db.execute("INSERT INTO game.daily_goal_payments(goal_id,character_id,day_start,ordinal,issuer_id,amount,paid_at) VALUES ($1::text::uuid,$2,2,1,1,$3::text::bigint,2)", &[&ids[1],&owner,&"1.5"]).await.is_err());
    assert!(receipts::insert_payment(&db,&pay).await.unwrap());
    let mut duplicate_ordinal = pay.clone(); duplicate_ordinal.goal_id = ids[1].clone();
    assert!(!receipts::insert_payment(&db,&duplicate_ordinal).await.unwrap());
    assert!(!receipts::insert_payment(&db,&pay).await.unwrap());
    assert_eq!(receipts::payment(&db,owner,&ids[0]).await.unwrap(),Some(pay.clone()));
    assert_eq!(receipts::payment_count(&db,owner,1).await.unwrap(),1);
    let audit = AuditRow { audit_id:format!("{:08x}-0000-0002-0000-000000000001", seq as u32), actor_id:owner, subject_id:owner, goal_id:ids[0].clone(), operation:"test".into(), payload:"{}".into(), before_state:"{}".into(), after_state:"{}".into(), recorded_at:2 };
    assert!(receipts::insert_audit(&db,&audit).await.unwrap());
    let mut conflict = audit.clone(); conflict.operation = "other".into();
    assert!(!receipts::insert_audit(&db,&conflict).await.unwrap());
    assert!(!receipts::insert_audit(&db,&audit).await.unwrap());
    assert_ne!(receipts::audit(&db,&audit.audit_id).await.unwrap().unwrap().operation, conflict.operation);
    let config = crate::config::tests::test_database();
    let (mut client, connection) = config.to_pg_config().unwrap().connect(tokio_postgres::NoTls).await.unwrap();
    tokio::spawn(async move { connection.await.unwrap(); });
    let tx = client.transaction().await.unwrap();
    let mut rollback_reward = reward.clone();
    rollback_reward.goal_id = ids[3].clone();
    rollback_reward.asset_id = format!("{:08x}-0000-0001-0001-000000000001", seq as u32);
    assert!(rewards::insert(&tx,owner,&rollback_reward).await.unwrap());
    assert!(rewards::earn(&tx,owner,&ids[3],0,0).await.unwrap());
    let mut rollback_pay = pay.clone(); rollback_pay.goal_id = ids[3].clone(); rollback_pay.ordinal = 2; rollback_pay.amount = 1;
    assert!(receipts::insert_payment(&tx,&rollback_pay).await.unwrap());
    let mut rollback_audit = audit.clone(); rollback_audit.goal_id = ids[3].clone();
    rollback_audit.audit_id = format!("{:08x}-0000-0002-0000-000000000002", seq as u32);
    assert!(receipts::insert_audit(&tx,&rollback_audit).await.unwrap());
    assert!(instances::set_unclaimed(&tx,owner,&ids[3],1,false).await.unwrap());
    tx.rollback().await.unwrap();
    assert!(rewards::list(&db,owner,&ids[3],false).await.unwrap().is_empty());
    assert!(receipts::payment(&db,owner,&ids[3]).await.unwrap().is_none());
    assert!(receipts::audit(&db,&rollback_audit.audit_id).await.unwrap().is_none());
    assert!(instances::get(&db,owner,&ids[3]).await.unwrap().unwrap().has_unclaimed);
    assert_eq!(receipts::audit(&db,&audit.audit_id).await.unwrap(),Some(audit));
    db.execute("DELETE FROM game.daily_goal_admin_audit WHERE subject_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.daily_goal_payments WHERE character_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.daily_goal_rewards WHERE goal_id IN (SELECT goal_id FROM game.daily_goal_instances WHERE character_id=$1)", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.daily_goal_instances WHERE character_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.daily_goal_periods WHERE character_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.lua_content_versions WHERE namespace=$1", &[&namespace]).await.unwrap();
    db.execute("DELETE FROM game.characters WHERE character_id=$1", &[&owner]).await.unwrap();
    db.execute("DELETE FROM game.accounts WHERE user_id=$1", &[&account]).await.unwrap();
}
