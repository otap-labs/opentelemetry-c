#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "verify-macos-shared-libraries.sh supports macOS only" >&2
  exit 1
fi

target_dir="${CARGO_TARGET_DIR:-target}"
profile="${OTEL_C_PROFILE:-debug}"
if [[ "$profile" != "debug" && "$profile" != "release" ]]; then
  echo "OTEL_C_PROFILE must be 'debug' or 'release'" >&2
  exit 1
fi

lib_dir="$target_dir/$profile"
api="$lib_dir/libopentelemetry_c_api.dylib"
sdk="$lib_dir/libopentelemetry_c_sdk.dylib"

if [[ "$profile" == "release" ]]; then
  CARGO_TARGET_DIR="$target_dir" cargo build --locked --release \
    -p opentelemetry-c-api
  OTEL_C_API_LINK_DIR="$lib_dir" CARGO_TARGET_DIR="$target_dir" cargo build --locked --release \
    -p opentelemetry-c-sdk --all-features
else
  CARGO_TARGET_DIR="$target_dir" cargo build --locked -p opentelemetry-c-api
  OTEL_C_API_LINK_DIR="$lib_dir" CARGO_TARGET_DIR="$target_dir" cargo build --locked \
    -p opentelemetry-c-sdk --all-features
fi

compare_exports() {
  local library="$1"
  local expected="$2"
  local actual
  actual="$(mktemp)"
  nm -gU "$library" | awk 'NF >= 3 {print $3}' | sed 's/^_//' | sort > "$actual"
  if ! diff -u "$expected" "$actual"; then
    rm -f "$actual"
    echo "symbol inventory mismatch for $library" >&2
    exit 1
  fi
  rm -f "$actual"
}

compare_exports "$api" api/exported-symbols.txt
compare_exports "$sdk" sdk/exported-symbols.txt
otool -L "$sdk" | grep -q '@rpath/libopentelemetry_c_api.dylib'

SDK_LIBRARY="$sdk" python3 - <<'PY'
import ctypes
import os

ctypes.CDLL(os.environ["SDK_LIBRARY"], mode=ctypes.RTLD_LOCAL)
PY
