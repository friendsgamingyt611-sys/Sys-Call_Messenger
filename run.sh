#!/usr/bin/env bash
# ============================================================
#  run.sh — SysCall Messenger (scm) Launcher
# ============================================================

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Load Cargo environment if available
if ! command -v cargo >/dev/null 2>&1; then
    if [ -f "$HOME/.cargo/env" ]; then
        source "$HOME/.cargo/env"
    fi
fi

BIN="$SCRIPT_DIR/target/release/scm"

# Build latest release binary
cargo build --release --quiet

# Sync latest binary to ~/.cargo/bin/scm if cargo bin directory exists
if [ -d "$HOME/.cargo/bin" ]; then
    cp -f "$BIN" "$HOME/.cargo/bin/scm" 2>/dev/null || true
fi

# Execute scm binary with all arguments passed through (0 args launches Interactive Shell)
exec "$BIN" "$@"
