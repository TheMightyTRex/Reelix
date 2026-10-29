# syntax=docker/dockerfile:1
# Multi-stage build: cargo-chef caches dependency layers for both the server and the wasm UI.

FROM rust:1-bookworm AS chef
RUN cargo install --locked cargo-chef
WORKDIR /src

# ---- Dependency recipe -------------------------------------------------------
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ---- Server binary -----------------------------------------------------------
FROM chef AS builder
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json --package reelix-server
COPY . .
RUN cargo build --release --locked --package reelix-server --bin reelix \
    && cp target/release/reelix /reelix

# ---- Web UI (Leptos CSR via Trunk) -------------------------------------------
FROM chef AS web
RUN rustup target add wasm32-unknown-unknown \
    && cargo install --locked trunk
COPY --from=planner /src/recipe.json recipe.json
RUN cargo chef cook --release --locked --recipe-path recipe.json \
    --target wasm32-unknown-unknown --package reelix-web
COPY . .
RUN cd crates/reelix-web && trunk build --release

# ---- Runtime -----------------------------------------------------------------
FROM debian:bookworm-slim AS runtime
# util-linux (setpriv) is part of the base image; tzdata makes TZ work.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /reelix /app/reelix
COPY --from=web /src/crates/reelix-web/dist /app/web
COPY docker/entrypoint.sh /app/entrypoint.sh
RUN chmod 0755 /app/entrypoint.sh /app/reelix

ENV REELIX_DATA_DIR=/config \
    REELIX_WEB_DIR=/app/web \
    PUID=1000 \
    PGID=1000 \
    UMASK=022

EXPOSE 7700
VOLUME /config
WORKDIR /app
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=30s --timeout=10s --start-period=20s --retries=3 CMD ["/app/reelix", "healthcheck"]
ENTRYPOINT ["/app/entrypoint.sh"]
