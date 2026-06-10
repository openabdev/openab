FROM ghcr.io/openabdev/openab-native:beta

USER root
RUN apt-get update && apt-get install -y --no-install-recommends iproute2 && rm -rf /var/lib/apt/lists/*
RUN groupadd -g 1000660000 sandbox && \
    useradd -u 1000660000 -g sandbox -m -s /bin/bash sandbox

USER sandbox
WORKDIR /home/sandbox
