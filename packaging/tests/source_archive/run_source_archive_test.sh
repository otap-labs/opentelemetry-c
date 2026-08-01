#!/usr/bin/env bash
# source_archive/run_source_archive_test.sh
#
# Creates a git archive of HEAD, extracts it outside the repository checkout,
# then runs a full cmake configure / build / install cycle from the archive and
# verifies the api_consumer test passes.
#
# Usage:
#   ./packaging/tests/source_archive/run_source_archive_test.sh [work_parent_dir]
#
# work_parent_dir: parent directory where a temporary work directory
#                  (otelc-archive-test-XXXXXX) will be created.
#                  Default: /tmp
# Requires: git, cmake, cargo, a C compiler.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../../.." && pwd)"

# IMPORTANT: the work directory MUST be outside the repository checkout so that
# git rev-parse does not discover the parent .git directory.
#
# To avoid accidental recursive deletion of arbitrary directories, the optional
# argument is treated as a parent directory only; this script always creates and
# removes its own uniquely named mktemp child directory.
if [[ -n "${1:-}" ]]; then
    WORK_PARENT="$1"
else
    WORK_PARENT="/tmp"
fi

mkdir -p "${WORK_PARENT}"
WORK_PARENT="$(cd "${WORK_PARENT}" && pwd)"

if [[ "${WORK_PARENT}" == "${REPO_ROOT}" || "${WORK_PARENT}" == "${REPO_ROOT}"/* ]]; then
    echo "ERROR: WORK_PARENT '${WORK_PARENT}' is inside the repository checkout." >&2
    echo "       Choose a directory under /tmp or another location outside the repo." >&2
    exit 1
fi

WORK_DIR="$(mktemp -d "${WORK_PARENT%/}/otelc-archive-test-XXXXXX")"

cleanup() {
    if [[ -n "${WORK_DIR:-}" && -d "${WORK_DIR}" ]]; then
        case "$(basename "${WORK_DIR}")" in
            otelc-archive-test-*) rm -rf "${WORK_DIR}" ;;
            *) echo "WARN: refusing to remove unexpected directory '${WORK_DIR}'" >&2 ;;
        esac
    fi
}
trap cleanup EXIT

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

echo "==> Building api_consumer from extracted archive (not original checkout)"
cmake -S "${EXTRACT_DIR}/packaging/tests/api_consumer" \
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

echo "==> Source archive test PASSED"
