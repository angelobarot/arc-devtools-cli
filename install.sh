#!/bin/sh
# Install or update arc-devtools to the latest, and register its opencode skill.
#
#   ./install.sh           # install the latest from GitHub (default)
#   ./install.sh --local   # install from this local checkout instead
#
# No clone needed:
#   curl -fsSL https://raw.githubusercontent.com/angelobarot/arc-devtools-cli/main/install.sh | sh
set -eu

REPO="https://github.com/angelobarot/arc-devtools-cli"
CARGO_BIN="${CARGO_HOME:-$HOME/.cargo}/bin"
ARC="$CARGO_BIN/arc-devtools"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: 'cargo' not found. Install Rust first: https://rustup.rs" >&2
  exit 1
fi

if [ "${1:-}" = "--local" ]; then
  src_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
  echo "Installing arc-devtools from local checkout: $src_dir"
  cargo install --path "$src_dir" --locked --force
else
  echo "Installing the latest arc-devtools from $REPO"
  cargo install --git "$REPO" --locked --force
fi

echo "Registering the opencode skill..."
"$ARC" install

echo
echo "Done."
if ! command -v arc-devtools >/dev/null 2>&1; then
  echo "Note: $CARGO_BIN is not on your PATH. Add it, then restart your shell:"
  echo "  echo 'export PATH=\"$CARGO_BIN:\$PATH\"' >> ~/.zshrc"
fi
echo "Next: fully quit and reopen opencode to load the skill."
