#!/usr/bin/env bash
set -euo pipefail

# Build and test the default HTTP configuration, then the isolated Metrics transport matrix.
cargo build --workspace --all-targets
cargo test --workspace --all-targets
cargo test -p opentelemetry-c-sdk --no-default-features
cargo test -p opentelemetry-c-sdk --lib --no-default-features --features otlp-http
cargo test -p opentelemetry-c-sdk --lib --no-default-features --features otlp-grpc
cargo test -p opentelemetry-c-sdk --lib --no-default-features --features otlp-http,otlp-grpc
cargo build -p opentelemetry-c-api -p opentelemetry-c-sdk --all-features
cargo test -p opentelemetry-c-sdk --test cross_artifact --all-features
cargo test -p opentelemetry-c-sdk --test custom_metric_exporter_cross_artifact --all-features
