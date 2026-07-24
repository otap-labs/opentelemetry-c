#!/usr/bin/env bash
set -euo pipefail

# Format and lint all crates in the workspace.
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
