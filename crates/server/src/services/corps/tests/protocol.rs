//! Client-facing corporation governance RPC and wire-shape regressions.
use infinity_core::consts::ACCOUNT_CASH;
use crate::{client::Client, dao::corporations::shares as ledger, db::Db, services::{Call, CallError, CallResult, tests::{self, fixture}}};
use marshal::{DbRowDescriptor, Value};

async fn rpc(client: &mut Client, service: &str, method: &str, args: &[Value], corp: Option<i64>) -> CallResult {
    let kwargs = Value::Dict(vec![]);
    let bind = corp.map(Value::Int);
    let call = Call { service, method, args, kwargs: &kwargs, bound: bind.is_some(), bind_params: bind.as_ref() };
    super::super::dispatch(client, &call).await.expect("governance method routed")
}

fn wire(value: Value) -> Value { marshal::loads(&marshal::dumps(&value)).unwrap() }

fn rowset(value: &Value) -> (DbRowDescriptor, &[Value]) {
    let Value::Reduce { data, list, dict } = value else { panic!("expected CRowset") };
    assert!(list.is_empty() && dict.is_empty());
    let parts = data.items().unwrap();
    assert_eq!(parts[0], Value::global("carbon.common.script.sys.crowset.CRowset"));
    let args = parts[1].items().unwrap();
    (DbRowDescriptor::from_value(&args[0]).unwrap(), args[1].items().unwrap())
}

fn indexed<'a>(value: &'a Value, key: &str) -> (DbRowDescriptor, &'a [(Value, Value)]) {
    let Value::Reduce { data, list, dict } = value else { panic!("expected CIndexedRowset") };
    assert!(list.is_empty());
    let parts = data.items().unwrap();
    assert_eq!(parts[0], Value::global("carbon.common.script.sys.crowset.CIndexedRowset"));
    let args = parts[1].items().unwrap();
    assert_eq!(args[1], Value::str(key));
    (DbRowDescriptor::from_value(&args[0]).unwrap(), dict)
}

async fn fingerprint(db: &Db, corp: i64, holder: i64) -> String {
    db.query_one("SELECT (SELECT COALESCE(string_agg(row_to_json(d)::text, '' ORDER BY operation_id), '') FROM game.corp_dividends d WHERE corporation_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(r)::text, '' ORDER BY r.operation_id,r.holder_id), '') FROM game.corp_dividend_recipients r JOIN game.corp_dividends d USING(operation_id) WHERE d.corporation_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(w)::text, '' ORDER BY account_key), '') FROM game.corp_wallets w WHERE corporation_id=$1) || '|' || (SELECT COALESCE(string_agg(character_id::text || ':' || balance::text, ',' ORDER BY character_id), '') FROM game.characters WHERE corporation_id=$1 OR character_id=$2) AS value", &[&corp, &holder]).await.unwrap().get("value")
}

