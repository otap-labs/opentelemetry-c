cmake_minimum_required(VERSION 3.21)

# Read coordinated version from api/Cargo.toml before project() declaration.
file(READ "${CMAKE_CURRENT_SOURCE_DIR}/api/Cargo.toml" _api_toml)
string(REGEX MATCH "\nversion = \"([0-9]+\\.[0-9]+\\.[0-9]+)\"" _m "${_api_toml}")
if(NOT CMAKE_MATCH_1)
    message(FATAL_ERROR "Cannot parse version from api/Cargo.toml")
endif()
set(OTEL_C_VERSION "${CMAKE_MATCH_1}")

project(OpenTelemetryC
    VERSION "${OTEL_C_VERSION}"
    DESCRIPTION "OpenTelemetry C API and SDK"
    LANGUAGES C CXX
    HOMEPAGE_URL "https://github.com/otap-labs/opentelemetry-c"
)

include(GNUInstallDirs)
include(CMakePackageConfigHelpers)

# ── Toolchain checks ──────────────────────────────────────────────────────────
find_program(CARGO_EXECUTABLE cargo)
find_program(RUSTC_EXECUTABLE rustc)

if(NOT CARGO_EXECUTABLE)
    message(FATAL_ERROR
        "cargo not found. Install the Rust toolchain from https://rustup.rs/ "
        "then re-run CMake.")
endif()
if(NOT RUSTC_EXECUTABLE)
    message(FATAL_ERROR
        "rustc not found. Install the Rust toolchain from https://rustup.rs/ "
        "then re-run CMake.")
endif()

execute_process(
    COMMAND "${RUSTC_EXECUTABLE}" --version
    OUTPUT_VARIABLE _rustc_version OUTPUT_STRIP_TRAILING_WHITESPACE)
execute_process(
    COMMAND "${CARGO_EXECUTABLE}" --version
    OUTPUT_VARIABLE _cargo_version OUTPUT_STRIP_TRAILING_WHITESPACE)
message(STATUS "Rust toolchain: ${_rustc_version} | ${_cargo_version}")

# ── Coordinated version check ─────────────────────────────────────────────────
foreach(_crate sdk abi)
    file(READ "${CMAKE_CURRENT_SOURCE_DIR}/${_crate}/Cargo.toml" _toml)
    string(REGEX MATCH "\nversion = \"([0-9]+\\.[0-9]+\\.[0-9]+)\"" _m "${_toml}")
    if(NOT CMAKE_MATCH_1 STREQUAL OTEL_C_VERSION)
        message(FATAL_ERROR
            "${_crate}/Cargo.toml version '${CMAKE_MATCH_1}' "
            "does not match api/Cargo.toml version '${OTEL_C_VERSION}'. "
            "All three packages must have identical versions.")
    endif()
endforeach()

message(STATUS "opentelemetry-c version: ${OTEL_C_VERSION}")

# ── Platform / library naming ─────────────────────────────────────────────────
if(APPLE)
    set(OTEL_C_LIB_SUFFIX ".dylib")
elseif(UNIX)
    set(OTEL_C_LIB_SUFFIX ".so")
else()
    message(FATAL_ERROR
        "Windows shared-library deployment is not currently supported. "
        "See VERSIONING.md and docs/PACKAGING.md.")
endif()

set(OTEL_C_API_LIB "libopentelemetry_c_api${OTEL_C_LIB_SUFFIX}")
set(OTEL_C_SDK_LIB "libopentelemetry_c_sdk${OTEL_C_LIB_SUFFIX}")

# ── Cargo target directory (under CMake build dir to avoid touching global state) ─
set(OTEL_C_CARGO_TARGET_DIR "${CMAKE_BINARY_DIR}/cargo-target"
    CACHE PATH "Cargo target directory (default: <build>/cargo-target)")

