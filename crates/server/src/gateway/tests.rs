use super::*;

pub(super) async fn gateway() -> Arc<Gateway> {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let sde = Arc::new(crate::sde::Sde::load(&db).await.unwrap());
    let world = World::new(db.clone(), sde.clone(), 1);
    Gateway::new(db, world, Arc::new(Config::default()))
}
