#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

# Sanitizer coverage for trace handles, API-owned context TLS, vtable conversion, and the real
# split-cdylib C caller. Requires Linux nightly with rust-src, matching the other signal scripts.

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "sanitize-traces.sh supports the Linux shared-library configuration only" >&2
  exit 1
fi

mode="${1:-address}"
case "$mode" in address|thread|leak|undefined) ;; *)
  echo "usage: $0 [address|thread|leak|undefined]" >&2; exit 1 ;;
esac
if ! rustup run nightly rustc -V >/dev/null 2>&1; then
  echo "nightly Rust with rust-src is required for Rust sanitizer builds" >&2; exit 1
fi

target="$(rustup run nightly rustc -vV | sed -n 's/^host: //p')"
export CARGO_BUILD_TARGET="$target"
export CARGO_TARGET_DIR="target/sanitizer-traces-$mode"
export CC="${CC:-clang}"
export CI=1

run_tests() {
  cargo +nightly test -Zbuild-std --target "$target" -p opentelemetry-c-api --lib context::
  cargo +nightly test -Zbuild-std --target "$target" -p opentelemetry-c-api \
    --test backed_null --test provider_race --test span_context_value
  cargo +nightly test -Zbuild-std --target "$target" \
    -p opentelemetry-c-sdk --lib --no-default-features vtable::tests::
  cargo +nightly build -Zbuild-std --target "$target" \
    -p opentelemetry-c-api -p opentelemetry-c-sdk
  cargo +nightly test -Zbuild-std --target "$target" \
    -p opentelemetry-c-sdk --test cross_artifact
}

case "$mode" in
  address)
    export RUSTFLAGS="-Zsanitizer=address -Cdebuginfo=1"
    export RUSTDOCFLAGS="$RUSTFLAGS"
    export CFLAGS="-fsanitize=address -fno-omit-frame-pointer"
    export ASAN_OPTIONS="${ASAN_OPTIONS:-detect_leaks=1:strict_string_checks=1}"
    run_tests ;;
  thread)
    export RUSTFLAGS="-Zsanitizer=thread -Cdebuginfo=1"
    export RUSTDOCFLAGS="$RUSTFLAGS"
    export CFLAGS="-fsanitize=thread -fno-omit-frame-pointer"
    export TSAN_OPTIONS="${TSAN_OPTIONS:-halt_on_error=1:second_deadlock_stack=1}"
    run_tests ;;
  leak)
    export RUSTFLAGS="-Zsanitizer=leak -Cdebuginfo=1"
    export RUSTDOCFLAGS="$RUSTFLAGS"
    export CFLAGS="-fsanitize=leak -fno-omit-frame-pointer"
    export LSAN_OPTIONS="${LSAN_OPTIONS:-exitcode=23}"
    run_tests ;;
  undefined)
    export CFLAGS="-fsanitize=undefined -fno-sanitize-recover=all"
    run_tests ;;
esac
