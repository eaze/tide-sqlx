use async_std::sync::RwLockWriteGuard;
use tide::utils::async_trait;
use tide::Request;

use sqlx::postgres::Postgres;

#[cfg(all(feature = "tracing", debug_assertions))]
use tracing_crate::{debug_span, Instrument};

use crate::{ConnectionWrap, ConnectionWrapInner, SQLxMiddleware};

/// An alias for `tide_sqlx::SQLxMiddleware<Postgres>`.
#[allow(dead_code)]
pub type PostgresMiddleware = SQLxMiddleware<Postgres>;

/// An extension trait for [tide::Request][] which does proper unwrapping of the connection from [`req.ext()`][].
///
/// Specialized for Postgres connections.
///
/// [`req.ext()`]: https://docs.rs/tide/0.15.0/tide/struct.Request.html#method.ext
/// [tide::Request]: https://docs.rs/tide/0.15.0/tide/struct.Request.html
#[cfg(feature = "postgres")]
#[cfg_attr(feature = "docs", doc(cfg(feature = "postgres")))]
#[async_trait]
pub trait PostgresRequestExt {
    /// Get the SQLx connection for the current Request.
    ///
    /// This will return a "write" guard from a read-write lock.
    /// Under the hood this will transparently be either a postgres transaction or a direct pooled connection.
    ///
    /// This will panic with an expect message if the `SQLxMiddleware` has not been run.
    ///
    /// ## Example
    ///
    /// ```no_run
    /// # #[async_std::main]
    /// # async fn main() -> anyhow::Result<()> {
    /// # use tide_sqlx::postgres::PostgresMiddleware;
    /// # use sqlx::postgres::Postgres;
    /// #
    /// # let mut app = tide::new();
    /// # app.with(PostgresMiddleware::new("postgres://localhost/a_database").await?);
    /// #
    /// use sqlx::Acquire; // Or sqlx::prelude::*;
    ///
    /// use tide_sqlx::postgres::PostgresRequestExt;
    ///
    /// app.at("/").post(|req: tide::Request<()>| async move {
    ///     let mut pg_conn = req.pg_conn().await;
    ///
    ///     sqlx::query("SELECT * FROM users")
    ///         .fetch_optional(pg_conn.acquire().await?)
    ///         .await;
    ///
    ///     Ok("")
    /// });
    /// # Ok(())
    /// # }
    /// ```
    async fn pg_conn<'req>(&'req self) -> RwLockWriteGuard<'req, ConnectionWrapInner<Postgres>>;
}

#[async_trait]
impl<T: Send + Sync + 'static> PostgresRequestExt for Request<T> {
    async fn pg_conn<'req>(&'req self) -> RwLockWriteGuard<'req, ConnectionWrapInner<Postgres>> {
        let pg_conn: &ConnectionWrap<Postgres> = self
            .ext()
            .expect("You must install SQLx middleware providing Postgres ConnectionWrap");
        let rwlock_fut = pg_conn.write();
        #[cfg(all(feature = "tracing", debug_assertions))]
        let rwlock_fut =
            rwlock_fut.instrument(debug_span!("Postgres connection RwLockWriteGuard acquire"));
        rwlock_fut.await
    }
}
