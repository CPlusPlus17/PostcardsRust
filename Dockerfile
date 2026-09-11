# Multi-stage Dockerfile for PostcardsRust
# Builds a secure, minimal container image linking system libcurl and OpenSSL.
# Uses native cross-compilation on $BUILDPLATFORM to achieve fast builds without QEMU compiler overhead.

# Stage 1: Build
FROM --platform=$BUILDPLATFORM rust:1-bookworm AS builder
ARG BUILDARCH
ARG TARGETARCH

WORKDIR /src

# Install build dependencies: OpenSSL, libcurl, pkg-config
# If cross-compiling (e.g. building arm64 on an amd64 host), install Debian multiarch cross toolchain
RUN if [ "$TARGETARCH" = "$BUILDARCH" ]; then \
        apt-get update && apt-get install -y --no-install-recommends \
            pkg-config \
            libssl-dev \
            libcurl4-openssl-dev \
            ca-certificates \
            && rm -rf /var/lib/apt/lists/*; \
    elif [ "$TARGETARCH" = "arm64" ]; then \
        dpkg --add-architecture arm64 && \
        apt-get update && apt-get install -y --no-install-recommends \
            gcc-aarch64-linux-gnu \
            libc6-dev-arm64-cross \
            libssl-dev:arm64 \
            libcurl4-openssl-dev:arm64 \
            pkg-config \
            ca-certificates \
            && rm -rf /var/lib/apt/lists/* && \
        rustup target add aarch64-unknown-linux-gnu; \
    else \
        echo "Unsupported cross target: $TARGETARCH on $BUILDARCH" && exit 1; \
    fi

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

# Build release binary using BuildKit cache mounts
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    if [ "$TARGETARCH" = "$BUILDARCH" ]; then \
        cargo build --release -p postcards-rust-cli && \
        mkdir -p /out && \
        cp /src/target/release/postcards-rust /out/postcards-rust; \
    elif [ "$TARGETARCH" = "arm64" ]; then \
        export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc && \
        export PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig && \
        export PKG_CONFIG_ALLOW_CROSS=1 && \
        cargo build --release --target aarch64-unknown-linux-gnu -p postcards-rust-cli && \
        mkdir -p /out && \
        cp /src/target/aarch64-unknown-linux-gnu/release/postcards-rust /out/postcards-rust; \
    fi

# Stage 2: Minimal runtime image
FROM debian:bookworm-slim AS runtime

# Install runtime libraries: libcurl4, libssl3, CA certificates, tzdata, and libheif-examples
RUN apt-get update && apt-get install -y --no-install-recommends \
    libcurl4 \
    libssl3 \
    ca-certificates \
    tzdata \
    libheif-examples \
    && rm -rf /var/lib/apt/lists/*

# Create dedicated non-root user and persistent directory
RUN groupadd -g 10001 postcards && \
    useradd -u 10001 -g postcards -s /bin/bash -m postcards && \
    mkdir -p /data/.postcards_rust && \
    chown -R postcards:postcards /data

# Copy compiled binary from builder
COPY --from=builder --chown=postcards:postcards /out/postcards-rust /usr/local/bin/postcards-rust

USER postcards:postcards
WORKDIR /data
ENV HOME=/data
ENV RUST_LOG=info

ENTRYPOINT ["postcards-rust"]
CMD ["daemon"]
