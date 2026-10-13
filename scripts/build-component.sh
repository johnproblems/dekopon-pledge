#!/usr/bin/env bash
# Builds pledge.wasm: the provider core module plus the component wrapper.
# Usage: scripts/build-component.sh [output.wasm]
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)
out=${1:-"$root/pledge.wasm"}
toolchain=$(sed -n 's/^channel = "\(.*\)"$/\1/p' "$root/rust-toolchain.toml")
expected_wasm_tools="1.259.0"

command -v rustup >/dev/null 2>&1 || { echo "error: rustup is required" >&2; exit 1; }
command -v wasm-tools >/dev/null 2>&1 || {
  echo "error: wasm-tools $expected_wasm_tools is required" >&2
  exit 1
}
actual_wasm_tools=$(wasm-tools --version | awk '{print $2}')
if [[ "$actual_wasm_tools" != "$expected_wasm_tools" ]]; then
  if [[ "${ALLOW_WASM_TOOLS_MISMATCH:-}" == "1" ]]; then
    echo "warning: expected wasm-tools $expected_wasm_tools, found $actual_wasm_tools" >&2
  else
    echo "error: expected wasm-tools $expected_wasm_tools, found $actual_wasm_tools (set ALLOW_WASM_TOOLS_MISMATCH=1 to override)" >&2
    exit 1
  fi
fi

rustup target add --toolchain "$toolchain" wasm32-unknown-unknown >/dev/null
(cd "$root" && rustup run "$toolchain" cargo build --locked --release --target wasm32-unknown-unknown)

core="$root/target/wasm32-unknown-unknown/release/dekopon_provider_pledge.wasm"
wasm-tools component new "$core" -o "$out"

size=$(wc -c <"$out")
printf 'built %s (%s bytes)\n' "$out" "$size"
if ((size >= 1024 * 1024)); then
  echo "error: component exceeds the 1 MiB budget" >&2
  exit 1
fi
if command -v sha256sum >/dev/null 2>&1; then
  (cd "$(dirname "$out")" && sha256sum "$(basename "$out")" >"$(basename "$out").sha256")
elif command -v shasum >/dev/null 2>&1; then
  (cd "$(dirname "$out")" && shasum -a 256 "$(basename "$out")" >"$(basename "$out").sha256")
else
  echo "error: sha256sum or shasum is required to write the sidecar file" >&2
  exit 1
fi
