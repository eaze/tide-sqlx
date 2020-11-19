# tide-sqlx

[Tide][] middleware for [SQLx][] pooled connections &amp; transactions.

----

A middleware which holds a pool of postgres connections, and automatically hands
each [tide::Request][] a connection, which may transparently be either a postgres transaction,
or a direct pooled connection.

By default, transactions are used for all http methods other than GET and HEAD.

When using this, use the `PostgresRequestExt` extenstion trait to get the connection.

## Example

```rust
#[async_std::main]
async fn main() -> anyhow::Result<()> {
    use sqlx::Acquire; // Or sqlx::prelude::*;

    use tide_sqlx::PostgresConnectionMiddleware;
    use tide_sqlx::PostgresRequestExt;

    let mut app = tide::new();
    app.with(PostgresConnectionMiddleware::new("postgres://localhost/geolocality", 5).await?);

    app.at("/").post(|req: tide::Request<()>| async move {
        let mut pg_conn = req.postgres_conn().await;

        pg_conn.acquire().await?; // Pass this to e.g. "fetch_optional()" from a sqlx::Query

        Ok("")
    });
    Ok(())
}
```

## Why you may want to use this

Postgres transactions are very useful because they allow easy, assured rollback if something goes wrong.
However, transactions incur extra runtime cost which is too expensive to justify for READ operations that _do not need_ this behavior.

In order to allow transactions to be used seamlessly in endpoints, this middleware manages a transaction if one is deemed desirable.


## License

Licensed under the [BlueOak Model License 1.0.0](LICENSE.md) — _[Contributions via DCO 1.1](contributing.md#developers-certificate-of-origin)_

[tide::Request]: https://docs.rs/tide/0.15.0/tide/struct.Request.html
[SQLx]: https://github.com/launchbadge/sqlx
[Tide]: https://github.com/http-rs/tide
