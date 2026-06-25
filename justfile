# crosslink project commands
# Install just: https://github.com/casey/just

set dotenv-load := false

# ─── Docs ────────────────────────────────────────────────────────────

docs_src  := "docs_src"
docs_out  := "docs"
docs_tmp  := ".docs_build"

# Render documentation: generate SVGs → quarto → collision check → copy to docs/
render-docs force="": _docs-generate-svgs
    #!/usr/bin/env bash
    set -euo pipefail

    tmp="{{docs_tmp}}"
    out="{{docs_out}}"
    force="{{force}}"

    # Clean previous staging dir
    rm -rf "$tmp"

    # Render into temp dir (override _quarto.yml output-dir)
    quarto render {{docs_src}} --output-dir "../$tmp"

    # Copy non-quarto assets into staging
    mkdir -p "$tmp/assets/img"
    cp {{docs_src}}/assets/img/banner.svg "$tmp/assets/img/banner.svg"
    cp {{docs_src}}/assets/img/forecast-wordmark.svg "$tmp/assets/img/forecast-wordmark.svg"

    # Collision check: warn if quarto output would overwrite manually-maintained files
    # Skip known quarto artifacts that are always regenerated
    collisions=0
    while IFS= read -r -d '' rendered; do
        rel="${rendered#$tmp/}"
        target="$out/$rel"

        # Skip quarto infrastructure (always regenerated)
        case "$rel" in
            site_libs/*|search.json|styles.css|sitemap.xml|robots.txt) continue ;;
        esac

        if [ -f "$target" ]; then
            # Manual files: anything in docs/ that has no corresponding source in docs_src
            src_qmd="{{docs_src}}/${rel%.html}.qmd"
            src_direct="{{docs_src}}/$rel"
            if [ ! -f "$src_qmd" ] && [ ! -f "$src_direct" ]; then
                echo "COLLISION: rendered output would overwrite manual file: $rel"
                collisions=$((collisions + 1))
            fi
        fi
    done < <(find "$tmp" -type f -print0)

    if [ "$collisions" -gt 0 ] && [ "$force" != "--force" ]; then
        echo ""
        echo "ERROR: $collisions collision(s) detected with manually-maintained files in $out/"
        echo "       These files exist in $out/ without a corresponding source in {{docs_src}}/."
        echo "       Re-run with: just render-docs --force"
        rm -rf "$tmp"
        exit 1
    elif [ "$collisions" -gt 0 ]; then
        echo "WARNING: $collisions collision(s) — overwriting due to --force"
    fi

    # Sync rendered output into docs/, preserving manual-only files
    rsync -a --delete --exclude='.gitkeep' \
        --filter='protect *.md' \
        "$tmp/" "$out/"

    # If --force, also overwrite the colliding files
    if [ "$force" = "--force" ] && [ "$collisions" -gt 0 ]; then
        rsync -a "$tmp/" "$out/"
    fi

    # Cleanup staging dir
    rm -rf "$tmp"

    just _docs-verify
    just _docs-lint

# Generate all SVG assets from Python render scripts
_docs-generate-svgs:
    @echo "generating SVGs..."
    python3 scripts/generate-banner.py -o {{docs_src}}/assets/img/banner.svg
    python3 scripts/generate-card-icons.py -o {{docs_src}}/assets/img/cards
    python3 scripts/generate-diagram-session.py -o {{docs_src}}/assets/img/session-flow.svg
    python3 scripts/generate-diagram-multi-agent.py -o {{docs_src}}/assets/img/multi-agent-flow.svg
    python3 scripts/generate-diagram-design.py -o {{docs_src}}/assets/img/design-flow.svg
    python3 scripts/generate-diagram-kickoff.py -o {{docs_src}}/assets/img/kickoff-flow.svg
    python3 scripts/generate-diagram-swarm.py -o {{docs_src}}/assets/img/swarm-flow.svg
    python3 scripts/generate-diagram-knowledge.py -o {{docs_src}}/assets/img/knowledge-flow.svg

# Verify expected docs outputs exist
_docs-verify:
    #!/usr/bin/env bash
    set -euo pipefail
    missing=0
    for f in \
        {{docs_out}}/index.html \
        {{docs_out}}/assets/img/banner.svg \
        {{docs_out}}/assets/img/forecast-wordmark.svg \
        {{docs_src}}/assets/img/session-flow.svg \
        {{docs_src}}/assets/img/multi-agent-flow.svg \
        {{docs_src}}/assets/img/design-flow.svg \
        {{docs_src}}/assets/img/kickoff-flow.svg \
        {{docs_src}}/assets/img/swarm-flow.svg \
        {{docs_src}}/assets/img/knowledge-flow.svg \
    ; do
        if [ ! -f "$f" ]; then
            echo "MISSING: $f"
            missing=$((missing + 1))
        fi
    done
    if [ "$missing" -gt 0 ]; then
        echo "ERROR: $missing expected docs artifact(s) missing"
        exit 1
    fi
    echo "docs: all expected artifacts present"

# Lint rendered HTML: broken images, missing local styles/scripts, broken internal links
_docs-lint:
    #!/usr/bin/env bash
    set -euo pipefail
    out="{{docs_out}}"
    file_count=0

    # Temp files for collecting errors/warnings (avoids subshell variable scoping)
    err_log=$(mktemp)
    warn_log=$(mktemp)
    trap 'rm -f "$err_log" "$warn_log"' EXIT

    echo "linting rendered docs..."

    # Extract a single attribute value from matched tags
    extract_attr() {
        local attr="$1"
        sed -E "s/.*${attr}=\"([^\"]+)\".*/\1/"
    }

    # Check each ref resolves to a file on disk
    check_refs() {
        local html="$1" pattern="$2" attr="$3" kind="$4" log="$5"
        local dir rel
        dir="$(dirname "$html")"
        rel="${html#$out/}"
        { grep -oE "$pattern" "$html" 2>/dev/null || true; } \
            | extract_attr "$attr" \
            | while IFS= read -r ref; do
                # Skip external, data URIs, protocol-relative
                case "$ref" in http://*|https://*|data:*|//*|mailto:*|javascript:*) continue ;; esac
                # Strip fragment and query string
                local clean="${ref%%#*}"
                clean="${clean%%\?*}"
                [ -z "$clean" ] && continue
                # Skip quarto infrastructure (site_libs is always regenerated)
                case "$clean" in */site_libs/*|site_libs/*) continue ;; esac
                if [ ! -f "$dir/$clean" ]; then
                    echo "BROKEN $kind: $rel -> $clean" | tee -a "$log"
                fi
            done
    }

    while IFS= read -r html; do
        file_count=$((file_count + 1))
        check_refs "$html" '<img [^>]*src="[^"]+"'     "src"  "IMAGE"      "$err_log"
        check_refs "$html" 'href="[^"]+\.css"'          "href" "STYLESHEET" "$err_log"
        check_refs "$html" '<script [^>]*src="[^"]+"'   "src"  "SCRIPT"     "$err_log"
        check_refs "$html" 'href="[^"]+\.html"'         "href" "LINK"       "$warn_log"
    done < <(find "$out" -name '*.html' -type f)

    error_count=$(wc -l < "$err_log" | tr -d ' ')
    warn_count=$(wc -l < "$warn_log" | tr -d ' ')

    echo ""
    echo "docs-lint: scanned $file_count HTML files"
    [ "$warn_count" -gt 0 ] && echo "  warnings: $warn_count broken internal link(s)"
    if [ "$error_count" -gt 0 ]; then
        echo "  errors: $error_count broken asset reference(s)"
        exit 1
    fi
    echo "  result: all asset references valid"

# ─── Build ───────────────────────────────────────────────────────────

# Build crosslink (debug)
build:
    cd crosslink && cargo build --locked

# Build crosslink (release)
build-release:
    cd crosslink && cargo build --locked --release

# ─── Lint ────────────────────────────────────────────────────────────

# Run all lints (fmt check + clippy strict)
lint: fmt-check clippy

# Check formatting
fmt-check:
    cd crosslink && cargo fmt --all -- --check

# Auto-format
fmt:
    cd crosslink && cargo fmt --all

# Clippy with CI-matching flags
clippy:
    cd crosslink && cargo clippy -- -D warnings -W clippy::unwrap_used -W clippy::expect_used

# ─── Test ────────────────────────────────────────────────────────────

# Run unit + integration tests
test:
    cd crosslink && cargo test --bin crosslink --verbose -- --skip proptest
    cd crosslink && cargo test --test cli_integration --verbose

# Run unit tests only
test-unit:
    cd crosslink && cargo test --bin crosslink --verbose -- --skip proptest

# Run integration tests only
test-integration:
    cd crosslink && cargo test --test cli_integration --verbose

# Run property-based tests (extended, 1000 cases)
test-proptest cases="1000":
    cd crosslink && PROPTEST_CASES={{cases}} cargo test proptest --bin crosslink -- --test-threads=1

# ─── Security ────────────────────────────────────────────────────────

# Run cargo-audit
audit:
    cd crosslink && cargo audit

# ─── Container image ─────────────────────────────────────────────────

# Build the crosslink-agent container image locally for the host architecture.
# Drops a static musl binary into the build context, then `docker buildx build
# --load` produces a single-arch image tagged ghcr.io/forecast-bio/crosslink-agent:<tag>.
# Default tag is `local` so this never collides with published `:nightly`/`:latest`.
build-image tag="local":
    #!/usr/bin/env bash
    set -euo pipefail
    HOST_ARCH="$(uname -m)"
    case "$HOST_ARCH" in
        x86_64|amd64) RUST_TARGET=x86_64-unknown-linux-musl; DOCKER_ARCH=amd64; PLATFORM=linux/amd64 ;;
        aarch64|arm64) RUST_TARGET=aarch64-unknown-linux-musl; DOCKER_ARCH=arm64; PLATFORM=linux/arm64 ;;
        *) echo "Unsupported host arch: $HOST_ARCH"; exit 1 ;;
    esac
    echo "==> Building crosslink for ${RUST_TARGET}"
    rustup target add "${RUST_TARGET}" >/dev/null
    cd crosslink && cargo build --locked --release --target "${RUST_TARGET}"
    cd ..
    cp "crosslink/target/${RUST_TARGET}/release/crosslink" \
       "crosslink/resources/container/crosslink-${DOCKER_ARCH}"
    echo "==> Building image ghcr.io/forecast-bio/crosslink-agent:{{tag}} for ${PLATFORM}"
    docker buildx build \
        --platform "${PLATFORM}" \
        --build-arg "TARGETARCH=${DOCKER_ARCH}" \
        --load \
        -t "ghcr.io/forecast-bio/crosslink-agent:{{tag}}" \
        crosslink/resources/container
    echo "==> Built ghcr.io/forecast-bio/crosslink-agent:{{tag}} (${DOCKER_ARCH})"

# Push a locally-built image to GHCR. Use only for emergency manual publishes;
# routine publishing is owned by .github/workflows/container-image.yml.
# Requires: `docker login ghcr.io` first (PAT with write:packages scope).
push-image tag:
    docker push "ghcr.io/forecast-bio/crosslink-agent:{{tag}}"

# ─── CI composite ────────────────────────────────────────────────────

# Run what CI runs (lint → build → test)
ci: lint build test
