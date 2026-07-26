#!/usr/bin/env bash
set -euo pipefail

seconds="${METRICS_FUZZ_SECONDS:-10}"
long_seconds="${METRICS_FUZZ_LONG_SECONDS:-0}"
for value in "$seconds" "$long_seconds"; do
  if ! [[ "$value" =~ ^[0-9]+$ ]]; then
    echo "METRICS_FUZZ_SECONDS and METRICS_FUZZ_LONG_SECONDS must be non-negative integers" >&2
    exit 1
  fi
done

targets=(metrics_inputs handle_kinds exporter_visitor)

cargo +nightly fuzz build
for target in "${targets[@]}"; do
  cargo +nightly fuzz run "$target" -- \
    -max_total_time="$seconds" -max_len=4096 -rss_limit_mb=2048
done

if ((long_seconds > 0)); then
  for target in metrics_inputs exporter_visitor; do
    cargo +nightly fuzz run "$target" -- \
      -max_total_time="$long_seconds" -max_len=4096 -rss_limit_mb=2048
  done
fi
