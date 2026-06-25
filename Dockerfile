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
LABEL org.opencontainers.image.source="https://github.com/sheawinkler/hermes-agent-ultra" \
      org.opencontainers.image.licenses="MIT"

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates tini gosu \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/hermes /usr/local/bin/hermes
COPY docker/entrypoint.sh /usr/local/bin/hermes-entrypoint
COPY LICENSE NOTICE /usr/share/doc/hermes-agent-ultra/
RUN chmod +x /usr/local/bin/hermes-entrypoint \
    && groupadd -g 10000 hermes \
    && useradd -u 10000 -g 10000 -m -s /bin/sh hermes \
    && mkdir -p /data \
    && chown -R 10000:10000 /data \
    && chmod -R a+rX /usr/local/bin /usr/share/doc/hermes-agent-ultra

ENV HERMES_HOME=/data
VOLUME ["/data"]

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/hermes-entrypoint"]
CMD ["hermes"]
