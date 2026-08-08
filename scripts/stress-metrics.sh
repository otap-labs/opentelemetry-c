#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

iterations="${METRICS_STRESS_ITERATIONS:-20}"
if ! [[ "$iterations" =~ ^[1-9][0-9]*$ ]]; then
  echo "METRICS_STRESS_ITERATIONS must be a positive integer" >&2
  exit 1
fi

echo "sha=$(git rev-parse HEAD)"
echo "iterations=$iterations"

# Compile once before the loop so repeated executions emphasize lifecycle ordering rather than
# build-system work.
cargo test -p opentelemetry-c-api --lib --no-run
cargo test -p opentelemetry-c-api --test metrics_provider_race --no-run
cargo test -p opentelemetry-c-sdk --lib --no-default-features \
  --features metrics-async-runtime --no-run

for iteration in $(seq 1 "$iterations"); do
  echo "=== Metrics lifecycle stress iteration $iteration/$iterations ==="

  cargo test -q -p opentelemetry-c-api --lib \
    metrics::tests::observable_destroy_defers_user_data_until_in_flight_callback_completes \
    -- --exact
  cargo test -q -p opentelemetry-c-api --test metrics_provider_race \
    global_meter_provider_lifetime_is_race_free -- --exact

  cargo test -q -p opentelemetry-c-sdk --lib --no-default-features \
    --features metrics-async-runtime \
    custom_metric_exporter::tests::shutdown_waits_for_in_flight_export_and_blocks_later_callbacks \
    -- --exact
  cargo test -q -p opentelemetry-c-sdk --lib --no-default-features \
    --features metrics-async-runtime \
    manual_metric_reader::tests::multiple_manual_readers_collect_independently -- --exact
  cargo test -q -p opentelemetry-c-sdk --lib --no-default-features \
    --features metrics-async-runtime \
    metrics_vtable::tests::multiple_readers_collect_independently_and_invoke_observables \
    -- --exact
  cargo test -q -p opentelemetry-c-sdk --lib --no-default-features \
    --features metrics-async-runtime \
    periodic_metric_reader::tests::multiple_async_readers_flush_independently -- --exact
  cargo test -q -p opentelemetry-c-sdk --lib --no-default-features \
    --features metrics-async-runtime \
    periodic_metric_reader::tests::async_reader_maps_timeout_for_cooperative_exporter_and_flushes_successfully \
    -- --exact
  cargo test -q -p opentelemetry-c-sdk --lib --no-default-features \
    --features metrics-async-runtime \
    sdk::tests::async_reader_lifecycle_calls_fail_closed_on_owned_runtime -- --exact
  cargo test -q -p opentelemetry-c-sdk --lib --no-default-features \
    --features metrics-async-runtime \
    sdk::tests::metrics_flush_and_shutdown_statuses_are_stable -- --exact
  cargo test -q -p opentelemetry-c-sdk --lib --no-default-features \
    --features metrics-async-runtime \
    sdk::tests::concurrent_metrics_install_and_shutdown_leave_no_registration -- --exact
  cargo test -q -p opentelemetry-c-sdk --lib --no-default-features \
    --features metrics-async-runtime \
    sdk::tests::concurrent_same_sdk_installs_track_the_published_token -- --exact
  cargo test -q -p opentelemetry-c-sdk --lib --no-default-features \
    --features metrics-async-runtime \
    sdk::tests::older_sdk_shutdown_cannot_clear_newer_registration -- --exact
  cargo test -q -p opentelemetry-c-sdk --lib --no-default-features \
    --features metrics-async-runtime \
    sdk::tests::repeated_install_and_destroy_without_shutdown_are_safe -- --exact
done
