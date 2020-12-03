//! A [Tide][] middleware which holds a pool of postgres connections, and automatically hands
//! each [tide::Request][] a connection, which may transparently be either a postgres transaction,
//! or a direct pooled connection.
//!
//! By default, transactions are used for all http methods other than GET and HEAD.
//!
//! When using this, use the `PostgresRequestExt` extenstion trait to get the connection.
//!
//! ## Example
//!
//! ```no_run
//! # #[async_std::main]
//! # async fn main() -> anyhow::Result<()> {
//! use sqlx::Acquire; // Or sqlx::prelude::*;
//! use sqlx::postgres::Postgres;
//!
//! use tide_sqlx::SQLxMiddleware;
//! use tide_sqlx::SQLxRequestExt;
//!
//! let mut app = tide::new();
//! app.with(SQLxMiddleware::<Postgres>::new("postgres://localhost/geolocality", 5).await?);
//!
//! app.at("/").post(|req: tide::Request<()>| async move {
//!     let mut pg_conn = req.sqlx_conn::<Postgres>().await;
//!
//!     sqlx::query("SELECT * FROM users")
//!         .fetch_optional(pg_conn.acquire().await?)
//!         .await;
//!
//!     Ok("")
//! });
//! # Ok(())
//! # }
//! ```
//!
//! ## Why you may want to use this
//!
//! Postgres transactions are very useful because they allow easy, assured rollback if something goes wrong.
//! However, transactions incur extra runtime cost which is too expensive to justify for READ operations that _do not need_ this behavior.
//!
//! In order to allow transactions to be used seamlessly in endpoints, this middleware manages a transaction if one is deemed desirable.
//!
//! [tide::Request]: https://docs.rs/tide/0.15.0/tide/struct.Request.html
//! [Tide]: https://docs.rs/tide/0.15.0/tide/

use std::fmt::{self, Debug};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use async_std::sync::{RwLock, RwLockWriteGuard};
use sqlx::pool::{Pool, PoolConnection, PoolOptions};
use sqlx::{Database, Transaction};
use tide::utils::async_trait;
use tide::{http::Method, Middleware, Next, Request, Result};

#[cfg(all(test, not(feature = "postgres")))]
compile_error!("The tests must be run with --features=postgres");

#[doc(hidden)]
pub enum ConnectionWrapInner<DB: Database> {
    Transacting(Transaction<'static, DB>),
    Plain(PoolConnection<DB>),
}

unsafe impl<DB: Database> Send for ConnectionWrapInner<DB> {}
unsafe impl<DB: Database> Sync for ConnectionWrapInner<DB> {}

impl<DB: Database> Debug for ConnectionWrapInner<DB> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transacting(_) => f.debug_struct("ConnectionWrapInner::Transacting").finish(),
            Self::Plain(_) => f.debug_struct("ConnectionWrapInner::Plain").finish(),
        }
    }
}

#[doc(hidden)]
pub type ConnectionWrap<DB> = Arc<RwLock<ConnectionWrapInner<DB>>>;

/// This middleware holds a pool of postgres connections, and automatically hands each
/// [tide::Request][] a connection, which may transparently be either a postgres transaction,
/// or a direct pooled connection.
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
/// use sqlx::postgres::Postgres;
///
/// use tide_sqlx::SQLxMiddleware;
/// use tide_sqlx::SQLxRequestExt;
///
/// let mut app = tide::new();
/// app.with(SQLxMiddleware::<Postgres>::new("postgres://localhost/a_database", 5).await?);
///
/// app.at("/").post(|req: tide::Request<()>| async move {
///     let mut pg_conn = req.sqlx_conn::<Postgres>().await;
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
///
/// [tide::Request]: https://docs.rs/tide/0.15.0/tide/struct.Request.html
#[derive(Debug, Clone)]
pub struct SQLxMiddleware<DB: Database> {
    pool: Pool<DB>,
}

