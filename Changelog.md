# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0] - 2021-02-23

- Deps: updated to sqlx 0.5

## [0.4.1] - 2021-02-22

- Deps: Relaxed dependency version constraints.

## [0.4.0] - 2021-01-29

- Change: No longer enables `"rustls"` by default.
- Deps: Updated to Tide 0.16

## [0.3.1] - 2020-12-03

- Docs: updated readme for 0.3.x apis & examples.

## [0.3.0] - 2020-12-03

This release adds the ability to be generic over any SQLx database _(except Sqlite, which is `!Sync`)_.

- Changed `PostgresConnectionMiddleware` to `SQLxMiddleware<DB>`.
- Changed `PostgresRequestExt` to `SQLxRequestExt`.
- `ConnectionWrapInner` no longer implements `Acquire` and `Executor`, but now implements `DerefMut`.
- Added `From<Pool<DB>> for SQLxMiddleware`.
- Added `postgres` submodule with helpers similar to the old 0.1.x api.

## [0.2.5] - 2020-11-19

- Deps: `default-features = false` for Tide.

## [0.2.4] - 2020-11-18

- Fix: properly expose `pg_conn()` from `PostgresRequestExt`.

## [0.2.3] - 2020-11-18

- Fix: actually fix Docs.rs build

## [0.2.2] - 2020-11-18

- Fix: added "docs" feature for Docs.rs
- Docs: updated readme.

## [0.2.1] - 2020-11-18

- Fix: corrected doc-test dev-deps.
- Docs: general refinements.
- Internal: added CI.

## [0.2.0] - 2020-11-18

- Init with `PostgresConnectionMiddleware` & `PostgresRequestExt`.

## [0.1.0] - 2020-11-18

- crates.io placeholder
