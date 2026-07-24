# Shared Rust build stage for all OpenAB images.
# Usage (requires BuildKit --build-context):
#   docker build --build-context builder=docker-image://openab-builder ...
#
# Or inline via multi-stage (preferred, zero-infra):
#   FROM rust:1-bookworm AS builder
#   COPY --from=openab-builder /build/target/release/openab /usr/local/bin/openab
#
# This file is the single source of truth for the cargo build cache pattern.
# All Dockerfile.* files reference it to stay DRY.

ARG BUILD_MODE=default
ARG FEATURES=""

FROM rust:1-bookworm AS builder
ARG BUILD_MODE
ARG FEATURES

WORKDIR /build

# 1. Copy manifests only → cache dependency compilation
COPY Cargo.toml Cargo.lock ./
COPY crates/openab-core/Cargo.toml crates/openab-core/Cargo.toml
COPY crates/openab-gateway/Cargo.toml crates/openab-gateway/Cargo.toml
COPY crates/openab-mcp/Cargo.toml crates/openab-mcp/Cargo.toml

# 2. Dummy sources for dep-only build
RUN mkdir -p src crates/openab-core/src crates/openab-gateway/src crates/openab-mcp/src \
    && echo 'fn main() {}' > src/main.rs \
    && echo '' > crates/openab-core/src/lib.rs \
    && echo '' > crates/openab-gateway/src/lib.rs \
    && echo '' > crates/openab-mcp/src/lib.rs \
    && cargo build --release \
    && rm -rf src crates/openab-core/src crates/openab-gateway/src crates/openab-mcp/src

# 3. Copy real sources and build
COPY crates/ crates/
COPY src/ src/
RUN touch src/main.rs crates/openab-core/src/lib.rs crates/openab-gateway/src/lib.rs crates/openab-mcp/src/lib.rs && \
    if [ "$BUILD_MODE" = "unified" ]; then \
      cargo build --release --features unified; \
    elif [ -n "$FEATURES" ]; then \
      cargo build --release --no-default-features --features "$FEATURES"; \
    else \
      cargo build --release; \
    fi
