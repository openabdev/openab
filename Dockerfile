# --- Build stage ---
FROM rust:1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release && rm -rf src
COPY src/ src/
RUN touch src/main.rs && cargo build --release

# --- Runtime stage ---
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl unzip && rm -rf /var/lib/apt/lists/*

# Install kiro-cli (auto-detect arch, copy binary directly)
RUN ARCH=$(dpkg --print-architecture) && \
    if [ "$ARCH" = "arm64" ]; then URL="https://desktop-release.q.us-east-1.amazonaws.com/latest/kirocli-aarch64-linux.zip"; \
    else URL="https://desktop-release.q.us-east-1.amazonaws.com/latest/kirocli-x86_64-linux.zip"; fi && \
    curl --proto '=https' --tlsv1.2 -sSf "$URL" -o /tmp/kirocli.zip && \
    unzip /tmp/kirocli.zip -d /tmp && \
    cp /tmp/kirocli/bin/* /usr/local/bin/ && \
    chmod +x /usr/local/bin/kiro-cli* && \
    rm -rf /tmp/kirocli /tmp/kirocli.zip

RUN mkdir -p /home/agent/.local/share/kiro-cli /home/agent/.kiro
ENV HOME=/home/agent
WORKDIR /home/agent

COPY --from=builder /build/target/release/agent-broker /usr/local/bin/agent-broker

ENTRYPOINT ["agent-broker"]
CMD ["/etc/agent-broker/config.toml"]
