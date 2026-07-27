#!/usr/bin/env bash
set -euo pipefail

# Sanitizer runs for the experimental Logs bridge.
#
# The Logs surface is the most pointer-dense part of the C API: a single `otel_logger_emit` call
# borrows a record, an attribute array, and an arbitrarily nested pool of value nodes, all owned
# by the caller and all required to be untouched after the call returns. AddressSanitizer and
# LeakSanitizer are therefore the primary evidence that the two-pass validator/converter neither
# reads past a caller buffer nor retains one, and ThreadSanitizer covers the emit/shutdown race.

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "sanitize-logs.sh supports the Linux shared-library configuration only" >&2
  exit 1
fi

mode="${1:-address}"
case "$mode" in
  address|thread|leak|undefined) ;;
  *)
    echo "usage: $0 [address|thread|leak|undefined]" >&2
    exit 1
    ;;
esac

if ! rustup run nightly rustc -V >/dev/null 2>&1; then
  echo "nightly Rust with rust-src is required for Rust sanitizer builds" >&2
  exit 1
fi

target="$(rustup run nightly rustc -vV | sed -n 's/^host: //p')"
export CARGO_BUILD_TARGET="$target"
export CARGO_TARGET_DIR="target/sanitizer-logs-$mode"
export CC="${CC:-clang}"
export CI=1
stress_iterations="${LOGS_SANITIZER_STRESS_ITERATIONS:-0}"
if ! [[ "$stress_iterations" =~ ^[0-9]+$ ]]; then
  echo "LOGS_SANITIZER_STRESS_ITERATIONS must be a non-negative integer" >&2
  exit 1
fi

run_instrumented_stress() {
  local iteration
  for ((iteration = 1; iteration <= stress_iterations; iteration++)); do
    echo "=== instrumented Logs lifecycle iteration $iteration/$stress_iterations ==="
    cargo +nightly test -Zbuild-std --target "$target" \
      -p opentelemetry-c-api --test logs_provider_race \
      global_logger_provider_lifetime_is_race_free -- --exact
    cargo +nightly test -Zbuild-std --target "$target" \
      -p opentelemetry-c-sdk --lib --no-default-features \
      sdk::tests::concurrent_emit_during_logs_shutdown_stays_defined -- --exact
    cargo +nightly test -Zbuild-std --target "$target" \
      -p opentelemetry-c-sdk --lib --no-default-features \
      sdk::tests::concurrent_logs_install_and_shutdown_leave_no_registration -- --exact
  done
}

run_rust_sanitizer() {
  local sanitizer="$1"
  export RUSTFLAGS="-Zsanitizer=$sanitizer -Cdebuginfo=1"
  export RUSTDOCFLAGS="$RUSTFLAGS"
  export CFLAGS="-fsanitize=$sanitizer -fno-omit-frame-pointer"

  cargo +nightly test -Zbuild-std --target "$target" \
    -p opentelemetry-c-api --test logs_noop --test logs_abi --test logs_provider_race
  cargo +nightly test -Zbuild-std --target "$target" \
    -p opentelemetry-c-sdk --lib --no-default-features
  # The cross-artifact test is the only one that exercises the real two-cdylib layout with a C
  # caller owning the record buffers, so it is where a stale borrow would actually show up.
  cargo +nightly build -Zbuild-std --target "$target" \
    -p opentelemetry-c-api -p opentelemetry-c-sdk
  cargo +nightly test -Zbuild-std --target "$target" \
    -p opentelemetry-c-sdk --test logs_cross_artifact
  run_instrumented_stress
}

case "$mode" in
  address)
    export ASAN_OPTIONS="${ASAN_OPTIONS:-detect_leaks=1:strict_string_checks=1:check_initialization_order=1}"
    run_rust_sanitizer address
    ;;
  thread)
    export TSAN_OPTIONS="${TSAN_OPTIONS:-halt_on_error=1:second_deadlock_stack=1}"
    run_rust_sanitizer thread
    ;;
  leak)
    export LSAN_OPTIONS="${LSAN_OPTIONS:-exitcode=23}"
    run_rust_sanitizer leak
    ;;
  undefined)
    unset RUSTFLAGS RUSTDOCFLAGS
    export CFLAGS="-fsanitize=undefined -fno-sanitize-recover=all"
    cargo build -p opentelemetry-c-api -p opentelemetry-c-sdk
    cargo test -p opentelemetry-c-sdk --test logs_cross_artifact
    ;;
esac
