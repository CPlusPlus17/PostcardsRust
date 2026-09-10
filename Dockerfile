# Multi-stage Dockerfile for PostcardsRust
# Builds a secure, minimal container image linking system libcurl and OpenSSL.

# Stage 1: Build
FROM rust:1-bookworm AS builder

WORKDIR /src

# Install build dependencies: OpenSSL, libcurl, pkg-config
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    libcurl4-openssl-dev \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Copy workspace manifests
COPY Cargo.toml Cargo.lock ./
COPY postcards-rust-core/Cargo.toml postcards-rust-core/
COPY postcards-rust-api/Cargo.toml postcards-rust-api/
COPY postcards-rust-plugin-base/Cargo.toml postcards-rust-plugin-base/
COPY postcards-rust-plugin-immich/Cargo.toml postcards-rust-plugin-immich/
COPY postcards-rust-plugin-google-photos/Cargo.toml postcards-rust-plugin-google-photos/
COPY postcards-rust-cli/Cargo.toml postcards-rust-cli/

# Copy all crate sources
COPY postcards-rust-core/ postcards-rust-core/
COPY postcards-rust-api/ postcards-rust-api/
COPY postcards-rust-plugin-base/ postcards-rust-plugin-base/
COPY postcards-rust-plugin-immich/ postcards-rust-plugin-immich/
COPY postcards-rust-plugin-google-photos/ postcards-rust-plugin-google-photos/
COPY postcards-rust-cli/ postcards-rust-cli/

# Build release binary
RUN cargo build --release -p postcards-rust-cli

# Stage 2: Minimal runtime image
FROM debian:bookworm-slim AS runtime

# Install runtime libraries: libcurl4, libssl3, CA certificates, and tzdata
RUN apt-get update && apt-get install -y --no-install-recommends \
    libcurl4 \
    libssl3 \
    ca-certificates \
    tzdata \
    && rm -rf /var/lib/apt/lists/*

# Create dedicated non-root user and persistent directory
RUN groupadd -g 10001 postcards && \
    useradd -u 10001 -g postcards -s /bin/bash -m postcards && \
    mkdir -p /data/.postcards_rust && \
    chown -R postcards:postcards /data

# Copy compiled binary from builder
COPY --from=builder --chown=postcards:postcards /src/target/release/postcards-rust /usr/local/bin/postcards-rust

USER postcards:postcards
WORKDIR /data
ENV HOME=/data
ENV RUST_LOG=info

ENTRYPOINT ["postcards-rust"]
CMD ["daemon"]
