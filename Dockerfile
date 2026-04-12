# ----- Build stage -----
FROM rust:1.79-bookworm AS builder

WORKDIR /app

# Copy workspace manifests first for better layer caching
COPY Cargo.toml Cargo.lock ./
COPY crates crates

# Build with all optional platform features
RUN cargo build --release --features "telegram,discord,slack" \
    || cargo build --release

# ----- Runtime stage -----
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/hermes /usr/local/bin/hermes

ENV HERMES_HOME=/data
VOLUME ["/data"]

ENTRYPOINT ["hermes"]
