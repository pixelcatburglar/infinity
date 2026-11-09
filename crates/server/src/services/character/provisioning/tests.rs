use super::*;

#[test]
fn unsupported_ship_has_no_rookie_fit() {
    let sde = Sde::default();
    assert!(sde.rookie_fits.get(-1).is_none());
}
