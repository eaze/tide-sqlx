//! A [Tide][] middleware which holds a pool of SQLx database connections, and automatically hands
//! each [tide::Request][] a connection, which may transparently be either a database transaction,
//! or a direct pooled database connection.
//!
//! By default, transactions are used for all http methods other than `GET` and `HEAD`.
//!
//! When using this, use the `SQLxRequestExt` extenstion trait to get the connection.
//!
//! ## Examples
//!
//! ### Basic
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
//! app.with(SQLxMiddleware::<Postgres>::new("postgres://localhost/a_database").await?);
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
//! ### From sqlx `PoolOptions` and with `ConnectOptions`
//! ```no_run
//! # #[async_std::main]
//! # async fn main() -> anyhow::Result<()> {
//! use log::LevelFilter;
//! use sqlx::{Acquire, ConnectOptions}; // Or sqlx::prelude::*;
//! use sqlx::postgres::{PgConnectOptions, PgPoolOptions, Postgres};
//!
//! use tide_sqlx::SQLxMiddleware;
//! use tide_sqlx::SQLxRequestExt;
//!
//! let mut connect_opts = PgConnectOptions::new();
//! connect_opts.log_statements(LevelFilter::Debug);
//!
//! let pg_pool = PgPoolOptions::new()
//!     .max_connections(5)
//!     .connect_with(connect_opts)
//!     .await?;
//!
//! let mut app = tide::new();
//! app.with(SQLxMiddleware::from(pg_pool));
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
//! Database transactions are very useful because they allow easy, assured rollback if something goes wrong.
//! However, transactions incur extra runtime cost which is too expensive to justify for READ operations that _do not need_ this behavior.
//!
//! In order to allow transactions to be used seamlessly in endpoints, this middleware manages a transaction if one is deemed desirable.
//!
//! [tide::Request]: https://docs.rs/tide/0.15.0/tide/struct.Request.html
//! [Tide]: https://docs.rs/tide/0.15.0/tide/

#![cfg_attr(feature = "docs", feature(doc_cfg))]

use std::fmt::{self, Debug};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use async_std::sync::{RwLock, RwLockWriteGuard};
use sqlx::pool::{Pool, PoolConnection};
use sqlx::{Database, Transaction};
use tide::http::Method;
use tide::utils::async_trait;
use tide::{Middleware, Next, Request, Result};

#[cfg(feature = "unsafe-nested-transactions")]
use sqlx::Connection;
#[cfg(feature = "unsafe-nested-transactions")]
use tide::http;

#[cfg(all(test, not(feature = "postgres")))]
compile_error!("The tests must be run with --features=test");

#[cfg(feature = "postgres")]
#[cfg_attr(feature = "docs", doc(cfg(feature = "postgres")))]
/// Helpers specific to Postgres
pub mod postgres;

#[doc(hidden)]
pub enum ConnectionWrapInner<DB>
where
    DB: Database,
    DB::Connection: Send + Sync + 'static,
{
    Transacting(Transaction<'static, DB>),
    Plain(PoolConnection<DB>),
}

impl<DB> Debug for ConnectionWrapInner<DB>
where
    DB: Database,
    DB::Connection: Send + Sync + 'static,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Transacting(_) => f.debug_struct("ConnectionWrapInner::Transacting").finish(),
            Self::Plain(_) => f.debug_struct("ConnectionWrapInner::Plain").finish(),
        }
    }
}

impl<DB> Deref for ConnectionWrapInner<DB>
where
    DB: Database,
    DB::Connection: Send + Sync + 'static,
{
    type Target = DB::Connection;

    fn deref(&self) -> &Self::Target {
        match self {
            ConnectionWrapInner::Plain(c) => c,
            ConnectionWrapInner::Transacting(c) => c,
        }
    }
}

impl<DB> DerefMut for ConnectionWrapInner<DB>
where
    DB: Database,
    DB::Connection: Send + Sync + 'static,
{
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self {
            ConnectionWrapInner::Plain(c) => c,
            ConnectionWrapInner::Transacting(c) => c,
        }
    }
}

#[doc(hidden)]
pub type ConnectionWrap<DB> = Arc<RwLock<ConnectionWrapInner<DB>>>;

/// This middleware holds a pool of SQLx database connections, and automatically hands each
/// [tide::Request][] a connection, which may transparently be either a database transaction,
/// or a direct pooled database connection.
///
/// By default, transactions are used for all http methods other than `GET` and `HEAD`.
///
/// When using this, use the `SQLxRequestExt` extenstion trait to get the connection.
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
/// app.with(SQLxMiddleware::<Postgres>::new("postgres://localhost/a_database").await?);
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
pub struct SQLxMiddleware<DB>
where
    DB: Database,
    DB::Connection: Send + Sync + 'static,
{
    pool: Pool<DB>,
}

impl<DB> SQLxMiddleware<DB>
where
    DB: Database,
    DB::Connection: Send + Sync + 'static,
{
    /// Create a new instance of `SQLxMiddleware`.
    pub async fn new(pgurl: &'_ str) -> std::result::Result<Self, sqlx::Error> {
        let pool: Pool<DB> = Pool::connect(pgurl).await?;
        Ok(Self { pool })
    }
}

impl<DB> From<Pool<DB>> for SQLxMiddleware<DB>
where
    DB: Database,
    DB::Connection: Send + Sync + 'static,
{
    /// Create a new instance of `SQLxMiddleware` from a `sqlx::Pool`.
    fn from(pool: Pool<DB>) -> Self {
        Self { pool }
    }
}

