//! Generic statement execution against the game and SDE PostgreSQL tables.
//! This server-side interface does not mirror a client module or manage transactions.
//! Db and managed transactions cache statements; native transactions and uncached views do not.

use std::{future::Future, panic::Location};
use tokio_postgres::{types::ToSql, Row};
use super::{timing, Db, Error, Transaction};

pub trait Executor: Sync {
    #[track_caller]
    fn query(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> impl Future<Output = Result<Vec<Row>, Error>> + Send;
    #[track_caller]
    fn query_one(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> impl Future<Output = Result<Row, Error>> + Send;
    #[track_caller]
    fn query_opt(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> impl Future<Output = Result<Option<Row>, Error>> + Send;
    #[track_caller]
    fn execute(&self, sql: &str, params: &[&(dyn ToSql + Sync)]) -> impl Future<Output = Result<u64, Error>> + Send;
    /// Borrow an executor that bypasses statement caching.
    fn uncached(&self) -> impl Executor + Send + '_;
}

impl Executor for Db {
    #[track_caller]
    fn query<'s, 'sql, 'params, 'pi>(&'s self, sql: &'sql str, params: &'params [&'pi (dyn ToSql + Sync)]) -> impl Future<Output = Result<Vec<Row>, Error>> + Send + use<'s, 'sql, 'params, 'pi> {
        Db::query(self, sql, params)
    }
    #[track_caller]
    fn query_one<'s, 'sql, 'params, 'pi>(&'s self, sql: &'sql str, params: &'params [&'pi (dyn ToSql + Sync)]) -> impl Future<Output = Result<Row, Error>> + Send + use<'s, 'sql, 'params, 'pi> {
        Db::query_one(self, sql, params)
    }
    #[track_caller]
    fn query_opt<'s, 'sql, 'params, 'pi>(&'s self, sql: &'sql str, params: &'params [&'pi (dyn ToSql + Sync)]) -> impl Future<Output = Result<Option<Row>, Error>> + Send + use<'s, 'sql, 'params, 'pi> {
        Db::query_opt(self, sql, params)
    }
    #[track_caller]
    fn execute<'s, 'sql, 'params, 'pi>(&'s self, sql: &'sql str, params: &'params [&'pi (dyn ToSql + Sync)]) -> impl Future<Output = Result<u64, Error>> + Send + use<'s, 'sql, 'params, 'pi> {
        Db::execute(self, sql, params)
    }
    fn uncached(&self) -> impl Executor + Send + '_ { Uncached::Db(self) }
}

impl<'a> Executor for Transaction<'a> {
    #[track_caller]
    fn query<'s, 'sql, 'params, 'pi>(&'s self, sql: &'sql str, params: &'params [&'pi (dyn ToSql + Sync)]) -> impl Future<Output = Result<Vec<Row>, Error>> + Send + use<'s, 'sql, 'params, 'pi, 'a> {
        let query = Transaction::query(self, sql, params);
        async move { query.await.map_err(Error::from) }
    }
    #[track_caller]
    fn query_one<'s, 'sql, 'params, 'pi>(&'s self, sql: &'sql str, params: &'params [&'pi (dyn ToSql + Sync)]) -> impl Future<Output = Result<Row, Error>> + Send + use<'s, 'sql, 'params, 'pi, 'a> {
        let query = Transaction::query_one(self, sql, params);
        async move { query.await.map_err(Error::from) }
    }
    #[track_caller]
    fn query_opt<'s, 'sql, 'params, 'pi>(&'s self, sql: &'sql str, params: &'params [&'pi (dyn ToSql + Sync)]) -> impl Future<Output = Result<Option<Row>, Error>> + Send + use<'s, 'sql, 'params, 'pi, 'a> {
        let query = Transaction::query_opt(self, sql, params);
        async move { query.await.map_err(Error::from) }
    }
    #[track_caller]
    fn execute<'s, 'sql, 'params, 'pi>(&'s self, sql: &'sql str, params: &'params [&'pi (dyn ToSql + Sync)]) -> impl Future<Output = Result<u64, Error>> + Send + use<'s, 'sql, 'params, 'pi, 'a> {
        let execute = Transaction::execute(self, sql, params);
        async move { execute.await.map_err(Error::from) }
    }
    fn uncached(&self) -> impl Executor + Send + '_ { Uncached::Client(self.0.client()) }
}

