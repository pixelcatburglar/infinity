use super::*;
pub(super) async fn science_readiness(db: &Db, client: &mut Client, f: Fixture, personal: &authorization::Location, sde: &crate::sde::Sde, prices: &HashMap<i32, f64>) {
    for activity in [RESEARCH_TIME, RESEARCH_MATERIAL, INVENTION] {
        let kind = if activity == INVENTION {
            sde.types.iter().find_map(|t| {
                let act = sde.industry.activity(t.type_id, INVENTION)?;
                let (product, runs, probability) = *act.products.first()?;
                if runs <= 0 || runs > i64::from(i32::MAX) || !probability.is_finite() || sde.types.get(product).is_none() { return None; }
                let cost = sde.industry.activity(act.products.first()?.0, MANUFACTURING)
                    .map(|manufacturing| manufacturing.materials.iter().map(|(kind, quantity)| prices.get(kind).copied().unwrap_or(0.0) * *quantity as f64).sum::<f64>())
                    .unwrap_or(0.0);
                (act.time > 0.0 && act.time < 1_000_000.0 && cost.is_finite() && cost < 100_000_000.0
                    && act.materials.iter().all(|(_, quantity)| *quantity > 0 && *quantity < 1_000_000))
                    .then_some(t.type_id)
            }).expect("invention blueprint in SDE")
        } else { f.bp };
        let act = sde.industry.activity(kind, activity).expect("science activity in SDE");
        for &(skill, level) in &act.skills {
            db.execute("INSERT INTO game.skills (character_id,type_id,level,skill_points) VALUES ($1,$2,$3,0) ON CONFLICT (character_id,type_id) DO UPDATE SET level=GREATEST(game.skills.level,EXCLUDED.level)", &[&f.actor, &skill, &level]).await.unwrap();
        }
        for &(material, quantity) in &act.materials {
            item(db, material, f.actor, f.station, FLAG_HANGAR, quantity.max(1) + 10, false).await;
        }
        let copy = activity == INVENTION;
        let blueprint = item(db, kind, f.actor, f.station, FLAG_HANGAR, if copy { BLUEPRINT_COPY_QUANTITY } else { 1 }, true).await;
        if copy { industry_dao::set_blueprint(db, blueprint, 0, 0, 1).await.unwrap(); }
        let product = if copy { act.products[0].0 } else { kind };
        let id = mechanics::install(client, &request(f, blueprint, activity, personal, personal, f.actor, ACCOUNT_CASH)).await.ok().unwrap().as_int().unwrap();
        let outputs_before: i64 = db.query_one("SELECT count(*) AS n FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4 AND item_id<>$5", &[&f.actor, &f.station, &FLAG_HANGAR, &product, &blueprint]).await.unwrap().get("n");
        let deadline = format!("industry.ready:{id}");
        let due: i64 = db.query_one("SELECT due_at FROM game.jobs WHERE job_id=$1", &[&deadline]).await.unwrap().get("due_at");
        assert_eq!(due, industry_dao::job(db, id).await.unwrap().unwrap().end);
        db.execute("UPDATE game.industry_jobs SET end_date=0 WHERE job_id=$1", &[&id]).await.unwrap();
        db.execute("UPDATE game.jobs SET due_at=0 WHERE job_id=$1", &[&deadline]).await.unwrap();
        let event = crate::scheduler::Job { id: deadline.clone(), kind: completion::KIND.into(), due_at: 0, payload: id.to_string() };
        completion::handle(db.clone(), client.world.clone(), event.clone()).await.unwrap();
        assert_eq!(industry_dao::job(db, id).await.unwrap().unwrap().status, INDUSTRY_JOB_STATUS_READY);
        assert!(industry_dao::has_active_blueprint_job(db, &[blueprint]).await.unwrap());
        let outputs_ready: i64 = db.query_one("SELECT count(*) AS n FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4 AND item_id<>$5", &[&f.actor, &f.station, &FLAG_HANGAR, &product, &blueprint]).await.unwrap().get("n");
        assert_eq!(outputs_ready, outputs_before);
        let before = fingerprint(db, f).await;
        completion::handle(db.clone(), client.world.clone(), event).await.unwrap();
        assert_eq!(fingerprint(db, f).await, before);
        mechanics::complete(client, id).await.ok().unwrap();
        let delivered = industry_dao::job(db, id).await.unwrap().unwrap();
        assert_eq!(delivered.status, STATUS_DELIVERED);
        assert!(db.query_opt("SELECT job_id FROM game.jobs WHERE job_id=$1", &[&deadline]).await.unwrap().is_none());
        if copy {
            assert!(matches!(delivered.successful_runs, Some(0 | 1)));
            assert!(industry_dao::blueprint(db, blueprint).await.unwrap().is_none());
            let outputs_after: i64 = db.query_one("SELECT count(*) AS n FROM game.items WHERE owner_id=$1 AND location_id=$2 AND flag_id=$3 AND type_id=$4 AND item_id<>$5", &[&f.actor, &f.station, &FLAG_HANGAR, &product, &blueprint]).await.unwrap().get("n");
            assert_eq!(outputs_after - outputs_before, i64::from(delivered.successful_runs.unwrap()));
        } else {
            let bp = industry_dao::blueprint(db, blueprint).await.unwrap().unwrap();
            assert_eq!((bp.me, bp.te), if activity == RESEARCH_TIME { (0, 2) } else { (1, 0) });
        }
    }
}
