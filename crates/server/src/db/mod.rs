//! PostgreSQL access for server services using game and SDE tables.
//! The bounded NoTls pool uses Verified recycling to reject terminated backends
//! before a statement; Fast can race the connection task and return a termination error.
//! Failed statements are not retried. Startup schema application can be disabled;
//! each application is recorded and a recorded-then-superseded schema is refused.
//! Each pooled connection caches prepared statements; schema batches stay raw.
//! Finished checkouts, statements, transactions and commits are timed per call
//! site; a cancelled or unfinished operation records no sample.
//! Account and market persistence is delegated to the DAO; login and startup policy remain here.
//! Unknown accounts are registered only when enabled by the development policy.
//! Startup reserves a server-owned simulated-pilot account; that identity cannot log in.

pub mod executor;
pub mod outbox;
mod schema;
pub(crate) mod timing;
pub mod transaction;

/// Transaction type used for atomic database operations.
pub type Transaction<'a> = transaction::CachedTransaction<'a>;

use crate::config::deployment::{DatabaseConfig, DevelopmentConfig};
use infinity_core::consts::{GM_ROLES, ROLE_PLAYER_LOGIN, ROLE_SERVICE};
use crate::dao::{accounts::{authentication, simulated}, market::startup};
use anyhow::{Context, Result};
use std::sync::atomic::{AtomicI64, Ordering};
use std::{future::Future, panic::Location, sync::Arc};
use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod};
use tokio_postgres::{types::ToSql, NoTls, Row};
use tracing::info;

const GAME_SCHEMA: &str = include_str!("../../sql/game_schema.sql");

/// Existing server policy for exponential database reconnect backoff.
const RECONNECT_BACKOFF_FACTOR: u64 = 2;

#[derive(Debug)]
pub enum Error {
    Pool(deadpool_postgres::PoolError),
    Postgres(tokio_postgres::Error),
    Progress(anyhow::Error),
    InvalidItemLockRequest(&'static str),
    /// An observed pre-lock snapshot changed before the write could settle.
    StaleSnapshot(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pool(e) => write!(f, "database pool: {e}"),
            Self::Progress(e) => write!(f, "progress: {e}"),
            Self::InvalidItemLockRequest(reason) => write!(f, "invalid item lock request: {reason}"),
            Self::StaleSnapshot(reason) => write!(f, "stale operation snapshot: {reason}"),
            Self::Postgres(e) => write!(f, "PostgreSQL: {e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Pool(e) => Some(e),
            Self::Progress(e) => Some(e.as_ref()),
            Self::InvalidItemLockRequest(_) | Self::StaleSnapshot(_) => None,
            Self::Postgres(e) => Some(e),
        }
    }
}

impl From<deadpool_postgres::PoolError> for Error {
    fn from(error: deadpool_postgres::PoolError) -> Self { Self::Pool(error) }
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self { Self::Postgres(error) }
}

#[derive(Clone)]
pub struct Db {
    pool: Pool,
    online: Arc<AtomicI64>,
    pub(crate) progress_registry: Arc<crate::progress::Registry>,
    loss_tolerant_async_commit: bool,
}


pub struct Account {
    pub user_id: i64,
    pub role: i64,
    pub user_type: i64,
}

impl Db {
    pub(crate) fn with_progress_registry(mut self, registry: crate::progress::Registry) -> Self {
        self.progress_registry = Arc::new(registry);
        self
    }

    pub(crate) fn progress_registry(&self) -> &crate::progress::Registry { &self.progress_registry }

    #[track_caller]
    pub fn query<'s, 'sql, 'params, 'pi>(&'s self, sql: &'sql str, params: &'params [&'pi (dyn ToSql + Sync)]) -> impl Future<Output = std::result::Result<Vec<Row>, Error>> + Send + use<'s, 'sql, 'params, 'pi> {
        let caller = Location::caller();
        async move {
            let client = timing::measure_at(timing::Kind::Checkout, caller, self.pool.get()).await?;
            Ok(timing::measure_at(timing::Kind::Statement, caller, async move {
                let statement = client.prepare_cached(sql).await?;
                client.query(&statement, params).await
            })
            .await?)
        }
    }

