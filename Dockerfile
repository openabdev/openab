# --- Build stage ---
ARG BUILD_MODE=default
ARG FEATURES=""

FROM rust:1-bookworm AS builder
ARG BUILD_MODE
ARG FEATURES

WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/openab-core/Cargo.toml crates/openab-core/Cargo.toml
COPY crates/openab-gateway/Cargo.toml crates/openab-gateway/Cargo.toml
RUN mkdir -p src crates/openab-core/src crates/openab-gateway/src \
    && echo 'fn main() {}' > src/main.rs \
    && echo '' > crates/openab-core/src/lib.rs \
    && echo '' > crates/openab-gateway/src/lib.rs \
    && cargo build --release \
    && rm -rf src crates/openab-core/src crates/openab-gateway/src
COPY crates/ crates/
COPY src/ src/
RUN touch src/main.rs crates/openab-core/src/lib.rs crates/openab-gateway/src/lib.rs && \
    if [ "$BUILD_MODE" = "unified" ]; then \
      cargo build --release --features unified; \
    elif [ -n "$FEATURES" ]; then \
      cargo build --release --no-default-features --features "$FEATURES"; \
    else \
      cargo build --release; \
    fi

# --- Runtime stage ---
FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl procps ripgrep tini unzip && rm -rf /var/lib/apt/lists/*

# Install kiro-cli (auto-detect arch, copy binary directly)
ARG KIRO_CLI_VERSION=2.13.0
ARG KIRO_SHA256_AMD64=d8c5277358b4a82b2d9a9ed2d52e110862536dc82b9e32c3719fc5f5a9834c94
ARG KIRO_SHA256_ARM64=95972602568c2065b7d8cc28924730304d40e612c0984ee0144d8ba452000be3
RUN ARCH=$(dpkg --print-architecture) && \
    if [ "$ARCH" = "arm64" ]; then \
      URL="https://prod.download.cli.kiro.dev/stable/${KIRO_CLI_VERSION}/kirocli-aarch64-linux.zip"; \
      SHA256="$KIRO_SHA256_ARM64"; \
    else \
      URL="https://prod.download.cli.kiro.dev/stable/${KIRO_CLI_VERSION}/kirocli-x86_64-linux.zip"; \
      SHA256="$KIRO_SHA256_AMD64"; \
    fi && \
    curl --proto '=https' --tlsv1.2 -sSf --retry 3 --retry-delay 5 "$URL" -o /tmp/kirocli.zip && \
    echo "$SHA256  /tmp/kirocli.zip" | sha256sum -c - && \
    unzip /tmp/kirocli.zip -d /tmp && \
    cp /tmp/kirocli/bin/* /usr/local/bin/ && \
    chmod +x /usr/local/bin/kiro-cli* && \
    rm -rf /tmp/kirocli /tmp/kirocli.zip

# Install gh CLI
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
      -o /usr/share/keyrings/githubcli-archive-keyring.gpg && \
    echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
      > /etc/apt/sources.list.d/github-cli.list && \
    apt-get update && apt-get install -y --no-install-recommends gh && \
    rm -rf /var/lib/apt/lists/*

RUN useradd -m -s /bin/bash -u 1000 agent
RUN mkdir -p /home/agent/.local/share/kiro-cli /home/agent/.kiro && \
    chown -R agent:agent /home/agent
ENV HOME=/home/agent
WORKDIR /home/agent

COPY --from=builder --chown=agent:agent /build/target/release/openab /usr/local/bin/openab

USER agent
HEALTHCHECK --interval=30s --timeout=5s --retries=3 \
  CMD pgrep -x openab || exit 1
ENV OPENAB_AGENT_COMMAND="kiro-cli acp --trust-all-tools"
ENV OPENAB_AGENT_AUTH_COMMAND="kiro-cli login --use-device-flow"

ENTRYPOINT ["tini", "--"]
CMD ["openab", "run", "-c", "/etc/openab/config.toml"]
