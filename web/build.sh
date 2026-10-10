#!/bin/sh
# Builds the wasm spike into web/pkg. Serve web/ with any static server.
set -e
cd "$(dirname "$0")/.."
cargo build -p falos-spike --lib --target wasm32-unknown-unknown --profile wasm
wasm-bindgen --target web --out-dir web/pkg target/wasm32-unknown-unknown/wasm/falos_spike.wasm
ls -la web/pkg/*.wasm
