# syntax=docker/dockerfile:1
ARG PG_MAJOR=18

# The builder stage has the Rust toolchain, cargo-pgrx, and the PGDG
# Postgres headers, but no Lead source.  CI runs the tests in it as the
# postgres user (Postgres refuses to run as root), so the directories that
# cargo and `cargo pgrx test` write to are world-writable.
FROM postgres:${PG_MAJOR}-trixie AS builder
ARG PG_MAJOR
ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PGRX_HOME=/usr/local/pgrx \
    USER=postgres \
    CARGO_NET_RETRY=10 \
    CARGO_HTTP_MULTIPLEXING=false \
    PATH=/usr/local/cargo/bin:$PATH \
    PG_CONFIG=/usr/lib/postgresql/${PG_MAJOR}/bin/pg_config

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        build-essential \
        ca-certificates \
        clang \
        curl \
        git \
        libclang-dev \
        libssl-dev \
        pkg-config \
        postgresql-server-dev-${PG_MAJOR} \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY rust-toolchain.toml .
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --default-toolchain none \
    && rustup toolchain install \
    && rustup component add clippy

RUN cargo install cargo-pgrx --version 0.19.1 --locked \
    && cargo pgrx init --pg${PG_MAJOR}=${PG_CONFIG} \
    && rm -rf "$CARGO_HOME/registry" \
    && chmod -R a+rwX "$CARGO_HOME" "$PGRX_HOME" \
        "$(${PG_CONFIG} --pkglibdir)" "$(${PG_CONFIG} --sharedir)/extension"

FROM builder AS package
ARG PG_MAJOR
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo pgrx package --debug --package tin \
        --no-default-features --features pg${PG_MAJOR} \
        --pg-config ${PG_CONFIG} --out-dir /out

# CI replaces this stage with the files from its own `cargo pgrx package`
# run, using `--build-context artifacts=<dir>`, and then skips the package
# stage.
FROM scratch AS artifacts
COPY --from=package /out/ /

FROM postgres:${PG_MAJOR}-trixie
ARG PG_MAJOR

LABEL org.opencontainers.image.title="Lead" \
      org.opencontainers.image.description="PostgreSQL ${PG_MAJOR} with the Lead extension (debug build, for development and CI, not production)" \
      org.opencontainers.image.source="https://github.com/planetscale/lead" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"

COPY --from=artifacts / /
COPY docker/initdb-tin.sql /docker-entrypoint-initdb.d/10-tin.sql

EXPOSE 5432
