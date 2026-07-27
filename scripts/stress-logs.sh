#!/usr/bin/env bash
set -euo pipefail

# Repeated execution of the Logs lifecycle tests whose failure modes are ordering-dependent
# rather than deterministic: global-slot installation racing shutdown, emission racing shutdown,
# and the API-owned global LoggerProvider slot under concurrent readers and re-registrations.
#
# A single pass of these tests proves very little, because a lost race usually still produces a
# passing interleaving. Running them repeatedly on an already-built binary is what turns them
# into evidence.

iterations="${LOGS_STRESS_ITERATIONS:-20}"
if ! [[ "$iterations" =~ ^[1-9][0-9]*$ ]]; then
  echo "LOGS_STRESS_ITERATIONS must be a positive integer" >&2
  exit 1
fi

echo "sha=$(git rev-parse HEAD)"
echo "iterations=$iterations"

# Compile once before the loop so repeated executions emphasize lifecycle ordering rather than
# build-system work.
cargo test -p opentelemetry-c-api --test logs_provider_race --no-run
cargo test -p opentelemetry-c-sdk --lib --no-default-features --no-run

for iteration in $(seq 1 "$iterations"); do
  echo "=== Logs lifecycle stress iteration $iteration/$iterations ==="

  cargo test -q -p opentelemetry-c-api --test logs_provider_race \
    global_logger_provider_lifetime_is_race_free -- --exact

  cargo test -q -p opentelemetry-c-sdk --lib --no-default-features \
    sdk::tests::concurrent_emit_during_logs_shutdown_stays_defined -- --exact
  cargo test -q -p opentelemetry-c-sdk --lib --no-default-features \
    sdk::tests::concurrent_logs_install_and_shutdown_leave_no_registration -- --exact
  cargo test -q -p opentelemetry-c-sdk --lib --no-default-features \
    sdk::tests::logs_shutdown_is_one_shot_and_blocks_later_installation_and_flush -- --exact
  cargo test -q -p opentelemetry-c-sdk --lib --no-default-features \
    sdk::tests::dropping_the_sdk_without_explicit_shutdown_clears_the_logs_global_slot -- --exact
  cargo test -q -p opentelemetry-c-sdk --lib --no-default-features \
    sdk::tests::logs_and_metrics_global_slots_are_independent -- --exact
  cargo test -q -p opentelemetry-c-sdk --lib --no-default-features \
    sdk::tests::saturated_batch_queue_survives_repeated_pipeline_lifecycles -- --exact
done
