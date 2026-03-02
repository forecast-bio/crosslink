#!/bin/bash
set -euo pipefail

# This entrypoint runs as root to handle UID remapping and system setup,
# then drops to the agent user via gosu for the final command.

# --- UID remapping ---
# Match the container agent user's UID/GID to the host user so bind-mounted
# files are accessible without permission issues (same approach as devcontainers).
if [ -n "${HOST_UID:-}" ] && [ "$(id -u agent)" != "$HOST_UID" ]; then
    echo "[crosslink-entrypoint] Remapping agent UID to $HOST_UID:${HOST_GID:-$HOST_UID}..."
    usermod -u "$HOST_UID" agent 2>/dev/null || true
    groupmod -g "${HOST_GID:-$HOST_UID}" agent 2>/dev/null || true
    chown -R agent:agent /home/agent 2>/dev/null || true
fi

# --- Auth setup ---
# Copy credentials from read-only host mount into writable config dir.
mkdir -p /home/agent/.claude
if [ -f /host-auth/.credentials.json ]; then
    cp /host-auth/.credentials.json /home/agent/.claude/.credentials.json
    chown agent:agent /home/agent/.claude/.credentials.json
    chmod 600 /home/agent/.claude/.credentials.json
fi

# --- Git config (written to agent's home as root, owned by agent) ---
AGENT_ID="${AGENT_ID:-container-agent}"
AGENT_HOME=$(getent passwd agent | cut -d: -f6)
GIT_CONFIG="$AGENT_HOME/.gitconfig"
cat > "$GIT_CONFIG" <<GITEOF
[user]
    name = crosslink-agent-${AGENT_ID}
    email = agent@crosslink.local
[safe]
    directory = *
GITEOF
chown agent:agent "$GIT_CONFIG"

# --- Toolchain detection ---
# Scan the first mounted workspace for project files
WORKSPACE=$(find /workspaces -maxdepth 1 -mindepth 1 -type d 2>/dev/null | head -1)
if [ -n "$WORKSPACE" ]; then
    echo "[crosslink-entrypoint] Detected workspace: $WORKSPACE"

    if [ -f "$WORKSPACE/Cargo.toml" ] || [ -f "$WORKSPACE/crosslink/Cargo.toml" ]; then
        if ! gosu agent bash -c 'command -v cargo' &>/dev/null; then
            echo "[crosslink-entrypoint] Installing Rust toolchain..."
            gosu agent bash -c 'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --quiet 2>&1'
        else
            echo "[crosslink-entrypoint] Rust toolchain already installed."
        fi
    fi

    if [ -f "$WORKSPACE/package.json" ]; then
        if ! command -v node &>/dev/null; then
            echo "[crosslink-entrypoint] Installing Node.js..."
            curl -fsSL https://deb.nodesource.com/setup_22.x | bash - 2>&1
            apt-get install -y --no-install-recommends nodejs 2>&1
        else
            echo "[crosslink-entrypoint] Node.js already installed."
        fi
    fi

    if [ -f "$WORKSPACE/pyproject.toml" ] || [ -f "$WORKSPACE/requirements.txt" ]; then
        if ! command -v uv &>/dev/null; then
            echo "[crosslink-entrypoint] Installing Python + uv..."
            apt-get update && apt-get install -y --no-install-recommends python3-pip python3-venv 2>&1
            gosu agent pip install --user --break-system-packages uv 2>&1
        else
            echo "[crosslink-entrypoint] Python + uv already installed."
        fi
    fi

    if [ -f "$WORKSPACE/go.mod" ]; then
        if ! command -v go &>/dev/null; then
            echo "[crosslink-entrypoint] Installing Go..."
            curl -fsSL https://go.dev/dl/go1.23.4.linux-amd64.tar.gz | tar -C /usr/local -xzf - 2>&1
        else
            echo "[crosslink-entrypoint] Go already installed."
        fi
    fi
fi

# --- Crosslink init ---
# Set up hooks, skills, and policy in the workspace so container agents are
# bound by the same rules as host agents.
if [ -n "$WORKSPACE" ] && command -v crosslink &>/dev/null; then
    echo "[crosslink-entrypoint] Initializing crosslink hooks in workspace..."
    gosu agent bash -c "cd '$WORKSPACE' && crosslink init --force" 2>&1 || true
fi

echo "[crosslink-entrypoint] Setup complete. Running command as agent..."
# Drop to agent user. PATH includes Claude CLI and cargo locations.
export PATH="/home/agent/.local/bin:/home/agent/.cargo/bin:/usr/local/go/bin:$PATH"
export CLAUDE_CONFIG_DIR=/home/agent/.claude
export HOME=/home/agent
exec gosu agent "$@"
