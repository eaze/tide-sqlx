use std::fmt::{self, Debug};
use std::sync::Arc;

use async_std::sync::{RwLock, RwLockWriteGuard};
use either::Either;
use futures_core::future::BoxFuture;
use futures_core::stream::BoxStream;
use sqlx::database::HasStatement;
use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgConnection, PgDone, PgPool, PgPoolOptions, PgRow, PgTypeInfo, Postgres};
use sqlx::{Acquire, Describe, Execute, Executor, Transaction};
use tide::utils::async_trait;
use tide::{http::Method, Middleware, Next, Request, Result};

#[doc(hidden)]
pub enum ConnectionWrapInner {
    Transacting(Transaction<'static, Postgres>),
    Plain(PoolConnection<Postgres>),
}

impl Debug for ConnectionWrapInner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transacting(_) => f.debug_struct("ConnectionWrapInner::Transacting").finish(),
            Self::Plain(_) => f.debug_struct("ConnectionWrapInner::Plain").finish(),
        }
    }
}

#[doc(hidden)]
pub type ConnectionWrap = Arc<RwLock<ConnectionWrapInner>>;

/// This middleware holds a pool of postgres connections, and automatically hands each [tide::Request][] a connection,
/// which may transparently be either a postgres transaction, or a direct pooled connection.
///
/// By default, transactions are used for all http methods other than GET and HEAD.
///
/// When using this, use the `PostgresRequestExt` extenstion trait to get the connection.
///
/// ## Example
///
/// ```no_run
/// # #[async_std::main]
/// # async fn main() -> anyhow::Result<()> {
/// use sqlx::Acquire; // Or sqlx::prelude::*;
///
/// use geolocality::middleware::PostgresConnectionMiddleware;
/// use geolocality::middleware::PostgresRequestExt;
///
/// let mut app = tide::new();
/// app.with(PostgresConnectionMiddleware::new("postgres://localhost/geolocality", 5).await?);
///
/// app.at("/").post(|req: tide::Request<()>| async move {
///     let mut pg_conn = req.postgres_conn().await;
///
///     pg_conn.acquire().await?; // Pass this to e.g. "fetch_optional()" from a sqlx::Query
///
///     Ok("")
/// });
/// # Ok(())
/// # }
/// ```
///
/// [tide::Request]: https://docs.rs/tide/0.14.0/tide/struct.Request.html
#[derive(Debug, Clone)]
pub struct PostgresConnectionMiddleware {
    pg_pool: PgPool,
}

impl PostgresConnectionMiddleware {
    /// Create a new instance of `PostgresConnectionMiddleware`.
    pub async fn new(
        pgurl: &'_ str,
        max_connections: u32,
    ) -> std::result::Result<Self, sqlx::Error> {
        let pg_pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .connect(pgurl)
            .await?;
        Ok(Self { pg_pool })
    }
}

// Why PostgresConnectionMiddleware exists:
//
// Postgres transactions are very useful because they allow easy, assured rollback if something goes wrong.
// However, transactions incur extra runtime cost which is too expensive to justify for READ operations that DO NOT NEED this behavior.
//
// In order to allow transactions to be used seamlessly in endpoints, this middleware manages a transaction if one is deemed desirable.
//
// This is complicated because of sqlx's typing. We would like a dynamic `sqlx::Executor`, however the Executor trait
// cannot be made into an object because it has generic methods.
// Rust does not allow this due to exponential fat-pointer table size.
// See https://doc.rust-lang.org/error-index.html#method-has-generic-type-parameters for more information.
//
// In order to get a concrete type for both which we can implement RefExecutor on, we make an enum with multiple types.
// The types must be concrete and non-generic because the outer type much be fetchable from `Request::ext`, which is a typemap.
//
// The type of the enum must be in an `Arc` because we want to be able to tell it to commit at the end of the middleware
// once we've gotten a response back. This is because anything in `Request::ext` is lost in the endpoint without manual movement
// to the `Response`. Tide may someday be able to do this automatically but not as of 0.14. An `Arc` is the correct choice to keep
// something between mutltiple owned contexts over a threaded futures executor.
//
// However interior mutability (`RwLock`) is also required because `Acquire` requires mutable self reference,
// requiring that we gain mutable lock from the `Arc`, which is not possible with an `Arc` alone.
//
// This makes using the extention of the request somewhat awkward, because it needs to be unwrapped into a `RwLockWriteGuard`,
// and so the `PostgresRequestExt` extension trait exists to make that nicer.

#[async_trait]
impl<State: Clone + Send + Sync + 'static> Middleware<State> for PostgresConnectionMiddleware {
    async fn handle(&self, mut req: Request<State>, next: Next<'_, State>) -> Result {
        // Dual-purpose: Avoid ever running twice, or pick up a test connection if one exists.
        //
        // TODO(Fishrock): implement recursive depth transactions.
        //   SQLx 0.4 Transactions which are recursive carry a Borrow to the containing Transaction.
        //   Blocked by language feature for Tide - Request extensions cannot hold Borrows.
        if req.ext::<ConnectionWrap>().is_some() {
            return Ok(next.run(req).await);
        }

        // TODO(Fishrock): Allow this to be overridden somehow. Maybe check part of the path.
        let is_safe = match req.method() {
            Method::Get => true,
            Method::Head => true,
            _ => false,
        };

        let conn_wrap_inner = if is_safe {
            ConnectionWrapInner::Plain(self.pg_pool.acquire().await?)
        } else {
            ConnectionWrapInner::Transacting(self.pg_pool.begin().await?)
        };
        let conn_wrap = Arc::new(RwLock::new(conn_wrap_inner));
        req.set_ext(conn_wrap.clone());

        let res = next.run(req).await;

        if res.error().is_none() {
            if let Ok(conn_wrap_inner) = Arc::try_unwrap(conn_wrap) {
                if let ConnectionWrapInner::Transacting(connection) = conn_wrap_inner.into_inner() {
                    // if we errored, sqlx::Transaction calls rollback on Drop.
                    connection.commit().await?;
                }
            } else {
                // If this is hit, it is likely that an http_types (surf::http / tide::http) Request has been kept alive and was not consumed.
                // This would be a programmer error.
                // Given the pool would slowly be resource-starved if we continue, there is no good way to continue.
                //
                // I'm bewildered, you're bewildered. Let's panic!
                panic!("We have err'd egregiously! Could not unwrap refcounted postgres conn for commit; handler may be storing connection or request inappropiately?")
            }
        }

        Ok(res)
    }
}

