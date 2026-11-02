//! Persistence for redeeming and store purchases used by userSvc and storeManager.
//! Uses game accounts, characters, items, redeem tokens and store purchases; delivery policy stays in services.
//! Account PLEX storage is exact numeric; returned client balances are clamped.
//! No direct client mirror; delivery and transaction policy remain with the services.
use crate::consts::CLIENT_MAX_PLEX;
use crate::dao::accounts::balances::Balance;
use crate::db::{executor::Executor, Error};

pub(crate) struct Token { pub token_id: i64, pub type_id: i32, pub quantity: i32, pub label: String, pub description: String, pub created: i64 }
impl Token { fn from_row(r: &tokio_postgres::Row) -> Self { Self { token_id:r.get("token_id"), type_id:r.get("type_id"), quantity:r.get("quantity"), label:r.get("label"), description:r.get("description"), created:r.get("created") } } }
pub(crate) struct Stations { pub home_station_id: Option<i64>, pub station_id: Option<i64> }
impl Stations { fn from_row(r: &tokio_postgres::Row) -> Self { Self { home_station_id:r.get("home_station_id"), station_id:r.get("station_id") } } }
struct User { user_id:i64 }
impl User { fn from_row(r:&tokio_postgres::Row)->Self { Self { user_id:r.get("user_id") } } }
struct Points { free_skill_points:i64 }
impl Points { fn from_row(r:&tokio_postgres::Row)->Self { Self { free_skill_points:r.get("free_skill_points") } } }
pub(crate) async fn grant(ex:&impl Executor, user:i64, type_id:i32, quantity:i32, label:&str, created:i64)->Result<u64,Error> { ex.execute("INSERT INTO game.redeem_tokens (user_id, type_id, quantity, label, created) VALUES ($1, $2, $3, $4, $5)",&[&user,&type_id,&quantity,&label,&created]).await }
pub(crate) async fn tokens(ex:&impl Executor,user:i64)->Result<Vec<Token>,Error> { Ok(ex.query("SELECT token_id, type_id, quantity, label, description, created FROM game.redeem_tokens WHERE user_id = $1 ORDER BY token_id",&[&user]).await?.iter().map(Token::from_row).collect()) }
pub(crate) async fn stations(ex:&impl Executor,char_id:i64,user:i64)->Result<Option<Stations>,Error> { Ok(ex.query_opt("SELECT home_station_id, station_id FROM game.characters WHERE character_id = $1 AND user_id = $2",&[&char_id,&user]).await?.map(|r| Stations::from_row(&r))) }
pub(crate) async fn lock_tokens(ex:&impl Executor,user:i64,ids:&[i64])->Result<Vec<Token>,Error> { Ok(ex.query("SELECT token_id, type_id, quantity, label, description, created FROM game.redeem_tokens WHERE user_id = $1 AND token_id = ANY($2) ORDER BY token_id FOR UPDATE",&[&user,&ids]).await?.iter().map(Token::from_row).collect()) }
pub(crate) async fn trash(ex:&impl Executor,user:i64,ids:&[i64])->Result<u64,Error> { ex.execute("DELETE FROM game.redeem_tokens WHERE user_id = $1 AND token_id = ANY($2)",&[&user,&ids]).await }
pub(crate) async fn add_points(ex:&impl Executor,char_id:i64,points:i64)->Result<i64,Error> { Ok(Points::from_row(&ex.query_one("UPDATE game.characters SET free_skill_points = free_skill_points + $2 WHERE character_id = $1 RETURNING free_skill_points",&[&char_id,&points]).await?).free_skill_points) }
pub(crate) async fn recipient(ex:&impl Executor,char_id:i64)->Result<Option<i64>,Error> { Ok(ex.query_opt("SELECT user_id FROM game.characters WHERE character_id = $1",&[&char_id]).await?.map(|r|User::from_row(&r).user_id)) }
pub(crate) async fn debit(ex:&impl Executor,user:i64,cost:i64)->Result<Option<i64>,Error> { Ok(ex.query_opt("UPDATE game.accounts SET plex_balance = plex_balance - $2::bigint WHERE user_id = $1 AND plex_balance >= $2::bigint RETURNING LEAST(plex_balance, $3::bigint)::bigint AS plex_balance",&[&user,&cost,&CLIENT_MAX_PLEX]).await?.map(|r|Balance::from_row(&r).plex_balance)) }
pub(crate) async fn purchase(ex:&impl Executor,user:i64,offer:i32,quantity:i32,price:i64)->Result<u64,Error> { ex.execute("INSERT INTO game.store_purchases (user_id, offer_id, quantity, price) VALUES ($1, $2, $3, $4)",&[&user,&offer,&quantity,&price]).await }
pub(crate) async fn free_purchase(ex:&impl Executor,user:i64,offer:i32)->Result<u64,Error> { ex.execute("INSERT INTO game.store_purchases (user_id, offer_id, quantity, price) VALUES ($1, $2, 1, 0)",&[&user,&offer]).await }
