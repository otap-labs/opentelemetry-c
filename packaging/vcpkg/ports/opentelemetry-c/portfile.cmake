# SPDX-License-Identifier: Apache-2.0

# vcpkg overlay port for opentelemetry-c
#
# LOCAL SOURCE port — builds from the repository source tree rather than
# downloading a tarball. Intended for use as a vcpkg overlay port.
#
# Usage (from repository root):
#   vcpkg install opentelemetry-c \
#     --overlay-ports=packaging/vcpkg/ports \
#     --overlay-triplets=packaging/vcpkg/triplets   # optional custom triplets
#
# See docs/PACKAGING.md for full instructions.

set(VCPKG_BUILD_TYPE release)

# This port builds shared libraries only. Static deployment is not supported.
set(VCPKG_LIBRARY_LINKAGE dynamic)

# Require a local source path. The port is not published to the vcpkg registry
# so there is no upstream tarball and no SHA512 checksum.
# Consumers must point vcpkg to the repository root via OTELC_SOURCE_PATH.
if(NOT DEFINED OTELC_SOURCE_PATH)
    # Default: two levels up from this portfile (packaging/vcpkg/ports/opentelemetry-c/)
    get_filename_component(OTELC_SOURCE_PATH "${CMAKE_CURRENT_LIST_DIR}/../../../.." ABSOLUTE)
endif()

if(NOT EXISTS "${OTELC_SOURCE_PATH}/CMakeLists.txt")
    message(FATAL_ERROR
        "opentelemetry-c portfile: OTELC_SOURCE_PATH='${OTELC_SOURCE_PATH}' does not contain "
        "CMakeLists.txt. Set OTELC_SOURCE_PATH to the repository root or run from within the "
        "repository tree.")
endif()

vcpkg_check_features(OUT_FEATURE_OPTIONS FEATURE_OPTIONS
    FEATURES
        "otlp-http"   OTEL_SDK_OTLP_HTTP
        "otlp-grpc"   OTEL_SDK_OTLP_GRPC
        "native-tls"  OTEL_SDK_NATIVE_TLS
        "rustls-tls"  OTEL_SDK_RUSTLS_TLS
        "grpc-tls"    OTEL_SDK_GRPC_TLS_RING
)

vcpkg_cmake_configure(
    SOURCE_PATH "${OTELC_SOURCE_PATH}"
    OPTIONS
        -DOTEL_SDK_NO_DEFAULT_FEATURES=ON
        ${FEATURE_OPTIONS}
)

vcpkg_cmake_install()

vcpkg_cmake_config_fixup(
    PACKAGE_NAME OpenTelemetryC
    CONFIG_PATH lib/cmake/OpenTelemetryC
)

vcpkg_fixup_pkgconfig()

# Remove debug artifacts (this is a release-only port)
file(REMOVE_RECURSE "${CURRENT_PACKAGES_DIR}/debug")

# Install license
file(INSTALL "${OTELC_SOURCE_PATH}/LICENSE"
    DESTINATION "${CURRENT_PACKAGES_DIR}/share/${PORT}"
    RENAME copyright)

# Usage file shown to consumers after `vcpkg install`
file(WRITE "${CURRENT_PACKAGES_DIR}/share/${PORT}/usage"
"opentelemetry-c is installed. To use in a CMake project:

  find_package(OpenTelemetryC CONFIG REQUIRED)
  target_link_libraries(my_target PRIVATE OpenTelemetryC::api)
  # or for SDK consumers:
  target_link_libraries(my_target PRIVATE OpenTelemetryC::sdk)

Set the library search path at runtime:
  Linux:  export LD_LIBRARY_PATH=<vcpkg-root>/installed/<triplet>/lib
  macOS:  export DYLD_LIBRARY_PATH=<vcpkg-root>/installed/<triplet>/lib
")
