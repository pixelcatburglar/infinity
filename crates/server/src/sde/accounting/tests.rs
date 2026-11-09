use super::*;
fn valid()->Vec<EntryType> { vec![EntryType{id:REF_MARKET_TRANSACTION,name_id:1},EntryType{id:REF_MARKET_ESCROW,name_id:2},EntryType{id:REF_BROKER_FEE,name_id:3},EntryType{id:REF_TRANSACTION_TAX,name_id:4},EntryType{id:REF_MARKET_SECURITY_TAX,name_id:5}] }
#[test]
fn rejects_missing_duplicate_and_malformed_entries() {
    assert!(Accounting::from_rows(valid()).is_ok());
    let mut with_undefined=valid(); with_undefined.push(EntryType{id:0,name_id:59792}); assert!(Accounting::from_rows(with_undefined).is_ok());
    let mut missing=valid(); missing.pop(); assert!(Accounting::from_rows(missing).is_err());
    let mut duplicate=valid(); duplicate.push(duplicate[0].clone()); assert!(Accounting::from_rows(duplicate).is_err());
    let mut malformed=valid(); malformed[0].name_id=0; assert!(Accounting::from_rows(malformed).is_err());
}
#[test]
fn requires_market_security_tax_in_complete_catalog() {
    let rows = valid();
    assert!(Accounting::from_rows(rows.clone()).is_ok());
    let without_security_tax = rows.into_iter().filter(|entry| entry.id != REF_MARKET_SECURITY_TAX).collect();
    let error = Accounting::from_rows(without_security_tax).err().expect("missing security tax must fail");
    assert_eq!(error.to_string(), format!("missing accounting entry type {REF_MARKET_SECURITY_TAX}"));
}
