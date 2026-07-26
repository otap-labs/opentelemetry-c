#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "benchmark-metrics.sh is intended for the Linux test VM" >&2
  exit 1
fi

repeats="${METRICS_BENCH_REPEATS:-3}"
if ! [[ "$repeats" =~ ^[1-9][0-9]*$ ]]; then
  echo "METRICS_BENCH_REPEATS must be a positive integer" >&2
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
echo "api_features=default"
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

run_benchmark api_hotpath \
  cargo bench -p opentelemetry-c-api --bench api_hotpath -- --noplot
run_benchmark sdk_hotpath \
  cargo bench -p opentelemetry-c-sdk --bench sdk_hotpath -- --noplot
run_benchmark metrics_allocations \
  cargo bench -p opentelemetry-c-sdk --bench metrics_allocations