impl<DB: Database> SQLxMiddleware<DB> {
    /// Create a new instance of `SQLxMiddleware`.
    pub async fn new(
        pgurl: &'_ str,
        max_connections: u32,
    ) -> std::result::Result<Self, sqlx::Error> {
        let pool: Pool<DB> = PoolOptions::new()
            .max_connections(max_connections)
            .connect(pgurl)
            .await?;
        Ok(Self { pool })
    }
}

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
impl<State: Clone + Send + Sync + 'static, DB: Database> Middleware<State> for SQLxMiddleware<DB> {
    async fn handle(&self, mut req: Request<State>, next: Next<'_, State>) -> Result {
        // Dual-purpose: Avoid ever running twice, or pick up a test connection if one exists.
        //
        // TODO(Fishrock): implement recursive depth transactions.
        //   SQLx 0.4 Transactions which are recursive carry a Borrow to the containing Transaction.
        //   Blocked by language feature for Tide - Request extensions cannot hold Borrows.
        if req.ext::<ConnectionWrap<DB>>().is_some() {
            return Ok(next.run(req).await);
        }

        // TODO(Fishrock): Allow this to be overridden somehow. Maybe check part of the path.
        let is_safe = match req.method() {
            Method::Get => true,
            Method::Head => true,
            _ => false,
        };

        let conn_wrap_inner = if is_safe {
            ConnectionWrapInner::Plain(self.pool.acquire().await?)
        } else {
            ConnectionWrapInner::Transacting(self.pool.begin().await?)
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
                panic!("We have err'd egregiously! Could not unwrap refcounted SQLx connection for COMMIT; handler may be storing connection or request inappropiately?")
            }
        }

        Ok(res)
    }
}

/// An extension trait for [tide::Request][] which does proper unwrapping of the connection from [`req.ext()`][].
///
/// [`req.ext()`]: https://docs.rs/tide/0.15.0/tide/struct.Request.html#method.ext
/// [tide::Request]: https://docs.rs/tide/0.15.0/tide/struct.Request.html
#[async_trait]
pub trait SQLxRequestExt {
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
    /// # use tide_sqlx::SQLxMiddleware;
    /// # use sqlx::postgres::Postgres;
    /// #
    /// # let mut app = tide::new();
    /// # app.with(SQLxMiddleware::<Postgres>::new("postgres://localhost/a_database", 5).await?);
    /// #
    /// use sqlx::Acquire; // Or sqlx::prelude::*;
    ///
    /// use tide_sqlx::SQLxRequestExt;
    ///
    /// app.at("/").post(|req: tide::Request<()>| async move {
    ///     let mut pg_conn = req.sqlx_conn::<Postgres>().await;
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
    async fn sqlx_conn<'req, DB: Database>(
        &'req self,
    ) -> RwLockWriteGuard<'req, ConnectionWrapInner<DB>>;
}

// postgres = [ "sqlx/postgres"]
// mysql = [ "sqlx/mysql" ]
// sqlite = [ "sqlx/sqlite" ]
// mssql = [ "sqlx/mssql" ]

#[async_trait]
impl<T: Send + Sync + 'static> SQLxRequestExt for Request<T> {
    async fn sqlx_conn<'req, DB>(&'req self) -> RwLockWriteGuard<'req, ConnectionWrapInner<DB>>
    where
        DB: Database,
    {
        let pg_conn: &ConnectionWrap<DB> = self
            .ext()
            .expect("You must install SQLx middleware providing ConnectionWrap");
        pg_conn.write().await
    }
}

impl<DB: Database> Deref for ConnectionWrapInner<DB> {
    type Target = DB::Connection;

    fn deref(&self) -> &Self::Target {
        match self {
            ConnectionWrapInner::Plain(c) => c,
            ConnectionWrapInner::Transacting(c) => c,
        }
    }
}

impl<DB: Database> DerefMut for ConnectionWrapInner<DB> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            ConnectionWrapInner::Plain(c) => c,
            ConnectionWrapInner::Transacting(c) => c,
        }
    }
}