impl<'a> Executor for tokio_postgres::Transaction<'a> {
    #[track_caller]
    fn query<'s, 'sql, 'params, 'pi>(&'s self, sql: &'sql str, params: &'params [&'pi (dyn ToSql + Sync)]) -> impl Future<Output = Result<Vec<Row>, Error>> + Send + use<'s, 'sql, 'params, 'pi, 'a> {
        timing::measure(timing::Kind::Statement, async move {
            tokio_postgres::Transaction::query(self, sql, params).await.map_err(Error::from)
        })
    }
    #[track_caller]
    fn query_one<'s, 'sql, 'params, 'pi>(&'s self, sql: &'sql str, params: &'params [&'pi (dyn ToSql + Sync)]) -> impl Future<Output = Result<Row, Error>> + Send + use<'s, 'sql, 'params, 'pi, 'a> {
        timing::measure(timing::Kind::Statement, async move {
            tokio_postgres::Transaction::query_one(self, sql, params).await.map_err(Error::from)
        })
    }
    #[track_caller]
    fn query_opt<'s, 'sql, 'params, 'pi>(&'s self, sql: &'sql str, params: &'params [&'pi (dyn ToSql + Sync)]) -> impl Future<Output = Result<Option<Row>, Error>> + Send + use<'s, 'sql, 'params, 'pi, 'a> {
        timing::measure(timing::Kind::Statement, async move {
            tokio_postgres::Transaction::query_opt(self, sql, params).await.map_err(Error::from)
        })
    }
    #[track_caller]
    fn execute<'s, 'sql, 'params, 'pi>(&'s self, sql: &'sql str, params: &'params [&'pi (dyn ToSql + Sync)]) -> impl Future<Output = Result<u64, Error>> + Send + use<'s, 'sql, 'params, 'pi, 'a> {
        timing::measure(timing::Kind::Statement, async move {
            tokio_postgres::Transaction::execute(self, sql, params).await.map_err(Error::from)
        })
    }
    fn uncached(&self) -> impl Executor + Send + '_ { Uncached::Client(self.client()) }
}

#[derive(Clone, Copy)]
enum Uncached<'a> {
    Db(&'a Db),
    Client(&'a tokio_postgres::Client),
}

impl<'a> Executor for Uncached<'a> {
    #[track_caller]
    fn query<'s, 'sql, 'params, 'pi>(&'s self, sql: &'sql str, params: &'params [&'pi (dyn ToSql + Sync)]) -> impl Future<Output = Result<Vec<Row>, Error>> + Send + use<'s, 'sql, 'params, 'pi, 'a> {
        let caller = Location::caller();
        async move {
            match self {
                Self::Db(db) => {
                    let client = timing::measure_at(timing::Kind::Checkout, caller, db.pool.get()).await?;
                    Ok(timing::measure_at(timing::Kind::Statement, caller, client.query(sql, params)).await?)
                }
                Self::Client(client) => Ok(timing::measure_at(timing::Kind::Statement, caller, client.query(sql, params)).await?),
            }
        }
    }
    #[track_caller]
    fn query_one<'s, 'sql, 'params, 'pi>(&'s self, sql: &'sql str, params: &'params [&'pi (dyn ToSql + Sync)]) -> impl Future<Output = Result<Row, Error>> + Send + use<'s, 'sql, 'params, 'pi, 'a> {
        let caller = Location::caller();
        async move {
            match self {
                Self::Db(db) => {
                    let client = timing::measure_at(timing::Kind::Checkout, caller, db.pool.get()).await?;
                    Ok(timing::measure_at(timing::Kind::Statement, caller, client.query_one(sql, params)).await?)
                }
                Self::Client(client) => Ok(timing::measure_at(timing::Kind::Statement, caller, client.query_one(sql, params)).await?),
            }
        }
    }
    #[track_caller]
    fn query_opt<'s, 'sql, 'params, 'pi>(&'s self, sql: &'sql str, params: &'params [&'pi (dyn ToSql + Sync)]) -> impl Future<Output = Result<Option<Row>, Error>> + Send + use<'s, 'sql, 'params, 'pi, 'a> {
        let caller = Location::caller();
        async move {
            match self {
                Self::Db(db) => {
                    let client = timing::measure_at(timing::Kind::Checkout, caller, db.pool.get()).await?;
                    Ok(timing::measure_at(timing::Kind::Statement, caller, client.query_opt(sql, params)).await?)
                }
                Self::Client(client) => Ok(timing::measure_at(timing::Kind::Statement, caller, client.query_opt(sql, params)).await?),
            }
        }
    }
    #[track_caller]
    fn execute<'s, 'sql, 'params, 'pi>(&'s self, sql: &'sql str, params: &'params [&'pi (dyn ToSql + Sync)]) -> impl Future<Output = Result<u64, Error>> + Send + use<'s, 'sql, 'params, 'pi, 'a> {
        let caller = Location::caller();
        async move {
            match self {
                Self::Db(db) => {
                    let client = timing::measure_at(timing::Kind::Checkout, caller, db.pool.get()).await?;
                    Ok(timing::measure_at(timing::Kind::Statement, caller, client.execute(sql, params)).await?)
                }
                Self::Client(client) => Ok(timing::measure_at(timing::Kind::Statement, caller, client.execute(sql, params)).await?),
            }
        }
    }
    fn uncached(&self) -> impl Executor + Send + '_ { *self }
}
