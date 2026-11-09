#!/usr/bin/env bash
# Build and run the Mullion host from a terminal that holds Screen Recording
# permission (macOS prompts the first time). Logs go to logs/host-<unix>.jsonl.
#
# Switches: copy .env.example to .env and edit, or export MULLION_* in the
# shell, or pass flags (flags win). `--show-config` prints the effective set.
set -euo pipefail
cd "$(dirname "$0")/.."
if [ -f .env ]; then
  set -a; . ./.env; set +a
fi
cargo build -p mullion-host --release
export RUST_LOG="${RUST_LOG:-}"
[ -z "$RUST_LOG" ] && unset RUST_LOG
exec ./target/release/mullion-host "$@"
