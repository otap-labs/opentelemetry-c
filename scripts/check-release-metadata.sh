#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

fail() {
    printf 'release metadata error: %s\n' "$*" >&2
    exit 1
}

package_value() {
    local file="$1"
    local key="$2"
    awk -v wanted="$key" '
        /^\[package\][[:space:]]*$/ { in_package = 1; next }
        /^\[/ { if (in_package) exit }
        in_package {
            line = $0
            sub(/[[:space:]]*#.*/, "", line)
            if (line ~ "^[[:space:]]*" wanted "[[:space:]]*=") {
                sub("^[[:space:]]*" wanted "[[:space:]]*=[[:space:]]*", "", line)
                gsub(/^[[:space:]]+|[[:space:]]+$/, "", line)
                gsub(/^"|"$/, "", line)
                print line
                exit
            }
        }
    ' "$file"
}

lock_version() {
    local package="$1"
    awk -v wanted="$package" '
        /^\[/ {
            name = ""
            version = ""
        }
        /^\[\[package\]\][[:space:]]*$/ {
            next
        }
        /^name[[:space:]]*=/ {
            value = $0
            sub(/^[^=]*=[[:space:]]*"/, "", value)
            sub(/"[[:space:]]*$/, "", value)
            name = value
            next
        }
        /^version[[:space:]]*=/ && name == wanted {
            value = $0
            sub(/^[^=]*=[[:space:]]*"/, "", value)
            sub(/"[[:space:]]*$/, "", value)
            print value
            exit
        }
    ' Cargo.lock
}

manifests=(api/Cargo.toml sdk/Cargo.toml abi/Cargo.toml)
product_version=""
declared_msrv=""

for manifest in "${manifests[@]}"; do
    package="$(package_value "$manifest" name)"
    version="$(package_value "$manifest" version)"
    msrv="$(package_value "$manifest" rust-version)"
    publish="$(package_value "$manifest" publish)"
    [[ -n "$package" ]] || fail "$manifest has no package name"
    [[ -n "$version" ]] || fail "$manifest has no package version"
    [[ -n "$msrv" ]] || fail "$manifest has no rust-version"
    [[ "$publish" == "false" ]] || fail "$manifest must set package publish = false (found '${publish:-missing}')"
    if [[ -z "$product_version" ]]; then
        product_version="$version"
    elif [[ "$version" != "$product_version" ]]; then
        fail "$manifest version $version does not match product version $product_version"
    fi
    if [[ -z "$declared_msrv" ]]; then
        declared_msrv="$msrv"
    elif [[ "$msrv" != "$declared_msrv" ]]; then
        fail "$manifest rust-version $msrv does not match product rust-version $declared_msrv"
    fi
    locked_version="$(lock_version "$package")"
    [[ "$locked_version" == "$version" ]] ||
        fail "Cargo.lock version for $package is '${locked_version:-missing}', expected $version"
done

if [[ -n "${VALIDATED_MSRV:-}" && "$VALIDATED_MSRV" != "$declared_msrv" ]]; then
    fail "validated MSRV $VALIDATED_MSRV does not match manifest rust-version $declared_msrv"
fi

git ls-files --error-unmatch Cargo.lock >/dev/null 2>&1 ||
    fail "Cargo.lock must be tracked"

for doc in README.md VERSIONING.md RELEASING.md SECURITY.md CONTRIBUTING.md docs/BUILDING.md; do
    [[ -f "$doc" ]] || fail "expected release document $doc is missing"
done

grep -Fq 'const VERSION: &str = env!("CARGO_PKG_VERSION");' api/src/lib.rs ||
    fail "api/src/lib.rs must derive the public C version from CARGO_PKG_VERSION"
for symbol in otel_version_major otel_version_minor otel_version_patch otel_version_string; do
    grep -Fq "$symbol" api/include/opentelemetry_c/common.h ||
        fail "api/include/opentelemetry_c/common.h is missing $symbol"
done

for changelog in api/CHANGELOG.md sdk/CHANGELOG.md; do
    grep -Eq '^## (Unreleased|vNext)$' "$changelog" ||
        fail "$changelog has no Unreleased section"
done

tag="${1:-}"
if [[ -z "$tag" && "${GITHUB_REF_TYPE:-}" == "tag" ]]; then
    tag="${GITHUB_REF_NAME:-}"
fi
if [[ -n "$tag" && "$tag" != "v$product_version" ]]; then
    fail "tag $tag does not match product version v$product_version"
fi

printf 'release metadata is consistent for opentelemetry-c %s (declared MSRV %s)\n' \
    "$product_version" "$declared_msrv"
