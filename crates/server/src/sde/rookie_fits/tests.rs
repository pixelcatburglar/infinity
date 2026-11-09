use super::*;
use infinity_core::consts::{CATEGORY_CHARGE, CATEGORY_DRONE, CATEGORY_MODULE, FLAG_FITTING_FIRST, FLAG_HI_SLOT_FIRST, FLAG_MED_SLOT_FIRST};
use infinity_core::consts::dogma::{ATTR_HI_SLOTS, ATTR_LOW_SLOTS, ATTR_MED_SLOTS};

fn class(id: i32) -> Option<(Option<i32>, Option<i32>)> {
    match id {
        601 | 588 | 596 | 606 => Some((Some(CATEGORY_SHIP), None)),
        3638 | 3636 | 3634 | 3640 | 3651 | 10039 | 21857 | 21853 | 30839 => Some((Some(CATEGORY_MODULE), None)),
        32465 => Some((Some(CATEGORY_DRONE), None)),
        800 => Some((Some(CATEGORY_CHARGE), None)),
        _ => None,
    }
}

fn rack_for(id: i32) -> Option<(i32, i32, i32)> {
    match id {
        3638 | 3636 | 3634 | 3640 | 3651 | 32465 | 601 | 800 => Some((0, ATTR_HI_SLOTS, FLAG_HI_SLOT_FIRST)),
        10039 | 21857 => Some((0, ATTR_MED_SLOTS, FLAG_MED_SLOT_FIRST)),
        21853 | 30839 => Some((0, ATTR_LOW_SLOTS, FLAG_FITTING_FIRST)),
        _ => None,
    }
}

fn slots_for(_ship: i32, _attr: i32) -> f64 { 2.0 }

fn parse(content: &str, required: impl IntoIterator<Item = i32>) -> anyhow::Result<RookieFits> {
    RookieFits::parse(content, class, rack_for, slots_for, required)
}

#[test]
fn embedded_catalogue_preserves_all_four_original_fits() {
    let catalogue = parse(CONTENT, [601, 588, 596, 606]).unwrap();
    let expected = [
        (601, vec![(3638,27,1),(3651,28,1),(10039,19,1),(21857,20,1),(30839,11,1),(32465,87,1)]),
        (588, vec![(3636,27,1),(3651,28,1),(10039,19,1),(21857,20,1),(30839,11,1),(32465,87,1)]),
        (596, vec![(3634,27,1),(3651,28,1),(21857,19,1),(21853,11,1),(30839,12,1),(32465,87,1)]),
        (606, vec![(3640,27,1),(3651,28,1),(21857,19,1),(21853,11,1),(30839,12,1),(32465,87,1)]),
    ];
    for (ship, items) in expected {
        let fit = catalogue.get(ship).unwrap();
        assert_eq!(fit.ship, ship);
        assert_eq!(fit.items, items);
    }
    assert!(catalogue.get(-1).is_none());
}

#[test]
fn rejects_malformed_and_invalid_definitions() {
    let invalid = [
        "not json",
        r#"[{"ship":601,"items":[],"extra":1}]"#,
        r#"[{"ship":601,"ship":601,"items":[[3638,27,1]]}]"#,
        r#"[{"ship":601,"items":[[3638,27,1]]},{"ship":601,"items":[[3638,27,1]]}]"#,
        r#"[{"ship":999,"items":[[3638,27,1]]}]"#,
        r#"[{"ship":3638,"items":[[3638,27,1]]}]"#,
        r#"[{"ship":601,"items":[[999,27,1]]}]"#,
        r#"[{"ship":601,"items":[[3638,27,0]]}]"#,
        r#"[{"ship":601,"items":[[3638,27,-1]]}]"#,
        r#"[{"ship":601,"items":[[3638,27,2]]}]"#,
        r#"[{"ship":601,"items":[[3638,0,1]]}]"#,
        r#"[{"ship":601,"items":[[3638,27,1],[3651,27,1]]}]"#,
        r#"[{"ship":601,"items":[[3638,87,1]]}]"#,
        r#"[{"ship":601,"items":[]}]"#,
        r#"[{"ship":601,"items":[[3638,27]]}]"#,
    ];
    for content in invalid {
        assert!(parse(content, [601]).is_err(), "accepted {content}");
    }
    assert!(parse(r#"[{"ship":601,"items":[[3638,27,1]]}]"#, [601, 588]).is_err());
}

#[test]
fn rejects_non_modules_in_fitting_slots() {
    for id in [32465, 601, 800] {
        let content = format!(r#"[{{"ship":601,"items":[[{id},27,1]]}}]"#);
        assert!(parse(&content, [601]).is_err(), "accepted type {id}");
    }
}

#[test]
fn rejects_module_in_wrong_rack() {
    assert!(parse(r#"[{"ship":601,"items":[[30839,27,1]]}]"#, [601]).is_err());
}

#[test]
fn rejects_slot_beyond_hull_capacity() {
    assert!(parse(r#"[{"ship":601,"items":[[3638,29,1]]}]"#, [601]).is_err());
}
