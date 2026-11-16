//! Atomic game and SDE database operations with bounded conflict retries.
//! This server-side facility has no client counterpart; it does not retry individual statements.
//! If rollback fails, the original operation error is returned without retrying
//! because the transaction's outcome is uncertain; the rollback failure is logged.
//! Managed transaction statements use the pooled connection's prepared-statement cache.
//! Checkout, statement, commit and managed transaction durations are sampled
//! for server diagnostics; one finished operation is one sample and timing
//! never changes a result, an error or the retry behaviour.
//! Commit durability defaults to the connection policy; loss-tolerant commits are an explicit opt-in.

use std::{future::Future, panic::Location, pin::Pin};
use tokio::time::Instant;
use tokio_postgres::{error::SqlState, types::ToSql, Row};
use super::{timing::{self, Kind}, Db, Error, Transaction};

/// Pooled transaction with cached statement methods and explicit uncached execution;
/// `Some((caller, started))` marks the managed scope its commit or rollback records.
pub struct CachedTransaction<'a>(pub(super) deadpool_postgres::Transaction<'a>, Option<(&'static Location<'static>, Instant)>);

impl<'a> From<deadpool_postgres::Transaction<'a>> for CachedTransaction<'a> {
    fn from(tx: deadpool_postgres::Transaction<'a>) -> Self { Self(tx, None) }
}

impl<'a> CachedTransaction<'a> {
    /// One statement sample covers `prepare_cached` plus the statement itself;
    /// a prepare or statement error still completes the sample.
    #[track_caller]
    pub fn query<'s, 'b, 'c, 'd>(&'s self, sql: &'b str, params: &'c [&'d (dyn ToSql + Sync)])
        -> impl Future<Output = Result<Vec<Row>, tokio_postgres::Error>> + Send + use<'a, 's, 'b, 'c, 'd>
    {
        timing::measure_at(Kind::Statement, Location::caller(), async move {
            let statement = self.0.prepare_cached(sql).await?;
            self.0.query(&statement, params).await
        })
    }
    #[track_caller]
    pub fn query_one<'s, 'b, 'c, 'd>(&'s self, sql: &'b str, params: &'c [&'d (dyn ToSql + Sync)])
        -> impl Future<Output = Result<Row, tokio_postgres::Error>> + Send + use<'a, 's, 'b, 'c, 'd>
    {
        timing::measure_at(Kind::Statement, Location::caller(), async move {
            let statement = self.0.prepare_cached(sql).await?;
            self.0.query_one(&statement, params).await
        })
    }
    #[track_caller]
    pub fn query_opt<'s, 'b, 'c, 'd>(&'s self, sql: &'b str, params: &'c [&'d (dyn ToSql + Sync)])
        -> impl Future<Output = Result<Option<Row>, tokio_postgres::Error>> + Send + use<'a, 's, 'b, 'c, 'd>
    {
        timing::measure_at(Kind::Statement, Location::caller(), async move {
            let statement = self.0.prepare_cached(sql).await?;
            self.0.query_opt(&statement, params).await
        })
    }
    #[track_caller]
    pub fn execute<'s, 'b, 'c, 'd>(&'s self, sql: &'b str, params: &'c [&'d (dyn ToSql + Sync)])
        -> impl Future<Output = Result<u64, tokio_postgres::Error>> + Send + use<'a, 's, 'b, 'c, 'd>
    {
        timing::measure_at(Kind::Statement, Location::caller(), async move {
            let statement = self.0.prepare_cached(sql).await?;
            self.0.execute(&statement, params).await
        })
    }
    /// A batch runs raw but still counts as one statement sample.
    #[track_caller]
    pub fn batch_execute<'s, 'b>(&'s self, sql: &'b str)
        -> impl Future<Output = Result<(), tokio_postgres::Error>> + Send + use<'a, 's, 'b>
    {
        timing::measure_at(Kind::Statement, Location::caller(), async move { self.0.batch_execute(sql).await })
    }
    /// An explicit commit times only itself; a managed transaction also records
    /// the whole scope opened before BEGIN against the same end instant.
    #[track_caller]
    pub fn commit(self) -> impl Future<Output = Result<(), tokio_postgres::Error>> + Send + use<'a> {
        let caller = Location::caller();
        async move {
            let commit_start = Instant::now();
            let result = self.0.commit().await;
            let end = Instant::now();
            match self.1 {
                Some((managed, started)) =>
                    timing::record_commit_transaction(managed, end.duration_since(commit_start), end.duration_since(started)),
                None => timing::record(Kind::Commit, caller, end.duration_since(commit_start)),
            }
            result
        }
    }
    pub async fn rollback(self) -> Result<(), tokio_postgres::Error> {
        let result = self.0.rollback().await;
        if let Some((caller, started)) = self.1 {
            timing::record(Kind::Transaction, caller, started.elapsed());
        }
        result
    }

    /// Ends a managed transaction by COMMIT: `commit` records the commit sample
    /// and the whole scope opened at `started` under `caller` together.
    pub(super) fn commit_with_transaction_at(
        mut self,
        caller: &'static Location<'static>,
        started: Instant,
    ) -> impl Future<Output = Result<(), tokio_postgres::Error>> + Send + use<'a> {
        self.1 = Some((caller, started));
        self.commit()
    }
    /// Ends a managed transaction by ROLLBACK: `rollback` records the whole
    /// scope opened at `started` under `caller`, success or failure alike.
    pub(super) fn rollback_with_transaction_at(
        mut self,
        caller: &'static Location<'static>,
        started: Instant,
    ) -> impl Future<Output = Result<(), tokio_postgres::Error>> + Send + use<'a> {
        self.1 = Some((caller, started));
        self.rollback()
    }
}


