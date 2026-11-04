//! Isolated characters and live game clients for chat socket tests.
use std::sync::Arc;
use crate::{client::Client, db::Db, chat::Hub, sde::Sde, space::World};
use tokio::net::{TcpListener, TcpStream};

#[derive(Clone, Copy)]
pub(super) struct Character { pub account: i64, pub id: i64 }

pub(super) async fn create(db: &Db) -> Character {
    db.transaction(|tx, _: &mut ()| Box::pin(async move {
        let account = tx.query_one("INSERT INTO game.accounts (user_name, password_hash, role) VALUES ('chat-test-' || nextval('game.accounts_user_id_seq')::text, $1, 0) RETURNING user_id", &[&&b"fixture"[..]]).await?.get("user_id");
        let id = tx.query_one("INSERT INTO game.characters (user_id, name, race_id, bloodline_id, gender_id, corporation_id) VALUES ($1, 'chat-test-' || nextval('game.characters_character_id_seq')::text, 1, 1, 1, 1) RETURNING character_id", &[&account]).await?.get("character_id");
        Ok::<_, crate::db::Error>(Character { account, id })
    }), |v, ()| async move { Ok(v) }).await.unwrap()
}

pub(super) async fn organization(db: &Db, character: i64) -> (i64, i64) {
    let corp: i64 = db.query_one("INSERT INTO game.corporations (name,ticker,ceo_id,creator_id,station_id,created) VALUES ('chat-org-' || nextval('game.corporations_corporation_id_seq')::text, 'CO' || currval('game.corporations_corporation_id_seq')::text, $1,$1,1,1) RETURNING corporation_id", &[&character]).await.unwrap().get("corporation_id");
    let alliance: i64 = db.query_one("INSERT INTO game.alliances (name,short_name,executor_corp_id,creator_corp_id,creator_char_id,created) VALUES ('chat-org-' || nextval('game.alliances_alliance_id_seq')::text, 'CO' || currval('game.alliances_alliance_id_seq')::text, $1,$1,$2,1) RETURNING alliance_id", &[&corp, &character]).await.unwrap().get("alliance_id");
    db.execute("UPDATE game.corporations SET alliance_id=$1 WHERE corporation_id=$2", &[&alliance, &corp]).await.unwrap();
    db.execute("UPDATE game.characters SET corporation_id=$1 WHERE character_id=$2", &[&corp, &character]).await.unwrap();
    (corp, alliance)
}

pub(super) async fn cleanup_organization(db: &Db, corp: i64, alliance: i64) {
    db.execute("UPDATE game.characters SET corporation_id=1 WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("UPDATE game.corporations SET alliance_id=NULL WHERE corporation_id=$1", &[&corp]).await.unwrap();
    db.execute("DELETE FROM game.alliances WHERE alliance_id=$1", &[&alliance]).await.unwrap();
    db.execute("DELETE FROM game.corporations WHERE corporation_id=$1", &[&corp]).await.unwrap();
}

pub(super) async fn cleanup(db: &Db, chars: &[Character]) {
    let ids: Vec<i64> = chars.iter().map(|c| c.id).collect();
    db.execute("DELETE FROM game.chat_channels WHERE owner_id = ANY($1)", &[&ids]).await.unwrap();
    for c in chars {
        db.execute("DELETE FROM game.characters WHERE character_id = $1", &[&c.id]).await.unwrap();
        db.execute("DELETE FROM game.accounts WHERE user_id = $1", &[&c.account]).await.unwrap();
    }
}

pub(super) async fn client(db: Db, sde: Arc<Sde>, world: Arc<World>, hub: Arc<Hub>) -> (Client, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let peer = TcpStream::connect(listener.local_addr().unwrap()).await.unwrap();
    let (stream, address) = listener.accept().await.unwrap();
    let cfg = Arc::new(crate::config::Config::default());
    let login = crate::handshake::Login { user_id: 1, user_name: "test".into(), role: 0, client_id: 1, session_id: 1, language_id: "EN".into() };
    (Client::new(crate::conn::Conn::new(stream, address), cfg, db, sde, hub, world, login), peer)
}
