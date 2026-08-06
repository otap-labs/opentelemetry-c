<!-- SPDX-License-Identifier: Apache-2.0 -->

# Packaging Guide

This document describes how to build, install, and consume the
`opentelemetry-c` libraries using the supported packaging mechanisms.

## Contents

- [Installation Layout](#installation-layout)
- [Building from Source with CMake](#building-from-source-with-cmake)
- [CMake Consumer Integration](#cmake-consumer-integration)
- [pkg-config Consumer Integration](#pkg-config-consumer-integration)
- [Conan 2 Integration](#conan-2-integration)
- [vcpkg Integration](#vcpkg-integration)
- [Homebrew Integration](#homebrew-integration)
- [SDK Feature Configuration](#sdk-feature-configuration)
- [Platform Support](#platform-support)
- [Uninstalling](#uninstalling)
- [Release and Distribution Policy](#release-and-distribution-policy)

---

## Installation Layout

After `cmake --install`, the prefix contains:

```
<prefix>/
  include/
    opentelemetry_c/
      api.h            ← umbrella API header
      common.h
      trace.h
      metrics.h
      logs.h
      sdk.h            ← SDK lifecycle header
      batch_span_processor.h
      otlp_trace_exporter.h
      otlp_log_exporter.h
      otlp_metric_exporter.h
      ... (all SDK headers)
  lib/
    libopentelemetry_c_api.so        (Linux)
    libopentelemetry_c_sdk.so        (Linux)
    libopentelemetry_c_api.dylib     (macOS)
    libopentelemetry_c_sdk.dylib     (macOS)
    pkgconfig/
      opentelemetry-c-api.pc
      opentelemetry-c-sdk.pc
    cmake/
      OpenTelemetryC/
        OpenTelemetryCConfig.cmake
        OpenTelemetryCConfigVersion.cmake
  share/
    doc/
      opentelemetry-c/
        README.md
        VERSIONING.md
        RELEASING.md
        SECURITY.md
        LICENSE
        BUILDING.md
        PACKAGING.md
```

---

## Building from Source with CMake

### Prerequisites

| Tool    | Minimum version |
|---------|----------------|
| CMake   | 3.21           |
| Rust    | see `rust-version` in `api/Cargo.toml` |
| C compiler | C11-capable |
| C++ compiler | C++17-capable (for C++ header tests only) |

Install Rust via [rustup](https://rustup.rs/):
```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### Basic build and install

```sh
cmake -S . -B build \
  -DCMAKE_BUILD_TYPE=Release \
  -DCMAKE_INSTALL_PREFIX=/usr/local

cmake --build build --parallel
cmake --install build
```

To install into a staging directory (e.g., for package creation):
```sh
cmake --install build --prefix /tmp/staging
# or use DESTDIR:
DESTDIR=/tmp/pkg cmake --install build
```

### Debug build

```sh
cmake -S . -B build-debug \
  -DCMAKE_BUILD_TYPE=Debug \
  -DCMAKE_INSTALL_PREFIX=/usr/local
cmake --build build-debug
```

`Debug` maps to the Cargo `dev` profile. All other `CMAKE_BUILD_TYPE`
values (including `RelWithDebInfo`, `MinSizeRel`) map to the Cargo
`release` profile.

---

## CMake Consumer Integration

After installation, downstream CMake projects locate the package with
`find_package`:

```cmake
cmake_minimum_required(VERSION 3.21)
project(my_app C)

find_package(OpenTelemetryC CONFIG REQUIRED)

# Instrumentation library: link only the API
add_library(my_instrumentation SHARED instrumentation.c)
target_link_libraries(my_instrumentation PRIVATE OpenTelemetryC::api)

# Application: also links the SDK to install a real pipeline
add_executable(my_app main.c)
target_link_libraries(my_app PRIVATE OpenTelemetryC::sdk)
```

Point `CMAKE_PREFIX_PATH` at the install prefix if it is not a standard
system path:
```sh
cmake -S . -B build \
  -DCMAKE_PREFIX_PATH=/path/to/install \
  -DCMAKE_BUILD_TYPE=Release
```

Set the library search path at runtime:
- **Linux**: `export LD_LIBRARY_PATH=/path/to/install/lib:$LD_LIBRARY_PATH`
- **macOS**: `export DYLD_LIBRARY_PATH=/path/to/install/lib:$DYLD_LIBRARY_PATH`

### Exported targets

| Target                 | Use case |
|------------------------|----------|
| `OpenTelemetryC::api`  | Instrumentation libraries (link only API) |
| `OpenTelemetryC::sdk`  | Applications (API + SDK, includes transitive API) |

---

## pkg-config Consumer Integration

```sh
export PKG_CONFIG_PATH=/path/to/install/lib/pkgconfig

# API-only consumer
cc -std=c11 $(pkg-config --cflags opentelemetry-c-api) \
   my_instrumentation.c \
   $(pkg-config --libs opentelemetry-c-api) \
   -o my_instrumentation.so -shared

# API + SDK consumer
cc -std=c11 $(pkg-config --cflags opentelemetry-c-sdk) \
   main.c \
   $(pkg-config --libs opentelemetry-c-sdk) \
   -o my_app
```

The `opentelemetry-c-sdk.pc` file declares `Requires: opentelemetry-c-api`,
so `pkg-config --libs opentelemetry-c-sdk` automatically includes the API
link flags.

---

## Conan 2 Integration

The repository provides a local Conan 2 recipe at `packaging/conan/`.

### Export the recipe to your local cache

```sh
cd /path/to/opentelemetry-c
conan export packaging/conan --name opentelemetry-c --version 0.1.0
```

### Use in a consumer `conanfile.py`

```python
from conan import ConanFile

class MyProject(ConanFile):
    requires = "opentelemetry-c/0.1.0"
    generators = "CMakeDeps", "CMakeToolchain"
```

Then in your CMake project:
```cmake
find_package(opentelemetry-c CONFIG REQUIRED)
target_link_libraries(my_target PRIVATE opentelemetry-c::api)
```

### Feature options

| Conan option            | Default | Description |
|-------------------------|---------|-------------|
| `otlp_http`             | `False` | OTLP HTTP transport |
| `otlp_grpc`             | `False` | OTLP gRPC transport |
| `native_tls`            | `True`  | Platform TLS (implies otlp-http) |
| `rustls_tls`            | `False` | rustls TLS (implies otlp-http) |
| `metrics_async_runtime` | `False` | Async periodic metrics reader |
| `no_default_features`   | `False` | Transport-free core build |

---

## vcpkg Integration

The repository provides a local vcpkg overlay port at
`packaging/vcpkg/ports/opentelemetry-c/`.

### Install as an overlay port

```sh
vcpkg install opentelemetry-c \
  --overlay-ports=/path/to/opentelemetry-c/packaging/vcpkg/ports
```

### Use in a CMake project with vcpkg toolchain

```cmake
find_package(OpenTelemetryC CONFIG REQUIRED)
target_link_libraries(my_target PRIVATE OpenTelemetryC::api)
```

### Feature flags (vcpkg)

Pass feature flags with the `[feature]` syntax:
```sh
vcpkg install "opentelemetry-c[otlp-http,native-tls]" \
  --overlay-ports=packaging/vcpkg/ports
```

---

## Homebrew Integration

For Homebrew tap maintainers, a formula generator script is provided at
`packaging/homebrew/generate-formula.sh`.

Once a tagged release tarball exists:
```sh
./packaging/homebrew/generate-formula.sh \
  https://github.com/otap-labs/opentelemetry-c/archive/refs/tags/v0.1.0.tar.gz \
  <sha256-of-tarball> \
  > opentelemetry-c.rb
```

The generated formula requires review before submission to a Homebrew tap.

---

## SDK Feature Configuration

The following CMake options control which Cargo features are compiled into
`libopentelemetry_c_sdk`:

| CMake option                     | Cargo feature          | Default |
|----------------------------------|------------------------|---------|
| `OTEL_SDK_NATIVE_TLS`            | `native-tls`           | `ON`    |
| `OTEL_SDK_OTLP_HTTP`             | `otlp-http`            | `OFF`   |
| `OTEL_SDK_OTLP_GRPC`             | `otlp-grpc`            | `OFF`   |
| `OTEL_SDK_RUSTLS_TLS`            | `rustls-tls`           | `OFF`   |
| `OTEL_SDK_GRPC_TLS_RING`         | `grpc-tls-ring`        | `OFF`   |
| `OTEL_SDK_OTLP_HTTP_GZIP`        | `otlp-http-gzip`       | `OFF`   |
| `OTEL_SDK_OTLP_HTTP_ZSTD`        | `otlp-http-zstd`       | `OFF`   |
| `OTEL_SDK_OTLP_GRPC_GZIP`        | `otlp-grpc-gzip`       | `OFF`   |
| `OTEL_SDK_OTLP_GRPC_ZSTD`        | `otlp-grpc-zstd`       | `OFF`   |
| `OTEL_SDK_METRICS_ASYNC_RUNTIME` | `metrics-async-runtime`| `OFF`   |
| `OTEL_SDK_NO_DEFAULT_FEATURES`   | (no default features)  | `OFF`   |
| `OTEL_SDK_EXTRA_FEATURES`        | (arbitrary CSV)        | `""`    |

### Mutual exclusions

- `OTEL_SDK_NATIVE_TLS` and `OTEL_SDK_RUSTLS_TLS` are mutually exclusive.
  CMake will error if both are `ON`.

### Transport-free build

For testing or embedding without any network transport:
```sh
cmake -S . -B build \
  -DOTEL_SDK_NO_DEFAULT_FEATURES=ON \
  -DCMAKE_BUILD_TYPE=Release
```

---

## Platform Support

| Platform | Status |
|----------|--------|
| Linux (x86_64, aarch64) | ✅ Supported |
| macOS (x86_64, arm64)   | ✅ Supported |
| Windows (MSVC only)     | ⚠️ Experimental DLL/import-library packaging |

---

## Uninstalling

```sh
cmake --build build --target uninstall
```

This removes only the files listed in `build/install_manifest.txt`. It
never performs recursive directory deletions.

Alternatively, remove files manually using `install_manifest.txt`:
```sh
xargs rm -f < build/install_manifest.txt
```

---

## Release and Distribution Policy

- **No prebuilt binaries** are published. All installations must be built
  from source.
- **`publish = false`** is set on all Cargo crates; they are not published
  to crates.io.
- The version is coordinated across all three crates (`api`, `sdk`, `abi`)
  and must match. The CMake build enforces this at configure time.
- See [RELEASING.md](../RELEASING.md) and [VERSIONING.md](../VERSIONING.md)
  for release workflow and versioning policy.
