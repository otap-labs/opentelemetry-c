#!/usr/bin/env bash
set -euo pipefail

# Repeatable benchmark protocol for the experimental Logs bridge, matching the Metrics script so
# results are collected under identical conditions and can be compared across signals.

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "benchmark-logs.sh is intended for the Linux test VM" >&2
  exit 1
fi

repeats="${LOGS_BENCH_REPEATS:-3}"
if ! [[ "$repeats" =~ ^[1-9][0-9]*$ ]]; then
  echo "LOGS_BENCH_REPEATS must be a positive integer" >&2
  exit 1
fi

echo "sha=$(git rev-parse HEAD)"
rustc -Vv
cargo -V
uname -a
lscpu
free -h
df -h /mnt/persist
uptime
echo "profile=bench"
echo "sdk_features=default (otlp-http via native-tls)"
echo "repeats=$repeats"

run_benchmark() {
  local name="$1"
  shift
  for run in $(seq 1 "$repeats"); do
    echo "=== $name run $run/$repeats ==="
    /usr/bin/time -v "$@"
  done
}

run_benchmark logs_hotpath \
  cargo bench -p opentelemetry-c-sdk --bench logs_hotpath -- --noplot
run_benchmark logs_allocations \
  cargo bench -p opentelemetry-c-sdk --bench logs_allocations
