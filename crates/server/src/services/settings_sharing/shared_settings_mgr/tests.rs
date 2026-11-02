use super::*;

#[test]
fn required_shapes_accept_client_sequences_and_reject_bad_entries() {
    let filter = Value::dict([("filter", Value::tuple([Value::unicode("样本"), Value::Int(1), Value::list([Value::tuple([Value::Int(1), Value::Int(2), Value::str("x")])])]))]);
    assert!(valid_shape(ITEMS, &filter));
    assert!(!valid_shape(BROADCAST, &filter));
    for color in [Value::None, Value::list([Value::Float(0.5), Value::Int(1), Value::Float(0.0)])] {
        let broadcast = Value::dict([("broadcastSettings", Value::list([Value::tuple([Value::str("special"), Value::Bool(false), color])]))]);
        assert!(valid_shape(BROADCAST, &broadcast));
    }
    for color in [Value::str("red"), Value::list([Value::Int(1)]), Value::list([Value::Int(1), Value::Int(2), Value::Int(3), Value::Int(4)]), Value::list([Value::Int(1), Value::str("x"), Value::Int(3)]), Value::list([Value::Float(f64::INFINITY), Value::Int(2), Value::Int(3)])] {
        let bad = Value::dict([("broadcastSettings", Value::list([Value::tuple([Value::Int(1), Value::Bool(true), color])]))]);
        assert!(!valid_shape(BROADCAST, &bad));
    }
    let bad = Value::dict([("broadcastSettings", Value::list([Value::tuple([Value::Int(1), Value::Int(1), Value::None])]))]);
    assert!(!valid_shape(BROADCAST, &bad));
    for condition in [Value::tuple([Value::str("kind"), Value::Int(2), Value::Bool(true)]), Value::tuple([Value::Int(1), Value::None, Value::Bool(true)])] {
        let bad = Value::dict([("filter", Value::tuple([Value::str("name"), Value::Int(1), Value::list([condition])]))]);
        assert!(!valid_shape(ITEMS, &bad));
    }
    let bad_mode = Value::dict([("filter", Value::tuple([Value::str("name"), Value::Bool(true), Value::list([])]))]);
    assert!(!valid_shape(ITEMS, &bad_mode));
}