# ── Cargo profile / build type mapping ───────────────────────────────────────
# Release  -> --release  (Cargo release profile)
# Debug    -> dev profile (default, produces target/debug/)
# All other CMake build types map to Release for packaging safety.
if(CMAKE_BUILD_TYPE STREQUAL "Debug")
    set(OTEL_C_CARGO_PROFILE "debug")
    set(OTEL_C_CARGO_PROFILE_FLAG "")
    message(STATUS "Cargo profile: dev (debug) -- CMAKE_BUILD_TYPE=Debug")
else()
    set(OTEL_C_CARGO_PROFILE "release")
    set(OTEL_C_CARGO_PROFILE_FLAG "--release")
    if(NOT CMAKE_BUILD_TYPE STREQUAL "Release")
        message(STATUS
            "CMAKE_BUILD_TYPE='${CMAKE_BUILD_TYPE}' has no exact Cargo mapping; "
            "using Cargo release profile for packaging.")
    else()
        message(STATUS "Cargo profile: release")
    endif()
endif()

set(OTEL_C_CARGO_LIB_DIR "${OTEL_C_CARGO_TARGET_DIR}/${OTEL_C_CARGO_PROFILE}")

# ── SDK feature options ───────────────────────────────────────────────────────
option(OTEL_SDK_OTLP_HTTP
    "Enable OTLP HTTP/protobuf transport (implies native-tls unless a TLS backend is set)" OFF)
option(OTEL_SDK_OTLP_GRPC
    "Enable OTLP gRPC/tonic transport" OFF)
option(OTEL_SDK_NATIVE_TLS
    "Use platform TLS for OTLP HTTP (default transport, implies otlp-http)" ON)
option(OTEL_SDK_RUSTLS_TLS
    "Use rustls for OTLP HTTP (implies otlp-http; mutually exclusive with NATIVE_TLS)" OFF)
option(OTEL_SDK_GRPC_TLS_RING
    "Enable gRPC TLS using ring and platform roots (implies otlp-grpc)" OFF)
option(OTEL_SDK_OTLP_HTTP_GZIP
    "Enable gzip compression for OTLP HTTP (implies otlp-http)" OFF)
option(OTEL_SDK_OTLP_HTTP_ZSTD
    "Enable zstd compression for OTLP HTTP (implies otlp-http)" OFF)
option(OTEL_SDK_OTLP_GRPC_GZIP
    "Enable gzip compression for OTLP gRPC (implies otlp-grpc)" OFF)
option(OTEL_SDK_OTLP_GRPC_ZSTD
    "Enable zstd compression for OTLP gRPC (implies otlp-grpc)" OFF)
option(OTEL_SDK_METRICS_ASYNC_RUNTIME
    "Enable SDK-owned async periodic Metrics reader" OFF)
option(OTEL_SDK_NO_DEFAULT_FEATURES
    "Start from an empty feature set (transport-free core SDK)" OFF)
set(OTEL_SDK_EXTRA_FEATURES ""
    CACHE STRING
    "Advanced: explicit comma-separated additional Cargo features (e.g. 'otlp-http,rustls-tls')")

# ── Feature validation ────────────────────────────────────────────────────────
if(OTEL_SDK_NATIVE_TLS AND OTEL_SDK_RUSTLS_TLS)
    message(FATAL_ERROR
        "OTEL_SDK_NATIVE_TLS and OTEL_SDK_RUSTLS_TLS are mutually exclusive TLS backends. "
        "Enable at most one.")
endif()

if(OTEL_SDK_GRPC_TLS_RING AND NOT OTEL_SDK_OTLP_GRPC)
    set(OTEL_SDK_OTLP_GRPC ON)
endif()

# ── Resolve Cargo feature flags ───────────────────────────────────────────────
set(_features "")

if(OTEL_SDK_NO_DEFAULT_FEATURES)
    set(_no_default "--no-default-features")
    set(OTEL_SDK_NATIVE_TLS OFF)
else()
    set(_no_default "")
    if(NOT OTEL_SDK_NATIVE_TLS)
        message(WARNING
            "OTEL_SDK_NATIVE_TLS=OFF without OTEL_SDK_NO_DEFAULT_FEATURES=ON: "
            "the Cargo 'default' feature still enables native-tls. "
            "Set OTEL_SDK_NO_DEFAULT_FEATURES=ON and select features explicitly "
            "to build without native-tls.")
    endif()
