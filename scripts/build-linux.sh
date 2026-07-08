#!/usr/bin/env sh
# Builds the Linux lehua runtime using Docker (run from the repo root).
# Output: dist-runtime/lehua-linux-x86_64
set -eu
cd "$(dirname "$0")/.."
mkdir -p dist-runtime
docker run --rm -v "$(pwd)":/work -w /work rust:1 sh -c '
    apt-get update -qq && apt-get install -y -qq g++ >/dev/null
    cargo build --release --target-dir /tmp/lehua-target
    cp /tmp/lehua-target/release/lehua dist-runtime/lehua-linux-x86_64
'
echo "built dist-runtime/lehua-linux-x86_64"
