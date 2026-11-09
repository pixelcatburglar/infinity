//! Tiered access, atomic bulk operations and known-folder persistence.
use super::*;

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn tier_matrix_and_bulk_refusals() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let guest = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut a, _a) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut b, _b) = crate::services::tests::client(test_db.clone(), guest.character).await;
        let admin = group(&mut a, "admin").await;
        let manage = group(&mut a, "manage").await;
        let use_group = group(&mut a, "use").await;
        let view = group(&mut a, "view").await;
        grant(&mut a, view, guest.character).await;
        let f = folder(&mut a, [Some(admin), Some(manage), Some(use_group), Some(view)]).await;
        let info = rpc(&mut b, "accessGroupBookmarkMgr", "GetFolderInfo", &[Value::Int(f)]).await.unwrap();
        assert_eq!(crate::services::value_field(&info, "accessLevel"), Some(&Value::Int(access::VIEW)));
        assert!(dao::known::known(&test_db, guest.character).await.unwrap().is_empty());
        assert!(rpc(&mut b, "accessGroupBookmarkMgr", "CreateSubfolder", &[Value::Int(f), Value::unicode("denied")]).await.is_err());
        assert!(rpc(&mut b, "accessGroupBookmarkMgr", "DeleteFolder", &[Value::Int(f)]).await.is_err());
        rpc(&mut b, "accessGroupBookmarkMgr", "AddToKnownFolders", &[Value::Int(f), Value::Bool(true)]).await.unwrap();
        let first = mutations::add(&a, owner.character, f, None, None, None, 1, [1.0, 2.0, 3.0], "owner", "", None).await.unwrap().items().unwrap()[0].as_int().unwrap();
        let dest_args = [Value::Bool(true), Value::unicode("destination"), Value::unicode(""), Value::None, Value::None, Value::None, Value::None];
        let dest_row = rpc(&mut b, "accessGroupBookmarkMgr", "AddFolder", &dest_args).await.unwrap();
        let dest = crate::services::value_field(&dest_row, "folderID").unwrap().as_int().unwrap();
        let copied = rpc(&mut b, "accessGroupBookmarkMgr", "CopyBookmarksToFolderAndSubfolder", &[Value::Int(f), Value::Int(dest), Value::None, ids(&[first])]).await.unwrap();
        let Value::Dict(rows) = &copied.items().unwrap()[0] else { panic!("expected copied bookmark dictionary"); };
        assert_eq!(rows.len(), 1);
        assert_eq!(crate::services::value_field(&rows[0].1, "creatorID"), Some(&Value::Int(guest.character)));
        assert!(rpc(&mut b, "accessGroupBookmarkMgr", "CopyBookmarksToFolderAndSubfolder", &[Value::Int(f), Value::Int(dest), Value::None, ids(&[first, i64::MAX])]).await.is_err());
        assert_eq!(dao::bookmarks(&test_db, &[dest], i64::MIN).await.unwrap().len(), 1);
        let no_expiry = mutations::add(&a, owner.character, f, None, None, None, 1, [0.0; 3], "none", "", Some(0)).await.unwrap();
        assert_eq!(no_expiry.items().unwrap()[7], Value::None);
        for (option, duration) in [(1, 4 * infinity_core::consts::FILETIME_HOUR), (2, 2 * infinity_core::consts::FILETIME_DAY), (3, 24 * infinity_core::consts::FILETIME_HOUR)] {
            let before = crate::client::filetime_now();
            let row = mutations::add(&a, owner.character, f, None, None, None, 1, [0.0; 3], "expiry", "", Some(option)).await.unwrap();
            let after = crate::client::filetime_now();
            let expiry = row.items().unwrap()[7].as_int().unwrap();
            assert!((before + duration..=after + duration).contains(&expiry));
        }
        for option in [4, i64::MAX] {
            assert!(mutations::add(&a, owner.character, f, None, None, None, 1, [0.0; 3], "invalid", "", Some(option)).await.is_err());
        }
        grant(&mut a, use_group, guest.character).await;
        let info = rpc(&mut b, "accessGroupBookmarkMgr", "GetFolderInfo", &[Value::Int(f)]).await.unwrap();
        assert_eq!(crate::services::value_field(&info, "accessLevel"), Some(&Value::Int(access::USE)));
        let row = rpc(&mut b, "accessGroupBookmarkMgr", "CreateSubfolder", &[Value::Int(f), Value::unicode("guest")]).await.unwrap();
        let sub = crate::services::value_field(&row, "subfolderID").unwrap().as_int().unwrap();
        let own = mutations::add(&b, guest.character, f, Some(sub), None, None, 1, [4.0, 5.0, 6.0], "own", "", None).await.unwrap().items().unwrap()[0].as_int().unwrap();
        assert!(rpc(&mut b, "accessGroupBookmarkMgr", "DeleteBookmarks", &[Value::Int(f), ids(&[first, own])]).await.is_err());
        assert!(rpc(&mut b, "accessGroupBookmarkMgr", "DeleteBookmarks", &[Value::Int(f), ids(&[own, i64::MAX])]).await.is_err());
        assert_eq!(dao::bookmarks_by_ids(&test_db, f, &[first, own]).await.unwrap().len(), 2);
        let other_sub = rpc(&mut a, "accessGroupBookmarkMgr", "CreateSubfolder", &[Value::Int(f), Value::unicode("owner sub")]).await.unwrap();
        let other_sub = crate::services::value_field(&other_sub, "subfolderID").unwrap().as_int().unwrap();
        assert!(rpc(&mut b, "accessGroupBookmarkMgr", "DeleteSubfolder", &[Value::Int(f), Value::Int(other_sub)]).await.is_err());
        let foreign_child = mutations::add(&a, owner.character, f, Some(sub), None, None, 1, [0.0; 3], "foreign", "", None).await.unwrap().items().unwrap()[0].as_int().unwrap();
        assert!(rpc(&mut b, "accessGroupBookmarkMgr", "DeleteSubfolder", &[Value::Int(f), Value::Int(sub)]).await.is_err());
        assert_eq!(dao::bookmarks_by_ids(&test_db, f, &[own, foreign_child]).await.unwrap().len(), 2);
        assert!(rpc(&mut b, "accessGroupBookmarkMgr", "MoveBookmarksToFolderAndSubfolder", &[Value::Int(f), Value::Int(f), Value::None, ids(&[own])]).await.is_err());
        grant(&mut a, manage, guest.character).await;
        let info = rpc(&mut b, "accessGroupBookmarkMgr", "GetFolderInfo", &[Value::Int(f)]).await.unwrap();
        assert_eq!(crate::services::value_field(&info, "accessLevel"), Some(&Value::Int(access::MANAGE)));
        assert_eq!(rpc(&mut b, "accessGroupBookmarkMgr", "DeleteBookmarks", &[Value::Int(f), ids(&[first])]).await.unwrap(), ids(&[first]));
        assert!(rpc(&mut b, "accessGroupBookmarkMgr", "DeleteFolder", &[Value::Int(f)]).await.is_err());
        grant(&mut a, admin, guest.character).await;
        let info = rpc(&mut b, "accessGroupBookmarkMgr", "GetFolderInfo", &[Value::Int(f)]).await.unwrap();
        assert_eq!(crate::services::value_field(&info, "accessLevel"), Some(&Value::Int(access::ADMIN)));
        rpc(&mut b, "accessGroupBookmarkMgr", "UpdateKnownFolderState", &[Value::Int(f), Value::Bool(false)]).await.unwrap();
        let reconnect = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        assert!(!dao::known::known(&reconnect, guest.character).await.unwrap().into_iter().find(|k| k.folder_id == f).unwrap().is_active);
        rpc(&mut b, "accessGroupBookmarkMgr", "RemoveFromKnownFolders", &[Value::Int(f)]).await.unwrap();
        let reconnect = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        assert!(!dao::known::known(&reconnect, guest.character).await.unwrap().iter().any(|k| k.folder_id == f));
        rpc(&mut b, "accessGroupBookmarkMgr", "DeleteFolder", &[Value::Int(f)]).await.unwrap();
        let personal_args = [Value::Bool(true), Value::unicode("personal"), Value::unicode(""), Value::None, Value::None, Value::None, Value::None];
        let row = rpc(&mut a, "accessGroupBookmarkMgr", "AddFolder", &personal_args).await.unwrap();
        let personal = crate::services::value_field(&row, "folderID").unwrap().as_int().unwrap();
        assert!(rpc(&mut b, "accessGroupBookmarkMgr", "GetFolderInfo", &[Value::Int(personal)]).await.is_err());
        rpc(&mut a, "accessGroupBookmarkMgr", "UpdateKnownFolderState", &[Value::Int(personal), Value::Bool(false)]).await.unwrap();
        let reconnect = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        assert!(!dao::known::known(&reconnect, owner.character).await.unwrap().into_iter().find(|k| k.folder_id == personal).unwrap().is_active);
        rpc(&mut a, "accessGroupBookmarkMgr", "RemoveFromKnownFolders", &[Value::Int(personal)]).await.unwrap();
        let reconnect = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        assert!(!dao::known::known(&reconnect, owner.character).await.unwrap().iter().any(|k| k.folder_id == personal));
    }).await;
    cleanup(&db, &[owner.character, guest.character]).await;
    for f in [owner, guest] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn fresh_collection_forgets_affiliation_lost_subscription() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let guest = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut a, _a) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut b, _b) = crate::services::tests::client(test_db.clone(), guest.character).await;
        let admin = group(&mut a, "admin").await;
        let view = group(&mut a, "corporation").await;
        let corp = crate::dao::corporations::registry::membership(&test_db, guest.character).await.unwrap().corporation_id;
        crate::dao::access_groups::set_member(&test_db, view, corp, crate::services::access_groups::MEMBER).await.unwrap();
        let f = folder(&mut a, [Some(admin), None, None, Some(view)]).await;
        rpc(&mut b, "accessGroupBookmarkMgr", "AddToKnownFolders", &[Value::Int(f), Value::Bool(true)]).await.unwrap();
        let bookmark = mutations::add(&a, owner.character, f, None, None, None, 1, [0.0; 3], "visible", "", None).await.unwrap().items().unwrap()[0].as_int().unwrap();
        let stable = group(&mut a, "stable").await;
        grant(&mut a, stable, guest.character).await;
        let inactive = folder(&mut a, [Some(admin), None, None, Some(stable)]).await;
        rpc(&mut b, "accessGroupBookmarkMgr", "AddToKnownFolders", &[Value::Int(inactive), Value::Bool(false)]).await.unwrap();
        test_db.execute("UPDATE game.characters SET corporation_id = $2 WHERE character_id = $1", &[&guest.character, &(corp + 1)]).await.unwrap();
        let collection = rpc(&mut b, "accessGroupBookmarkMgr", "GetMyActiveBookmarks", &[]).await.unwrap();
        assert!(!collection.items().unwrap()[0].items().unwrap().iter().any(|row| crate::services::value_field(row, "folderID") == Some(&Value::Int(f))));
        assert!(!collection.items().unwrap()[1].items().unwrap().iter().any(|row| crate::services::value_field(row, "bookmarkID") == Some(&Value::Int(bookmark))));
        assert!(collection.items().unwrap()[0].items().unwrap().iter().any(|row| crate::services::value_field(row, "folderID") == Some(&Value::Int(inactive)) && crate::services::value_field(row, "isActive") == Some(&Value::Bool(false))));
        assert!(dao::known::known(&test_db, guest.character).await.unwrap().iter().any(|row| row.folder_id == inactive && !row.is_active));
        assert!(!dao::known::known(&test_db, guest.character).await.unwrap().iter().any(|row| row.folder_id == f));
        test_db.execute("UPDATE game.characters SET corporation_id = $2 WHERE character_id = $1", &[&guest.character, &corp]).await.unwrap();
        let collection = rpc(&mut b, "accessGroupBookmarkMgr", "GetMyActiveBookmarks", &[]).await.unwrap();
        assert!(!collection.items().unwrap()[0].items().unwrap().iter().any(|row| crate::services::value_field(row, "folderID") == Some(&Value::Int(f))));
        assert!(!dao::known::known(&test_db, guest.character).await.unwrap().iter().any(|row| row.folder_id == f));
        rpc(&mut b, "accessGroupBookmarkMgr", "AddToKnownFolders", &[Value::Int(f), Value::Bool(true)]).await.unwrap();
        assert!(dao::known::known(&test_db, guest.character).await.unwrap().iter().any(|row| row.folder_id == f));
    }).await;
    cleanup(&db, &[owner.character, guest.character]).await;
    for f in [owner, guest] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn creator_has_no_bypass_and_independent_roles_combine() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let other = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut a, _a) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut b, _b) = crate::services::tests::client(test_db.clone(), other.character).await;
        let admin = group(&mut a, "admin").await;
        let manage = group(&mut b, "manage").await;
        let use_group = group(&mut b, "use").await;
        let view = group(&mut b, "view").await;
        let admin_grant = Value::Dict(vec![(Value::Int(other.character), Value::Int(crate::services::access_groups::ADMIN.into()))]);
        rpc(&mut a, "ownerGroupManager", "AddMembers", &[Value::Int(admin), admin_grant]).await.unwrap();
        let f = folder(&mut a, [Some(admin), Some(manage), Some(use_group), Some(view)]).await;
        let stored = dao::folder(&test_db, f).await.unwrap().unwrap();
        assert_eq!((stored.owner_id, stored.admin_group_id, stored.manage_group_id, stored.use_group_id, stored.view_group_id),
                   (owner.character, Some(admin), Some(manage), Some(use_group), Some(view)));
        grant(&mut b, use_group, owner.character).await;
        grant(&mut b, view, owner.character).await;
        rpc(&mut b, "ownerGroupManager", "DeleteMembers", &[Value::Int(admin), ids(&[owner.character])]).await.unwrap();
        let info = rpc(&mut a, "accessGroupBookmarkMgr", "GetFolderInfo", &[Value::Int(f)]).await.unwrap();
        assert_eq!(crate::services::value_field(&info, "accessLevel"), Some(&Value::Int(access::USE)));
        assert_eq!(crate::services::value_field(&info, "creatorID"), Some(&Value::Int(owner.character)));
        assert_eq!(crate::services::value_field(&info, "ownerID"), Some(&Value::Int(owner.character)));
        rpc(&mut b, "ownerGroupManager", "UpdateMembershipTypes", &[Value::Int(use_group), ids(&[owner.character]), Value::Int(crate::services::access_groups::EXCLUDED.into())]).await.unwrap();
        let info = rpc(&mut a, "accessGroupBookmarkMgr", "GetFolderInfo", &[Value::Int(f)]).await.unwrap();
        assert_eq!(crate::services::value_field(&info, "accessLevel"), Some(&Value::Int(access::VIEW)));
        rpc(&mut b, "ownerGroupManager", "DeleteMembers", &[Value::Int(view), ids(&[owner.character])]).await.unwrap();
        assert!(rpc(&mut a, "accessGroupBookmarkMgr", "GetFolderInfo", &[Value::Int(f)]).await.is_err());
        assert!(rpc(&mut a, "accessGroupBookmarkMgr", "DeleteFolder", &[Value::Int(f)]).await.is_err());
    }).await;
    cleanup(&db, &[owner.character, other.character]).await;
    for f in [owner, other] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn alliance_loss_on_folder_read_forgets_subscription_without_notification() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let guest = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let corp: i64 = test_db.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('bookmark-' || nextval('game.corporations_corporation_id_seq')::text, 'B' || currval('game.corporations_corporation_id_seq')::text, $1,$1,1,1) RETURNING corporation_id", &[&guest.character]).await.unwrap().get("corporation_id");
        let alliance: i64 = test_db.query_one("INSERT INTO game.alliances (name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES ('bookmark-' || nextval('game.alliances_alliance_id_seq')::text, 'B' || currval('game.alliances_alliance_id_seq')::text, $1,$1,$2,1) RETURNING alliance_id", &[&corp, &guest.character]).await.unwrap().get("alliance_id");
        test_db.execute("UPDATE game.corporations SET alliance_id=$1 WHERE corporation_id=$2", &[&alliance, &corp]).await.unwrap();
        test_db.execute("UPDATE game.characters SET corporation_id=$1 WHERE character_id=$2", &[&corp, &guest.character]).await.unwrap();
        let (mut a, _a) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (mut b, _b) = crate::services::tests::client(test_db.clone(), guest.character).await;
        b.world = a.world.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        a.world.register_pilot(guest.character, tx);
        let admin = group(&mut a, "admin").await;
        let view = group(&mut a, "alliance").await;
        crate::dao::access_groups::set_member(&test_db, view, alliance, crate::services::access_groups::MEMBER).await.unwrap();
        let f = folder(&mut a, [Some(admin), None, None, Some(view)]).await;
        let id = mutations::add(&a, owner.character, f, None, None, None, 1, [0.0; 3], "alliance", "", None).await.unwrap().items().unwrap()[0].as_int().unwrap();
        rpc(&mut b, "accessGroupBookmarkMgr", "AddToKnownFolders", &[Value::Int(f), Value::Bool(true)]).await.unwrap();
        while rx.try_recv().is_ok() {}
        test_db.execute("UPDATE game.corporations SET alliance_id=NULL WHERE corporation_id=$1", &[&corp]).await.unwrap();
        assert!(rpc(&mut b, "accessGroupBookmarkMgr", "GetFolderInfo", &[Value::Int(f)]).await.is_err());
        let reconnect = Db::connect(&crate::config::tests::test_database()).await.unwrap();
        assert!(!dao::known::known(&reconnect, guest.character).await.unwrap().iter().any(|row| row.folder_id == f));
        let collection = rpc(&mut b, "accessGroupBookmarkMgr", "GetMyActiveBookmarks", &[]).await.unwrap();
        assert!(!collection.items().unwrap()[1].items().unwrap().iter().any(|row| crate::services::value_field(row, "bookmarkID") == Some(&Value::Int(id))));
        assert!(rx.try_recv().is_err());
        test_db.execute("UPDATE game.corporations SET alliance_id=$1 WHERE corporation_id=$2", &[&alliance, &corp]).await.unwrap();
        let collection = rpc(&mut b, "accessGroupBookmarkMgr", "GetMyActiveBookmarks", &[]).await.unwrap();
        assert!(!collection.items().unwrap()[0].items().unwrap().iter().any(|row| crate::services::value_field(row, "folderID") == Some(&Value::Int(f))));
        rpc(&mut b, "accessGroupBookmarkMgr", "AddToKnownFolders", &[Value::Int(f), Value::Bool(true)]).await.unwrap();
        assert!(dao::known::known(&test_db, guest.character).await.unwrap().iter().any(|row| row.folder_id == f));
    }).await;
    cleanup(&db, &[owner.character, guest.character]).await;
    db.execute("UPDATE game.characters SET corporation_id=1 WHERE character_id=$1", &[&guest.character]).await.unwrap();
    db.execute("UPDATE game.corporations SET alliance_id=NULL WHERE creator_id=$1", &[&guest.character]).await.unwrap();
    db.execute("DELETE FROM game.alliances WHERE creator_char_id=$1", &[&guest.character]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE creator_id=$1", &[&guest.character]).await.unwrap();
    for f in [owner, guest] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}
