//! Shared bookmark wire and authorization regressions.
mod permissions;
mod notifications;

use super::*;
use crate::{client::Client, db::Db, services::{Call, CallError}};
use crate::services::tests::fixture;

async fn rpc(c: &mut Client, service: &str, method: &str, args: &[Value]) -> Result<Value, CallError> {
    let kwargs = Value::Dict(vec![]);
    crate::services::dispatch(c, &Call { service, method, args, kwargs: &kwargs, bound: false, bind_params: None }).await
}
async fn group(c: &mut Client, name: &str) -> i64 {
    rpc(c, "ownerGroupManager", "Create", &[Value::unicode(name), Value::unicode("")]).await.unwrap().as_int().unwrap()
}
async fn grant(c: &mut Client, group: i64, member: i64) {
    let rows = Value::Dict(vec![(Value::Int(member), Value::Int(0))]);
    assert_eq!(rpc(c, "ownerGroupManager", "AddMembers", &[Value::Int(group), rows]).await.unwrap(), Value::list([]));
}
async fn folder(c: &mut Client, groups: [Option<i64>; 4]) -> i64 {
    let mut args = vec![Value::Bool(false), Value::unicode("shared"), Value::unicode("description")];
    args.extend(groups.into_iter().map(|id| id.map(Value::Int).unwrap_or(Value::None)));
    let row = rpc(c, "accessGroupBookmarkMgr", "AddFolder", &args).await.unwrap();
    crate::services::value_field(&row, "folderID").unwrap().as_int().unwrap()
}
fn ids(ids: &[i64]) -> Value { Value::list(ids.iter().copied().map(Value::Int)) }
async fn cleanup(db: &Db, chars: &[i64]) {
    db.execute("DELETE FROM game.bookmarks WHERE folder_id IN (SELECT folder_id FROM game.bookmark_folders WHERE owner_id = ANY($1))", &[&chars]).await.unwrap();
    db.execute("DELETE FROM game.bookmark_subfolders WHERE folder_id IN (SELECT folder_id FROM game.bookmark_folders WHERE owner_id = ANY($1))", &[&chars]).await.unwrap();
    db.execute("DELETE FROM game.bookmark_folders WHERE owner_id = ANY($1)", &[&chars]).await.unwrap();
    db.execute("DELETE FROM game.access_group_logs WHERE character_id = ANY($1)", &[&chars]).await.unwrap();
    db.execute("DELETE FROM game.access_groups WHERE creator_id = ANY($1)", &[&chars]).await.unwrap();
}

