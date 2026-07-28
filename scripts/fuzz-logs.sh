#!/usr/bin/env bash
set -euo pipefail

# Structured fuzzing of the experimental Logs bridge.
#
# `logs_records` drives the record validator, whose hardest job is proving that a
# caller-supplied flat value node pool is in bounds, strictly forward, singly referenced, and
# within the depth and size budgets *before* any of it is converted. `handle_kinds` covers
# cross-signal handle confusion, which Logs makes newly interesting because loggers and meters
# come from entirely separate global slots and are distinguished only by their handle kind tag.
# `log_exporter_callbacks` covers the opposite direction: the custom exporter hands SDK-owned
# pointers *out* to C, so it fuzzes the callback table prefix and then asserts, from inside the
# callback, that the exported view really satisfies its published pool invariants.
#
# No fuzz target ever dereferences a fuzzer-supplied address: only lengths, tags, indices, and
# structure sizes are fuzzed, and every pointer is either NULL or points at a live buffer.

seconds="${LOGS_FUZZ_SECONDS:-10}"
long_seconds="${LOGS_FUZZ_LONG_SECONDS:-0}"
for value in "$seconds" "$long_seconds"; do
  if ! [[ "$value" =~ ^[0-9]+$ ]]; then
    echo "LOGS_FUZZ_SECONDS and LOGS_FUZZ_LONG_SECONDS must be non-negative integers" >&2
    exit 1
  fi
done

targets=(logs_records handle_kinds log_exporter_callbacks)

cargo +nightly fuzz build
for target in "${targets[@]}"; do
  cargo +nightly fuzz run "$target" -- \
    -max_total_time="$seconds" -max_len=4096 -rss_limit_mb=2048
done

if ((long_seconds > 0)); then
  cargo +nightly fuzz run logs_records -- \
    -max_total_time="$long_seconds" -max_len=4096 -rss_limit_mb=2048
fi
