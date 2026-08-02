#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

# Structured fuzzing of the experimental Traces surface.
#
# `trace_propagation` drives W3C Trace Context extraction and injection: it feeds arbitrary
# byte strings as the traceparent and tracestate header values and round-trips any successfully
# extracted context back out, exercising the strict bounded parser and the length-query /
# too-small-buffer inject contract. `span_start_ex` drives the versioned extended span-start
# descriptor (`otel_span_start_options_ex_t`): it fuzzes `struct_size` field gating, the
# reserved word, the parent/parent_context exclusion, NULL-array-with-non-zero-count rejection,
# and the link array walk (each link carrying its own context handle and attribute array) that
# the SDK reconstructs into span contexts and links. `sampler_config` drives the versioned
# built-in sampler descriptor (`otel_sampler_config_t`): it fuzzes `struct_size` field gating,
# the reserved words, the ratio bound, and the parent-based root-type validation. `span_limits`
# drives the versioned span-limits descriptor (`otel_span_limits_t`): it fuzzes `struct_size`
# field gating and the reserved word. `trace_exporter_callbacks` drives the custom Traces
# exporter callback ABI: it fuzzes `struct_size` gating and callback-presence of the callback
# table (which `otel_custom_trace_exporter_new` must reject without taking ownership on any
# malformation), then validates that every exported span batch view the SDK produces satisfies
# the published invariants (declared struct sizes, zeroed reserved fields, in-bounds
# record/event/link counts, a non-NULL scope, and known scalar/array attribute tags), and that
# `state_destroy` runs exactly once iff it was both provided and covered by `struct_size`.
#
# No fuzz target ever dereferences a fuzzer-supplied address: only lengths, tags, counts, and
# structure sizes are fuzzed, and every pointer is either NULL or points at a live buffer.

seconds="${TRACES_FUZZ_SECONDS:-10}"
long_seconds="${TRACES_FUZZ_LONG_SECONDS:-0}"
for value in "$seconds" "$long_seconds"; do
  if ! [[ "$value" =~ ^[0-9]+$ ]]; then
    echo "TRACES_FUZZ_SECONDS and TRACES_FUZZ_LONG_SECONDS must be non-negative integers" >&2
    exit 1
  fi
done

targets=(trace_propagation span_start_ex sampler_config span_limits trace_exporter_callbacks)

cargo +nightly fuzz build
for target in "${targets[@]}"; do
  cargo +nightly fuzz run "$target" -- \
    -max_total_time="$seconds" -max_len=4096 -rss_limit_mb=2048
done

if ((long_seconds > 0)); then
  cargo +nightly fuzz run span_start_ex -- \
    -max_total_time="$long_seconds" -max_len=4096 -rss_limit_mb=2048
fi