#[test]
fn stored_creator_and_unicode_wire_rows_roundtrip() {
    let folder = dao::Folder { folder_id: 10, owner_id: 20, name: "目录".into(), description: "描述".into(), is_personal: false, admin_group_id: Some(30), manage_group_id: Some(31), use_group_id: Some(32), view_group_id: Some(33) };
    let sub = dao::Subfolder { subfolder_id: 40, folder_id: 10, creator_id: 21, name: "子目录".into() };
    let bookmark = dao::Bookmark { bookmark_id: 50, folder_id: 10, subfolder_id: Some(40), item_id: Some(60), type_id: Some(70), location_id: 80, x: 1.5, y: 2.5, z: 3.5, memo: "位置".into(), note: "笔记".into(), created: 90, expiry: Some(100), creator_id: 22 };
    let f = folder_row(&folder, false, access::VIEW);
    assert_eq!(crate::services::value_field(&f, "creatorID"), Some(&Value::Int(20)));
    assert_eq!(crate::services::value_field(&f, "ownerID"), Some(&Value::Int(20)));
    assert_eq!(crate::services::value_field(&f, "isPersonal"), Some(&Value::Bool(false)));
    assert_eq!(crate::services::value_field(&f, "isActive"), Some(&Value::Bool(false)));
    for (key, id) in [("adminGroupID", 30), ("manageGroupID", 31), ("useGroupID", 32), ("viewGroupID", 33)] { assert_eq!(crate::services::value_field(&f, key), Some(&Value::Int(id))); }
    assert_eq!(crate::services::value_field(&f, "folderName"), Some(&Value::unicode("目录")));
    assert_eq!(crate::services::value_field(&f, "folderID"), Some(&Value::Int(10)));
    assert_eq!(crate::services::value_field(&f, "description"), Some(&Value::unicode("描述")));
    assert_eq!(crate::services::value_field(&f, "accessLevel"), Some(&Value::Int(access::VIEW)));
    let personal = folder_row(&dao::Folder { is_personal: true, ..folder.clone() }, true, access::PERSONAL);
    assert_eq!(crate::services::value_field(&personal, "accessLevel"), Some(&Value::Int(1)));
    let s = subfolder_row(&sub);
    assert_eq!(crate::services::value_field(&s, "creatorID"), Some(&Value::Int(21)));
    assert_eq!(crate::services::value_field(&s, "subfolderName"), Some(&Value::unicode("子目录")));
    assert_eq!(crate::services::value_field(&s, "subfolderID"), Some(&Value::Int(40)));
    assert_eq!(crate::services::value_field(&s, "folderID"), Some(&Value::Int(10)));
    let b = bookmark_row(&bookmark);
    assert_eq!(crate::services::value_field(&b, "creatorID"), Some(&Value::Int(22)));
    assert_eq!(crate::services::value_field(&b, "subfolderID"), Some(&Value::Int(40)));
    assert_eq!(crate::services::value_field(&b, "memo"), Some(&Value::unicode("位置")));
    assert_eq!(crate::services::value_field(&b, "note"), Some(&Value::unicode("笔记")));
    assert_eq!(crate::services::value_field(&b, "x"), Some(&Value::Float(1.5)));
    for (key, value) in [
        ("bookmarkID", Value::Int(50)), ("folderID", Value::Int(10)),
        ("itemID", Value::Int(60)), ("typeID", Value::Int(70)),
        ("flag", Value::None), ("locationID", Value::Int(80)),
        ("y", Value::Float(2.5)), ("z", Value::Float(3.5)),
        ("created", Value::Int(90)), ("expiry", Value::Int(100)),
    ] { assert_eq!(crate::services::value_field(&b, key), Some(&value)); }
    for row in [f, s, b] { assert_eq!(marshal::loads(&marshal::dumps(&row)).unwrap(), row); }
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL and external flock"]
async fn navigation_resolves_only_authorized_valid_bookmarks() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let guest = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut a, _) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let (b, _) = crate::services::tests::client(test_db.clone(), guest.character).await;
        let system = a.sde.map.all_celestials().find_map(|body| body.solar_system_id).unwrap();
        let admin = group(&mut a, "admin").await;
        let manage = group(&mut a, "manage").await;
        let use_group = group(&mut a, "use").await;
        let view = group(&mut a, "view").await;
        let shared = folder(&mut a, [Some(admin), Some(manage), Some(use_group), Some(view)]).await;
        let id = mutations::add(&a, owner.character, shared, None, None, None, system, [1.0, 2.0, 3.0], "place", "", None).await.unwrap().items().unwrap()[0].as_int().unwrap();
        assert!(navigation::resolve_for_use(&b, id).await.is_err());
        grant(&mut a, view, guest.character).await;
        assert!(navigation::resolve_for_use(&b, id).await.is_err());
        for group in [use_group, manage, admin] {
            grant(&mut a, group, guest.character).await;
            let destination = navigation::resolve_for_use(&b, id).await.unwrap();
            assert_eq!((destination.system, destination.pos, destination.bookmark_id), (system, [1.0, 2.0, 3.0], id));
        }
        assert!(navigation::resolve_for_use(&b, i64::MAX).await.is_err());
        let args = [Value::Bool(true), Value::unicode("personal"), Value::unicode(""), Value::None, Value::None, Value::None, Value::None];
        let row = rpc(&mut a, "accessGroupBookmarkMgr", "AddFolder", &args).await.unwrap();
        let personal = crate::services::value_field(&row, "folderID").unwrap().as_int().unwrap();
        let own = mutations::add(&a, owner.character, personal, None, None, None, system, [4.0, 5.0, 6.0], "own", "", None).await.unwrap().items().unwrap()[0].as_int().unwrap();
        assert!(navigation::resolve_for_use(&a, own).await.is_ok());
        assert!(navigation::resolve_for_use(&b, own).await.is_err());
        test_db.execute("UPDATE game.bookmarks SET expiry = $2 WHERE bookmark_id = $1", &[&own, &filetime_now()]).await.unwrap();
        assert!(navigation::resolve_for_use(&a, own).await.is_err());
        test_db.execute("UPDATE game.bookmarks SET expiry = NULL, x = $2 WHERE bookmark_id = $1", &[&own, &f64::NAN]).await.unwrap();
        assert!(navigation::resolve_for_use(&a, own).await.is_err());
        test_db.execute("UPDATE game.bookmarks SET x = 0, location_id = 0 WHERE bookmark_id = $1", &[&own]).await.unwrap();
        assert!(navigation::resolve_for_use(&a, own).await.is_err());
    }).await;
    cleanup(&db, &[owner.character, guest.character]).await;
    for f in [owner, guest] { fixture::cleanup(&db, f).await.unwrap(); }
    result.unwrap();
}

#[tokio::test]
#[ignore = "requires dedicated INFINITY_TEST_DATABASE_URL with imported SDE"]
async fn beyonce_bookmark_routes_create_once_and_validate_scan_results() {
    crate::tests::init_config();
    let db = Db::connect(&crate::config::tests::test_database()).await.unwrap();
    let owner = fixture::create(&db).await.unwrap();
    let test_db = db.clone();
    let result = tokio::spawn(async move {
        let (mut client, _connection) = crate::services::tests::client(test_db.clone(), owner.character).await;
        let args = [Value::Bool(true), Value::unicode("personal"), Value::unicode(""), Value::None, Value::None, Value::None, Value::None];
        let row = rpc(&mut client, "accessGroupBookmarkMgr", "AddFolder", &args).await.unwrap();
        let folder = crate::services::value_field(&row, "folderID").unwrap().as_int().unwrap();
        let celestial = client.sde.map.all_celestials().find(|body| body.solar_system_id.is_some() && body.x.is_some() && body.y.is_some() && body.z.is_some()).unwrap();
        let system = celestial.solar_system_id.unwrap();
        let item = celestial.item_id;
        client.world.system(system).await.unwrap();
        client.session.apply(vec![("solarsystemid", Value::Int(system)), ("solarsystemid2", Value::Int(system))]);
        let result = rpc(&mut client, "beyonce", "BookmarkLocation", &[Value::Int(item), Value::Int(folder), Value::unicode("position"), Value::unicode(""), Value::None]).await.unwrap();
        let rows = dao::bookmarks(&test_db, &[folder], i64::MIN).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(result.items().unwrap()[0].as_int(), Some(rows[0].bookmark_id));
        assert!(rpc(&mut client, "beyonce", "BookmarkScanResult", &[Value::Int(system), Value::unicode("missing"), Value::unicode(""), Value::str("missing"), Value::Int(folder), Value::None]).await.is_err());
        assert_eq!(dao::bookmarks(&test_db, &[folder], i64::MIN).await.unwrap().len(), 1);
    }).await;
    cleanup(&db, &[owner.character]).await;
    fixture::cleanup(&db, owner).await.unwrap();
    result.unwrap();
}