endif()

macro(_add_feature feat cmake_var)
    if(${cmake_var})
        list(APPEND _features "${feat}")
    endif()
endmacro()

_add_feature("otlp-http"               OTEL_SDK_OTLP_HTTP)
_add_feature("otlp-grpc"               OTEL_SDK_OTLP_GRPC)
_add_feature("native-tls"              OTEL_SDK_NATIVE_TLS)
_add_feature("rustls-tls"              OTEL_SDK_RUSTLS_TLS)
_add_feature("grpc-tls-ring"           OTEL_SDK_GRPC_TLS_RING)
_add_feature("otlp-http-gzip"          OTEL_SDK_OTLP_HTTP_GZIP)
_add_feature("otlp-http-zstd"          OTEL_SDK_OTLP_HTTP_ZSTD)
_add_feature("otlp-grpc-gzip"          OTEL_SDK_OTLP_GRPC_GZIP)
_add_feature("otlp-grpc-zstd"         OTEL_SDK_OTLP_GRPC_ZSTD)
_add_feature("metrics-async-runtime"   OTEL_SDK_METRICS_ASYNC_RUNTIME)

if(OTEL_SDK_EXTRA_FEATURES)
    string(REPLACE "," ";" _extra_list "${OTEL_SDK_EXTRA_FEATURES}")
    list(APPEND _features ${_extra_list})
endif()

list(REMOVE_DUPLICATES _features)

if(_features)
    list(JOIN _features "," _features_joined)
    set(OTEL_C_CARGO_FEATURES_FLAG "--features" "${_features_joined}")
else()
    set(OTEL_C_CARGO_FEATURES_FLAG "")
    set(_features_joined "(default)")
endif()

message(STATUS "Cargo SDK features: ${_features_joined} (no-default=${OTEL_SDK_NO_DEFAULT_FEATURES})")

# ── Build targets ─────────────────────────────────────────────────────────────
set(_cargo_build_env
    "CARGO_TARGET_DIR=${OTEL_C_CARGO_TARGET_DIR}"
)

set(_cargo_build_cmd
    "${CARGO_EXECUTABLE}" build --locked
    -p opentelemetry-c-api
    -p opentelemetry-c-sdk
    ${OTEL_C_CARGO_PROFILE_FLAG}
    ${_no_default}
    ${OTEL_C_CARGO_FEATURES_FLAG}
)

set(OTEL_C_API_LIB_PATH "${OTEL_C_CARGO_LIB_DIR}/${OTEL_C_API_LIB}")
set(OTEL_C_SDK_LIB_PATH "${OTEL_C_CARGO_LIB_DIR}/${OTEL_C_SDK_LIB}")

# Always-run target: Cargo decides whether recompilation is needed based on
# Rust source files, Cargo.toml manifests, Cargo.lock, and build scripts.
# Do NOT use add_custom_command(OUTPUT ...) here — that causes CMake to skip
# the Cargo invocation whenever the output files already exist, leaving stale
# libraries after any Rust source change.
add_custom_target(opentelemetry_c_cargo ALL
    COMMAND ${CMAKE_COMMAND} -E env ${_cargo_build_env}
            ${_cargo_build_cmd}
    WORKING_DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}"
    COMMENT "Building opentelemetry-c API and SDK with Cargo (incremental)"
    BYPRODUCTS
        "${OTEL_C_API_LIB_PATH}"
        "${OTEL_C_SDK_LIB_PATH}"
    VERBATIM
)

# Verify the expected artifacts exist after Cargo runs.
add_custom_command(TARGET opentelemetry_c_cargo POST_BUILD
    COMMAND ${CMAKE_COMMAND} -DPATH=${OTEL_C_API_LIB_PATH} -P
        ${CMAKE_CURRENT_SOURCE_DIR}/cmake/VerifyFile.cmake
    COMMAND ${CMAKE_COMMAND} -DPATH=${OTEL_C_SDK_LIB_PATH} -P
        ${CMAKE_CURRENT_SOURCE_DIR}/cmake/VerifyFile.cmake
    VERBATIM
)