/// Server design: retry one complete transaction after a serialization conflict or deadlock.
const MAX_TRANSACTION_RETRIES: usize = 1;

pub type TxFuture<'a, T, E> = Pin<Box<dyn Future<Output = Result<T, E>> + Send + 'a>>;

pub trait TransactionError: From<Error> {
    fn is_retryable_transaction(&self) -> bool;
}

pub(crate) fn retryable_sqlstate(code: Option<&SqlState>) -> bool {
    code.is_some_and(|code| *code == SqlState::T_R_DEADLOCK_DETECTED || *code == SqlState::T_R_SERIALIZATION_FAILURE)
}

pub(crate) fn retryable_postgres(error: &tokio_postgres::Error) -> bool {
    retryable_sqlstate(error.code())
}

impl TransactionError for Error {
    fn is_retryable_transaction(&self) -> bool {
        match self {
            Error::Postgres(error) => retryable_postgres(error),
            Error::Pool(_) | Error::Progress(_) | Error::InvalidItemLockRequest(_) => false,
            Error::StaleSnapshot(_) => true,
        }
    }
}

/// Commit durability chosen per managed transaction: `Default` keeps the
/// connection's own policy; `LossTolerant` opts one transaction into
/// `SET LOCAL synchronous_commit = off` when the pool allows it.
#[derive(Clone, Copy)]
pub(crate) enum CommitMode {
    Default,
    LossTolerant,
}

