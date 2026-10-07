# syntax=docker/dockerfile:1
ARG PG_MAJOR=18

FROM postgres:${PG_MAJOR}-trixie AS builder
ARG PG_MAJOR
ENV DEBIAN_FRONTEND=noninteractive \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
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
    && rustup toolchain install

RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo install cargo-pgrx --version 0.19.1 --locked \
    && cargo pgrx init --pg${PG_MAJOR}=${PG_CONFIG}

COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo pgrx package --debug --package tin \
        --no-default-features --features pg${PG_MAJOR} \
        --pg-config ${PG_CONFIG} --out-dir /out

FROM postgres:${PG_MAJOR}-trixie
ARG PG_MAJOR

LABEL org.opencontainers.image.title="Lead" \
      org.opencontainers.image.description="PostgreSQL ${PG_MAJOR} with the Lead extension (debug build, for development and CI, not production)" \
      org.opencontainers.image.source="https://github.com/planetscale/lead" \
      org.opencontainers.image.licenses="AGPL-3.0-or-later"

COPY --from=builder /out/ /
COPY docker/initdb-tin.sql /docker-entrypoint-initdb.d/10-tin.sql

EXPOSE 5432