/// An extension trait for tide::Request which does proper unwrapping of the connection from [`req.ext()`][].
///
/// [`req.ext()`]: https://docs.rs/tide/0.14.0/tide/struct.Request.html#method.ext
#[async_trait]
pub trait PostgresRequestExt<'req> {
    /// Get the postgres connection for the current Request.
    ///
    /// This will return a "write" guard from a read-write lock.
    /// Under the hood this will transparently be either a postgres transaction or a direct pooled connection.
    ///
    /// This will panic with an expect message if the `PostgresConnectionMiddleware` has not been run.
    ///
    /// ## Example
    ///
    /// ```no_run
    /// # #[async_std::main]
    /// # async fn main() -> anyhow::Result<()> {
    /// # use geolocality::middleware::PostgresConnectionMiddleware;
    /// #
    /// # let mut app = tide::new();
    /// # app.with(PostgresConnectionMiddleware::new("postgres://localhost/geolocality", 5).await?);
    /// #
    /// use sqlx::Acquire; // Or sqlx::prelude::*;
    ///
    /// use geolocality::middleware::PostgresRequestExt;
    ///
    /// app.at("/").post(|req: tide::Request<()>| async move {
    ///     let mut pg_conn = req.postgres_conn().await;
    ///
    ///     // Pass this to e.g. "fetch_optional()" from a sqlx::Query
    ///     pg_conn.acquire().await?;
    ///
    ///     Ok("")
    /// });
    /// # Ok(())
    /// # }
    /// ```
    async fn postgres_conn(&'req self) -> RwLockWriteGuard<'req, ConnectionWrapInner>;
}

#[async_trait]
impl<'req, T: Send + Sync + 'static> PostgresRequestExt<'req> for Request<T> {
    async fn postgres_conn(&'req self) -> RwLockWriteGuard<'req, ConnectionWrapInner> {
        let pg_conn: &ConnectionWrap = self
            .ext()
            .expect("You must install Postgres middleware providing ConnectionWrap");
        pg_conn.write().await
    }
}

impl<'c> Acquire<'c> for &'c mut ConnectionWrapInner {
    type Database = Postgres;

    type Connection = &'c mut PgConnection;

    fn acquire(self) -> BoxFuture<'c, sqlx::Result<Self::Connection>> {
        match self {
            ConnectionWrapInner::Plain(c) => c.acquire(),
            ConnectionWrapInner::Transacting(c) => c.acquire(),
        }
    }

    fn begin(self) -> BoxFuture<'c, sqlx::Result<Transaction<'c, Postgres>>> {
        match self {
            ConnectionWrapInner::Plain(c) => c.begin(),
            ConnectionWrapInner::Transacting(c) => c.begin(),
        }
    }
}

impl<'c> Executor<'c> for &'c mut ConnectionWrapInner {
    type Database = Postgres;

    fn fetch_many<'e, 'q: 'e, E: 'q>(
        self,
        query: E,
    ) -> BoxStream<'e, sqlx::Result<Either<PgDone, PgRow>>>
    where
        'c: 'e,
        E: Execute<'q, Self::Database>,
    {
        match self {
            ConnectionWrapInner::Plain(c) => c.fetch_many(query),
            ConnectionWrapInner::Transacting(c) => c.fetch_many(query),
        }
    }

    fn fetch_optional<'e, 'q: 'e, E: 'q>(
        self,
        query: E,
    ) -> BoxFuture<'e, sqlx::Result<Option<PgRow>>>
    where
        'c: 'e,
        E: Execute<'q, Self::Database>,
    {
        match self {
            ConnectionWrapInner::Plain(c) => c.fetch_optional(query),
            ConnectionWrapInner::Transacting(c) => c.fetch_optional(query),
        }
    }

    fn prepare_with<'e, 'q: 'e>(
        self,
        sql: &'q str,
        parameters: &'e [PgTypeInfo],
    ) -> BoxFuture<'e, sqlx::Result<<Self::Database as HasStatement<'q>>::Statement>>
    where
        'c: 'e,
    {
        match self {
            ConnectionWrapInner::Plain(c) => c.prepare_with(sql, parameters),
            ConnectionWrapInner::Transacting(c) => c.prepare_with(sql, parameters),
        }
    }

    #[doc(hidden)]
    fn describe<'e, 'q: 'e>(
        self,
        sql: &'q str,
    ) -> BoxFuture<'e, sqlx::Result<Describe<Self::Database>>>
    where
        'c: 'e,
    {
        match self {
            ConnectionWrapInner::Plain(c) => c.describe(sql),
            ConnectionWrapInner::Transacting(c) => c.describe(sql),
        }
    }
}
