#!/usr/bin/env bash
set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
BOLD='\033[1m'
RESET='\033[0m'

info()  { printf "${GREEN}==>${RESET} ${BOLD}%s${RESET}\n" "$*"; }
error() { printf "${RED}error:${RESET} %s\n" "$*" >&2; exit 1; }

# Check dependencies
command -v cargo >/dev/null 2>&1 || error "cargo not found. Install Rust first: https://rustup.rs"
command -v git   >/dev/null 2>&1 || error "git not found."

REPO="https://github.com/123hi123/gd.git"
TMPDIR="$(mktemp -d)"
trap 'rm -rf "$TMPDIR"' EXIT

info "cloning gd..."
git clone --depth 1 "$REPO" "$TMPDIR/gd"

info "building gd (release)..."
cargo install --path "$TMPDIR/gd/crates/gd-cli"
cargo install --path "$TMPDIR/gd/crates/gd-daemon"

info "running setup..."
gd setup

info "done! restart your shell or run: exec \$SHELL"
