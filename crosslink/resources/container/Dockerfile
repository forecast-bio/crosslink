FROM ubuntu:24.04

# Core tooling (python3 required for crosslink hooks)
RUN apt-get update && apt-get install -y --no-install-recommends \
    git openssh-client curl ca-certificates jq python3 sudo \
    && rm -rf /var/lib/apt/lists/*

# Remove default ubuntu user (occupies UID 1000, conflicts with host UID remapping)
RUN userdel -r ubuntu 2>/dev/null || true

# Non-root agent user with sudo for toolchain installs
RUN useradd -m -s /bin/bash agent && echo "agent ALL=(ALL) NOPASSWD:ALL" >> /etc/sudoers.d/agent
USER agent
WORKDIR /home/agent

# Default shell to bash (Claude CLI install script requires it)
SHELL ["/bin/bash", "-c"]

# Claude CLI
RUN curl -fsSL https://claude.ai/install.sh | bash

# Install gosu for clean privilege dropping in entrypoint
USER root
RUN curl -fsSL "https://github.com/tianon/gosu/releases/download/1.17/gosu-amd64" -o /usr/local/bin/gosu \
    && chmod +x /usr/local/bin/gosu

# Crosslink binary
COPY --chown=agent crosslink /usr/local/bin/crosslink
COPY entrypoint.sh /usr/local/bin/crosslink-entrypoint.sh
RUN chmod +x /usr/local/bin/crosslink-entrypoint.sh

WORKDIR /workspaces
# Entrypoint runs as root to handle UID remapping, then drops to agent via gosu
ENTRYPOINT ["/usr/local/bin/crosslink-entrypoint.sh"]
