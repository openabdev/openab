# --- Build stage ---
FROM rust:1-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo build --release && rm -rf src
COPY src/ src/
RUN touch src/main.rs && cargo build --release

# --- Runtime stage ---
FROM amazonlinux:2023
RUN dnf upgrade -y && \
    dnf install -y --allowerasing ca-certificates curl procps-ng unzip git \
      python3.12 python3.12-pip nodejs20-npm shadow-utils tar gzip && \
    dnf clean all && \
    ln -sf /usr/bin/python3.12 /usr/bin/python3 && \
    ln -sf /usr/bin/python3 /usr/bin/python && \
    python3 -m pip install --no-cache-dir --upgrade pip

# Install tini
ARG TINI_VERSION=v0.19.0
RUN ARCH=$(uname -m) && \
    if [ "$ARCH" = "aarch64" ]; then TINI_ARCH="arm64"; else TINI_ARCH="amd64"; fi && \
    curl -sSfL "https://github.com/krallin/tini/releases/download/${TINI_VERSION}/tini-${TINI_ARCH}" \
      -o /usr/local/bin/tini && \
    chmod +x /usr/local/bin/tini

# Install ripgrep
ARG RIPGREP_VERSION=14.1.1
RUN ARCH=$(uname -m) && \
    if [ "$ARCH" = "aarch64" ]; then RG_TARGET="aarch64-unknown-linux-gnu"; else RG_TARGET="x86_64-unknown-linux-musl"; fi && \
    curl -sSfL "https://github.com/BurntSushi/ripgrep/releases/download/${RIPGREP_VERSION}/ripgrep-${RIPGREP_VERSION}-${RG_TARGET}.tar.gz" \
      -o /tmp/rg.tar.gz && \
    tar -xzf /tmp/rg.tar.gz -C /tmp && \
    cp /tmp/ripgrep-*/rg /usr/local/bin/ && \
    rm -rf /tmp/rg.tar.gz /tmp/ripgrep-*

# Install kiro-cli (auto-detect arch, copy binary directly)
ARG KIRO_CLI_VERSION=2.0.0
RUN ARCH=$(uname -m) && \
    if [ "$ARCH" = "aarch64" ]; then URL="https://prod.download.cli.kiro.dev/stable/${KIRO_CLI_VERSION}/kirocli-aarch64-linux.zip"; \
    else URL="https://prod.download.cli.kiro.dev/stable/${KIRO_CLI_VERSION}/kirocli-x86_64-linux.zip"; fi && \
    curl --proto '=https' --tlsv1.2 -sSf --retry 3 --retry-delay 5 "$URL" -o /tmp/kirocli.zip && \
    unzip /tmp/kirocli.zip -d /tmp && \
    cp /tmp/kirocli/bin/* /usr/local/bin/ && \
    chmod +x /usr/local/bin/kiro-cli* && \
    rm -rf /tmp/kirocli /tmp/kirocli.zip

# Install gh CLI
RUN ARCH=$(uname -m) && \
    if [ "$ARCH" = "aarch64" ]; then GH_ARCH="linux_arm64"; else GH_ARCH="linux_amd64"; fi && \
    GH_VERSION=$(curl -sL https://api.github.com/repos/cli/cli/releases/latest | python3 -c "import sys,json;print(json.load(sys.stdin)['tag_name'].lstrip('v'))") && \
    curl -sSfL "https://github.com/cli/cli/releases/download/v${GH_VERSION}/gh_${GH_VERSION}_${GH_ARCH}.tar.gz" \
      -o /tmp/gh.tar.gz && \
    tar -xzf /tmp/gh.tar.gz -C /tmp && \
    cp /tmp/gh_*/bin/gh /usr/local/bin/ && \
    rm -rf /tmp/gh.tar.gz /tmp/gh_*

# Install AWS CLI v2
RUN ARCH=$(uname -m) && \
    if [ "$ARCH" = "aarch64" ]; then URL="https://awscli.amazonaws.com/awscli-exe-linux-aarch64.zip"; \
    else URL="https://awscli.amazonaws.com/awscli-exe-linux-x86_64.zip"; fi && \
    curl -sSf "$URL" -o /tmp/awscli.zip && \
    unzip -q /tmp/awscli.zip -d /tmp && \
    /tmp/aws/install && \
    rm -rf /tmp/aws /tmp/awscli.zip

# Install kubectl
RUN ARCH=$(uname -m) && \
    if [ "$ARCH" = "aarch64" ]; then K8S_ARCH="arm64"; else K8S_ARCH="amd64"; fi && \
    curl -sSfL "https://dl.k8s.io/release/$(curl -sL https://dl.k8s.io/release/stable.txt)/bin/linux/${K8S_ARCH}/kubectl" \
      -o /usr/local/bin/kubectl && \
    chmod +x /usr/local/bin/kubectl

RUN useradd -m -s /bin/bash -u 1000 agent
RUN mkdir -p /home/agent/.local/share/kiro-cli /home/agent/.kiro && \
    chown -R agent:agent /home/agent
ENV HOME=/home/agent
WORKDIR /home/agent

COPY --from=builder --chown=agent:agent /build/target/release/openab /usr/local/bin/openab

USER agent
HEALTHCHECK --interval=30s --timeout=5s --retries=3 \
  CMD pgrep -x openab || exit 1
ENTRYPOINT ["tini", "--"]
CMD ["openab", "run", "/etc/openab/config.toml"]