impl Db {
    #[track_caller]
    pub fn transaction<'s, T, R, E, O, F, D, DFut>(
        &'s self,
        operation: F,
        dispatch: D,
    ) -> impl Future<Output = Result<R, E>> + Send + use<'s, T, R, E, O, F, D, DFut>
    where
        T: Send,
        R: Send,
        E: TransactionError + Send,
        O: Default + crate::progress::Prepare + crate::progress::AfterCommit,
        F: for<'call, 'tx> FnMut(&'call Transaction<'tx>, &'call mut O) -> TxFuture<'call, T, E> + Send,
        D: FnOnce(T, O) -> DFut + Send,
        DFut: Future<Output = Result<R, E>> + Send,
    {
        self.transaction_with_commit(CommitMode::Default, operation, dispatch)
    }

    /// The single bounded-retry runner behind `transaction`; a `LossTolerant`
    /// attempt applies `SET LOCAL synchronous_commit = off` right after BEGIN
    /// when the pool allows it, and its failure joins the operation error path.
    #[track_caller]
    pub(crate) fn transaction_with_commit<'s, T, R, E, O, F, D, DFut>(
        &'s self,
        mode: CommitMode,
        mut operation: F,
        dispatch: D,
    ) -> impl Future<Output = Result<R, E>> + Send + use<'s, T, R, E, O, F, D, DFut>
    where
        T: Send,
        R: Send,
        E: TransactionError + Send,
        O: Default + crate::progress::Prepare + crate::progress::AfterCommit,
        F: for<'call, 'tx> FnMut(&'call Transaction<'tx>, &'call mut O) -> TxFuture<'call, T, E> + Send,
        D: FnOnce(T, O) -> DFut + Send,
        DFut: Future<Output = Result<R, E>> + Send,
    {
        let caller = Location::caller();
        async move {
            let invocation = (u128::from(crate::handshake::random_u64()) << u64::BITS) | u128::from(crate::handshake::random_u64());
            let mut retries = 0;
            let (value, mut outbox, inserted) = loop {
                let mut connection = timing::measure_at(Kind::Checkout, caller, self.pool.get())
                    .await.map_err(Error::from).map_err(E::from)?;
                let started = Instant::now();
                let tx = Transaction::from(connection.transaction().await.map_err(Error::from).map_err(E::from)?);
                let mut outbox = O::default();
                let value = match async {
                    if matches!(mode, CommitMode::LossTolerant) && self.loss_tolerant_async_commit {
                        tx.batch_execute("SET LOCAL synchronous_commit = off")
                            .await.map_err(Error::from).map_err(E::from)?;
                    }
                    operation(&tx, &mut outbox).await
                }.await {
                    Ok(value) => value,
                    Err(error) => {
                        if let Err(rollback_error) = tx.rollback_with_transaction_at(caller, started).await {
                            tracing::warn!("transaction rollback failed: {rollback_error}");
                            return Err(error);
                        }
                        if retries < MAX_TRANSACTION_RETRIES && error.is_retryable_transaction() {
                            retries += 1;
                            continue;
                        }
                        return Err(error);
                    }
                };
                let inserted = match crate::progress::prepare_batch(&mut outbox, &tx, invocation, self.progress_registry()).await {
                    Ok(inserted) => inserted,
                    Err(error) => {
                        let error = E::from(error);
                        if let Err(rollback_error) = tx.rollback_with_transaction_at(caller, started).await {
                            tracing::warn!("transaction rollback failed: {rollback_error}");
                            return Err(error);
                        }
                        if retries < MAX_TRANSACTION_RETRIES && error.is_retryable_transaction() {
                            retries += 1;
                            continue;
                        }
                        return Err(error);
                    }
                };
                match tx.commit_with_transaction_at(caller, started).await {
                    Ok(()) => {
                        drop(connection);
                        break (value, outbox, inserted);
                    }
                    Err(error) => {
                        let error = E::from(Error::from(error));
                        if retries < MAX_TRANSACTION_RETRIES && error.is_retryable_transaction() {
                            retries += 1;
                            continue;
                        }
                        return Err(error);
                    }
                }
            };
            let publication = outbox.after_commit(self.progress_registry(), &inserted);
            let result = dispatch(value, outbox).await;
            publication.publish();
            result
        }
    }
}
#[cfg(all(test, feature = "tests-storage"))] mod tests;
#[cfg(all(test, any(feature = "tests-economy", feature = "tests-space")))]
mod fixtures;