// This is complicated because of sqlx's typing. We would like a dynamic `sqlx::Executor`, however the Executor trait
// cannot be made into an object because it has generic methods.
// Rust does not allow this due to exponential fat-pointer table size.
// See https://doc.rust-lang.org/error-index.html#method-has-generic-type-parameters for more information.
//
// In order to get a concrete type for both which we can deref to a `Connection` on, we make an enum with multiple types.
// The types must be concrete and non-generic because the outer type much be fetchable from `Request::ext`, which is a typemap.
//
// The type of the enum must be in an `Arc` because we want to be able to tell it to commit at the end of the middleware
// once we've gotten a response back. This is because anything in `Request::ext` is lost in the endpoint without manual movement
// to the `Response`. Tide may someday be able to do this automatically but not as of 0.15. An `Arc` is the correct choice to keep
// something between mutltiple owned contexts over a threaded futures executor.
//
// However interior mutability (`RwLock`) is also required because `Acquire` requires mutable self reference,
// requiring that we gain mutable lock from the `Arc`, which is not possible with an `Arc` alone.
//
// This makes using the extention of the request somewhat awkward, because it needs to be unwrapped into a `RwLockWriteGuard`,
// and so the `SQLxRequestExt` extension trait exists to make that nicer.

#[async_trait]
impl<State, DB> Middleware<State> for SQLxMiddleware<DB>
where
    State: Clone + Send + Sync + 'static,
    DB: Database,
    DB::Connection: Send + Sync + 'static,
{
    async fn handle(&self, mut req: Request<State>, next: Next<'_, State>) -> Result {
        // TODO(Fishrock): Allow this to be overridden somehow. Maybe check part of the path.
        let is_safe = match req.method() {
            Method::Get => true,
            Method::Head => true,
            _ => false,
        };

        let conn_wrap_inner = if req.ext::<ConnectionWrap<DB>>().is_some() {
            #[cfg(feature = "unsafe-nested-transactions")]
            {
                let inner_req: &mut http::Request = req.as_mut();
                let conn_wrap = inner_req
                    .ext_mut()
                    .remove::<ConnectionWrap<DB>>()
                    .expect("This was literally just checked.");
                let mut sqlx_conn = conn_wrap.write().await;

                let nested_trans = sqlx_conn.begin().await?;

                // UNSAFE.
                //
                // Rust is presently unable to express the guarentees we need here.
                //
                // Tide is meant to run on Async-Std, which uses a _threaded futures executor_.
                // A threaded futures executor requires that all Futures be `Send`, so that they can be sent across thread bounadries.
                // `Send` implies that any lifetimes must be `'static`. This is actually the limitation in Rust.
                // Why `'static'` is implied is answered here: https://stackoverflow.com/a/26783347
                // In short:
                // `Send` implies that said object carries no reference to the current thread stack.
                // If an object has no reference to the current stack, its lifetime bound is `'static`.
                //
                // However Rust already has to ensure that other Futures scopes outlive inner futures scopes even in the case of a threaded executor.
                // So in this case `Send` could theoretically hold a lifetime of an outer scope and still be sound.
                // In practice, async closures, once implemented by the language, will have to do exactly this same thing to work with threaded executors.
                // When that happens and Tide moves to being able to use async closures, this will no longer be useful, thankfully.
                //
                // So, since Rust already enforces most of the memory safety we'd need anyways, this should be "safe" to do.
                //
                // Also, storing anything attached from middleware is already going to cause panics regardless, because that would mean an `Arc` would be unwrapped twice.
                // This lifetime nonsense is of course still worse in that you may not immediately panic but will suffer from arbitrary memory corruption.
                unsafe {
                    ConnectionWrapInner::Transacting(extend_transaction_lifetime(nested_trans))
                }
                // End unsafe.
            }
            #[cfg(not(feature = "unsafe-nested-transactions"))]
            {
                // Dual-purpose: Avoid ever running twice, or pick up a test connection if one exists.
                return Ok(next.run(req).await);
            }
        } else if is_safe {
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
    /// # use tide_sqlx::SQLxMiddleware;
    /// # use sqlx::postgres::Postgres;
    /// #
    /// # let mut app = tide::new();
    /// # app.with(SQLxMiddleware::<Postgres>::new("postgres://localhost/a_database").await?);
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
    async fn sqlx_conn<'req, DB>(&'req self) -> RwLockWriteGuard<'req, ConnectionWrapInner<DB>>
    where
        DB: Database,
        DB::Connection: Send + Sync + 'static;
}

#[async_trait]
impl<T: Send + Sync + 'static> SQLxRequestExt for Request<T> {
    async fn sqlx_conn<'req, DB>(&'req self) -> RwLockWriteGuard<'req, ConnectionWrapInner<DB>>
    where
        DB: Database,
        DB::Connection: Send + Sync + 'static,
    {
        let sqlx_conn: &ConnectionWrap<DB> = self
            .ext()
            .expect("You must install SQLx middleware providing ConnectionWrap");
        sqlx_conn.write().await
    }
}

/// EXTREMELY UNSAFE. MAY LEAD TO MEMORY CORRUPTION.
///
/// See https://doc.rust-lang.org/std/mem/fn.transmute.html for more information.
///
/// This is made even more unsafe than just `transmute<'a, 'static>` because `sqlx::Transaction` has an enum internally,
/// which does not have a known size, and so `transmute_copy` is required even though we aren't changing the enum varient internally.
/// Presumably this is a limitation of Rust that may be solved in the future.
///
/// See https://doc.rust-lang.org/std/mem/fn.transmute_copy.html for even more specific information.
#[cfg(feature = "unsafe-nested-transactions")]
unsafe fn extend_transaction_lifetime<'c, DB>(
    transaction: Transaction<'c, DB>,
) -> Transaction<'static, DB>
where
    DB: Database,
{
    std::mem::transmute_copy::<Transaction<'c, DB>, Transaction<'static, DB>>(&transaction)
}
