#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "verify-shared-libraries.sh supports Linux only" >&2
  exit 1
fi

for tool in nm readelf objdump ldd python3; do
  command -v "$tool" >/dev/null || {
    echo "required tool is unavailable: $tool" >&2
    exit 1
  }
done

compare_symbols() {
  local library="$1"
  local expected="$2"
  local mode="$3"
  local actual
  actual="$(mktemp)"
  if [[ "$mode" == "defined" ]]; then
    nm -D --defined-only "$library" | awk '{print $3}' | grep '^otel_' | sort > "$actual"
  else
    nm -D --undefined-only "$library" | awk '{print $2}' | grep '^otel_api_' | sort > "$actual"
  fi
  if ! diff -u "$expected" "$actual"; then
    rm -f "$actual"
    echo "symbol inventory mismatch for $library" >&2
    exit 1
  fi
  rm -f "$actual"
}

verify_profile() {
  local profile="$1"
  local cargo_profile=()
  if [[ "$profile" == "release" ]]; then
    cargo_profile=(--release)
  fi
  local target_dir="target/shared-$profile"
  local lib_dir="$target_dir/$profile"
  local api="$lib_dir/libopentelemetry_c_api.so"
  local sdk="$lib_dir/libopentelemetry_c_sdk.so"

  CARGO_TARGET_DIR="$target_dir" cargo build --locked \
    -p opentelemetry-c-api -p opentelemetry-c-sdk --all-features "${cargo_profile[@]}"

  compare_symbols "$api" api/exported-symbols.txt defined
  compare_symbols "$sdk" sdk/exported-symbols.txt defined
  compare_symbols "$sdk" sdk/api-import-symbols.txt undefined
  if nm -D --defined-only "$sdk" | awk '{print $3}' | grep -q '^otel_api_'; then
    echo "SDK must not define API-owned registration symbols" >&2
    exit 1
  fi

  readelf -h "$api" | grep -q 'Type:.*DYN'
  readelf -h "$sdk" | grep -q 'Type:.*DYN'
  objdump -T "$api" | grep -q 'otel_global_meter_provider'
  objdump -T "$sdk" | grep -q 'otel_sdk_set_metrics_as_global'
  # Logs is a separate signal with its own global slot, so prove its symbols are exported
  # from the real artifacts too rather than assuming the allowlist diff covered them.
  objdump -T "$api" | grep -q 'otel_global_logger_provider'
  objdump -T "$sdk" | grep -q 'otel_sdk_set_logs_as_global'
  ldd "$api"
  ldd "$sdk"

  API_LIBRARY="$api" SDK_LIBRARY="$sdk" python3 - <<'PY'
import ctypes
import os

ctypes.CDLL(os.environ["API_LIBRARY"], mode=ctypes.RTLD_GLOBAL)
ctypes.CDLL(os.environ["SDK_LIBRARY"], mode=ctypes.RTLD_GLOBAL)
PY

  CI=1 CARGO_TARGET_DIR="$target_dir" cargo test --locked \
    -p opentelemetry-c-sdk --test c_header_compile --all-features "${cargo_profile[@]}"
  CI=1 CARGO_TARGET_DIR="$target_dir" cargo test --locked \
    -p opentelemetry-c-sdk --test cross_artifact --all-features "${cargo_profile[@]}"
  CI=1 CARGO_TARGET_DIR="$target_dir" cargo test --locked \
    -p opentelemetry-c-sdk --test custom_metric_exporter_cross_artifact \
    --all-features "${cargo_profile[@]}"
  CI=1 CARGO_TARGET_DIR="$target_dir" cargo test --locked \
    -p opentelemetry-c-sdk --test logs_cross_artifact --all-features "${cargo_profile[@]}"
  CI=1 CARGO_TARGET_DIR="$target_dir" cargo test --locked \
    -p opentelemetry-c-sdk --test custom_log_exporter_cross_artifact \
    --all-features "${cargo_profile[@]}"
}

echo "sha=$(git rev-parse HEAD)"
verify_profile debug
verify_profile release
