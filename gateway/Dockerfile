# --- Build stage ---
FROM rust:1-bookworm AS builder
WORKDIR /build
COPY gateway/Cargo.toml gateway/Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release && rm -rf src
COPY gateway/src/ src/
RUN touch src/main.rs && cargo build --release

# --- Runtime stage ---
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/target/release/openab-gateway /usr/local/bin/openab-gateway
EXPOSE 8080
ENTRYPOINT ["openab-gateway"]