# ── IMPORTED targets for consumers within this build ─────────────────────────
add_library(OpenTelemetryC::api SHARED IMPORTED GLOBAL)
set_target_properties(OpenTelemetryC::api PROPERTIES
    IMPORTED_LOCATION "${OTEL_C_API_LIB_PATH}"
    INTERFACE_INCLUDE_DIRECTORIES "${CMAKE_CURRENT_SOURCE_DIR}/api/include"
)
add_dependencies(OpenTelemetryC::api opentelemetry_c_cargo)

add_library(OpenTelemetryC::sdk SHARED IMPORTED GLOBAL)
set_target_properties(OpenTelemetryC::sdk PROPERTIES
    IMPORTED_LOCATION "${OTEL_C_SDK_LIB_PATH}"
    INTERFACE_INCLUDE_DIRECTORIES
        "${CMAKE_CURRENT_SOURCE_DIR}/api/include;${CMAKE_CURRENT_SOURCE_DIR}/sdk/include"
    INTERFACE_LINK_LIBRARIES OpenTelemetryC::api
)
add_dependencies(OpenTelemetryC::sdk opentelemetry_c_cargo)

# ── Header installation ───────────────────────────────────────────────────────
set(OTEL_C_API_INCLUDE_DIR "${CMAKE_CURRENT_SOURCE_DIR}/api/include/opentelemetry_c")
set(OTEL_C_SDK_INCLUDE_DIR "${CMAKE_CURRENT_SOURCE_DIR}/sdk/include/opentelemetry_c")

# Detect duplicate header names between api and sdk include directories.
file(GLOB _api_headers RELATIVE "${OTEL_C_API_INCLUDE_DIR}" "${OTEL_C_API_INCLUDE_DIR}/*.h")
file(GLOB _sdk_headers RELATIVE "${OTEL_C_SDK_INCLUDE_DIR}" "${OTEL_C_SDK_INCLUDE_DIR}/*.h")
foreach(_h ${_api_headers})
    if("${_h}" IN_LIST _sdk_headers)
        file(SHA256 "${OTEL_C_API_INCLUDE_DIR}/${_h}" _api_sha)
        file(SHA256 "${OTEL_C_SDK_INCLUDE_DIR}/${_h}" _sdk_sha)
        if(NOT _api_sha STREQUAL _sdk_sha)
            message(FATAL_ERROR
                "Duplicate header '${_h}' found in both api/include and sdk/include "
                "with different content. Resolve the conflict before installing.")
        else()
            message(STATUS "Duplicate header '${_h}' found in api+sdk include dirs (byte-identical, OK)")
        endif()
    endif()
endforeach()

install(
    DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}/api/include/opentelemetry_c"
    DESTINATION "${CMAKE_INSTALL_INCLUDEDIR}"
    FILES_MATCHING PATTERN "*.h"
)
install(
    DIRECTORY "${CMAKE_CURRENT_SOURCE_DIR}/sdk/include/opentelemetry_c"
    DESTINATION "${CMAKE_INSTALL_INCLUDEDIR}"
    FILES_MATCHING PATTERN "*.h"
)

# ── Library installation ──────────────────────────────────────────────────────
install(
    FILES
        "${OTEL_C_API_LIB_PATH}"
        "${OTEL_C_SDK_LIB_PATH}"
    DESTINATION "${CMAKE_INSTALL_LIBDIR}"
)

# ── pkg-config ────────────────────────────────────────────────────────────────
configure_file(
    "${CMAKE_CURRENT_SOURCE_DIR}/cmake/opentelemetry-c-api.pc.in"
    "${CMAKE_CURRENT_BINARY_DIR}/opentelemetry-c-api.pc"
    @ONLY
)
configure_file(
    "${CMAKE_CURRENT_SOURCE_DIR}/cmake/opentelemetry-c-sdk.pc.in"
    "${CMAKE_CURRENT_BINARY_DIR}/opentelemetry-c-sdk.pc"
    @ONLY
)
install(
    FILES
        "${CMAKE_CURRENT_BINARY_DIR}/opentelemetry-c-api.pc"
        "${CMAKE_CURRENT_BINARY_DIR}/opentelemetry-c-sdk.pc"
    DESTINATION "${CMAKE_INSTALL_LIBDIR}/pkgconfig"
)