    #[track_caller]
    pub fn query_one<'s, 'sql, 'params, 'pi>(&'s self, sql: &'sql str, params: &'params [&'pi (dyn ToSql + Sync)]) -> impl Future<Output = std::result::Result<Row, Error>> + Send + use<'s, 'sql, 'params, 'pi> {
        let caller = Location::caller();
        async move {
            let client = timing::measure_at(timing::Kind::Checkout, caller, self.pool.get()).await?;
            Ok(timing::measure_at(timing::Kind::Statement, caller, async move {
                let statement = client.prepare_cached(sql).await?;
                client.query_one(&statement, params).await
            })
            .await?)
        }
    }

    #[track_caller]
    pub fn query_opt<'s, 'sql, 'params, 'pi>(&'s self, sql: &'sql str, params: &'params [&'pi (dyn ToSql + Sync)]) -> impl Future<Output = std::result::Result<Option<Row>, Error>> + Send + use<'s, 'sql, 'params, 'pi> {
        let caller = Location::caller();
        async move {
            let client = timing::measure_at(timing::Kind::Checkout, caller, self.pool.get()).await?;
            Ok(timing::measure_at(timing::Kind::Statement, caller, async move {
                let statement = client.prepare_cached(sql).await?;
                client.query_opt(&statement, params).await
            })
            .await?)
        }
    }

    #[track_caller]
    pub fn execute<'s, 'sql, 'params, 'pi>(&'s self, sql: &'sql str, params: &'params [&'pi (dyn ToSql + Sync)]) -> impl Future<Output = std::result::Result<u64, Error>> + Send + use<'s, 'sql, 'params, 'pi> {
        let caller = Location::caller();
        async move {
            let client = timing::measure_at(timing::Kind::Checkout, caller, self.pool.get()).await?;
            Ok(timing::measure_at(timing::Kind::Statement, caller, async move {
                let statement = client.prepare_cached(sql).await?;
                client.execute(&statement, params).await
            })
            .await?)
        }
    }


    pub async fn connect(cfg: &DatabaseConfig) -> Result<Db> {
        let config = cfg.to_pg_config()?;
        info!(hosts = ?config.get_hosts(), ports = ?config.get_ports(), user = ?config.get_user(), dbname = ?config.get_dbname(), pool_size = cfg.pool_size, "connecting to PostgreSQL");
        let manager = Manager::from_config(config, NoTls, ManagerConfig { recycling_method: RecyclingMethod::Verified });
        let wait_timeout = (cfg.checkout_timeout_secs != 0).then(|| std::time::Duration::from_secs(cfg.checkout_timeout_secs));
        let pool = Pool::builder(manager).max_size(cfg.pool_size).wait_timeout(wait_timeout).runtime(deadpool_postgres::Runtime::Tokio1).build().context("building PostgreSQL pool")?;
        let mut delay = Some(cfg.startup_retry_secs[0]);
        let mut client = loop {
            match timing::measure(timing::Kind::Checkout, pool.get()).await {
                Ok(c) => break c,
                Err(e) => match delay {
                    Some(seconds) if seconds <= cfg.startup_retry_secs[1] => {
                        info!("database not ready ({e}); retrying in {seconds}s");
                        tokio::time::sleep(std::time::Duration::from_secs(seconds)).await;
                        delay = seconds.checked_mul(RECONNECT_BACKOFF_FACTOR);
                    }
                    _ => return Err(e).context("connecting to PostgreSQL"),
                },
            }
        };
        // Schema, market cutover and its history commit together before any startup market writes.
        let caller = Location::caller();
        let started = tokio::time::Instant::now();
        let tx = Transaction::from(client.transaction().await.context("starting schema transaction")?);
        schema::apply(&tx, GAME_SCHEMA, cfg, crate::build_identity::commit()).await.context("applying game schema")?;
        tx.commit_with_transaction_at(caller, started).await.context("committing schema transaction")?;
        drop(client);
        let db = Db { pool, online: Arc::new(AtomicI64::new(0)), progress_registry: Arc::new(crate::progress::Registry::default()), loss_tolerant_async_commit: cfg.loss_tolerant_async_commit };
        if !simulated::ensure(&db).await.context("ensuring simulated-pilot account")? {
            anyhow::bail!("reserved account {:?} is not a valid server-owned account", simulated::ACCOUNT_NAME);
        }
        load_retail_prices(&db).await.context("loading retail prices")?;
        let seeded = startup::seeded(&db).await?;
        if !seeded {
            let n = startup::seed(&db).await.context("seeding the market")?;
            info!("seeded {n} NPC market orders");
        }
        // NPC demand: a region-wide buy order beside every NPC sell order, so loot and ore
        // always sell somewhere.
        let bids_seeded = startup::bids_seeded(&db).await?;
        if !bids_seeded {
            let n = startup::seed_bids(&db).await.context("seeding NPC buy orders")?;
            info!("seeded {n} NPC buy orders");
        }
        // NPC orders follow the retail price list (seeded before it was available, or updated since).
        let n = startup::reprice(&db).await?;
        if n > 0 {
            info!("repriced {n} NPC market orders from retail prices");
        }
        Ok(db)
    }

    pub fn online_count(&self) -> i64 {
        self.online.load(Ordering::Relaxed)
    }
    pub fn add_online(&self, delta: i64) {
        self.online.fetch_add(delta, Ordering::Relaxed);
    }

    /// Verify credentials, optionally registering unknown user names on first login.
    pub async fn login(&self, user_name: &str, password_hash: &[u8], policy: &DevelopmentConfig) -> Result<Option<Account>> {
        let row = authentication::find(self, user_name).await?;
        let (user_id, role, user_type) = match row {
            Some(r) => {
                if r.role & ROLE_SERVICE != 0 || r.password_hash != password_hash {
                    return Ok(None);
                }
                (r.user_id, r.role, r.user_type)
            }
            None => {
                if !policy.auto_register_accounts {
                    return Ok(None);
                }
                let role = ROLE_PLAYER_LOGIN | if policy.auto_register_role == "gm" { GM_ROLES } else { 0 };
                let r = authentication::register(self, user_name, password_hash, role, crate::config::game().accounts.starting_plex).await?;
                info!("registered new account {user_name:?}");
                (r.user_id, role, r.user_type)
            }
        };
        authentication::last_login(self, user_id).await?;
        Ok(Some(Account { user_id, role, user_type: user_type as i64 }))
    }
}

/// Replace `game.retail_prices` with the captured retail price list.
async fn load_retail_prices(db: &Db) -> Result<()> {
    let prices = infinity_content::retail::average_prices();
    let (ids, (avg, adj)): (Vec<i32>, (Vec<f64>, Vec<f64>)) = prices.into_iter().map(|(t, a, j)| (t, (a, j))).unzip();
    startup::retail_prices(db, &ids, &avg, &adj).await?;
    info!("{} retail market prices loaded", ids.len());
    Ok(())
}

#[cfg(all(test, any(feature = "tests-runtime", feature = "tests-storage", feature = "tests-space")))] mod tests;
