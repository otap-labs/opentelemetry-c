#!/usr/bin/env bash
# source_archive/run_source_archive_test.sh
#
# Creates a git archive of HEAD, extracts it outside the repository checkout,
# then runs a full cmake configure / build / install cycle from the archive and
# verifies the api_consumer test passes.
#
# Usage:
#   ./packaging/tests/source_archive/run_source_archive_test.sh [build_dir]
#
# build_dir: where to place the extracted archive and build outputs
#            (default: a subdirectory of the repo root named archive-test-work)
# Requires: git, cmake, cargo, a C compiler.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"

WORK_DIR="${1:-${REPO_ROOT}/archive-test-work}"
rm -rf "${WORK_DIR}"
mkdir -p "${WORK_DIR}"

ARCHIVE_FILE="${WORK_DIR}/opentelemetry-c-HEAD.tar.gz"
EXTRACT_DIR="${WORK_DIR}/source"
BUILD_DIR="${WORK_DIR}/build"
INSTALL_DIR="${WORK_DIR}/install"
CONSUMER_BUILD="${WORK_DIR}/consumer-build"

echo "==> Creating git archive of HEAD"
(cd "${REPO_ROOT}" && git archive --format=tar.gz HEAD -o "${ARCHIVE_FILE}")

echo "==> Extracting archive to ${EXTRACT_DIR}"
mkdir -p "${EXTRACT_DIR}"
tar -xzf "${ARCHIVE_FILE}" -C "${EXTRACT_DIR}"

echo "==> Verifying archive is outside the git checkout"
# The extracted directory must not be a git repo
if (cd "${EXTRACT_DIR}" && git rev-parse --git-dir) 2>/dev/null; then
    echo "FAIL: Extracted archive is a git repo (it shouldn't be)" >&2
    exit 1
fi

echo "==> Configuring from archive source"
cmake -S "${EXTRACT_DIR}" \
      -B "${BUILD_DIR}" \
      -DCMAKE_BUILD_TYPE=Release \
      -DCMAKE_INSTALL_PREFIX="${INSTALL_DIR}" \
      -DOTEL_SDK_NO_DEFAULT_FEATURES=ON

echo "==> Building"
cmake --build "${BUILD_DIR}" --parallel

echo "==> Installing to ${INSTALL_DIR}"
cmake --install "${BUILD_DIR}"

echo "==> Verifying install layout"
for path in \
    "${INSTALL_DIR}/include/opentelemetry_c/api.h" \
    "${INSTALL_DIR}/lib/libopentelemetry_c_api"* \
    "${INSTALL_DIR}/lib/libopentelemetry_c_sdk"* \
    "${INSTALL_DIR}/lib/pkgconfig/opentelemetry-c-api.pc" \
    "${INSTALL_DIR}/lib/pkgconfig/opentelemetry-c-sdk.pc" \
    "${INSTALL_DIR}/lib/cmake/OpenTelemetryC/OpenTelemetryCConfig.cmake" \
    "${INSTALL_DIR}/lib/cmake/OpenTelemetryC/OpenTelemetryCConfigVersion.cmake"
do
    if ! ls ${path} >/dev/null 2>&1; then
        echo "FAIL: Missing expected install artifact: ${path}" >&2
        exit 1
    fi
done

echo "==> Building api_consumer against installed prefix"
cmake -S "${REPO_ROOT}/packaging/tests/api_consumer" \
      -B "${CONSUMER_BUILD}" \
      -DCMAKE_PREFIX_PATH="${INSTALL_DIR}" \
      -DCMAKE_BUILD_TYPE=Release

cmake --build "${CONSUMER_BUILD}"

echo "==> Running api_consumer"
if [[ "$(uname)" == "Darwin" ]]; then
    DYLD_LIBRARY_PATH="${INSTALL_DIR}/lib" "${CONSUMER_BUILD}/api_consumer"
else
    LD_LIBRARY_PATH="${INSTALL_DIR}/lib" "${CONSUMER_BUILD}/api_consumer"
fi

echo "==> Cleaning up work directory"
rm -rf "${WORK_DIR}"

echo "==> Source archive test PASSED"
