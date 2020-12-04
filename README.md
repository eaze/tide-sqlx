# tide-sqlx

[Tide][] middleware for [SQLx][] pooled connections &amp; transactions.

----

A [Tide][] middleware which holds a pool of SQLx database connections, and automatically hands
each [tide::Request][] a connection, which may transparently be either a database transaction,
or a direct pooled database connection.

By default, transactions are used for all http methods other than `GET` and `HEAD`.

When using this, use the `SQLxRequestExt` extenstion trait to get the connection.

## Examples

### Basic
```rust
#[async_std::main]
async fn main() -> anyhow::Result<()> {
    use sqlx::Acquire; // Or sqlx::prelude::*;
    use sqlx::postgres::Postgres;

    use tide_sqlx::SQLxMiddleware;
    use tide_sqlx::SQLxRequestExt;

    let mut app = tide::new();
    app.with(SQLxMiddleware::<Postgres>::new("postgres://localhost/a_database").await?);

    app.at("/").post(|req: tide::Request<()>| async move {
        let mut pg_conn = req.sqlx_conn::<Postgres>().await;

        sqlx::query("SELECT * FROM users")
            .fetch_optional(pg_conn.acquire().await?)
            .await;

        Ok("")
    });
    Ok(())
}
```

### From sqlx `PoolOptions` and with `ConnectOptions`
```rust
#[async_std::main]
async fn main() -> anyhow::Result<()> {
    use log::LevelFilter;
    use sqlx::{Acquire, ConnectOptions}; // Or sqlx::prelude::*;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions, Postgres};

    use tide_sqlx::SQLxMiddleware;
    use tide_sqlx::SQLxRequestExt;

    let mut connect_opts = PgConnectOptions::new();
    connect_opts.log_statements(LevelFilter::Debug);

    let pg_pool = PgPoolOptions::new()
        .max_connections(5)
        .connect_with(connect_opts)
        .await?;

    let mut app = tide::new();
    app.with(SQLxMiddleware::from(pg_pool));

    app.at("/").post(|req: tide::Request<()>| async move {
        let mut pg_conn = req.sqlx_conn::<Postgres>().await;

        sqlx::query("SELECT * FROM users")
            .fetch_optional(pg_conn.acquire().await?)
            .await;

        Ok("")
    });
    Ok(())
}
```

## Why you may want to use this

Database transactions are very useful because they allow easy, assured rollback if something goes wrong.
However, transactions incur extra runtime cost which is too expensive to justify for READ operations that _do not need_ this behavior.

In order to allow transactions to be used seamlessly in endpoints, this middleware manages a transaction if one is deemed desirable.

## License

Licensed under the [BlueOak Model License 1.0.0](LICENSE.md) — _[Contributions via DCO 1.1](contributing.md#developers-certificate-of-origin)_

[tide::Request]: https://docs.rs/tide/0.15.0/tide/struct.Request.html
[SQLx]: https://github.com/launchbadge/sqlx
[Tide]: https://github.com/http-rs/tide
