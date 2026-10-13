#!/bin/sh
# Builds the browser editor: Rust core -> WASM, bindings, TypeScript.
set -e
cd "$(dirname "$0")/.."
cargo build -p papir-wasm --target wasm32-unknown-unknown --release
wasm-bindgen --target web --out-dir web/pkg target/wasm32-unknown-unknown/release/papir_wasm.wasm
# TypeScript → JavaScript: Node is needed for this step only; without it, in a container
if command -v node >/dev/null 2>&1; then
  [ -x web/node_modules/.bin/tsc ] || (cd web && npm install --no-audit --no-fund --silent)
  (cd web && ./node_modules/.bin/tsc -p .)
else
  docker run --rm --user "$(id -u):$(id -g)" -e HOME=/tmp -v "$PWD/web":/web -w /web node:20-alpine \
    sh -c 'npm install --no-audit --no-fund --silent && npx tsc -p .'
fi
echo "web/pkg/papir_wasm_bg.wasm: $(gzip -9 -c web/pkg/papir_wasm_bg.wasm | wc -c) bytes gzipped"
