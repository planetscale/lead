# Lead

Lead is a deliberately non-production Postgres text-search extension for exercising TIN-compatible application SQL in development, test, CI, and staging environments.

It favors correctness and a small implementation over production query performance.

It is intentionally unsuitable for production workloads. Every index scan returns all heap pages as candidates; Postgres rechecks their visible rows for exact TINQL and MVCC behavior. The index stores no search data.

## Build

[Install `cargo-pgrx`](https://github.com/pgcentralfoundation/pgrx/blob/develop/cargo-pgrx/README.md) version 0.19.1 exactly and initialize it for the Postgres major versions you need, then build or package with one version feature:

```sh
cargo pgrx package --package tin --no-default-features --features pg18
```

For an interactive development database, run:

```sh
cargo pgrx run pg18 --package tin
```

Then run `CREATE EXTENSION tin` in the database. Lead loads on demand and does not require `shared_preload_libraries` or `session_preload_libraries`.

## Compatibility boundary

Lead provides the `tin` access method, the `==>` operator, TINQL parsing, tokenizer and index reloptions (including the Snowball `stemmer` option), and the scoring functions `tin.score`, `tin.full_score`, `tin.max_score`, and `tin.score_inspect`, plus explicit and implicitly bound `tin.highlight` and `tin.highlight_ansi`. Postgres 17 and 18 are build targets. Search results are exact because the access method returns whole-page candidates and Postgres evaluates `==>` against each visible heap tuple, including expression and partial-index rechecks.

The planner binds each `==>` and each `tin.highlight` or `tin.highlight_ansi` call to the analysis of the tin index that covers its column, so a stemmed index matches and highlights inflected words in queries over that column, including joins, partitions, and cached plans. That includes `body ==> ANY(...)`, whose array elements are bound to the index analysis too; `tin.highlight` marks only the implicit queries of plain `==>` quals, as TIN does, though an `ANY(...)` search binds the analysis of an explicit `query`. `tin.tokenize`, `tin.ql_parse`, `tin.highlight`, and `tin.highlight_ansi` take a trailing `stemmer` argument as in TIN 1.0.4; their 1.0.3 signatures remain as `tin.tokenize_v1_0_3`, `tin.ql_parse_v1_0_3`, `tin.highlight_v1_0_3`, and `tin.highlight_ansi_v1_0_3`. As in TIN, changing `stemmer` on an existing index requires `REINDEX`.

Scoring deliberately rescans and retokenizes the visible indexed column or expression, once per statement and search, under that statement's snapshot. A score call must be in the same query level as the matching `==>` predicate, and a partial tin index binds only when the query's quals imply its `WHERE` condition, as in tin. Implicit highlighting binds the same way and, like tin, returns the text unmarked when no index binds a search; passing its `query` argument explicitly works without a bound predicate.

## Execution and storage

Each scan reads the table's current block count and adds every block to a lossy bitmap. Postgres owns row visibility, query rechecks, and table maintenance. Index builds evaluate indexed expressions and predicates for validation and statistics; inserts and VACUUM have no index entries to maintain.

Lead allocates no extension shared memory and creates no files outside Postgres's normal relation storage. Server restarts and crash recovery do not require rebuilding Lead indexes: scans use the recovered heap directly.


## Tests

Run the local unit and Postgres tests for a supported Postgres major version with:

```sh
cargo pgrx test pg18 --package tin --no-default-features --features pg18
```

Developers with access to the TIN private source may also run the more comprehensive test suite that comes with that:

```sh
TIN_PRIVATE_REPO=/path/to/full-tin script/run-private-regress pg18
```

## TINQL guide

The [TINQL guide](tinql/docs/src/SUMMARY.md) documents the query language. To build it with mdBook, run from the repository root:

```sh
cargo install mdbook --version 0.5.2 --locked
mdbook build tinql/docs
```

Open `tinql/docs/book/index.html` in your browser to read the book.

## Contributing

We intend for Lead to be a slow but correct substitute for TIN, for use at small scales in development and testing environments.  If you find cases where it's unsuitable for that, please contact PlanetScale through normal support channels or open an issue in this repo.  The most helpful bug reports will include information about what you expected Lead to do (which is normally whatever TIN would do in the same situation) versus what it actually did.  Help us recreate the problem so we can fix it.
