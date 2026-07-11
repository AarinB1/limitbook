#!/usr/bin/env bash
# Builds the wasm module + JS glue into web/pkg/ (gitignored).
#
# Prereqs:
#   rustup target add wasm32-unknown-unknown
#   cargo install wasm-bindgen-cli --version 0.2.126 --locked
#
# Run the demo (fixture is fetched relative to the repo root):
#   python3 -m http.server 8000        # from the repo root
#   open http://localhost:8000/web/
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build -p limitbook-wasm --target wasm32-unknown-unknown --release
wasm-bindgen --target web --out-dir web/pkg \
    target/wasm32-unknown-unknown/release/limitbook_wasm.wasm

echo "Built web/pkg. Serve the repo root and open /web/."