# ── CMake package configuration ───────────────────────────────────────────────
set(OTEL_C_CMAKE_CONFIG_DIR "${CMAKE_INSTALL_LIBDIR}/cmake/OpenTelemetryC")

configure_package_config_file(
    "${CMAKE_CURRENT_SOURCE_DIR}/cmake/OpenTelemetryCConfig.cmake.in"
    "${CMAKE_CURRENT_BINARY_DIR}/OpenTelemetryCConfig.cmake"
    INSTALL_DESTINATION "${OTEL_C_CMAKE_CONFIG_DIR}"
    # PATH_VARS bakes the configured lib/include dir names (e.g. lib64, include)
    # into the installed config and relocates them relative to the install prefix,
    # so the package works correctly after installation or prefix relocation.
    PATH_VARS CMAKE_INSTALL_LIBDIR CMAKE_INSTALL_INCLUDEDIR
    NO_CHECK_REQUIRED_COMPONENTS_MACRO
)

write_basic_package_version_file(
    "${CMAKE_CURRENT_BINARY_DIR}/OpenTelemetryCConfigVersion.cmake"
    VERSION "${OTEL_C_VERSION}"
    # Pre-1.0 policy (VERSIONING.md): different 0.x minor versions are incompatible.
    # SameMinorVersion requires the same major.minor — patch upgrades are compatible.
    COMPATIBILITY SameMinorVersion
)

install(
    FILES
        "${CMAKE_CURRENT_BINARY_DIR}/OpenTelemetryCConfig.cmake"
        "${CMAKE_CURRENT_BINARY_DIR}/OpenTelemetryCConfigVersion.cmake"
    DESTINATION "${OTEL_C_CMAKE_CONFIG_DIR}"
)

# ── Documentation installation ────────────────────────────────────────────────
foreach(_doc README.md VERSIONING.md RELEASING.md SECURITY.md LICENSE)
    if(EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/${_doc}")
        install(FILES "${CMAKE_CURRENT_SOURCE_DIR}/${_doc}"
            DESTINATION "${CMAKE_INSTALL_DATADIR}/doc/opentelemetry-c")
    endif()
endforeach()

if(EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/docs/BUILDING.md")
    install(FILES "${CMAKE_CURRENT_SOURCE_DIR}/docs/BUILDING.md"
        DESTINATION "${CMAKE_INSTALL_DATADIR}/doc/opentelemetry-c")
endif()

if(EXISTS "${CMAKE_CURRENT_SOURCE_DIR}/docs/PACKAGING.md")
    install(FILES "${CMAKE_CURRENT_SOURCE_DIR}/docs/PACKAGING.md"
        DESTINATION "${CMAKE_INSTALL_DATADIR}/doc/opentelemetry-c")
endif()

foreach(_compliance METRICS_COMPLIANCE LOGS_COMPLIANCE TRACES_COMPLIANCE)
    set(_cf "${CMAKE_CURRENT_SOURCE_DIR}/${_compliance}.md")
    if(EXISTS "${_cf}")
        install(FILES "${_cf}"
            DESTINATION "${CMAKE_INSTALL_DATADIR}/doc/opentelemetry-c")
    endif()
endforeach()

# ── Uninstall target ──────────────────────────────────────────────────────────
configure_file(
    "${CMAKE_CURRENT_SOURCE_DIR}/cmake/cmake_uninstall.cmake.in"
    "${CMAKE_CURRENT_BINARY_DIR}/cmake_uninstall.cmake"
    IMMEDIATE @ONLY
)
add_custom_target(uninstall
    COMMAND "${CMAKE_COMMAND}" -P "${CMAKE_CURRENT_BINARY_DIR}/cmake_uninstall.cmake"
    COMMENT "Removing installed files listed in install_manifest.txt"
)
