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
    && apt-get install -y --no-install-recommends ca-certificates tini gosu \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/hermes /usr/local/bin/hermes
COPY docker/entrypoint.sh /usr/local/bin/hermes-entrypoint
RUN chmod +x /usr/local/bin/hermes-entrypoint \
    && groupadd -g 10000 hermes \
    && useradd -u 10000 -g 10000 -m -s /bin/sh hermes \
    && mkdir -p /data \
    && chown -R 10000:10000 /data \
    && chmod -R a+rX /usr/local/bin

ENV HERMES_HOME=/data
VOLUME ["/data"]

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/hermes-entrypoint"]
CMD ["hermes"]
