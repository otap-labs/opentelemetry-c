#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "sanitize-metrics.sh supports the Linux shared-library configuration only" >&2
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
export CARGO_TARGET_DIR="target/sanitizer-$mode"
export CC="${CC:-clang}"
export CI=1
stress_iterations="${METRICS_SANITIZER_STRESS_ITERATIONS:-0}"
if ! [[ "$stress_iterations" =~ ^[0-9]+$ ]]; then
  echo "METRICS_SANITIZER_STRESS_ITERATIONS must be a non-negative integer" >&2
  exit 1
fi

run_instrumented_stress() {
  local iteration
  for ((iteration = 1; iteration <= stress_iterations; iteration++)); do
    echo "=== instrumented Metrics lifecycle iteration $iteration/$stress_iterations ==="
    cargo +nightly test -Zbuild-std --target "$target" \
      -p opentelemetry-c-api --test metrics_provider_race \
      global_meter_provider_lifetime_is_race_free -- --exact
    cargo +nightly test -Zbuild-std --target "$target" \
      -p opentelemetry-c-sdk --lib --no-default-features --features metrics-async-runtime \
      custom_metric_exporter::tests::shutdown_waits_for_in_flight_export_and_blocks_later_callbacks \
      -- --exact
    cargo +nightly test -Zbuild-std --target "$target" \
      -p opentelemetry-c-sdk --lib --no-default-features --features metrics-async-runtime \
      periodic_metric_reader::tests::multiple_async_readers_flush_independently -- --exact
    cargo +nightly test -Zbuild-std --target "$target" \
      -p opentelemetry-c-sdk --lib --no-default-features --features metrics-async-runtime \
      sdk::tests::concurrent_metrics_install_and_shutdown_leave_no_registration -- --exact
  done
}

run_rust_sanitizer() {
  local sanitizer="$1"
  export RUSTFLAGS="-Zsanitizer=$sanitizer -Cdebuginfo=1"
  export RUSTDOCFLAGS="$RUSTFLAGS"
  export CFLAGS="-fsanitize=$sanitizer -fno-omit-frame-pointer"

  cargo +nightly test -Zbuild-std --target "$target" \
    -p opentelemetry-c-api --tests
  cargo +nightly test -Zbuild-std --target "$target" \
    -p opentelemetry-c-sdk --lib --no-default-features
  cargo +nightly test -Zbuild-std --target "$target" \
    -p opentelemetry-c-sdk --lib --no-default-features --features metrics-async-runtime
  cargo +nightly build -Zbuild-std --target "$target" \
    -p opentelemetry-c-api -p opentelemetry-c-sdk --no-default-features \
    --features metrics-async-runtime
  cargo +nightly test -Zbuild-std --target "$target" \
    -p opentelemetry-c-sdk --test custom_metric_exporter_cross_artifact \
    --no-default-features --features metrics-async-runtime
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
    cargo build -p opentelemetry-c-api -p opentelemetry-c-sdk --no-default-features
    cargo test -p opentelemetry-c-sdk --test custom_metric_exporter_cross_artifact \
      --no-default-features
    ;;
esac