async fn vote_fingerprint(db: &Db, corp: i64) -> String {
    db.query_one("SELECT (SELECT COALESCE(string_agg(row_to_json(c)::text, '' ORDER BY id), '') FROM game.corp_vote_cases c WHERE corporation_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(o)::text, '' ORDER BY o.case_id,o.option_id), '') FROM game.corp_vote_options o JOIN game.corp_vote_cases c ON c.id=o.case_id WHERE c.corporation_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(s)::text, '' ORDER BY s.case_id,s.holder_id), '') FROM game.corp_vote_snapshot s JOIN game.corp_vote_cases c ON c.id=s.case_id WHERE c.corporation_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(b)::text, '' ORDER BY b.case_id,b.character_id), '') FROM game.corp_vote_ballots b JOIN game.corp_vote_cases c ON c.id=b.case_id WHERE c.corporation_id=$1) || '|' || (SELECT COALESCE(string_agg(row_to_json(h)::text, '' ORDER BY h.case_id,h.holder_id), '') FROM game.corp_vote_claims h JOIN game.corp_vote_cases c ON c.id=h.case_id WHERE c.corporation_id=$1) AS value", &[&corp]).await.unwrap().get("value")
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn real_client_governance_dispatch_and_wire_shapes() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let ceo = fixture::create(&db).await.unwrap();
    let holder = fixture::create(&db).await.unwrap();
    let outsider = fixture::create(&db).await.unwrap();
    let corp: i64 = db.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('governance-wire-' || nextval('game.corporations_corporation_id_seq')::text,'GW' || currval('game.corporations_corporation_id_seq')::text,$1,$1,1,1) RETURNING corporation_id", &[&ceo.character]).await.unwrap().get("corporation_id");
    db.execute("UPDATE game.characters SET corporation_id=$1,corp_roles=$3 WHERE character_id=$2", &[&corp, &ceo.character, &infinity_core::consts::ROLE_DIRECTOR]).await.unwrap();
    assert!(ledger::initialize(&db, corp, 100).await.unwrap());
    db.execute("INSERT INTO game.corp_wallets (corporation_id,account_key,balance) VALUES ($1,$2,100),($1,$3,500)", &[&corp, &ACCOUNT_CASH, &(ACCOUNT_CASH + 1)]).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _peer) = tests::client(test_db.clone(), ceo.character).await;
        client.session.apply(vec![("corpid", Value::Int(corp)), ("corpAccountKey", Value::Int(i64::from(ACCOUNT_CASH + 1)))]);
        let args = [Value::Int(corp), Value::Int(ceo.character), Value::Int(40)];
        let treasury = wire(rpc(&mut client, "corpRegistry", "GetSharesByShareholder", &[Value::Int(1)], Some(corp)).await.unwrap());
        let (desc, rows) = rowset(&treasury);
        let row = rows.iter().find(|row| desc.get(row, "corporationID") == Some(Value::Int(corp))).unwrap();
        assert_eq!(desc.get(row, "shareholderID"), Some(Value::Int(corp)));
        assert_eq!(desc.get(row, "shareholderCorporationID"), Some(Value::None));
        assert_eq!(desc.get(row, "shares"), Some(Value::Int(100)));
        rpc(&mut client, "corpRegistry", "MoveCompanyShares", &args, Some(corp)).await.unwrap();
        let args = [Value::Int(corp), Value::Int(holder.character), Value::Int(60)];
        rpc(&mut client, "corpRegistry", "MoveCompanyShares", &args, Some(corp)).await.unwrap();
        assert_eq!(ledger::balance(&test_db, corp, holder.character).await.unwrap(), 60);
        for (flag, expected_holder, expected_shares) in [(0, ceo.character, 40), (1, corp, 0)] {
            let value = wire(rpc(&mut client, "corpRegistry", "GetSharesByShareholder", &[Value::Int(flag)], Some(corp)).await.unwrap());
            let (desc, rows) = rowset(&value);
            for column in ["corporationID", "shareholderID", "shareholderCorporationID", "shares"] { assert!(desc.columns.iter().any(|(name, _)| name == column)); }
            if flag == 0 {
                let row = rows.iter().find(|row| desc.get(row, "corporationID") == Some(Value::Int(corp))).unwrap();
                assert_eq!(desc.get(row, "shareholderID"), Some(Value::Int(expected_holder)));
                assert_eq!(desc.get(row, "shares"), Some(Value::Int(expected_shares)));
                assert_eq!(desc.get(row, "shareholderCorporationID"), Some(Value::Int(corp)));
            } else { assert!(rows.iter().all(|row| desc.get(row, "corporationID") != Some(Value::Int(corp)))); }
        }
        let value = wire(rpc(&mut client, "corpRegistry", "GetShareholders", &[Value::Int(corp)], Some(corp)).await.unwrap());
        let (desc, rows) = indexed(&value, "shareholderID");
        let row = rows.iter().find(|(key, _)| *key == Value::Int(holder.character)).unwrap();
        assert_eq!(desc.get(&row.1, "shares"), Some(Value::Int(60)));
        assert_eq!(desc.get(&row.1, "corporationID"), Some(Value::Int(corp)));
        assert_eq!(desc.get(&row.1, "shareholderID"), Some(Value::Int(holder.character)));
        assert!(desc.get(&row.1, "shareholderCorporationID").is_some());

        let before = fingerprint(&test_db, corp, holder.character).await;
        for args in [vec![Value::Int(0), Value::Float(10.0)], vec![Value::Int(2), Value::Float(10.0)], vec![Value::Int(1)], vec![Value::Int(1), Value::Float(10.0), Value::Int(1)], vec![Value::Int(1), Value::Int(1)], vec![Value::Int(1), Value::Int(9007199254740993)]] {
            assert!(matches!(rpc(&mut client, "corpRegistry", "PayoutDividend", &args, Some(corp)).await, Err(CallError::User { .. })));
            assert_eq!(fingerprint(&test_db, corp, holder.character).await, before);
        }
        let holder_before = crate::dao::wallets::characters::balance(&test_db, holder.character).await.unwrap();
        rpc(&mut client, "corpRegistry", "PayoutDividend", &[Value::Int(1), Value::Float(100.0)], Some(corp)).await.unwrap();
        let row = test_db.query_one("SELECT (SELECT balance::text FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$3) AS master, (SELECT balance::text FROM game.corp_wallets WHERE corporation_id=$1 AND account_key=$4) AS division, (SELECT balance::text FROM game.characters WHERE character_id=$2) AS holder, (SELECT count(*) FROM game.corp_dividends WHERE corporation_id=$1 AND division=$3) AS receipts", &[&corp, &holder.character, &ACCOUNT_CASH, &(ACCOUNT_CASH + 1)]).await.unwrap();
        assert_eq!(crate::isk::IskAmount::parse_decimal(&row.get::<_, String>("master")).unwrap(), crate::isk::IskAmount::ZERO);
        assert_eq!(crate::isk::IskAmount::parse_decimal(&row.get::<_, String>("division")).unwrap(), crate::isk::IskAmount::from_cents(50_000).unwrap());
        assert_eq!(crate::isk::IskAmount::parse_decimal(&row.get::<_, String>("holder")).unwrap(), crate::isk::IskAmount::from_cents(crate::services::tests::fixture::isk_cents(holder_before) + 6_000).unwrap());
        assert_eq!(row.get::<_, i64>("receipts"), 1);
        let before = fingerprint(&test_db, corp, holder.character).await;
        assert!(matches!(rpc(&mut client, "corpRegistry", "PayoutDividend", &[Value::Int(1), Value::Float(10.0)], Some(corp)).await, Err(CallError::User { .. })));
        assert_eq!(fingerprint(&test_db, corp, holder.character).await, before);

        let vote_args = |kind| vec![Value::unicode("Proposal"), Value::unicode("Description"), Value::Int(kind), Value::list([Value::unicode("Yes"), Value::unicode("No")]), Value::Int(1)];
        let before = vote_fingerprint(&test_db, corp).await;
        for kind in [0, 1, 2, 3, 5, 6] {
            assert!(matches!(rpc(&mut client, "voteManager", "InsertVoteCase", &vote_args(kind), None).await, Err(CallError::User { .. })));
            assert_eq!(vote_fingerprint(&test_db, corp).await, before);
        }
        assert!(matches!(rpc(&mut client, "voteManager", "ActivateSanctionedAction", &[], None).await, Err(CallError::User { .. })));
        assert_eq!(vote_fingerprint(&test_db, corp).await, before);
        assert_eq!(wire(rpc(&mut client, "voteManager", "CanRunForCEO", &[], None).await.unwrap()), Value::Bool(false));
        let actions = wire(rpc(&mut client, "voteManager", "GetCorpSanctionedActions", &[], None).await.unwrap());
        let (_, entries) = indexed(&actions, "voteCaseID");
        assert!(entries.is_empty());
        let case = rpc(&mut client, "voteManager", "InsertVoteCase", &vote_args(4), None).await.unwrap().as_int().unwrap();
        let args = [Value::Int(corp), Value::Int(case)];
        let cases = wire(rpc(&mut client, "voteManager", "GetVoteCasesByCorporation", &[Value::Int(corp)], None).await.unwrap());
        let Value::Dict(statuses) = cases else { panic!("expected status map") };
        let open = &statuses.iter().find(|(key, _)| *key == Value::Int(2)).unwrap().1;
        let closed = &statuses.iter().find(|(key, _)| *key == Value::Int(1)).unwrap().1;
        let (header, open_rows) = rowset(open);
        let (closed_header, closed_rows) = rowset(closed);
        assert_eq!(header, closed_header);
        assert!(closed_rows.is_empty());
        assert_eq!(open_rows.len(), 1);
        assert_eq!(header.get(&open_rows[0], "voteCaseID"), Some(Value::Int(case)));
        assert_eq!(header.get(&open_rows[0], "voteCaseText"), Some(Value::unicode("Proposal")));
        let single = wire(rpc(&mut client, "voteManager", "GetVoteCase", &args, None).await.unwrap());
        let Value::DbRow { descriptor, .. } = &single else { panic!("expected case row") };
        assert_eq!(DbRowDescriptor::from_value(descriptor).unwrap(), header);
        assert_eq!(single, open_rows[0]);
        rpc(&mut client, "voteManager", "InsertVote", &[Value::Int(corp), Value::Int(case), Value::Int(0)], None).await.unwrap();
        client.session.apply(vec![("charid", Value::Int(holder.character))]);
        rpc(&mut client, "voteManager", "InsertVote", &[Value::Int(corp), Value::Int(case), Value::Int(1)], None).await.unwrap();
        let votes = wire(rpc(&mut client, "voteManager", "GetVotes", &[Value::Int(case)], None).await.unwrap());
        let (desc, rows) = rowset(&votes);
        assert_eq!(rows.len(), 1);
        assert_eq!(desc.get(&rows[0], "characterID"), Some(Value::Int(holder.character)));
        assert_eq!(desc.get(&rows[0], "optionID"), Some(Value::Int(1)));
        assert_eq!(desc.get(&rows[0], "shares"), Some(Value::Int(60)));
        let options = wire(rpc(&mut client, "voteManager", "GetVoteCaseOptions", &args, None).await.unwrap());
        let (desc, rows) = rowset(&options);
        assert_eq!(rows.len(), 2);
        for (index, (text, weight)) in [("Yes", 40), ("No", 60)].into_iter().enumerate() {
            assert_eq!(desc.get(&rows[index], "optionID"), Some(Value::Int(index as i64)));
            assert_eq!(desc.get(&rows[index], "optionText"), Some(Value::unicode(text)));
            assert_eq!(desc.get(&rows[index], "votesFor"), Some(Value::Int(weight)));
        }
        client.session.apply(vec![("charid", Value::Int(outsider.character))]);
        assert_eq!(wire(rpc(&mut client, "voteManager", "CanViewVotes", &[Value::Int(corp)], None).await.unwrap()), Value::Bool(false));
    }).await;
    db.execute("DELETE FROM game.jobs WHERE job_id IN (SELECT 'corp.vote.close:' || id::text FROM game.corp_vote_cases WHERE corporation_id=$1)", &[&corp]).await.unwrap();
    for table in ["corp_vote_claims", "corp_vote_ballots", "corp_vote_snapshot", "corp_vote_options"] {
        db.execute(&format!("DELETE FROM game.{table} WHERE case_id IN (SELECT id FROM game.corp_vote_cases WHERE corporation_id=$1)"), &[&corp]).await.unwrap();
    }
    db.execute("DELETE FROM game.corp_vote_cases WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_dividend_recipients WHERE operation_id IN (SELECT operation_id FROM game.corp_dividends WHERE corporation_id=$1)", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_dividends WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_share_transfers WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_shares WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.corp_wallets WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=$1", &[&ceo.character]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
    fixture::cleanup(&db, ceo).await.unwrap();
    fixture::cleanup(&db, holder).await.unwrap();
    fixture::cleanup(&db, outsider).await.unwrap();
    result.unwrap();
}
