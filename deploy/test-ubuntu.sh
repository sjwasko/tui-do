#!/usr/bin/env bash
# Build and test tui-do in an ubuntu:26.04 container.
#
# Ubuntu LTS is a tier-1 target alongside Omarchy, and the two diverge sharply on
# glibc -- a green build on an Arch workstation does not imply a green build here.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec docker run --rm -t \
  -v "$REPO:/src:ro" \
  -v tui-do-ubuntu-target:/build/target \
  -v tui-do-ubuntu-cargo:/usr/local/cargo/registry \
  -w /src ubuntu:26.04 bash -eux -c '
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends curl ca-certificates build-essential pkg-config >/dev/null
    curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable >/dev/null
    . "$HOME/.cargo/env"
    export CARGO_TARGET_DIR=/build/target
    cargo build --workspace
    cargo test --workspace
  '
