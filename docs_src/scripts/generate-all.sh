#!/usr/bin/env bash
# Generate all documentation visual assets
# Prerequisites: brew install vhs
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
OUT_DIR="$SCRIPT_DIR/../assets/img"
mkdir -p "$OUT_DIR"

echo "=== Generating VHS terminal GIFs ==="
for tape in "$SCRIPT_DIR"/vhs/*.tape; do
    name=$(basename "$tape" .tape)
    echo "  Recording: $name"
    vhs "$tape" -o "$OUT_DIR/$name.gif"
done

echo "=== Rendering Mermaid diagrams ==="
if command -v mmdc &>/dev/null; then
    for mmd in "$SCRIPT_DIR"/mermaid/*.mmd; do
        name=$(basename "$mmd" .mmd)
        echo "  Rendering: $name"
        mmdc -i "$mmd" -o "$OUT_DIR/$name.svg" -t dark
    done
else
    echo "  Skipping Mermaid (install: npm install -g @mermaid-js/mermaid-cli)"
fi

echo "=== Generating SVG diagrams ==="
PROJ_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
for script in "$PROJ_ROOT"/scripts/generate-diagram-*.py; do
    if [ -f "$script" ]; then
        name=$(basename "$script" .py | sed 's/generate-//')
        echo "  Generating: $name"
        python3 "$script" -o "$OUT_DIR/$name.svg"
    fi
done
if [ -f "$PROJ_ROOT/scripts/generate-banner.py" ]; then
    echo "  Generating: banner"
    python3 "$PROJ_ROOT/scripts/generate-banner.py" -o "$PROJ_ROOT/images/banner.svg"
fi

echo "=== Done ==="
ls -lh "$OUT_DIR"
