#!/usr/bin/env bash
# Build and run the Mullion host from a terminal that holds Screen Recording
# permission (macOS prompts the first time). Logs go to logs/host-<unix>.jsonl.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build -p mullion-host --release
export RUST_LOG="${RUST_LOG:-mullion_host=info,str0m=warn}"
exec ./target/release/mullion-host "$@"
