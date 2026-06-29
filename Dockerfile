# syntax=docker/dockerfile:1.7

# ----- Build stage -----
FROM rust:1.79-bookworm AS builder

WORKDIR /app

# Copy workspace manifests first for better layer caching
COPY --link Cargo.toml Cargo.lock ./
COPY --link crates crates

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

COPY --link --from=builder --chmod=0755 /app/target/release/hermes /usr/local/bin/hermes
COPY --link --chmod=0755 docker/entrypoint.sh /usr/local/bin/hermes-entrypoint
COPY --link LICENSE NOTICE /usr/share/doc/hermes-agent-ultra/
RUN chmod 0644 /usr/share/doc/hermes-agent-ultra/LICENSE /usr/share/doc/hermes-agent-ultra/NOTICE \
    && groupadd -g 10000 hermes \
    && useradd -u 10000 -g 10000 -m -s /bin/sh hermes \
    && install -d -o 10000 -g 10000 -m 0755 /data

ENV HERMES_HOME=/data
VOLUME ["/data"]

ENTRYPOINT ["/usr/bin/tini", "--", "/usr/local/bin/hermes-entrypoint"]
CMD ["hermes"]
