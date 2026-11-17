//! Nested transactions used by database fixtures; no additional connection or cache.
use super::CachedTransaction;
impl CachedTransaction<'_> {
    #[cfg(feature = "tests-space")]
    pub(crate) async fn transaction(&mut self) -> Result<CachedTransaction<'_>, tokio_postgres::Error> {
        Ok(self.0.transaction().await?.into())
    }
    #[cfg(feature = "tests-economy")]
    pub(crate) async fn savepoint(&mut self, name: &str) -> Result<CachedTransaction<'_>, tokio_postgres::Error> {
        Ok(self.0.savepoint(name).await?.into())
    }
}
