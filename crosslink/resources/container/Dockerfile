# E-ana tablet — container image for crosslink agent execution
#
# Build context expectation:
#   - `crosslink-amd64` and/or `crosslink-arm64` binaries placed alongside this
#     Dockerfile (the binary matching ${TARGETARCH} is COPYed into the image).
#   - For multi-arch buildx builds, set --platform linux/amd64,linux/arm64.
#   - For local single-arch builds (`just build-image`), only the host-arch
#     binary needs to be present; buildx auto-fills TARGETARCH.
FROM ubuntu:24.04

# Buildx auto-populates these for multi-arch builds; provide safe defaults so
# the Dockerfile is also usable with plain `docker build` on amd64.
ARG TARGETARCH=amd64
ARG GOSU_VERSION=1.17

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

# Install gosu for clean privilege dropping in entrypoint.
# Architecture is selected via the buildx-provided TARGETARCH so the same
# Dockerfile produces working images for amd64 and arm64.
USER root
RUN curl -fsSL "https://github.com/tianon/gosu/releases/download/${GOSU_VERSION}/gosu-${TARGETARCH}" -o /usr/local/bin/gosu \
    && chmod +x /usr/local/bin/gosu

# Crosslink binary — picked from the build context per target arch.
# Caller must place crosslink-${TARGETARCH} (e.g. crosslink-amd64, crosslink-arm64)
# in the build context next to this Dockerfile.
COPY --chown=agent crosslink-${TARGETARCH} /usr/local/bin/crosslink
RUN chmod +x /usr/local/bin/crosslink
COPY entrypoint.sh /usr/local/bin/crosslink-entrypoint.sh
RUN chmod +x /usr/local/bin/crosslink-entrypoint.sh

WORKDIR /workspaces
# Entrypoint runs as root to handle UID remapping, then drops to agent via gosu
ENTRYPOINT ["/usr/local/bin/crosslink-entrypoint.sh"]
