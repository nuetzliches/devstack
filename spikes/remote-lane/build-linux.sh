#!/usr/bin/env bash
# The static Linux devstack (x86_64-unknown-linux-musl) a node mounts into a
# lane container (ADR-0028), built in Docker like the release does.
set -euo pipefail
export MSYS_NO_PATHCONV=1
ROOT="$(cd "$(dirname "$0")/../.." && (pwd -W 2>/dev/null || pwd))"
docker run --rm -v "$ROOT:/src" -v devstack-cargo-registry:/usr/local/cargo/registry \
  -v devstack-linux-target:/target -e CARGO_TARGET_DIR=/target -e DEBIAN_FRONTEND=noninteractive -w /src rust:latest \
  sh -c "apt-get update -qq >/dev/null && apt-get install -y -qq musl-tools >/dev/null && rustup target add x86_64-unknown-linux-musl >/dev/null 2>&1 && cargo build --release -p devstack --target x86_64-unknown-linux-musl && mkdir -p spikes/remote-lane/bin && cp /target/x86_64-unknown-linux-musl/release/devstack spikes/remote-lane/bin/devstack"
