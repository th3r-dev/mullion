#!/usr/bin/env bash
# Rebuild crates/app/assets/vendor/noble.js from the versions pinned in
# package-lock.json (npm ci checks every package against its recorded hash).
set -euo pipefail
cd "$(dirname "$0")"
npm ci --no-audit --no-fund --silent
out=../../crates/app/assets/vendor/noble.js
mkdir -p "$(dirname "$out")"
{
  echo "/* Mullion vendor bundle: @noble/curves, @noble/hashes, @noble/ciphers $(node -p "require('./package.json').dependencies['@noble/curves']")"
  echo "   (c) Paul Miller, MIT License, https://paulmillr.com/noble/"
  echo "   Built by tools/vendor-crypto/build.sh; do not edit. Exposes window.MullionNoble. */"
  # Assign window.MullionNoble explicitly: the app evals each script inside a
  # function (Dioxus eval), where a top-level var would stay local.
  npx esbuild entry.js --bundle --minify --format=iife --global-name=__mullionNoble --target=es2020 --legal-comments=none \
    --footer:js='window.MullionNoble=__mullionNoble;' 
} > "$out"
echo "$out: $(wc -c < "$out") bytes"
