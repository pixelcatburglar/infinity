//! Database regressions for market book row decoding.
use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn book_price_decode_preserves_values_and_error_kinds() {
    let db = db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    async fn decode(db: &db::Db, price: &str) -> Result<BookOrder, db::Error> {
        BookOrder::from_row(&db.query_one(&format!("SELECT {price} AS price, 10::bigint AS vol_remaining, -1::bigint AS range, 7::bigint AS order_id, 1::bigint AS min_volume"), &[]).await.unwrap())
    }
    let book = decode(&db, "'123.45'::text").await.unwrap();
    assert_eq!(book.price.decimal(), "123.45");
    assert_eq!((book.vol_remaining, book.range, book.order_id, book.min_volume), (10, -1, 7, 1));
    assert_eq!(decode(&db, "'9999999999999999999999999999999999.99'::text").await.unwrap().price.decimal(), "9999999999999999999999999999999999.99");
    for price in ["'abc'::text", "'1.001'::text", "'10000000000000000000000000000000000.00'::text"] {
        let Err(db::Error::Progress(e)) = decode(&db, price).await else { panic!("{price}") };
        assert_eq!(e.to_string(), "invalid or out-of-range ISK amount", "{price}");
        assert!(e.downcast_ref::<crate::isk::AmountError>().is_some(), "{price}");
    }
    let Err(db::Error::Postgres(_)) = decode(&db, "NULL::text").await else { panic!("NULL price") };
}
