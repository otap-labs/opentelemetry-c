#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

# Check repository licensing metadata before language-specific linting.
scripts/check-license-headers.py

# Format and lint all crates in the workspace.
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
