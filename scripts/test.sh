#!/usr/bin/env bash
set -euo pipefail

# Build and test the workspace. The default feature set builds the OTLP
# exporter; the no-default-features run exercises the SDK core without OTLP.
cargo build --workspace --all-targets
cargo test --workspace --all-targets
cargo test -p opentelemetry-c-sdk --no-default-features
