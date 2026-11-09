use super::*;
mod entity;
use crate::dao::sovereignty::systems::SystemRow;

fn tombstone(revision: i64) -> HubPublication {
    HubPublication { system: SystemRow { system_id: 7, hub_item_id: None,
        claim_corporation_id: None, alliance_id: None, claim_time: None,
        ownership_epoch: 2, revision }, hub: None, capture: None }
}

#[test]
fn tombstone_high_water_rejects_repeated_and_older_publications() {
    let mut system = System::new(7, Arc::default(), 1_000);
    apply(&mut system, tombstone(4), true, &[]).unwrap();
    assert_eq!(system.sovereignty.version, Some((2, 4)));
    let mut old = tombstone(3);
    old.system.hub_item_id = Some(42);
    apply(&mut system, old, false, &[]).unwrap();
    let mut repeated = tombstone(4);
    repeated.system.hub_item_id = Some(42);
    apply(&mut system, repeated, false, &[]).unwrap();
    assert_eq!(system.sovereignty.hub, None);
    assert_eq!(system.sovereignty.version, Some((2, 4)));
    assert!(system.ball(42).is_none());
}
