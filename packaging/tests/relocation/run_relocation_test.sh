#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

# relocation/run_relocation_test.sh
#
# Install opentelemetry-c to a temporary prefix A, then move it to prefix B,
# update the rpath/install-name if needed, and verify that a CMake consumer
# built against B does not reference A. Confirms the CMake package config is
# truly relocatable.
#
# Usage:
#   ./packaging/tests/relocation/run_relocation_test.sh <build_dir>
#
# Requires: cmake, a C compiler.
# Must be run after `cmake --build <build_dir>`.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"

if [[ $# -lt 1 ]]; then
    echo "Usage: $0 <cmake_build_dir>" >&2
    exit 1
fi

BUILD_DIR="$(cd "$1" && pwd)"

# Create two prefix directories inside the build dir (avoids /tmp)
PREFIX_A="${BUILD_DIR}/reloc-prefix-a"
PREFIX_B="${BUILD_DIR}/reloc-prefix-b"
CONSUMER_BUILD="${BUILD_DIR}/reloc-consumer-build"

rm -rf "${PREFIX_A}" "${PREFIX_B}" "${CONSUMER_BUILD}"
mkdir -p "${PREFIX_A}"

echo "==> Installing to prefix A: ${PREFIX_A}"
cmake --install "${BUILD_DIR}" --prefix "${PREFIX_A}"

echo "==> Moving prefix A -> B"
mv "${PREFIX_A}" "${PREFIX_B}"

echo "==> Verifying no references to prefix A in cmake config"
if grep -r "${PREFIX_A}" "${PREFIX_B}/lib/cmake" 2>/dev/null; then
    echo "FAIL: Stale prefix A path found in cmake config after relocation" >&2
    exit 1
fi

echo "==> Building api_consumer against prefix B"
cmake -S "${REPO_ROOT}/packaging/tests/api_consumer" \
      -B "${CONSUMER_BUILD}" \
      -DCMAKE_PREFIX_PATH="${PREFIX_B}" \
      -DCMAKE_BUILD_TYPE=Release

cmake --build "${CONSUMER_BUILD}"

echo "==> Verifying consumer binary does not reference prefix A"
if strings "${CONSUMER_BUILD}/api_consumer" 2>/dev/null | grep -F "${PREFIX_A}"; then
    echo "FAIL: Consumer binary references old prefix A" >&2
    exit 1
fi

echo "==> Running consumer binary from prefix B"
# Set RPATH / DYLD_LIBRARY_PATH so the binary finds the moved library
if [[ "$(uname)" == "Darwin" ]]; then
    DYLD_LIBRARY_PATH="${PREFIX_B}/lib" "${CONSUMER_BUILD}/api_consumer"
else
    LD_LIBRARY_PATH="${PREFIX_B}/lib" "${CONSUMER_BUILD}/api_consumer"
fi

echo "==> Relocation test PASSED"