async fn rpc(client: &mut Client, method: &str, args: &[Value]) -> CallResult {
    let kwargs = Value::Dict(vec![]);
    crate::services::dispatch(client, &Call { service: "sharedSettingsMgr", method, args, kwargs: &kwargs, bound: false, bind_params: None }).await
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with SDE"]
async fn dispatch_shares_both_types_across_accounts() {
    crate::tests::init_config();
    let db = crate::db::Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let a = crate::services::tests::fixture::create(&db).await.unwrap();
    let b = crate::services::tests::fixture::create(&db).await.unwrap();
    let (mut client, _peer) = crate::services::tests::client(db.clone(), a.character).await;
    client.login.user_id = a.account;
    let (mut receiver, _receiver_peer) = crate::services::tests::client(db.clone(), b.character).await;
    receiver.login.user_id = b.account;
    let mut ids = Vec::new();
    let result: anyhow::Result<()> = async {
        let data = [
            (ITEMS, Value::dict([("filter", Value::tuple([Value::unicode("名称\"中文"), Value::Int(1), Value::list([Value::tuple([Value::Int(1), Value::Int(2), Value::unicode("条件\"")]), Value::tuple([Value::Int(3), Value::Int(4), Value::Bool(false)])])]))]), serde_json::json!([["filter", ["名称\"中文", 1, [[1, 2, "条件\""], [3, 4, false]]]]])),
            (BROADCAST, Value::dict([("broadcastSettings", Value::list([Value::tuple([Value::Int(1), Value::Bool(true), Value::None]), Value::tuple([Value::str("special"), Value::Bool(false), Value::tuple([Value::Float(0.5), Value::Int(1), Value::Float(0.0)])])]))]), serde_json::json!([["broadcastSettings", [[1, true, null], ["special", false, [0.5, 1, 0.0]]]]])),
        ];
        let mut originals = Vec::new();
        for (kind, value, expected) in data {
            client.login.user_id = a.account;
            let reply = rpc(&mut client, "StoreSettingLinkAndGetID", &[Value::Int(i64::from(kind)), value]).await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
            let reply = marshal::loads(&marshal::dumps(&reply))?;
            let Value::Instance(_, fields) = reply else { anyhow::bail!("expected keyval") };
            let sq = fields.get("sqID").and_then(Value::as_int).ok_or_else(|| anyhow::anyhow!("missing ID"))?;
            ids.push(sq);
            anyhow::ensure!(matches!(fields.as_ref(), Value::Dict(entries) if entries.len() == 3) && fields.get("hashvalue").is_some() && fields.get("sqID").is_some() && fields.get("settingTypeID").is_some(), "incorrect keyval fields");
            let hash = fields.get("hashvalue").ok_or_else(|| anyhow::anyhow!("missing hash"))?.clone();
            anyhow::ensure!(fields.get("settingTypeID") == Some(&Value::Int(i64::from(kind))), "wrong type");
            let key = Value::tuple([hash.clone(), Value::Int(sq), Value::Int(i64::from(kind))]);
            let payload = rpc(&mut receiver, "GetStoredSharedSetting", &[key.clone()]).await.map_err(|e| anyhow::anyhow!("{e:?}"))?;
            let Value::Bytes(bytes) = payload else { anyhow::bail!("missing payload") };
            anyhow::ensure!(serde_json::from_slice::<serde_json::Value>(&bytes)? == expected, "payload differs from shared settings");
            originals.push((key, bytes.clone()));
            use sha2::Digest;
            let Value::Bytes(hex) = &hash else { anyhow::bail!("digest is not a byte string") };
            let digest = sha2::Sha256::digest(&bytes);
            anyhow::ensure!(hex.len() == digest.len() * 2 && hex.iter().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b)), "incorrect digest");
            for (pair, byte) in hex.chunks_exact(2).zip(digest) {
                anyhow::ensure!(u8::from_str_radix(std::str::from_utf8(pair)?, 16)? == byte, "incorrect digest");
            }
            let tampered = Value::tuple([Value::str("wrong"), Value::Int(sq), Value::Int(i64::from(kind))]);
            anyhow::ensure!(rpc(&mut receiver, "GetStoredSharedSetting", &[tampered]).await.map_err(|e| anyhow::anyhow!("{e:?}"))? == Value::None, "tampered link resolved");
            for bad in [
                Value::tuple([hash.clone(), Value::Int(sq), Value::Int(if kind == ITEMS { i64::from(BROADCAST) } else { i64::from(ITEMS) })]),
                Value::tuple([hash.clone(), Value::Int(0), Value::Int(i64::from(kind))]),
            ] {
                anyhow::ensure!(rpc(&mut receiver, "GetStoredSharedSetting", &[bad]).await.map_err(|e| anyhow::anyhow!("{e:?}"))? == Value::None, "wrong key resolved");
            }
        }
        let reopened = crate::db::Db::connect(&crate::config::tests::test_database()).await?;
        let (mut reopened_receiver, _peer) = crate::services::tests::client(reopened, b.character).await;
        reopened_receiver.login.user_id = b.account;
        for (key, bytes) in originals {
            anyhow::ensure!(rpc(&mut reopened_receiver, "GetStoredSharedSetting", &[key]).await.map_err(|e| anyhow::anyhow!("{e:?}"))? == Value::Bytes(bytes), "snapshot did not persist");
        }
        client.login.user_id = 0;
        anyhow::ensure!(matches!(rpc(&mut client, "GetStoredSharedSetting", &[]).await, Err(crate::services::CallError::User { .. })), "anonymous request accepted");
        client.login.user_id = a.account;
        anyhow::ensure!(matches!(rpc(&mut client, "StoreSettingLinkAndGetID", &[Value::Int(2), Value::dict([] as [(&str, Value); 0])]).await, Err(crate::services::CallError::User { .. })), "invalid request accepted");
        let oversized = Value::dict([("filter", Value::tuple([Value::unicode("x".repeat(crate::config::game().shared_settings.max_payload_bytes)), Value::Int(1), Value::list([])]))]);
        anyhow::ensure!(matches!(rpc(&mut client, "StoreSettingLinkAndGetID", &[Value::Int(2), oversized]).await, Err(crate::services::CallError::User { .. })), "oversized request accepted");
        let mut nested = Value::None;
        for _ in 0..crate::config::game().shared_settings.max_nesting_depth { nested = Value::list([nested]); }
        let too_deep = Value::dict([("filter", Value::tuple([Value::str("test"), Value::Int(1), Value::list([])])), ("extra", nested)]);
        anyhow::ensure!(matches!(rpc(&mut client, "StoreSettingLinkAndGetID", &[Value::Int(2), too_deep]).await, Err(crate::services::CallError::User { .. })), "deep request accepted");
        anyhow::ensure!(matches!(rpc(&mut client, "GetStoredSharedSetting", &[Value::tuple([Value::str("bad")])]).await, Err(crate::services::CallError::User { .. })), "malformed key accepted");
        let count: i64 = db.query_one("SELECT count(*) AS count FROM game.shared_setting_links WHERE creator_user_id = $1", &[&a.account]).await?.get("count");
        anyhow::ensure!(count == 2, "rejected request inserted a row");
        client.login.user_id = i64::MAX;
        let valid = Value::dict([("filter", Value::tuple([Value::str("test"), Value::Int(1), Value::list([])]))]);
        anyhow::ensure!(matches!(rpc(&mut client, "StoreSettingLinkAndGetID", &[Value::Int(2), valid]).await, Err(crate::services::CallError::Internal(_))), "database failure was not propagated");
        Ok(())
    }.await;
    db.execute("DELETE FROM game.shared_setting_links WHERE sq_id = ANY($1)", &[&ids]).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, a).await.unwrap();
    crate::services::tests::fixture::cleanup(&db, b).await.unwrap();
    result.unwrap();
}
