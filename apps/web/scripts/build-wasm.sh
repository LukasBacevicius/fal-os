#!/bin/sh
# Builds crates/client to wasm and generates the JS bindings into src/wasm/pkg.
#   sh scripts/build-wasm.sh        optimized (profile `wasm`: fat LTO, slower to build)
#   sh scripts/build-wasm.sh dev    fast incremental build (profile `wasm-dev`)
# Needs: rustup target wasm32-unknown-unknown, wasm-bindgen CLI 0.2.129, and llvm-ar
# (`rustup component add llvm-tools`): Apple's `ar` writes archives rustc can't read for the
# wasm objects that loro's `psm` dependency ships.
set -e
cd "$(dirname "$0")/../../.."
profile=wasm
[ "$1" = "dev" ] && profile=wasm-dev

host=$(rustc -vV | sed -n 's/^host: //p')
llvm_ar="$(rustc --print sysroot)/lib/rustlib/$host/bin/llvm-ar"
if [ -x "$llvm_ar" ]; then
  export AR_wasm32_unknown_unknown="$llvm_ar"
elif command -v llvm-ar >/dev/null 2>&1; then
  export AR_wasm32_unknown_unknown="$(command -v llvm-ar)"
else
  echo "error: llvm-ar not found. Run: rustup component add llvm-tools" >&2
  exit 1
fi

cargo build -p falos-client --lib --target wasm32-unknown-unknown --profile "$profile"
wasm-bindgen --target web --out-dir apps/web/src/wasm/pkg \
  "target/wasm32-unknown-unknown/$profile/falos_client.wasm"
ls -la apps/web/src/wasm/pkg/falos_client_bg.wasm
